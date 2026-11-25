// SPDX-License-Identifier: GPL-3.0-or-later
//! Controlled CPU-only queued backend tests. No GPU execution or sleeps.
use std::sync::{Arc, Mutex};
use xenora_loader::GuestMemory;
use xenora_loader::graphics::*;

#[derive(Default)]
struct TicketState {
    result: Option<Result<GraphicsPassOutput, GraphicsError>>,
    rejected: Vec<GraphicsQueuePoint>,
    drops: usize,
}
struct Ticket(Arc<Mutex<TicketState>>);
impl GraphicsQueuedPendingSubmission for Ticket {
    fn poll(&mut self) -> Option<Result<GraphicsPassOutput, GraphicsError>> {
        self.0.lock().unwrap().result.take()
    }
    fn wait(&mut self) -> Result<GraphicsPassOutput, GraphicsError> {
        self.poll()
            .expect("release controlled completion before wait")
    }
    fn reject_dependency(&mut self, point: GraphicsQueuePoint) {
        self.0.lock().unwrap().rejected.push(point);
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.0.lock().unwrap().drops += 1;
    }
}
#[derive(Default)]
struct Backend {
    requests: Vec<GraphicsQueuedSubmissionRequest>,
    tickets: Vec<Arc<Mutex<TicketState>>>,
}
struct Harness {
    ctx: GraphicsContext,
    backend: Arc<Mutex<Backend>>,
    memory: GuestMemory,
    device: GraphicsHandle,
    queues: [GraphicsHandle; 2],
    allocators: [GraphicsHandle; 2],
    lists: [GraphicsHandle; 2],
    targets: [GraphicsHandle; 2],
    vertex: GraphicsHandle,
}
impl Harness {
    fn new() -> Self {
        let mut ctx = GraphicsContext::new();
        let device = ctx.create_device().unwrap();
        let queues = std::array::from_fn(|_| {
            ctx.create_command_queue(device, CommandListType::Direct)
                .unwrap()
        });
        let allocators = std::array::from_fn(|_| {
            ctx.create_command_allocator(device, CommandListType::Direct)
                .unwrap()
        });
        let lists = std::array::from_fn(|i| {
            ctx.create_command_list(device, allocators[i], CommandListType::Direct)
                .unwrap()
        });
        let targets = std::array::from_fn(|_| {
            ctx.create_committed_resource(
                device,
                ResourceDesc {
                    dimension: ResourceDimension::Texture2D,
                    width: 1,
                    height: 1,
                    depth_or_array_size: 1,
                    mip_levels: 1,
                    format_rgba8: true,
                },
                ResourceStates::RENDER_TARGET,
                None,
            )
            .unwrap()
        });
        let vertex = ctx
            .create_committed_resource(
                device,
                ResourceDesc {
                    dimension: ResourceDimension::Buffer,
                    width: 48,
                    height: 1,
                    depth_or_array_size: 1,
                    mip_levels: 1,
                    format_rgba8: false,
                },
                ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
                None,
            )
            .unwrap();
        let backend = Arc::new(Mutex::new(Backend::default()));
        let capture = backend.clone();
        ctx.set_metal_queued_backend(move |request| {
            let ticket = Arc::new(Mutex::new(TicketState::default()));
            let mut backend = capture.lock().unwrap();
            backend.requests.push(request);
            backend.tickets.push(ticket.clone());
            Ok(Box::new(Ticket(ticket)))
        });
        Self {
            ctx,
            backend,
            memory: GuestMemory::new(),
            device,
            queues,
            allocators,
            lists,
            targets,
            vertex,
        }
    }
    fn producer(&mut self) {
        self.ctx
            .cmd_set_color_targets(self.lists[0], &[self.targets[0]], &[[17, 31, 47, 255]])
            .unwrap();
        self.ctx
            .cmd_resource_barrier(
                self.lists[0],
                self.targets[0],
                ResourceStates::RENDER_TARGET,
                ResourceStates::PIXEL_SHADER_RESOURCE,
            )
            .unwrap();
        self.ctx.close_command_list(self.lists[0]).unwrap();
    }
    fn consumer(&mut self) {
        let pipeline = self
            .ctx
            .create_graphics_pipeline(
                self.device,
                include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin"),
                include_bytes!("../../../fixtures/rdna2/x9-sampled-arithmetic-ps.bin"),
            )
            .unwrap();
        let list = self.lists[1];
        self.ctx
            .cmd_set_color_targets(list, &[self.targets[1]], &[[0; 4]])
            .unwrap();
        self.ctx.cmd_set_viewport(list, 1, 1).unwrap();
        self.ctx
            .cmd_set_graphics_pipeline(list, Some(pipeline))
            .unwrap();
        self.ctx
            .cmd_set_sampled_texture(
                list,
                Some(self.targets[0]),
                xenora_texture::SamplerDesc::nearest_clamp(),
            )
            .unwrap();
        self.ctx
            .cmd_set_vertex_buffer(list, self.vertex, 0, 16)
            .unwrap();
        self.ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
        self.ctx.close_command_list(list).unwrap();
    }
    fn submit(
        &mut self,
        slot: usize,
        waits: &[GraphicsQueuePoint],
    ) -> Result<GraphicsHandle, GraphicsError> {
        self.ctx
            .submit_command_list_after(self.queues[slot], self.lists[slot], waits, &self.memory)
    }
    fn complete(&self, slot: usize, rgba: [u8; 4]) {
        self.backend.lock().unwrap().tickets[slot]
            .lock()
            .unwrap()
            .result = Some(Ok(GraphicsPassOutput {
            color_layouts: None,
            colors: vec![rgba.to_vec()],
            depth: None,
        }));
    }
    fn reset(&mut self, slot: usize) {
        self.ctx
            .reset_command_list(self.lists[slot], self.allocators[slot])
            .unwrap();
    }
}

#[test]
fn cpu_queued_produced_source_planned_state_and_dependent_first_collection() {
    let mut h = Harness::new();
    h.producer();
    h.consumer();
    let a = h.submit(0, &[]).unwrap();
    let point = h.ctx.submission_point(a).unwrap();
    assert!(h.submit(1, &[]).is_err()); // Committed host state is still RT.
    let b = h.submit(1, &[point]).unwrap();
    {
        let backend = h.backend.lock().unwrap();
        assert_eq!(backend.requests.len(), 2);
        assert_eq!(backend.requests[1].waits, [point]);
        match &backend.requests[1].draws[0].textures[0] {
            Some(GraphicsTextureInput::Produced {
                version,
                width,
                height,
                mip_levels,
            }) => {
                assert_eq!(
                    *version,
                    GraphicsProducedVersion {
                        point,
                        output: GraphicsProducedOutput::Color(0)
                    }
                );
                assert_eq!((*width, *height, *mip_levels), (1, 1, 1));
            }
            _ => panic!("consumer must reference native output, never old host pixels"),
        }
    }
    h.complete(1, [8, 31, 47, 255]);
    assert_eq!(
        h.ctx.poll_submission(b).unwrap(),
        GraphicsSubmissionStatus::Pending
    );
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    h.complete(0, [17, 31, 47, 255]);
    assert_eq!(
        h.ctx.poll_submission(b).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    assert_eq!(
        h.ctx.last_presentation.lock().unwrap().pixels,
        [8, 31, 47, 255]
    );
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 2);
    h.ctx.release_submission(a).unwrap();
    assert_eq!(
        h.backend.lock().unwrap().tickets[0].lock().unwrap().drops,
        0
    );
    assert!(h.ctx.poll_submission(a).is_err());
    h.ctx.release_submission(b).unwrap();
    assert_eq!(
        h.backend.lock().unwrap().tickets[0].lock().unwrap().drops,
        1
    );
    assert_eq!(
        h.backend.lock().unwrap().tickets[1].lock().unwrap().drops,
        1
    );
}

#[test]
fn cpu_queued_failed_producer_drains_consumer_and_recovers_new_generation() {
    let mut h = Harness::new();
    h.producer();
    h.consumer();
    let a = h.submit(0, &[]).unwrap();
    let point = h.ctx.submission_point(a).unwrap();
    let b = h.submit(1, &[point]).unwrap();
    // Wrong output length is a loader publication failure, even after native success.
    h.backend.lock().unwrap().tickets[0].lock().unwrap().result = Some(Ok(GraphicsPassOutput {
        color_layouts: None,
        colors: vec![vec![1]],
        depth: None,
    }));
    assert_eq!(
        h.ctx.poll_submission(a).unwrap(),
        GraphicsSubmissionStatus::Failed
    );
    assert_eq!(
        h.backend.lock().unwrap().tickets[1]
            .lock()
            .unwrap()
            .rejected,
        [point]
    );
    assert_eq!(
        h.ctx.poll_submission(b).unwrap(),
        GraphicsSubmissionStatus::Pending
    );
    h.complete(1, [99; 4]);
    assert_eq!(
        h.ctx.wait_submission(b).unwrap(),
        GraphicsSubmissionStatus::Failed
    );
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    h.ctx.release_submission(a).unwrap();
    h.ctx.release_submission(b).unwrap();
    h.reset(0);
    h.producer();
    let successor = h.submit(0, &[]).unwrap();
    {
        let backend = h.backend.lock().unwrap();
        assert_eq!(backend.requests[2].generation, 2);
        assert_eq!(backend.requests[2].point.value, 2);
    }
    h.complete(2, [17, 31, 47, 255]);
    assert_eq!(
        h.ctx.wait_submission(successor).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    h.ctx.release_submission(successor).unwrap();
}

#[test]
fn cpu_queued_destroyed_producer_output_keeps_ticket_until_descendant_release() {
    let mut h = Harness::new();
    h.producer();
    h.consumer();
    let a = h.submit(0, &[]).unwrap();
    let point = h.ctx.submission_point(a).unwrap();
    let b = h.submit(1, &[point]).unwrap();
    h.reset(0);
    h.reset(1);
    h.ctx.destroy_resource(h.targets[0]).unwrap();
    h.ctx.destroy_resource(h.targets[1]).unwrap();
    h.ctx.destroy_resource(h.vertex).unwrap();
    h.complete(0, [17, 31, 47, 255]);
    h.complete(1, [8, 31, 47, 255]);
    assert_eq!(
        h.ctx.wait_submission(b).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    assert!(h.ctx.destroy_resource(h.targets[0]).is_err());
    assert_eq!(
        h.ctx.submission_outcome(b).unwrap().unwrap().pixels,
        [8, 31, 47, 255]
    );
    h.ctx.release_submission(a).unwrap();
    h.ctx.release_submission(b).unwrap();
}

#[test]
fn cpu_queued_capacity_future_points_and_legacy_producer_load_reject_before_dispatch() {
    let mut h = Harness::new();
    h.producer();
    h.consumer();
    let a = h.submit(0, &[]).unwrap();
    let point = h.ctx.submission_point(a).unwrap();
    assert!(h.submit(0, &[]).is_err());
    assert!(
        h.submit(
            1,
            &[GraphicsQueuePoint {
                value: point.value + 1,
                ..point
            }]
        )
        .is_err()
    );
    assert!(h.submit(1, &[point, point]).is_err());
    h.reset(1);
    h.ctx
        .cmd_resource_barrier(
            h.lists[1],
            h.targets[0],
            ResourceStates::PIXEL_SHADER_RESOURCE,
            ResourceStates::RENDER_TARGET,
        )
        .unwrap();
    h.ctx
        .cmd_set_color_targets_with_loads(h.lists[1], &[h.targets[0]], &[GraphicsColorLoad::Load])
        .unwrap();
    h.ctx.close_command_list(h.lists[1]).unwrap();
    assert!(h.submit(1, &[point]).is_err());
    assert_eq!(h.backend.lock().unwrap().requests.len(), 1);
    h.complete(0, [17, 31, 47, 255]);
    h.ctx.wait_submission(a).unwrap();
    h.ctx.release_submission(a).unwrap();
}

#[test]
fn cpu_queued_independent_reverse_collection_does_not_regress_presentation() {
    let mut h = Harness::new();
    for slot in 0..2 {
        h.ctx
            .cmd_set_color_targets(h.lists[slot], &[h.targets[slot]], &[[0; 4]])
            .unwrap();
        h.ctx.close_command_list(h.lists[slot]).unwrap();
    }
    let a = h.submit(0, &[]).unwrap();
    let b = h.submit(1, &[]).unwrap();
    assert!(h.submit(0, &[]).is_err());
    h.complete(1, [2, 3, 5, 7]);
    assert_eq!(
        h.ctx.poll_submission(b).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    h.complete(0, [11, 13, 17, 19]);
    assert_eq!(
        h.ctx.poll_submission(a).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    assert_eq!(h.ctx.last_presentation.lock().unwrap().pixels, [2, 3, 5, 7]);
    assert_eq!(
        h.ctx.submission_outcome(a).unwrap().unwrap().pixels,
        [11, 13, 17, 19]
    );
    h.ctx.release_submission(a).unwrap();
    h.ctx.release_submission(b).unwrap();
}

#[test]
fn cpu_queued_collected_producer_still_supplies_version_and_implicit_queue_wait() {
    let mut h = Harness::new();
    h.producer();
    h.consumer();
    let a = h.submit(0, &[]).unwrap();
    let point = h.ctx.submission_point(a).unwrap();
    h.complete(0, [17, 31, 47, 255]);
    h.ctx.wait_submission(a).unwrap();
    // Queue0 is now free; queue order supplies A without an explicit wait argument.
    let b = h
        .ctx
        .submit_command_list_after(h.queues[0], h.lists[1], &[], &h.memory)
        .unwrap();
    {
        let backend = h.backend.lock().unwrap();
        assert_eq!(backend.requests[1].waits, [point]);
        assert!(matches!(
            backend.requests[1].draws[0].textures[0],
            Some(GraphicsTextureInput::Produced { .. })
        ));
    }
    h.ctx.release_submission(a).unwrap();
    assert_eq!(
        h.backend.lock().unwrap().tickets[0].lock().unwrap().drops,
        0
    );
    h.complete(1, [8, 31, 47, 255]);
    h.ctx.wait_submission(b).unwrap();
    h.ctx.release_submission(b).unwrap();
}

#[test]
fn cpu_queued_cached_consumer_success_cannot_publish_after_producer_rejection() {
    let mut h = Harness::new();
    h.producer();
    h.consumer();
    let a = h.submit(0, &[]).unwrap();
    let point = h.ctx.submission_point(a).unwrap();
    let b = h.submit(1, &[point]).unwrap();
    // The worker may already have queued success for B before the loader discovers
    // A's malformed output. A later rejection cannot retract that channel message.
    h.complete(1, [99; 4]);
    h.backend.lock().unwrap().tickets[0].lock().unwrap().result = Some(Ok(GraphicsPassOutput {
        color_layouts: None,
        colors: vec![vec![1]],
        depth: None,
    }));
    assert_eq!(
        h.ctx.poll_submission(b).unwrap(),
        GraphicsSubmissionStatus::Failed
    );
    assert_eq!(
        h.ctx.poll_submission(a).unwrap(),
        GraphicsSubmissionStatus::Failed
    );
    assert!(h.ctx.submission_outcome(b).unwrap().is_none());
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    assert_eq!(
        h.backend.lock().unwrap().tickets[1]
            .lock()
            .unwrap()
            .rejected,
        [point]
    );
    h.ctx.release_submission(a).unwrap();
    h.ctx.release_submission(b).unwrap();
}

#[test]
fn cpu_queued_view_preserves_producer_provenance_after_reset_and_view_destruction() {
    let mut h = Harness::new();
    let capture = h.backend.clone();
    h.ctx.set_metal_view_queued_backend(move |request| {
        let ticket = Arc::new(Mutex::new(TicketState::default()));
        let mut backend = capture.lock().unwrap();
        backend.requests.push(request);
        backend.tickets.push(ticket.clone());
        Ok(Box::new(Ticket(ticket)))
    });
    h.producer();
    let desc = GraphicsTextureViewDesc {
        base_mip: 0,
        mip_count: 1,
        format: GraphicsTextureViewFormat::Rgba8UnormSrgb,
    };
    let view = h
        .ctx
        .create_texture_view(h.device, h.targets[0], desc)
        .unwrap();
    let pipeline = h
        .ctx
        .create_graphics_pipeline(
            h.device,
            include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin"),
            include_bytes!("../../../fixtures/rdna2/x9-sampled-arithmetic-ps.bin"),
        )
        .unwrap();
    let list = h.lists[1];
    h.ctx
        .cmd_set_color_targets(list, &[h.targets[1]], &[[0; 4]])
        .unwrap();
    h.ctx.cmd_set_viewport(list, 1, 1).unwrap();
    h.ctx
        .cmd_set_graphics_pipeline(list, Some(pipeline))
        .unwrap();
    h.ctx
        .cmd_set_sampled_views(
            list,
            [Some(view), None],
            [Some(xenora_texture::SamplerDesc::nearest_clamp()), None],
        )
        .unwrap();
    h.ctx.cmd_set_vertex_buffer(list, h.vertex, 0, 16).unwrap();
    h.ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
    h.ctx.close_command_list(list).unwrap();
    let a = h.submit(0, &[]).unwrap();
    let point = h.ctx.submission_point(a).unwrap();
    assert!(h.submit(1, &[]).is_err());
    let b = h.submit(1, &[point]).unwrap();
    {
        let backend = h.backend.lock().unwrap();
        let draw = &backend.requests[1].draws[0];
        assert_eq!(draw.views, [Some(desc), None]);
        assert!(
            matches!(&draw.textures[0], Some(GraphicsTextureInput::Produced { version, width: 1, height: 1, mip_levels: 1 }) if *version == GraphicsProducedVersion { point, output: GraphicsProducedOutput::Color(0) })
        );
    }
    assert!(h.ctx.destroy_texture_view(view).is_err());
    h.reset(0);
    h.reset(1);
    assert!(h.ctx.destroy_resource(h.targets[0]).is_err());
    h.ctx.destroy_texture_view(view).unwrap();
    h.ctx.destroy_resource(h.targets[0]).unwrap();
    h.complete(0, [17, 31, 47, 255]);
    h.complete(1, [1, 3, 7, 255]);
    assert_eq!(
        h.ctx.wait_submission(b).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    h.ctx.release_submission(a).unwrap();
    h.ctx.release_submission(b).unwrap();
}

#[test]
fn cpu_queued_typed_host_bc_coexists_with_produced_rgba_and_retires_after_reset() {
    use xenora_texture::TextureFormat;
    let mut h = Harness::new();
    let capture = h.backend.clone();
    h.ctx.set_metal_typed_queued_backend(
        GraphicsTextureCapabilities {
            r8: false,
            bc1: true,
            bc3: false,
        },
        move |request| {
            let ticket = Arc::new(Mutex::new(TicketState::default()));
            let mut backend = capture.lock().unwrap();
            backend.requests.push(request);
            backend.tickets.push(ticket.clone());
            Ok(Box::new(Ticket(ticket)))
        },
    );
    h.producer();
    let bc = h
        .ctx
        .create_texture_resource(
            h.device,
            GraphicsTextureDesc {
                width: 4,
                height: 4,
                mip_levels: 1,
                format: TextureFormat::Bc1RgbaUnorm,
            },
            ResourceStates::PIXEL_SHADER_RESOURCE,
        )
        .unwrap();
    let pipeline = h
        .ctx
        .create_graphics_pipeline(
            h.device,
            include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin"),
            include_bytes!("../../../fixtures/rdna2/x9-multislot-ps.bin"),
        )
        .unwrap();
    let list = h.lists[1];
    h.ctx
        .cmd_set_color_targets(list, &[h.targets[1]], &[[0; 4]])
        .unwrap();
    h.ctx.cmd_set_viewport(list, 1, 1).unwrap();
    h.ctx
        .cmd_set_graphics_pipeline(list, Some(pipeline))
        .unwrap();
    h.ctx
        .cmd_set_sampled_bindings(
            list,
            [Some(bc), Some(h.targets[0])],
            [Some(xenora_texture::SamplerDesc::nearest_clamp()); 2],
        )
        .unwrap();
    h.ctx.cmd_set_vertex_buffer(list, h.vertex, 0, 16).unwrap();
    h.ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
    h.ctx.close_command_list(list).unwrap();
    let a = h.submit(0, &[]).unwrap();
    let point = h.ctx.submission_point(a).unwrap();
    let b = h.submit(1, &[point]).unwrap();
    {
        let backend = h.backend.lock().unwrap();
        let draw = &backend.requests[1].draws[0];
        match &draw.textures[0] {
            Some(GraphicsTextureInput::Host(snapshot)) => {
                assert_eq!(snapshot.format, TextureFormat::Bc1RgbaUnorm);
                assert_eq!(&*snapshot.pixels, &[0; 8]);
            }
            _ => panic!("BC input must preserve compressed host snapshot"),
        }
        assert!(
            matches!(&draw.textures[1], Some(GraphicsTextureInput::Produced { version, .. }) if version.point == point)
        );
    }
    assert!(h.ctx.destroy_resource(bc).is_err());
    h.reset(0);
    h.reset(1);
    h.ctx.destroy_resource(bc).unwrap();
    h.complete(0, [17, 31, 47, 255]);
    h.complete(1, [0, 31, 47, 255]);
    assert_eq!(
        h.ctx.wait_submission(b).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    h.ctx.release_submission(a).unwrap();
    h.ctx.release_submission(b).unwrap();
}

#[test]
fn cpu_queued_uniform_downgrades_and_missing_later_draw_leave_no_point_or_publication() {
    for mode in 0..4 {
        let mut h = Harness::new();
        // Seed an observable prior presentation without a GPU or pending ticket.
        h.ctx.set_metal_attachment_backend(|_, _, _, _, _| {
            panic!("clear-only pass needs no CPU callback")
        });
        h.ctx
            .cmd_set_color_targets(h.lists[0], &[h.targets[0]], &[[17, 31, 47, 255]])
            .unwrap();
        h.ctx.cmd_set_viewport(h.lists[0], 1, 1).unwrap();
        h.ctx.close_command_list(h.lists[0]).unwrap();
        h.ctx
            .execute_command_list(h.queues[0], h.lists[0], &h.memory)
            .unwrap();
        h.reset(0);
        let before = h.ctx.last_presentation.lock().unwrap().clone();
        assert_eq!(before.pixels, [17, 31, 47, 255]);
        let pipeline = h
            .ctx
            .create_graphics_pipeline(
                h.device,
                include_bytes!("../../../fixtures/rdna2/x9-runtime-uniform-vs.bin"),
                include_bytes!("../../../fixtures/rdna2/x9-runtime-uniform-ps.bin"),
            )
            .unwrap();
        h.ctx
            .set_metal_uniform_queued_backend(GraphicsTextureCapabilities::default(), |_| {
                panic!("CPU-only: invalid batch reached uniform queued backend")
            });
        match mode {
            0 => {}
            1 => h
                .ctx
                .set_metal_queued_backend(|_| panic!("downgraded queued backend called")),
            2 => h
                .ctx
                .set_metal_view_queued_backend(|_| panic!("downgraded view queued backend called")),
            _ => h
                .ctx
                .set_metal_typed_queued_backend(GraphicsTextureCapabilities::default(), |_| {
                    panic!("downgraded typed queued backend called")
                }),
        }
        let list = h.lists[0];
        h.ctx
            .cmd_set_color_targets(list, &[h.targets[0]], &[[0; 4]])
            .unwrap();
        h.ctx.cmd_set_viewport(list, 1, 1).unwrap();
        h.ctx
            .cmd_set_graphics_pipeline(list, Some(pipeline))
            .unwrap();
        h.ctx.cmd_set_vertex_buffer(list, h.vertex, 0, 16).unwrap();
        h.ctx.cmd_set_shader_uniforms(list, Some([0; 8])).unwrap();
        h.ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
        if mode == 0 {
            h.ctx.cmd_set_shader_uniforms(list, None).unwrap();
            h.ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
        }
        h.ctx.close_command_list(list).unwrap();
        let marker = h.ctx.create_device().unwrap();
        assert_eq!(
            h.submit(0, &[]).unwrap_err(),
            GraphicsError::InvalidDescriptor
        );
        assert_eq!(
            h.ctx.create_device().unwrap().id,
            marker.id + 1,
            "failed queued validation consumed a receipt handle"
        );
        let after = h.ctx.last_presentation.lock().unwrap().clone();
        assert_eq!(after.frame_index, before.frame_index);
        assert_eq!(after.pixels, before.pixels);
        assert!(h.backend.lock().unwrap().requests.is_empty());
        // Reinstall and submit on the same queue: neither a node nor point was consumed.
        h.reset(0);
        h.ctx
            .cmd_set_color_targets(list, &[h.targets[0]], &[[0; 4]])
            .unwrap();
        h.ctx.cmd_set_viewport(list, 1, 1).unwrap();
        h.ctx
            .cmd_set_graphics_pipeline(list, Some(pipeline))
            .unwrap();
        h.ctx.cmd_set_vertex_buffer(list, h.vertex, 0, 16).unwrap();
        h.ctx.cmd_set_shader_uniforms(list, Some([9; 8])).unwrap();
        h.ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
        h.ctx.close_command_list(list).unwrap();
        let capture = h.backend.clone();
        h.ctx.set_metal_uniform_queued_backend(
            GraphicsTextureCapabilities::default(),
            move |request| {
                assert_eq!(request.draws.len(), 1);
                assert_eq!(request.draws[0].uniforms, Some([9; 8]));
                let ticket = Arc::new(Mutex::new(TicketState::default()));
                let mut backend = capture.lock().unwrap();
                backend.requests.push(request);
                backend.tickets.push(ticket.clone());
                Ok(Box::new(Ticket(ticket)))
            },
        );
        let receipt = h.submit(0, &[]).unwrap();
        assert_eq!(h.ctx.submission_point(receipt).unwrap().value, 1);
        assert_eq!(h.backend.lock().unwrap().requests.len(), 1);
        h.complete(0, [53, 59, 61, 255]);
        assert_eq!(
            h.ctx.wait_submission(receipt).unwrap(),
            GraphicsSubmissionStatus::Complete
        );
        h.ctx.release_submission(receipt).unwrap();
        assert_eq!(
            h.ctx.last_presentation.lock().unwrap().pixels,
            [53, 59, 61, 255]
        );
    }
}

impl Harness {
    fn enable_produced_loads(&mut self) {
        let capture = self.backend.clone();
        self.ctx.set_metal_produced_color_queued_backend(
            GraphicsTextureCapabilities::default(),
            move |request| {
                let ticket = Arc::new(Mutex::new(TicketState::default()));
                let mut backend = capture.lock().unwrap();
                backend.requests.push(request);
                backend.tickets.push(ticket.clone());
                Ok(Box::new(Ticket(ticket)))
            },
        );
    }

    fn attachment_pass(
        &mut self,
        slot: usize,
        targets: &[GraphicsHandle],
        loads: &[GraphicsColorLoad],
        before: ResourceStates,
    ) {
        let list = self.lists[slot];
        if before != ResourceStates::RENDER_TARGET {
            for target in targets {
                self.ctx
                    .cmd_resource_barrier(list, *target, before, ResourceStates::RENDER_TARGET)
                    .unwrap();
            }
        }
        self.ctx
            .cmd_set_color_targets_with_loads(list, targets, loads)
            .unwrap();
        for target in targets {
            self.ctx
                .cmd_resource_barrier(
                    list,
                    *target,
                    ResourceStates::RENDER_TARGET,
                    ResourceStates::COPY_SOURCE,
                )
                .unwrap();
        }
        self.ctx.close_command_list(list).unwrap();
    }

    fn complete_colors(&self, index: usize, colors: Vec<Vec<u8>>) {
        self.backend.lock().unwrap().tickets[index]
            .lock()
            .unwrap()
            .result = Some(Ok(GraphicsPassOutput {
            color_layouts: None,
            colors,
            depth: None,
        }));
    }

    fn read_color(&mut self, target: GraphicsHandle) -> [u8; 4] {
        use xenora_loader::MemoryPermissions;
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 4, MemoryPermissions::READ_WRITE)
            .unwrap();
        self.ctx
            .copy_resource_to_guest(target, 0, 0x1000, 4, &mut memory)
            .unwrap();
        let mut bytes = [0; 4];
        memory.read_bytes(0x1000, &mut bytes).unwrap();
        bytes
    }
}

#[test]
fn cpu_produced_load_mixed_mrt_uses_exact_slot_and_commits_in_both_collection_orders() {
    for consumer_first in [false, true] {
        let mut h = Harness::new();
        h.enable_produced_loads();
        let targets = h.targets;
        h.attachment_pass(
            0,
            &targets,
            &[
                GraphicsColorLoad::Clear([11; 4]),
                GraphicsColorLoad::Clear([23; 4]),
            ],
            ResourceStates::RENDER_TARGET,
        );
        let a = h.submit(0, &[]).unwrap();
        let point = h.ctx.submission_point(a).unwrap();
        h.attachment_pass(
            1,
            &[targets[1], targets[0]],
            &[GraphicsColorLoad::Load, GraphicsColorLoad::Clear([37; 4])],
            ResourceStates::COPY_SOURCE,
        );
        assert!(h.submit(1, &[]).is_err()); // No dependency, no access to planned states/version.
        let b = h.submit(1, &[point]).unwrap();
        // Make controlled tickets drainable even when a metadata assertion fails.
        h.complete_colors(0, vec![vec![11; 4], vec![23; 4]]);
        h.complete_colors(1, vec![vec![53; 4], vec![37; 4]]);
        let b_point = h.ctx.submission_point(b).unwrap();
        {
            let backend = h.backend.lock().unwrap();
            let request = &backend.requests[1];
            assert_eq!(request.generation, backend.requests[0].generation);
            assert_eq!(request.waits, [point]);
            assert!(
                matches!(request.colors[0], GraphicsQueuedColorInput::Produced(version)
                if version == GraphicsProducedVersion { point, output: GraphicsProducedOutput::Color(1) })
            );
            assert!(matches!(
                request.colors[1],
                GraphicsQueuedColorInput::Clear([37, 37, 37, 37])
            ));
            assert_eq!(
                request.outputs,
                [
                    GraphicsProducedVersion {
                        point: b_point,
                        output: GraphicsProducedOutput::Color(0)
                    },
                    GraphicsProducedVersion {
                        point: b_point,
                        output: GraphicsProducedOutput::Color(1)
                    }
                ]
            );
        }
        // Each receipt reserves 2*8 destination/readback bytes + 4 outcome bytes.
        assert_eq!(h.ctx.queued_color_allocation_bytes(), 40);
        if !consumer_first {
            assert_eq!(
                h.ctx.wait_submission(a).unwrap(),
                GraphicsSubmissionStatus::Complete
            );
        }
        assert_eq!(
            h.ctx.wait_submission(b).unwrap(),
            GraphicsSubmissionStatus::Complete
        );
        assert_eq!(
            h.ctx.wait_submission(a).unwrap(),
            GraphicsSubmissionStatus::Complete
        );
        h.ctx.release_submission(a).unwrap();
        assert_eq!(h.ctx.queued_color_allocation_bytes(), 40); // ancestor still retained
        h.ctx.release_submission(b).unwrap();
        assert_eq!(h.ctx.queued_color_allocation_bytes(), 0);
        assert_eq!(h.read_color(targets[0]), [37; 4]);
        assert_eq!(h.read_color(targets[1]), [53; 4]); // older producer cannot overwrite B
    }
}

#[test]
fn cpu_produced_load_failures_preserve_atomic_colors_and_refund_after_pruning() {
    for producer_fails in [false, true] {
        let mut h = Harness::new();
        h.enable_produced_loads();
        let targets = h.targets;
        h.attachment_pass(
            0,
            &targets,
            &[
                GraphicsColorLoad::Clear([11; 4]),
                GraphicsColorLoad::Clear([23; 4]),
            ],
            ResourceStates::RENDER_TARGET,
        );
        let a = h.submit(0, &[]).unwrap();
        let point = h.ctx.submission_point(a).unwrap();
        h.attachment_pass(
            1,
            &targets,
            &[GraphicsColorLoad::Load; 2],
            ResourceStates::COPY_SOURCE,
        );
        let b = h.submit(1, &[point]).unwrap();
        if producer_fails {
            h.complete_colors(0, vec![vec![11; 4], vec![23]]); // malformed final MRT
            h.complete_colors(1, vec![vec![53; 4], vec![59; 4]]); // cached success is invalid
        } else {
            h.complete_colors(0, vec![vec![11; 4], vec![23; 4]]);
            h.complete_colors(1, vec![vec![53; 4], vec![59]]); // no partial first MRT commit
        }
        assert_eq!(
            h.ctx.wait_submission(b).unwrap(),
            GraphicsSubmissionStatus::Failed
        );
        assert_eq!(
            h.ctx.last_presentation.lock().unwrap().frame_index,
            if producer_fails { 0 } else { 1 }
        );
        if producer_fails {
            assert_eq!(
                h.backend.lock().unwrap().tickets[1]
                    .lock()
                    .unwrap()
                    .rejected,
                [point]
            );
        }
        h.ctx.release_submission(a).unwrap();
        assert_eq!(h.ctx.queued_color_allocation_bytes(), 40);
        h.ctx.release_submission(b).unwrap();
        assert_eq!(h.ctx.queued_color_allocation_bytes(), 0);
        if !producer_fails {
            assert_eq!(h.read_color(targets[0]), [11; 4]);
            assert_eq!(h.read_color(targets[1]), [23; 4]);
        } else {
            // Producer barriers failed too. Transition the still-RT resources
            // separately, then inspect their actual host bytes through public readback.
            h.reset(0);
            h.ctx
                .set_metal_attachment_backend(|_, _, _, _, _| panic!("barrier-only CPU pass"));
            for target in targets {
                h.ctx
                    .cmd_resource_barrier(
                        h.lists[0],
                        target,
                        ResourceStates::RENDER_TARGET,
                        ResourceStates::COPY_SOURCE,
                    )
                    .unwrap();
            }
            h.ctx.close_command_list(h.lists[0]).unwrap();
            h.ctx
                .execute_command_list(h.queues[0], h.lists[0], &h.memory)
                .unwrap();
            assert_eq!(h.read_color(targets[0]), [0; 4]);
            assert_eq!(h.read_color(targets[1]), [0; 4]);
        }
    }
}

#[test]
fn cpu_produced_load_pending_destroy_retains_versions_and_reservations_until_prune() {
    let mut h = Harness::new();
    h.enable_produced_loads();
    let target = h.targets[0];
    h.attachment_pass(
        0,
        &[target],
        &[GraphicsColorLoad::Clear([17; 4])],
        ResourceStates::RENDER_TARGET,
    );
    let a = h.submit(0, &[]).unwrap();
    let point = h.ctx.submission_point(a).unwrap();
    h.attachment_pass(
        1,
        &[target],
        &[GraphicsColorLoad::Load],
        ResourceStates::COPY_SOURCE,
    );
    let b = h.submit(1, &[point]).unwrap();
    h.reset(0);
    h.reset(1);
    h.ctx.destroy_resource(target).unwrap();
    assert_eq!(h.ctx.queued_color_allocation_bytes(), 24);
    h.complete(0, [17; 4]);
    h.complete(1, [31; 4]);
    h.ctx.wait_submission(b).unwrap();
    assert!(h.ctx.destroy_resource(target).is_err()); // commit never resurrects it
    h.ctx.release_submission(a).unwrap();
    assert_eq!(
        h.backend.lock().unwrap().tickets[0].lock().unwrap().drops,
        0
    );
    assert_eq!(h.ctx.queued_color_allocation_bytes(), 24);
    h.ctx.release_submission(b).unwrap();
    assert_eq!(
        h.backend.lock().unwrap().tickets[0].lock().unwrap().drops,
        1
    );
    assert_eq!(h.ctx.queued_color_allocation_bytes(), 0);
}

#[test]
fn cpu_produced_load_backend_rejection_consumes_no_point_handle_or_reservation() {
    let mut h = Harness::new();
    h.enable_produced_loads();
    let target = h.targets[0];
    h.attachment_pass(
        0,
        &[target],
        &[GraphicsColorLoad::Clear([17; 4])],
        ResourceStates::RENDER_TARGET,
    );
    let a = h.submit(0, &[]).unwrap();
    let point = h.ctx.submission_point(a).unwrap();
    h.attachment_pass(
        1,
        &[target],
        &[GraphicsColorLoad::Load],
        ResourceStates::COPY_SOURCE,
    );
    h.ctx
        .set_metal_produced_color_queued_backend(GraphicsTextureCapabilities::default(), |_| {
            Err(GraphicsError::BackendError(
                "CPU-only precommit rejection".into(),
            ))
        });
    let marker = h.ctx.create_device().unwrap();
    assert!(h.submit(1, &[point]).is_err());
    assert_eq!(h.ctx.create_device().unwrap().id, marker.id + 1);
    assert_eq!(h.ctx.queued_color_allocation_bytes(), 12);
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    h.enable_produced_loads();
    let b = h.submit(1, &[point]).unwrap();
    assert_eq!(h.ctx.submission_point(b).unwrap().value, 1);
    h.complete(0, [17; 4]);
    h.complete(1, [31; 4]);
    h.ctx.wait_submission(b).unwrap();
    h.ctx.release_submission(a).unwrap();
    h.ctx.release_submission(b).unwrap();
    assert_eq!(h.ctx.queued_color_allocation_bytes(), 0);
}

#[test]
fn cpu_queued_host_load_keeps_owned_bytes_and_charges_snapshot_storage() {
    let mut h = Harness::new();
    let target = h.targets[0];
    h.attachment_pass(
        0,
        &[target],
        &[GraphicsColorLoad::Load],
        ResourceStates::RENDER_TARGET,
    );
    let a = h.submit(0, &[]).unwrap(); // Host Load remains valid on the old queued installer.
    {
        let backend = h.backend.lock().unwrap();
        match &backend.requests[0].colors[0] {
            GraphicsQueuedColorInput::Host(bytes) => assert_eq!(&**bytes, &[0; 4]),
            _ => panic!("committed host storage must remain a Host input"),
        }
    }
    assert_eq!(h.ctx.queued_color_allocation_bytes(), 16);
    assert_eq!(MAX_QUEUED_COLOR_BYTES, 200 * 1024 * 1024);
    h.complete(0, [7; 4]);
    h.ctx.wait_submission(a).unwrap();
    assert_eq!(h.ctx.queued_color_allocation_bytes(), 16);
    h.ctx.release_submission(a).unwrap();
    assert_eq!(h.ctx.queued_color_allocation_bytes(), 0);
    assert_eq!(h.read_color(target), [7; 4]);
}

#[test]
fn cpu_replacing_produced_color_installer_revokes_load_capability() {
    let mut h = Harness::new();
    h.enable_produced_loads();
    let target = h.targets[0];
    h.attachment_pass(
        0,
        &[target],
        &[GraphicsColorLoad::Clear([17; 4])],
        ResourceStates::RENDER_TARGET,
    );
    let a = h.submit(0, &[]).unwrap();
    let point = h.ctx.submission_point(a).unwrap();
    h.attachment_pass(
        1,
        &[target],
        &[GraphicsColorLoad::Load],
        ResourceStates::COPY_SOURCE,
    );
    h.ctx
        .set_metal_uniform_queued_backend(GraphicsTextureCapabilities::default(), |_| {
            panic!("old uniform queued callback received Produced Load")
        });
    assert_eq!(
        h.submit(1, &[point]).unwrap_err(),
        GraphicsError::InvalidDescriptor
    );
    assert_eq!(h.backend.lock().unwrap().requests.len(), 1);
    assert_eq!(h.ctx.queued_color_allocation_bytes(), 12);
    h.complete(0, [17; 4]);
    h.ctx.wait_submission(a).unwrap();
    h.ctx.release_submission(a).unwrap();
    assert_eq!(h.ctx.queued_color_allocation_bytes(), 0);
}

impl Harness {
    fn enable_produced_depth(&mut self) {
        let capture = self.backend.clone();
        self.ctx.set_metal_produced_attachment_queued_backend(
            GraphicsTextureCapabilities::default(),
            move |request| {
                let ticket = Arc::new(Mutex::new(TicketState::default()));
                let mut backend = capture.lock().unwrap();
                backend.requests.push(request);
                backend.tickets.push(ticket.clone());
                Ok(Box::new(Ticket(ticket)))
            },
        );
    }

    fn depth_pass(
        &mut self,
        slot: usize,
        depth: GraphicsHandle,
        load: GraphicsDepthLoad,
        states: [ResourceStates; 3],
        draw: Option<bool>,
    ) {
        let [before, during, after] = states;
        let list = self.lists[slot];
        if before != during {
            self.ctx
                .cmd_resource_barrier(list, depth, before, during)
                .unwrap();
        }
        self.ctx
            .cmd_set_color_targets(list, &[self.targets[slot]], &[[7; 4]])
            .unwrap();
        self.ctx
            .cmd_set_depth_target(list, Some(depth), load)
            .unwrap();
        self.ctx.cmd_set_viewport(list, 1, 1).unwrap();
        if let Some(writes) = draw {
            self.ctx
                .cmd_set_vertex_buffer(list, self.vertex, 0, 16)
                .unwrap();
            self.ctx
                .cmd_set_draw_state(
                    list,
                    GraphicsDrawState {
                        depth_compare: Some(GraphicsDepthCompare::Less),
                        depth_write: writes,
                        ..GraphicsDrawState::default()
                    },
                )
                .unwrap();
            self.ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
        }
        if during != after {
            self.ctx
                .cmd_resource_barrier(list, depth, during, after)
                .unwrap();
        }
        self.ctx.close_command_list(list).unwrap();
    }

    fn complete_depth(&self, index: usize, color: [u8; 4], bits: u32) {
        self.backend.lock().unwrap().tickets[index]
            .lock()
            .unwrap()
            .result = Some(Ok(GraphicsPassOutput {
            color_layouts: None,
            colors: vec![color.to_vec()],
            depth: Some(Arc::new([bits])),
        }));
    }
}

#[test]
fn cpu_pending_depth_load_uses_exact_version_before_any_host_snapshot_exists() {
    for (consumer_first, writable) in [(false, false), (true, false), (false, true), (true, true)] {
        let expected_bits = if writable { 0x3f000000 } else { 0x3e800000 };
        let mut h = Harness::new();
        h.enable_produced_depth();
        let depth = h.ctx.create_depth_resource(h.device, (1, 1)).unwrap();
        h.depth_pass(
            0,
            depth,
            GraphicsDepthLoad::Clear(0.25),
            [
                ResourceStates::DEPTH_WRITE,
                ResourceStates::DEPTH_WRITE,
                ResourceStates::DEPTH_READ,
            ],
            None,
        );
        let a = h.submit(0, &[]).unwrap();
        let point = h.ctx.submission_point(a).unwrap();
        h.depth_pass(
            1,
            depth,
            GraphicsDepthLoad::Load,
            [
                ResourceStates::DEPTH_READ,
                if writable {
                    ResourceStates::DEPTH_WRITE
                } else {
                    ResourceStates::DEPTH_READ
                },
                ResourceStates::DEPTH_WRITE,
            ],
            Some(writable),
        );
        assert!(h.submit(1, &[]).is_err());
        let b = h.submit(1, &[point]).unwrap();
        {
            let backend = h.backend.lock().unwrap();
            let version = GraphicsProducedVersion {
                point,
                output: GraphicsProducedOutput::Depth,
            };
            assert_eq!(backend.requests[0].depth_output, Some(version));
            assert!(
                matches!(&backend.requests[1].depth, Some(GraphicsQueuedDepthInput::Produced(v)) if *v == version)
            );
            assert_eq!(
                backend.requests[1].generation,
                backend.requests[0].generation
            );
            assert_eq!(backend.requests[1].waits, [point]);
            assert_eq!(backend.requests[1].draws[0].state.depth_write, writable);
            assert_eq!(
                backend.requests[1].outputs[0].output,
                GraphicsProducedOutput::Color(0)
            );
            assert_eq!(
                backend.requests[1].depth_output.unwrap().output,
                GraphicsProducedOutput::Depth
            );
        }
        assert_eq!(h.ctx.queued_depth_allocation_bytes(), 16);
        h.complete_depth(0, [11; 4], 0x3e800000);
        h.complete_depth(1, [23; 4], expected_bits);
        if !consumer_first {
            h.ctx.wait_submission(a).unwrap();
        }
        assert_eq!(
            h.ctx.wait_submission(b).unwrap(),
            GraphicsSubmissionStatus::Complete
        );
        h.ctx.wait_submission(a).unwrap();
        h.ctx.release_submission(a).unwrap();
        assert_eq!(h.ctx.queued_depth_allocation_bytes(), 16);
        h.ctx.release_submission(b).unwrap();
        assert_eq!(h.ctx.queued_depth_allocation_bytes(), 0);
        // A subsequent host input exposes the actual committed opaque snapshot.
        h.reset(0);
        h.depth_pass(
            0,
            depth,
            GraphicsDepthLoad::Load,
            [ResourceStates::DEPTH_WRITE; 3],
            Some(true),
        );
        let c = h.submit(0, &[]).unwrap();
        {
            let backend = h.backend.lock().unwrap();
            match &backend.requests[2].depth {
                Some(GraphicsQueuedDepthInput::Host(snapshot)) => assert_eq!(
                    snapshot.downcast_ref::<[u32; 1]>().unwrap(),
                    &[expected_bits]
                ),
                _ => panic!("released producer must resolve committed host depth"),
            }
        }
        h.complete_depth(2, [31; 4], 0x3f000000);
        h.ctx.wait_submission(c).unwrap();
        h.ctx.release_submission(c).unwrap();
    }
}

#[test]
fn cpu_produced_depth_clear_and_read_only_rules_reject_before_dispatch() {
    for invalid in 0..3 {
        let mut h = Harness::new();
        h.enable_produced_depth();
        let depth = h.ctx.create_depth_resource(h.device, (1, 1)).unwrap();
        h.depth_pass(
            0,
            depth,
            GraphicsDepthLoad::Clear(0.25),
            [
                ResourceStates::DEPTH_WRITE,
                ResourceStates::DEPTH_WRITE,
                ResourceStates::DEPTH_READ,
            ],
            None,
        );
        let a = h.submit(0, &[]).unwrap();
        let point = h.ctx.submission_point(a).unwrap();
        h.depth_pass(
            1,
            depth,
            if invalid == 0 {
                GraphicsDepthLoad::Clear(0.875)
            } else {
                GraphicsDepthLoad::Load
            },
            [ResourceStates::DEPTH_READ; 3],
            if invalid == 1 { Some(true) } else { None },
        );
        if invalid == 2 {
            // Replacing the full installer with color-only must revoke depth support.
            h.ctx.set_metal_produced_color_queued_backend(
                GraphicsTextureCapabilities::default(),
                |_| panic!("color-only callback received Produced depth"),
            );
        }
        let marker = h.ctx.create_device().unwrap();
        assert!(h.submit(1, &[point]).is_err());
        assert_eq!(h.ctx.create_device().unwrap().id, marker.id + 1);
        assert_eq!(h.ctx.queued_depth_allocation_bytes(), 8);
        assert_eq!(h.backend.lock().unwrap().requests.len(), 1);
        h.enable_produced_depth();
        h.reset(1);
        h.depth_pass(
            1,
            depth,
            GraphicsDepthLoad::Clear(0.875),
            [
                ResourceStates::DEPTH_READ,
                ResourceStates::DEPTH_WRITE,
                ResourceStates::DEPTH_WRITE,
            ],
            None,
        );
        let b = h.submit(1, &[point]).unwrap();
        assert_eq!(h.ctx.submission_point(b).unwrap().value, 1);
        assert!(
            matches!(h.backend.lock().unwrap().requests[1].depth, Some(GraphicsQueuedDepthInput::Clear(v)) if v == 0.875)
        );
        h.complete_depth(0, [11; 4], 0x3e800000);
        h.complete_depth(1, [23; 4], 0x3f600000);
        h.ctx.wait_submission(b).unwrap();
        h.ctx.release_submission(a).unwrap();
        h.ctx.release_submission(b).unwrap();
        assert_eq!(h.ctx.queued_depth_allocation_bytes(), 0);
    }
}

#[test]
fn cpu_produced_depth_failed_publication_cannot_commit_color_or_depth() {
    for failure in 0..3 {
        let mut h = Harness::new();
        h.enable_produced_depth();
        let depth = h.ctx.create_depth_resource(h.device, (1, 1)).unwrap();
        h.depth_pass(
            0,
            depth,
            GraphicsDepthLoad::Clear(0.25),
            [ResourceStates::DEPTH_WRITE; 3],
            None,
        );
        let a = h.submit(0, &[]).unwrap();
        let point = h.ctx.submission_point(a).unwrap();
        // Consumer has two colors so a malformed final color is a late rejection.
        let list = h.lists[1];
        h.ctx
            .cmd_set_color_targets(list, &h.targets, &[[0; 4]; 2])
            .unwrap();
        h.ctx
            .cmd_set_depth_target(list, Some(depth), GraphicsDepthLoad::Load)
            .unwrap();
        h.ctx.close_command_list(list).unwrap();
        let b = h.submit(1, &[point]).unwrap();
        h.complete_depth(0, [11; 4], 0x3e800000);
        let consumer = if failure == 0 {
            GraphicsPassOutput {
                color_layouts: None,
                colors: vec![vec![23; 4], vec![31]],
                depth: Some(Arc::new([0x3f000000u32])),
            }
        } else if failure == 1 {
            GraphicsPassOutput {
                color_layouts: None,
                colors: vec![vec![23; 4], vec![31; 4]],
                depth: None,
            }
        } else {
            // Producer color publication fails after native completion; B has cached success.
            h.backend.lock().unwrap().tickets[0].lock().unwrap().result =
                Some(Ok(GraphicsPassOutput {
                    color_layouts: None,
                    colors: vec![vec![11]],
                    depth: Some(Arc::new([0x3e800000u32])),
                }));
            GraphicsPassOutput {
                color_layouts: None,
                colors: vec![vec![23; 4], vec![31; 4]],
                depth: Some(Arc::new([0x3f000000u32])),
            }
        };
        h.backend.lock().unwrap().tickets[1].lock().unwrap().result = Some(Ok(consumer));
        assert_eq!(
            h.ctx.wait_submission(b).unwrap(),
            GraphicsSubmissionStatus::Failed
        );
        assert_eq!(
            h.ctx.last_presentation.lock().unwrap().frame_index,
            if failure == 2 { 0 } else { 1 }
        );
        h.ctx.release_submission(a).unwrap();
        h.ctx.release_submission(b).unwrap();
        assert_eq!(h.ctx.queued_depth_allocation_bytes(), 0);
        h.reset(0);
        h.depth_pass(
            0,
            depth,
            GraphicsDepthLoad::Load,
            [ResourceStates::DEPTH_WRITE; 3],
            None,
        );
        if failure == 2 {
            assert!(h.submit(0, &[]).is_err()); // failed producer never initialized host depth
        } else {
            let c = h.submit(0, &[]).unwrap();
            {
                let backend = h.backend.lock().unwrap();
                match &backend.requests[2].depth {
                    Some(GraphicsQueuedDepthInput::Host(snapshot)) => {
                        assert_eq!(snapshot.downcast_ref::<[u32; 1]>().unwrap(), &[0x3e800000])
                    }
                    _ => panic!("consumer failure must preserve producer's committed snapshot"),
                }
                assert!(backend.requests[2].generation > backend.requests[0].generation);
            }
            h.complete_depth(2, [41; 4], 0x3e800000);
            h.ctx.wait_submission(c).unwrap();
            h.ctx.release_submission(c).unwrap();
        }
    }
}

#[test]
fn cpu_produced_depth_retirement_keeps_logical_bytes_and_version_reservations_charged() {
    let mut h = Harness::new();
    h.enable_produced_depth();
    let depth = h.ctx.create_depth_resource(h.device, (1, 1)).unwrap();
    // Uninitialized depth objects reserve logical bytes without allocating snapshots.
    for _ in 0..63 {
        h.ctx.create_depth_resource(h.device, (1024, 1024)).unwrap();
    }
    let filler = MAX_CONTEXT_RESOURCE_BYTES - 63 * 1024 * 1024 * 4 - 60;
    let buffer = |bytes| ResourceDesc {
        dimension: ResourceDimension::Buffer,
        width: bytes as u64,
        height: 1,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: false,
    };
    h.ctx
        .create_committed_resource(h.device, buffer(filler), ResourceStates::COMMON, None)
        .unwrap();
    h.depth_pass(
        0,
        depth,
        GraphicsDepthLoad::Clear(0.25),
        [ResourceStates::DEPTH_WRITE; 3],
        None,
    );
    let a = h.submit(0, &[]).unwrap();
    let point = h.ctx.submission_point(a).unwrap();
    h.depth_pass(
        1,
        depth,
        GraphicsDepthLoad::Load,
        [ResourceStates::DEPTH_WRITE; 3],
        None,
    );
    let b = h.submit(1, &[point]).unwrap();
    h.reset(0);
    h.reset(1);
    h.ctx.destroy_resource(depth).unwrap();
    assert!(
        h.ctx
            .create_committed_resource(h.device, buffer(4), ResourceStates::COMMON, None)
            .is_err()
    );
    h.complete_depth(0, [11; 4], 0x3e800000);
    h.complete_depth(1, [23; 4], 0x3e800000);
    h.ctx.wait_submission(b).unwrap();
    h.ctx.release_submission(a).unwrap();
    assert_eq!(h.ctx.queued_depth_allocation_bytes(), 16);
    assert!(
        h.ctx
            .create_committed_resource(h.device, buffer(4), ResourceStates::COMMON, None)
            .is_err()
    );
    h.ctx.release_submission(b).unwrap();
    assert_eq!(h.ctx.queued_depth_allocation_bytes(), 0);
    assert!(h.ctx.destroy_resource(depth).is_err());
    h.ctx
        .create_committed_resource(h.device, buffer(4), ResourceStates::COMMON, None)
        .unwrap();
}

#[test]
fn cpu_read_only_depth_versions_require_dependency_even_with_initialized_host_snapshot() {
    let mut h = Harness::new();
    h.enable_produced_depth();
    let depth = h.ctx.create_depth_resource(h.device, (1, 1)).unwrap();
    h.depth_pass(
        0,
        depth,
        GraphicsDepthLoad::Clear(0.25),
        [
            ResourceStates::DEPTH_WRITE,
            ResourceStates::DEPTH_WRITE,
            ResourceStates::DEPTH_READ,
        ],
        None,
    );
    let seed = h.submit(0, &[]).unwrap();
    h.complete_depth(0, [7; 4], 0x3e800000);
    h.ctx.wait_submission(seed).unwrap();
    h.ctx.release_submission(seed).unwrap();
    h.reset(0);
    h.depth_pass(
        0,
        depth,
        GraphicsDepthLoad::Load,
        [ResourceStates::DEPTH_READ; 3],
        Some(false),
    );
    let a = h.submit(0, &[]).unwrap();
    let point = h.ctx.submission_point(a).unwrap();
    h.depth_pass(
        1,
        depth,
        GraphicsDepthLoad::Load,
        [ResourceStates::DEPTH_READ; 3],
        Some(false),
    );
    assert!(h.submit(1, &[]).is_err());
    assert_eq!(h.backend.lock().unwrap().requests.len(), 2);
    let b = h.submit(1, &[point]).unwrap();
    assert!(matches!(&h.backend.lock().unwrap().requests[2].depth,
        Some(GraphicsQueuedDepthInput::Produced(version)) if version.point == point && version.output == GraphicsProducedOutput::Depth));
    h.complete_depth(1, [11; 4], 0x3e800000);
    h.complete_depth(2, [23; 4], 0x3e800000);
    h.ctx.wait_submission(b).unwrap();
    h.ctx.release_submission(a).unwrap();
    h.ctx.release_submission(b).unwrap();
}
