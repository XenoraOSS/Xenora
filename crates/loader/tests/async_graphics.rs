// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU-only asynchronous transaction tests; manually released completion, no GPU or sleeps.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use xenora_loader::graphics::*;
use xenora_loader::{GuestMemory, MemoryPermissions};

#[derive(Default)]
struct Control {
    result: Mutex<Option<Result<GraphicsPassOutput, GraphicsError>>>,
    waits: AtomicUsize,
    polls: AtomicUsize,
    requests: Mutex<Vec<GraphicsSubmissionRequest>>,
}

struct Pending(Arc<Control>);
impl GraphicsPendingSubmission for Pending {
    fn poll(&mut self) -> Option<Result<GraphicsPassOutput, GraphicsError>> {
        self.0.polls.fetch_add(1, Ordering::SeqCst);
        self.0.result.lock().unwrap().take()
    }
    fn wait(&mut self) -> Result<GraphicsPassOutput, GraphicsError> {
        self.0.waits.fetch_add(1, Ordering::SeqCst);
        self.0
            .result
            .lock()
            .unwrap()
            .take()
            .expect("test must release completion before wait/drop")
    }
}

impl Control {
    fn complete(&self, pixels: &[u8]) {
        *self.result.lock().unwrap() = Some(Ok(GraphicsPassOutput {
            colors: vec![pixels.to_vec()],
            depth: None,
        }));
    }
}

struct Harness {
    ctx: GraphicsContext,
    memory: GuestMemory,
    device: GraphicsHandle,
    queue: GraphicsHandle,
    allocator: GraphicsHandle,
    list: GraphicsHandle,
    vertex: GraphicsHandle,
    control: Arc<Control>,
}

fn buffer(size: u64) -> ResourceDesc {
    ResourceDesc {
        dimension: ResourceDimension::Buffer,
        width: size,
        height: 1,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: false,
    }
}

impl Harness {
    fn new() -> Self {
        let mut ctx = GraphicsContext::new();
        let device = ctx.create_device().unwrap();
        let queue = ctx
            .create_command_queue(device, CommandListType::Direct)
            .unwrap();
        let allocator = ctx
            .create_command_allocator(device, CommandListType::Direct)
            .unwrap();
        let list = ctx
            .create_command_list(device, allocator, CommandListType::Direct)
            .unwrap();
        let vertex = ctx
            .create_committed_resource(
                device,
                buffer(48),
                ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
                Some(0x1000),
            )
            .unwrap();
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 48, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory
            .map_region(0x2000, 128, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory
            .map_region(0x3000, 8, MemoryPermissions::READ_WRITE)
            .unwrap();
        let vertices: [f32; 12] = [-1., -1., 0., 1., 1., -1., 0., 1., 0., 1., 0., 1.];
        memory
            .write_bytes(
                0x1000,
                &vertices
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let control = Arc::new(Control::default());
        let worker = control.clone();
        ctx.set_metal_async_backend(move |request| {
            worker.requests.lock().unwrap().push(request);
            Ok(Box::new(Pending(worker.clone())))
        });
        Self {
            ctx,
            memory,
            device,
            queue,
            allocator,
            list,
            vertex,
            control,
        }
    }

    fn target(&mut self) -> GraphicsHandle {
        self.ctx
            .create_committed_resource(
                self.device,
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
    }

    fn record(&mut self, target: GraphicsHandle) {
        self.ctx
            .cmd_set_color_targets_with_loads(self.list, &[target], &[GraphicsColorLoad::Load])
            .unwrap();
        self.ctx.cmd_set_viewport(self.list, 1, 1).unwrap();
        self.ctx
            .cmd_set_vertex_buffer(self.list, self.vertex, 0, 16)
            .unwrap();
        self.ctx.cmd_draw_instanced(self.list, 3, 1, 0, 0).unwrap();
    }

    fn submit(&mut self) -> GraphicsHandle {
        self.ctx.close_command_list(self.list).unwrap();
        self.ctx
            .submit_command_list(self.queue, self.list, &self.memory)
            .unwrap()
    }

    fn reset(&mut self) {
        self.ctx
            .reset_command_list(self.list, self.allocator)
            .unwrap();
    }

    fn create_guest_device(&mut self) -> u64 {
        let mut descriptor = [0; 128];
        descriptor[..8].copy_from_slice(&1u64.to_le_bytes());
        descriptor[8..16].copy_from_slice(&1u64.to_le_bytes());
        self.memory.write_bytes(0x2000, &descriptor).unwrap();
        xenora_loader::graphics_service::graphics_call_v1(
            &mut self.ctx,
            &mut self.memory,
            0x2000,
            128,
            0x3000,
        )
        .unwrap()
    }
}

#[test]
fn pending_snapshots_survive_reset_and_commit_only_once() {
    let mut h = Harness::new();
    let target = h.target();
    h.record(target);
    h.ctx
        .cmd_resource_barrier(
            h.list,
            target,
            ResourceStates::RENDER_TARGET,
            ResourceStates::COPY_SOURCE,
        )
        .unwrap();
    let ticket = h.submit();
    assert_eq!(
        h.ctx.poll_submission(ticket).unwrap(),
        GraphicsSubmissionStatus::Pending
    );
    assert_eq!(h.control.waits.load(Ordering::SeqCst), 0);
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    assert!(
        h.ctx
            .submit_command_list(h.queue, h.list, &h.memory)
            .is_err()
    );
    assert!(
        h.ctx
            .execute_command_list(h.queue, h.list, &h.memory)
            .is_err()
    );
    assert!(h.ctx.release_submission(ticket).is_err());
    assert!(
        h.ctx
            .copy_resource_to_guest(target, 0, 0x3000, 4, &mut h.memory)
            .is_err()
    );
    h.reset();
    h.memory.write_bytes(0x1000, &[0; 48]).unwrap();
    h.ctx
        .cmd_set_color_targets(h.list, &[target], &[[9; 4]])
        .unwrap();
    {
        let requests = h.control.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].draws[0].vertices,
            vec![[-1., -1., 0., 1.], [1., -1., 0., 1.], [0., 1., 0., 1.]]
        );
        assert_eq!(
            requests[0].colors[0].previous.as_deref(),
            Some([0; 4].as_slice())
        );
    }
    h.control.complete(&[11, 22, 33, 44]);
    assert_eq!(
        h.ctx.poll_submission(ticket).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    assert_eq!(
        h.ctx.wait_submission(ticket).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    assert_eq!(h.control.waits.load(Ordering::SeqCst), 0);
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 1);
    h.ctx
        .copy_resource_to_guest(target, 0, 0x3000, 4, &mut h.memory)
        .unwrap();
    let mut pixels = [0; 4];
    h.memory.read_bytes(0x3000, &mut pixels).unwrap();
    assert_eq!(pixels, [11, 22, 33, 44]);
    assert!(
        h.ctx
            .submit_command_list(h.queue, h.list, &h.memory)
            .is_err()
    );
    h.ctx.release_submission(ticket).unwrap();
    assert!(h.ctx.poll_submission(ticket).is_err());
}

#[test]
fn retired_targets_and_superseded_bindings_keep_object_charges_until_completion() {
    let mut h = Harness::new();
    let superseded = h.target();
    let target = h.target();
    h.ctx
        .cmd_set_color_targets_with_loads(h.list, &[superseded], &[GraphicsColorLoad::Load])
        .unwrap();
    h.record(target);
    let ticket = h.submit();
    assert!(h.ctx.destroy_resource(target).is_err());
    h.reset();
    h.ctx.destroy_resource(superseded).unwrap();
    h.ctx.destroy_resource(target).unwrap();
    assert!(
        h.ctx
            .cmd_set_color_targets(h.list, &[target], &[[0; 4]])
            .is_err()
    );
    let mut created = 0;
    while h.create_guest_device() == 0 {
        created += 1;
    }
    // Five harness objects + two retained targets + one receipt.
    assert_eq!(created, 4088);
    h.control.complete(&[8, 7, 6, 5]);
    assert_eq!(
        h.ctx.wait_submission(ticket).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    assert_eq!(h.ctx.last_presentation.lock().unwrap().pixels, [8, 7, 6, 5]);
    assert!(
        h.ctx.destroy_resource(target).is_err(),
        "completion must not resurrect target"
    );
    assert_eq!(h.create_guest_device(), 0);
    assert_eq!(h.create_guest_device(), 0);
    assert_eq!(h.create_guest_device(), 2);
    h.ctx.release_submission(ticket).unwrap();
    assert_eq!(h.create_guest_device(), 0);
    assert_eq!(h.create_guest_device(), 2);
}

#[test]
fn failed_completion_preserves_colors_states_and_presentation() {
    for malformed in [false, true] {
        let mut h = Harness::new();
        let target = h.target();
        h.record(target);
        h.ctx
            .cmd_resource_barrier(
                h.list,
                target,
                ResourceStates::RENDER_TARGET,
                ResourceStates::COPY_SOURCE,
            )
            .unwrap();
        let ticket = h.submit();
        *h.control.result.lock().unwrap() = Some(if malformed {
            Ok(GraphicsPassOutput {
                colors: vec![vec![99; 3]],
                depth: None,
            })
        } else {
            Err(GraphicsError::BackendError("controlled failure".into()))
        });
        assert_eq!(
            h.ctx.wait_submission(ticket).unwrap(),
            GraphicsSubmissionStatus::Failed
        );
        assert!(h.ctx.submission_error(ticket).unwrap().is_some());
        assert_eq!(
            h.ctx.poll_submission(ticket).unwrap(),
            GraphicsSubmissionStatus::Failed
        );
        assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
        h.reset();
        // Synchronous execution is allowed with a terminal, unreleased receipt.
        h.ctx.set_require_metal_without_backend();
        h.ctx
            .cmd_set_color_targets_with_loads(h.list, &[target], &[GraphicsColorLoad::Load])
            .unwrap();
        h.ctx.close_command_list(h.list).unwrap();
        assert_eq!(
            h.ctx
                .execute_command_list(h.queue, h.list, &h.memory)
                .unwrap()
                .pixels,
            [0; 4]
        );
        h.ctx.release_submission(ticket).unwrap();
    }
}

#[test]
fn context_drop_waits_for_drain_without_publishing() {
    let mut h = Harness::new();
    let target = h.target();
    h.record(target);
    let _ticket = h.submit();
    h.reset();
    h.ctx.destroy_resource(target).unwrap();
    let presentation = h.ctx.last_presentation.clone();
    let control = h.control.clone();
    control.complete(&[1, 2, 3, 4]);
    drop(h);
    assert_eq!(control.waits.load(Ordering::SeqCst), 1);
    assert_eq!(presentation.lock().unwrap().frame_index, 0);
}

#[test]
fn synchronous_call_on_async_backend_waits_and_uses_same_commit() {
    let mut h = Harness::new();
    let target = h.target();
    h.record(target);
    h.ctx.close_command_list(h.list).unwrap();
    h.control.complete(&[3, 5, 7, 9]);
    let result = h
        .ctx
        .execute_command_list(h.queue, h.list, &h.memory)
        .unwrap();
    assert_eq!(result.pixels, [3, 5, 7, 9]);
    assert_eq!(h.control.waits.load(Ordering::SeqCst), 1);
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 1);
}

#[test]
fn synchronous_async_failure_releases_internal_receipt() {
    let mut h = Harness::new();
    let target = h.target();
    h.record(target);
    h.ctx.close_command_list(h.list).unwrap();
    *h.control.result.lock().unwrap() = Some(Err(GraphicsError::BackendError("failure".into())));
    assert!(matches!(
        h.ctx.execute_command_list(h.queue, h.list, &h.memory),
        Err(GraphicsError::BackendError(_))
    ));
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    h.control.complete(&[2, 4, 6, 8]);
    assert_eq!(
        h.ctx
            .execute_command_list(h.queue, h.list, &h.memory)
            .unwrap()
            .pixels,
        [2, 4, 6, 8]
    );
    // Success also releases its receipt, allowing another ordinary submission.
    let ticket = h
        .ctx
        .submit_command_list(h.queue, h.list, &h.memory)
        .unwrap();
    h.control.complete(&[1, 3, 5, 7]);
    assert_eq!(
        h.ctx.wait_submission(ticket).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    assert_eq!(
        h.ctx.submission_outcome(ticket).unwrap().unwrap().pixels,
        [1, 3, 5, 7]
    );
    assert!(
        h.ctx
            .execute_command_list(h.queue, h.list, &h.memory)
            .is_err()
    );
    h.ctx.release_submission(ticket).unwrap();
}

#[test]
fn pinned_barrier_resources_reject_transfers_and_superseded_pipeline_retires() {
    let mut h = Harness::new();
    let target = h.target();
    let upload = h
        .ctx
        .create_committed_resource(h.device, buffer(4), ResourceStates::COPY_DEST, None)
        .unwrap();
    let readback = h
        .ctx
        .create_committed_resource(h.device, buffer(4), ResourceStates::COPY_SOURCE, None)
        .unwrap();
    let pipeline = h
        .ctx
        .create_graphics_pipeline(
            h.device,
            include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin"),
            include_bytes!("../../../fixtures/rdna2/x9-constant-ps.bin"),
        )
        .unwrap();
    h.ctx
        .cmd_set_graphics_pipeline(h.list, Some(pipeline))
        .unwrap();
    h.ctx.cmd_set_graphics_pipeline(h.list, None).unwrap();
    h.ctx
        .cmd_resource_barrier(
            h.list,
            upload,
            ResourceStates::COPY_DEST,
            ResourceStates::COPY_DEST,
        )
        .unwrap();
    h.ctx
        .cmd_resource_barrier(
            h.list,
            readback,
            ResourceStates::COPY_SOURCE,
            ResourceStates::COPY_SOURCE,
        )
        .unwrap();
    h.record(target);
    let ticket = h.submit();
    assert_eq!(
        h.ctx
            .copy_guest_to_resource(upload, 0, 0x1000, 4, &h.memory),
        Err(GraphicsError::ResourceLifetimeViolation)
    );
    assert_eq!(
        h.ctx
            .copy_resource_to_guest(readback, 0, 0x3000, 4, &mut h.memory),
        Err(GraphicsError::ResourceLifetimeViolation)
    );
    h.reset();
    h.ctx.destroy_graphics_pipeline(pipeline).unwrap();
    assert!(
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .is_err()
    );
    h.ctx.destroy_resource(upload).unwrap();
    h.ctx.destroy_resource(readback).unwrap();
    let mut created = 0;
    while h.create_guest_device() == 0 {
        created += 1;
    }
    // Five harness objects, target, two buffers, pipeline and receipt remain charged.
    assert_eq!(created, 4086);
    h.control.complete(&[0; 4]);
    assert_eq!(
        h.ctx.wait_submission(ticket).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    for _ in 0..3 {
        assert_eq!(h.create_guest_device(), 0);
    }
    assert_eq!(h.create_guest_device(), 2);
}

#[test]
fn validation_or_submit_rejection_creates_no_receipt() {
    let mut h = Harness::new();
    let target = h.target();
    assert!(
        h.ctx
            .submit_command_list(h.queue, h.list, &h.memory)
            .is_err()
    );
    assert!(h.control.requests.lock().unwrap().is_empty());
    h.record(target);
    h.ctx.close_command_list(h.list).unwrap();
    h.ctx.set_metal_async_backend(|_| {
        Err(GraphicsError::BackendError(
            "submit rejected, drained".into(),
        ))
    });
    assert!(
        h.ctx
            .submit_command_list(h.queue, h.list, &h.memory)
            .is_err()
    );
    let mut created = 0;
    while h.create_guest_device() == 0 {
        created += 1;
    }
    assert_eq!(created, 4090); // Five harness objects plus one target, no receipt.
}

#[test]
fn malformed_second_color_does_not_commit_depth_or_first_color() {
    let mut h = Harness::new();
    let a = h.target();
    let b = h.target();
    let depth = h.ctx.create_depth_resource(h.device, (1, 1)).unwrap();
    // Seed committed colors and depth through the same async completion path.
    h.ctx
        .cmd_set_color_targets(h.list, &[a, b], &[[1, 2, 3, 4], [5, 6, 7, 8]])
        .unwrap();
    h.ctx
        .cmd_set_depth_target(h.list, Some(depth), GraphicsDepthLoad::Clear(1.0))
        .unwrap();
    let seed = h.submit();
    *h.control.result.lock().unwrap() = Some(Ok(GraphicsPassOutput {
        colors: vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]],
        depth: Some(Arc::new(37u32)),
    }));
    assert_eq!(
        h.ctx.wait_submission(seed).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    h.ctx.release_submission(seed).unwrap();
    h.reset();
    h.ctx
        .cmd_set_color_targets_with_loads(h.list, &[a, b], &[GraphicsColorLoad::Load; 2])
        .unwrap();
    h.ctx
        .cmd_set_depth_target(h.list, Some(depth), GraphicsDepthLoad::Load)
        .unwrap();
    let failed = h.submit();
    *h.control.result.lock().unwrap() = Some(Ok(GraphicsPassOutput {
        colors: vec![vec![99; 4], vec![88; 3]],
        depth: Some(Arc::new(91u32)),
    }));
    assert_eq!(
        h.ctx.wait_submission(failed).unwrap(),
        GraphicsSubmissionStatus::Failed
    );
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 1);
    assert_eq!(h.ctx.last_presentation.lock().unwrap().pixels, [1, 2, 3, 4]);
    h.ctx.release_submission(failed).unwrap();
    let probe = h
        .ctx
        .submit_command_list(h.queue, h.list, &h.memory)
        .unwrap();
    {
        let requests = h.control.requests.lock().unwrap();
        let request = requests.last().unwrap();
        assert_eq!(
            request.colors[0].previous.as_deref(),
            Some([1, 2, 3, 4].as_slice())
        );
        assert_eq!(
            request.colors[1].previous.as_deref(),
            Some([5, 6, 7, 8].as_slice())
        );
        assert_eq!(
            *request
                .depth
                .as_ref()
                .unwrap()
                .previous
                .as_ref()
                .unwrap()
                .downcast_ref::<u32>()
                .unwrap(),
            37
        );
    }
    // Missing depth must also fail without advancing the presentation.
    *h.control.result.lock().unwrap() = Some(Ok(GraphicsPassOutput {
        colors: vec![vec![99; 4], vec![88; 4]],
        depth: None,
    }));
    assert_eq!(
        h.ctx.wait_submission(probe).unwrap(),
        GraphicsSubmissionStatus::Failed
    );
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 1);
}

#[test]
fn poisoned_presentation_lock_prevents_resource_commit() {
    let mut h = Harness::new();
    let target = h.target();
    h.record(target);
    let ticket = h.submit();
    let presentation = h.ctx.last_presentation.clone();
    let _ = std::panic::catch_unwind(move || {
        let _guard = presentation.lock().unwrap();
        panic!("controlled presentation poison");
    });
    h.control.complete(&[99; 4]);
    assert_eq!(
        h.ctx.wait_submission(ticket).unwrap(),
        GraphicsSubmissionStatus::Failed
    );
    h.ctx.last_presentation.clear_poison();
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    h.ctx.release_submission(ticket).unwrap();
    let probe = h
        .ctx
        .submit_command_list(h.queue, h.list, &h.memory)
        .unwrap();
    assert_eq!(
        h.control.requests.lock().unwrap().last().unwrap().colors[0]
            .previous
            .as_deref(),
        Some([0; 4].as_slice())
    );
    h.control.complete(&[0; 4]);
    h.ctx.wait_submission(probe).unwrap();
}

#[test]
fn retired_superseded_pipelines_remain_charged_until_terminal_collection() {
    const VS: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin");
    const PS: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-constant-ps.bin");
    let mut h = Harness::new();
    let target = h.target();
    let mut pipelines = Vec::new();
    for _ in 0..256 {
        let pipeline = h.ctx.create_graphics_pipeline(h.device, VS, PS).unwrap();
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        pipelines.push(pipeline);
    }
    // Supersede every pipeline before drawing; all recorded bindings still pin.
    h.ctx.cmd_set_graphics_pipeline(h.list, None).unwrap();
    h.record(target);
    let ticket = h.submit();
    assert_eq!(
        h.ctx.poll_submission(ticket).unwrap(),
        GraphicsSubmissionStatus::Pending
    );
    h.reset();
    for pipeline in pipelines {
        h.ctx.destroy_graphics_pipeline(pipeline).unwrap();
    }
    let while_pending = h.ctx.create_graphics_pipeline(h.device, VS, PS);
    // Backend completion alone does not retire objects: the loader must collect it.
    h.control.complete(&[1, 2, 3, 4]);
    let before_collection = h.ctx.create_graphics_pipeline(h.device, VS, PS);
    assert_eq!(
        h.ctx.wait_submission(ticket).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    assert_eq!(while_pending, Err(GraphicsError::InvalidDescriptor));
    assert_eq!(before_collection, Err(GraphicsError::InvalidDescriptor));
    let replacement = h.ctx.create_graphics_pipeline(h.device, VS, PS).unwrap();
    h.ctx.destroy_graphics_pipeline(replacement).unwrap();
    h.ctx.release_submission(ticket).unwrap();
}

#[test]
fn retired_barrier_only_buffers_keep_byte_budget_until_terminal_collection() {
    use xenora_loader::memory::MemoryError;

    let mut h = Harness::new();
    let target = h.target();
    // Guest-backed vertices still allocate 48 host bytes. Together with the
    // 4-byte target, four (64 MiB - 13) buffers exactly fill the 256 MiB budget.
    const BUFFER_BYTES: u64 = 64 * 1024 * 1024 - 13;
    let mut buffers = Vec::new();
    for _ in 0..4 {
        let resource = h
            .ctx
            .create_committed_resource(
                h.device,
                buffer(BUFFER_BYTES),
                ResourceStates::COPY_DEST,
                None,
            )
            .unwrap();
        h.ctx
            .cmd_resource_barrier(
                h.list,
                resource,
                ResourceStates::COPY_DEST,
                ResourceStates::COPY_DEST,
            )
            .unwrap();
        buffers.push(resource);
    }
    h.ctx
        .cmd_set_color_targets(h.list, &[target], &[[1, 2, 3, 4]])
        .unwrap();
    let ticket = h.submit();
    h.reset();
    for resource in buffers {
        h.ctx.destroy_resource(resource).unwrap();
    }
    // No large buffer is uploaded or copied into the owned backend request.
    let while_pending =
        h.ctx
            .create_committed_resource(h.device, buffer(1), ResourceStates::COPY_DEST, None);
    h.control.complete(&[1, 2, 3, 4]);
    let before_collection =
        h.ctx
            .create_committed_resource(h.device, buffer(1), ResourceStates::COPY_DEST, None);
    // Also exercise the depth allocation path's shared byte-budget accounting.
    let depth_before_collection = h.ctx.create_depth_resource(h.device, (1, 1));
    assert_eq!(
        h.ctx.wait_submission(ticket).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    assert_eq!(
        while_pending,
        Err(GraphicsError::Memory(MemoryError::AllocationFailed))
    );
    assert_eq!(
        before_collection,
        Err(GraphicsError::Memory(MemoryError::AllocationFailed))
    );
    assert_eq!(
        depth_before_collection,
        Err(GraphicsError::Memory(MemoryError::AllocationFailed))
    );
    assert!(h.control.requests.lock().unwrap()[0].draws.is_empty());

    // The receipt remains live, but terminal collection refunds all retired bytes.
    // Refill only after retirement to keep peak resource storage at 256 MiB.
    let mut replacements = Vec::new();
    for _ in 0..4 {
        replacements.push(
            h.ctx
                .create_committed_resource(
                    h.device,
                    buffer(BUFFER_BYTES),
                    ResourceStates::COPY_DEST,
                    None,
                )
                .unwrap(),
        );
    }
    assert_eq!(
        h.ctx
            .create_committed_resource(h.device, buffer(1), ResourceStates::COPY_DEST, None,),
        Err(GraphicsError::Memory(MemoryError::AllocationFailed))
    );
    for resource in replacements {
        h.ctx.destroy_resource(resource).unwrap();
    }
    let depth = h.ctx.create_depth_resource(h.device, (1, 1)).unwrap();
    h.ctx.destroy_resource(depth).unwrap();
    h.ctx.release_submission(ticket).unwrap();
}

#[test]
fn cpu_multislot_async_owns_all_four_pairs_and_superseded_resources() {
    let mut h = Harness::new();
    let worker = h.control.clone();
    h.ctx.set_metal_multislot_async_backend(move |request| {
        worker.requests.lock().unwrap().push(request);
        Ok(Box::new(Pending(worker.clone())))
    });
    let pipeline = h
        .ctx
        .create_graphics_pipeline(
            h.device,
            include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin"),
            include_bytes!("../../../fixtures/rdna2/x9-multislot-ps.bin"),
        )
        .unwrap();
    let texture = h
        .ctx
        .create_committed_resource(
            h.device,
            ResourceDesc {
                dimension: ResourceDimension::Texture2D,
                width: 1,
                height: 1,
                depth_or_array_size: 1,
                mip_levels: 1,
                format_rgba8: true,
            },
            ResourceStates::PIXEL_SHADER_RESOURCE,
            None,
        )
        .unwrap();
    let superseded = h
        .ctx
        .create_committed_resource(
            h.device,
            ResourceDesc {
                dimension: ResourceDimension::Texture2D,
                width: 1,
                height: 1,
                depth_or_array_size: 1,
                mip_levels: 1,
                format_rgba8: true,
            },
            ResourceStates::PIXEL_SHADER_RESOURCE,
            None,
        )
        .unwrap();
    let samplers = [Some(xenora_texture::SamplerDesc::nearest_clamp()); 2];
    h.ctx
        .cmd_set_sampled_bindings(h.list, [Some(superseded); 2], samplers)
        .unwrap();
    h.ctx
        .cmd_set_sampled_bindings(h.list, [Some(texture); 2], samplers)
        .unwrap();
    h.ctx
        .cmd_set_graphics_pipeline(h.list, Some(pipeline))
        .unwrap();
    let target = h.target();
    h.record(target);
    let receipt = h.submit();
    {
        let requests = h.control.requests.lock().unwrap();
        let draw = &requests[0].draws[0];
        assert_eq!(draw.pipeline.as_ref().unwrap().sampled_pairs, 15);
        assert!(draw.sampled.is_none());
        assert!(Arc::ptr_eq(
            &draw.sampled_bindings.textures[0].as_ref().unwrap().pixels,
            &draw.sampled_bindings.textures[1].as_ref().unwrap().pixels
        ));
    }
    assert!(h.ctx.destroy_resource(superseded).is_err());
    h.reset();
    h.ctx.destroy_resource(texture).unwrap();
    h.ctx.destroy_resource(superseded).unwrap();
    h.ctx.destroy_graphics_pipeline(pipeline).unwrap();
    assert_eq!(
        h.ctx.poll_submission(receipt).unwrap(),
        GraphicsSubmissionStatus::Pending
    );
    h.control.complete(&[13, 17, 19, 23]);
    assert_eq!(
        h.ctx.wait_submission(receipt).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    h.ctx.release_submission(receipt).unwrap();
}
