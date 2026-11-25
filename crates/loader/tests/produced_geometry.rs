// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU-only produced geometry admission; no GPU work.
use std::sync::{Arc, Mutex};
use xenora_loader::graphics::*;
use xenora_loader::{GuestMemory, MemoryPermissions};
#[derive(Default)]
struct ControlledState {
    result: Option<Result<GraphicsQueuedWorkOutput, GraphicsError>>,
    rejected: Vec<GraphicsQueuePoint>,
}
struct ControlledTicket(Arc<Mutex<ControlledState>>);
impl GraphicsQueuedPendingWork for ControlledTicket {
    fn poll(&mut self) -> Option<Result<GraphicsQueuedWorkOutput, GraphicsError>> {
        self.0.lock().unwrap().result.take()
    }
    fn wait(&mut self) -> Result<GraphicsQueuedWorkOutput, GraphicsError> {
        self.poll().expect("release CPU ticket before wait")
    }
    fn reject_dependency(&mut self, producer: GraphicsQueuePoint) {
        self.0.lock().unwrap().rejected.push(producer);
    }
}
#[derive(Default)]
struct ControlledBackend {
    requests: Vec<GraphicsQueuedWorkRequest>,
    tickets: Vec<Arc<Mutex<ControlledState>>>,
    reject_next: bool,
}
struct CopyHarness {
    ctx: GraphicsContext,
    memory: GuestMemory,
    device: GraphicsHandle,
    queues: [GraphicsHandle; 2],
    allocators: [GraphicsHandle; 2],
    lists: [GraphicsHandle; 2],
    backend: Arc<Mutex<ControlledBackend>>,
}
impl CopyHarness {
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
        let backend = Arc::new(Mutex::new(ControlledBackend::default()));
        let captured = backend.clone();
        ctx.set_metal_geometry_graph_backend(
            GraphicsTextureCapabilities::default(),
            move |request| {
                let state = Arc::new(Mutex::new(ControlledState::default()));
                let mut backend = captured.lock().unwrap();
                if std::mem::take(&mut backend.reject_next) {
                    return Err(GraphicsError::InvalidDescriptor);
                }
                backend.requests.push(request);
                backend.tickets.push(state.clone());
                Ok(Box::new(ControlledTicket(state)))
            },
        );
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 4096, MemoryPermissions::READ_WRITE)
            .unwrap();
        Self {
            ctx,
            memory,
            device,
            queues,
            allocators,
            lists,
            backend,
        }
    }
    fn resource(&mut self, texture: bool, bytes: &[u8]) -> GraphicsHandle {
        let resource = self
            .ctx
            .create_committed_resource(
                self.device,
                ResourceDesc {
                    dimension: if texture {
                        ResourceDimension::Texture2D
                    } else {
                        ResourceDimension::Buffer
                    },
                    width: if texture {
                        (bytes.len() / 4) as u64
                    } else {
                        bytes.len() as u64
                    },
                    height: 1,
                    depth_or_array_size: 1,
                    mip_levels: 1,
                    format_rgba8: texture,
                },
                ResourceStates::COPY_DEST,
                None,
            )
            .unwrap();
        self.memory.write_bytes(0x1000, bytes).unwrap();
        self.ctx
            .copy_guest_to_resource(resource, 0, 0x1000, bytes.len(), &self.memory)
            .unwrap();
        resource
    }
    fn record(
        &mut self,
        slot: usize,
        source: GraphicsHandle,
        destination: GraphicsHandle,
        source_state: ResourceStates,
        after: ResourceStates,
    ) {
        let list = self.lists[slot];
        if source_state != ResourceStates::COPY_SOURCE {
            self.ctx
                .cmd_resource_barrier(list, source, source_state, ResourceStates::COPY_SOURCE)
                .unwrap();
        }
        self.ctx
            .cmd_copy_resource(list, destination, source)
            .unwrap();
        if after != ResourceStates::COPY_DEST {
            self.ctx
                .cmd_resource_barrier(list, destination, ResourceStates::COPY_DEST, after)
                .unwrap();
        }
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
    fn complete(&self, index: usize, output: GraphicsQueuedWorkOutput) {
        self.backend.lock().unwrap().tickets[index]
            .lock()
            .unwrap()
            .result = Some(Ok(output));
    }
    fn reset(&mut self, slot: usize) {
        self.ctx
            .reset_command_list(self.lists[slot], self.allocators[slot])
            .unwrap();
    }
    fn read(&mut self, handle: GraphicsHandle, count: usize) -> Vec<u8> {
        self.ctx
            .copy_resource_to_guest(handle, 0, 0x1000, count, &mut self.memory)
            .unwrap();
        let mut bytes = vec![0; count];
        self.memory.read_bytes(0x1000, &mut bytes).unwrap();
        bytes
    }
}
fn copy_bytes(bytes: &[u8]) -> GraphicsQueuedWorkOutput {
    GraphicsQueuedWorkOutput::Copy {
        layout: GraphicsCopyLayout::Buffer {
            byte_len: bytes.len(),
        },
        bytes: bytes.to_vec(),
    }
}

#[test]
fn pending_positions_are_admitted_without_stale_host_fetch() {
    let mut h = CopyHarness::new();
    let source = h.resource(false, &[0; 48]);
    let positions = h.resource(false, &[0xff; 48]);
    let target = h.resource(true, &[0; 4]);
    h.record(
        0,
        source,
        positions,
        ResourceStates::COPY_DEST,
        ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
    );
    let a = h.submit(0, &[]).unwrap();
    let point = h.ctx.submission_point(a).unwrap();
    let list = h.lists[1];
    h.ctx
        .cmd_resource_barrier(
            list,
            target,
            ResourceStates::COPY_DEST,
            ResourceStates::RENDER_TARGET,
        )
        .unwrap();
    h.ctx
        .cmd_set_color_targets(list, &[target], &[[0; 4]])
        .unwrap();
    h.ctx.cmd_set_viewport(list, 1, 1).unwrap();
    h.ctx.cmd_set_vertex_buffer(list, positions, 0, 16).unwrap();
    h.ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
    h.ctx
        .cmd_resource_barrier(
            list,
            target,
            ResourceStates::RENDER_TARGET,
            ResourceStates::COPY_SOURCE,
        )
        .unwrap();
    h.ctx.close_command_list(list).unwrap();
    let result = h.submit(1, &[point]);
    h.complete(0, copy_bytes(&[0; 48]));
    if result.is_ok() {
        h.complete(
            1,
            GraphicsQueuedWorkOutput::Render(GraphicsPassOutput {
                color_layouts: None,
                colors: vec![vec![9, 17, 33, 255]],
                depth: None,
            }),
        );
    }
    let b = result.expect("queued produced position input must be admitted");
    assert_eq!(
        h.ctx.wait_submission(b).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    h.ctx.release_submission(a).unwrap();
    h.ctx.release_submission(b).unwrap();
    h.reset(0);
    h.reset(1);
    assert_eq!(h.read(target, 4), [9, 17, 33, 255]);
}

impl CopyHarness {
    fn pending_positions(&mut self) -> (GraphicsHandle, GraphicsHandle, GraphicsQueuePoint) {
        let source = self.resource(false, &[0; 48]);
        let positions = self.resource(false, &[0xff; 48]);
        self.record(
            0,
            source,
            positions,
            ResourceStates::COPY_DEST,
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
        );
        let receipt = self.submit(0, &[]).unwrap();
        let point = self.ctx.submission_point(receipt).unwrap();
        (positions, receipt, point)
    }
    fn begin_render(&mut self, positions: GraphicsHandle) -> GraphicsHandle {
        let target = self.resource(true, &[3, 5, 7, 11]);
        let list = self.lists[1];
        self.ctx
            .cmd_resource_barrier(
                list,
                target,
                ResourceStates::COPY_DEST,
                ResourceStates::RENDER_TARGET,
            )
            .unwrap();
        self.ctx
            .cmd_set_color_targets(list, &[target], &[[0; 4]])
            .unwrap();
        self.ctx.cmd_set_viewport(list, 1, 1).unwrap();
        let pipeline = self
            .ctx
            .create_graphics_pipeline(
                self.device,
                include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin"),
                include_bytes!("../../../fixtures/rdna2/x9-pixel-arithmetic-ps.bin"),
            )
            .unwrap();
        self.ctx
            .cmd_set_graphics_pipeline(list, Some(pipeline))
            .unwrap();
        self.ctx
            .cmd_set_vertex_buffer(list, positions, 0, 16)
            .unwrap();
        target
    }
    fn end_render(&mut self, target: GraphicsHandle) {
        self.ctx
            .cmd_resource_barrier(
                self.lists[1],
                target,
                ResourceStates::RENDER_TARGET,
                ResourceStates::COPY_SOURCE,
            )
            .unwrap();
        self.ctx.close_command_list(self.lists[1]).unwrap();
    }
}

#[test]
fn exact_produced_version_and_host_colors_are_owned() {
    let mut h = CopyHarness::new();
    let (positions, a, point) = h.pending_positions();
    let colors = h.resource(false, &[0; 48]);
    let target = h.begin_render(positions);
    let list = h.lists[1];
    h.ctx
        .cmd_resource_barrier(
            list,
            colors,
            ResourceStates::COPY_DEST,
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
        )
        .unwrap();
    h.ctx.cmd_set_vertex_colors(list, colors, 0, 16).unwrap();
    h.ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
    h.end_render(target);
    let result = h.submit(1, &[point]);
    h.complete(0, copy_bytes(&[0; 48]));
    if result.is_ok() {
        h.complete(
            1,
            GraphicsQueuedWorkOutput::Render(GraphicsPassOutput {
                color_layouts: None,
                colors: vec![vec![13, 17, 19, 23]],
                depth: None,
            }),
        );
    }
    let b = result.unwrap();
    {
        let backend = h.backend.lock().unwrap();
        let GraphicsQueuedWorkRequest::Render(request) = &backend.requests[1] else {
            panic!("render")
        };
        let draw = &request.draws[0];
        assert!(draw.vertices.is_empty() && draw.colors.is_empty());
        let geometry = draw.geometry.as_ref().unwrap();
        let GraphicsCopyInput::Produced(version) = geometry.positions.buffer.source else {
            panic!("exact producer")
        };
        assert_eq!(version.point, point);
        assert_eq!(version.output, GraphicsProducedOutput::Copy(0));
        assert!(request.waits.contains(&point));
        let GraphicsCopyInput::Host(bytes) = &geometry.colors.as_ref().unwrap().buffer.source
        else {
            panic!("host")
        };
        assert_eq!(&**bytes, &[0; 48]);
    }
    h.reset(0);
    h.reset(1);
    h.ctx.destroy_resource(positions).unwrap();
    h.ctx.destroy_resource(colors).unwrap();
    assert_eq!(
        h.ctx.wait_submission(b).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    h.ctx.release_submission(a).unwrap();
    h.ctx.release_submission(b).unwrap();
    assert_eq!(h.read(target, 4), [13, 17, 19, 23]);
}

#[test]
fn logical_bounds_and_mixed_host_nan_reject_before_dispatch_then_retry() {
    for invalid in 0..3 {
        let mut h = CopyHarness::new();
        let (positions, a, point) = h.pending_positions();
        let colors = h.resource(false, &[0xff; 48]);
        let target = h.begin_render(positions);
        let list = h.lists[1];
        if invalid == 1 {
            h.ctx
                .cmd_resource_barrier(
                    list,
                    colors,
                    ResourceStates::COPY_DEST,
                    ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
                )
                .unwrap();
            h.ctx.cmd_set_vertex_colors(list, colors, 0, 16).unwrap();
        }
        h.ctx
            .cmd_draw_instanced(list, 3, 1, u32::from(invalid == 0), 0)
            .unwrap();
        h.end_render(target);
        let waits = if invalid == 2 { vec![] } else { vec![point] };
        let rejected = h.submit(1, &waits);
        h.complete(0, copy_bytes(&[0; 48]));
        assert!(rejected.is_err());
        assert_eq!(h.backend.lock().unwrap().requests.len(), 1);
        h.reset(1);
        // Failed admission did not publish the target barrier or consume a queue point.
        h.ctx
            .cmd_resource_barrier(
                list,
                target,
                ResourceStates::COPY_DEST,
                ResourceStates::RENDER_TARGET,
            )
            .unwrap();
        h.ctx
            .cmd_set_color_targets(list, &[target], &[[0; 4]])
            .unwrap();
        h.ctx.cmd_set_viewport(list, 1, 1).unwrap();
        h.ctx.cmd_set_vertex_buffer(list, positions, 0, 16).unwrap();
        h.ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
        h.end_render(target);
        let b = h.submit(1, &[point]).unwrap();
        assert_eq!(h.ctx.submission_point(b).unwrap().value, 1);
        h.complete(
            1,
            GraphicsQueuedWorkOutput::Render(GraphicsPassOutput {
                color_layouts: None,
                colors: vec![vec![29, 31, 37, 41]],
                depth: None,
            }),
        );
        assert_eq!(
            h.ctx.wait_submission(b).unwrap(),
            GraphicsSubmissionStatus::Complete
        );
        h.ctx.release_submission(a).unwrap();
        h.ctx.release_submission(b).unwrap();
        h.reset(0);
        h.reset(1);
        assert_eq!(h.read(target, 4), [29, 31, 37, 41]);
    }
}

#[test]
fn old_graph_installer_revokes_geometry_capability() {
    let mut h = CopyHarness::new();
    h.ctx
        .set_metal_graph_queued_backend(GraphicsTextureCapabilities::default(), {
            let backend = h.backend.clone();
            move |request| {
                let ticket = Arc::new(Mutex::new(ControlledState::default()));
                let mut b = backend.lock().unwrap();
                b.requests.push(request);
                b.tickets.push(ticket.clone());
                Ok(Box::new(ControlledTicket(ticket)))
            }
        });
    let (positions, a, point) = h.pending_positions();
    let target = h.begin_render(positions);
    h.ctx.cmd_draw_instanced(h.lists[1], 3, 1, 0, 0).unwrap();
    h.end_render(target);
    let result = h.submit(1, &[point]);
    h.complete(0, copy_bytes(&[0; 48]));
    assert!(matches!(result, Err(GraphicsError::NativeMetalRequired)));
    assert_eq!(h.backend.lock().unwrap().requests.len(), 1);
    assert_eq!(
        h.ctx.wait_submission(a).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    h.ctx.release_submission(a).unwrap();
}

#[test]
fn cached_render_success_cannot_publish_after_invalid_geometry_producer_output() {
    let mut h = CopyHarness::new();
    let (positions, a, point) = h.pending_positions();
    let target = h.begin_render(positions);
    h.ctx.cmd_draw_instanced(h.lists[1], 3, 1, 0, 0).unwrap();
    h.end_render(target);
    let b = h.submit(1, &[point]).unwrap();
    h.complete(
        1,
        GraphicsQueuedWorkOutput::Render(GraphicsPassOutput {
            color_layouts: None,
            colors: vec![vec![255; 4]],
            depth: None,
        }),
    );
    assert_eq!(
        h.ctx.poll_submission(b).unwrap(),
        GraphicsSubmissionStatus::Pending
    );
    h.complete(0, copy_bytes(&[0; 47]));
    assert_eq!(
        h.ctx.wait_submission(b).unwrap(),
        GraphicsSubmissionStatus::Failed
    );
    h.ctx.release_submission(a).unwrap();
    h.ctx.release_submission(b).unwrap();
    h.reset(0);
    h.reset(1);
    let list = h.lists[1];
    h.ctx
        .cmd_resource_barrier(
            list,
            target,
            ResourceStates::COPY_DEST,
            ResourceStates::COPY_SOURCE,
        )
        .unwrap();
    h.ctx.close_command_list(list).unwrap();
    // No successful render may have published its pixels or target state.
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    h.ctx
        .set_metal_backend(|_, _, _| panic!("barrier-only CPU execution"));
    h.ctx
        .execute_command_list(h.queues[1], list, &h.memory)
        .unwrap();
    h.reset(1);
    assert_eq!(h.read(target, 4), [3, 5, 7, 11]);
}

#[test]
fn backend_admission_error_does_not_consume_point_or_commit_state() {
    let mut h = CopyHarness::new();
    let (positions, a, point) = h.pending_positions();
    let target = h.begin_render(positions);
    h.ctx.cmd_draw_instanced(h.lists[1], 3, 1, 0, 0).unwrap();
    h.end_render(target);
    h.backend.lock().unwrap().reject_next = true;
    assert!(h.submit(1, &[point]).is_err());
    assert_eq!(h.backend.lock().unwrap().requests.len(), 1);
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    let b = h.submit(1, &[point]).unwrap();
    h.complete(0, copy_bytes(&[0; 48]));
    h.complete(
        1,
        GraphicsQueuedWorkOutput::Render(GraphicsPassOutput {
            color_layouts: None,
            colors: vec![vec![2, 3, 5, 7]],
            depth: None,
        }),
    );
    assert_eq!(h.ctx.submission_point(b).unwrap().value, 1);
    assert_eq!(
        h.ctx.wait_submission(b).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    h.ctx.release_submission(a).unwrap();
    h.ctx.release_submission(b).unwrap();
    h.reset(0);
    h.reset(1);
    assert_eq!(h.read(target, 4), [2, 3, 5, 7]);
}

#[test]
fn mixed_host_indices_validate_final_index_and_signed_base_before_dispatch() {
    for (dxgi, bytes, base, accepted) in [
        (57, vec![0, 0, 1, 0, 3, 0], 0, false),
        (
            42,
            [0u32, 1, 3]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect(),
            0,
            false,
        ),
        (
            42,
            [0x8000_0000u32; 3]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect(),
            i32::MIN,
            true,
        ),
    ] {
        let mut h = CopyHarness::new();
        let (positions, a, point) = h.pending_positions();
        let index = h.resource(false, &bytes);
        let target = h.begin_render(positions);
        let list = h.lists[1];
        h.ctx
            .cmd_resource_barrier(
                list,
                index,
                ResourceStates::COPY_DEST,
                ResourceStates::INDEX_BUFFER,
            )
            .unwrap();
        h.ctx.cmd_set_index_buffer(list, index, 0, dxgi).unwrap();
        h.ctx
            .cmd_draw_indexed_instanced(list, 3, 1, 0, base, 0)
            .unwrap();
        h.end_render(target);
        let result = h.submit(1, &[point]);
        h.complete(0, copy_bytes(&[0; 48]));
        if let Ok(b) = result {
            h.complete(
                1,
                GraphicsQueuedWorkOutput::Render(GraphicsPassOutput {
                    color_layouts: None,
                    colors: vec![vec![2, 3, 5, 7]],
                    depth: None,
                }),
            );
            assert!(accepted);
            assert_eq!(
                h.ctx.wait_submission(b).unwrap(),
                GraphicsSubmissionStatus::Complete
            );
            h.ctx.release_submission(b).unwrap();
        } else {
            assert!(!accepted);
            assert_eq!(h.backend.lock().unwrap().requests.len(), 1);
            h.ctx.wait_submission(a).unwrap();
        }
        h.ctx.release_submission(a).unwrap();
    }
}
