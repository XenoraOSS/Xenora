// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::{Duration, Instant};
use xenora_loader::graphics::{
    CommandListType, GraphicsBackendId, GraphicsContext, GraphicsError, GraphicsHandle,
    ResourceDesc, ResourceDimension, ResourceStates,
};
use xenora_loader::memory::{GuestMemory, MemoryPermissions};

const DIRECT: CommandListType = CommandListType::Direct;
const DEADLINE: Duration = Duration::from_secs(1);
const ZERO: Duration = Duration::ZERO;

struct Draws {
    device: GraphicsHandle,
    queues: [GraphicsHandle; 2],
    lists: [GraphicsHandle; 2],
    targets: [GraphicsHandle; 2],
    memory: GuestMemory,
}

impl Draws {
    fn new(ctx: &mut GraphicsContext) -> Self {
        let device = ctx.create_device().unwrap();
        let queues = std::array::from_fn(|_| ctx.create_command_queue(device, DIRECT).unwrap());
        let allocator = ctx.create_command_allocator(device, DIRECT).unwrap();
        let vertex_address = 0x20_000;
        let mut memory = GuestMemory::new();
        // In a 4×3 viewport these clip coordinates map to (0,0), (2.2,0)
        // and (0,2.85). No pixel centre lands on an edge.
        let vertices: [f32; 12] = [
            -1.0, 1.0, 0.5, 1.0, 0.1, 1.0, 0.5, 1.0, -1.0, -0.9, 0.5, 1.0,
        ];
        let bytes: Vec<_> = vertices.into_iter().flat_map(f32::to_le_bytes).collect();
        memory
            .map_region_with_data(vertex_address, 4096, MemoryPermissions::READ_ONLY, &bytes)
            .unwrap();
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
                Some(vertex_address),
            )
            .unwrap();
        let targets = std::array::from_fn(|_| {
            ctx.create_committed_resource(
                device,
                ResourceDesc {
                    dimension: ResourceDimension::Texture2D,
                    width: 4,
                    height: 3,
                    depth_or_array_size: 1,
                    mip_levels: 1,
                    format_rgba8: true,
                },
                ResourceStates::RENDER_TARGET,
                None,
            )
            .unwrap()
        });
        let lists = std::array::from_fn(|index| {
            let list = ctx.create_command_list(device, allocator, DIRECT).unwrap();
            ctx.cmd_set_viewport(list, 4, 3).unwrap();
            ctx.cmd_set_render_targets(list, Some(targets[index]), clear(index))
                .unwrap();
            ctx.cmd_set_vertex_buffer(list, vertex, 0, 16).unwrap();
            ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
            ctx.close_command_list(list).unwrap();
            list
        });
        Self {
            device,
            queues,
            lists,
            targets,
            memory,
        }
    }
}

fn clear(index: usize) -> [u8; 4] {
    [[19, 43, 93, 255], [83, 21, 11, 255]][index]
}

fn expected(index: usize) -> Vec<u8> {
    (0..12)
        .flat_map(|pixel| match pixel {
            0 | 1 | 4 => [255, 0, 0, 255],
            _ => clear(index),
        })
        .collect()
}

fn two_draws(ctx: &mut GraphicsContext, cross_queue: bool, backend: GraphicsBackendId) -> Vec<u64> {
    let draws = Draws::new(ctx);
    let [producer, second] = draws.queues;
    let consumer = if cross_queue { second } else { producer };
    let fence = ctx.create_fence(draws.device, 3).unwrap();
    let mut completed = vec![ctx.fence_completed_value(fence).unwrap()];

    if cross_queue {
        // Wait returns even though the signal has not been submitted. Until
        // it is submitted, retrying the consumer must leave its target alone.
        ctx.queue_wait(consumer, fence, 7).unwrap();
        assert_eq!(
            ctx.destroy_fence(fence),
            Err(GraphicsError::ResourceLifetimeViolation)
        );
        assert_eq!(
            ctx.execute_command_list(consumer, draws.lists[1], &draws.memory),
            Err(GraphicsError::FenceTimeout {
                requested: 7,
                completed: 3
            })
        );
        assert_eq!(ctx.resource_bytes(draws.targets[1]).unwrap(), &[0; 48]);
    }

    let first = ctx
        .execute_command_list(producer, draws.lists[0], &draws.memory)
        .unwrap();
    assert_eq!(first.backend, backend);
    assert_eq!(first.draw_count, 1);
    assert_eq!(first.pixels, expected(0));
    assert_eq!(ctx.fence_completed_value(fence).unwrap(), 3);
    ctx.queue_signal(producer, fence, 7).unwrap();
    if !cross_queue {
        ctx.queue_wait(consumer, fence, 7).unwrap();
    }
    let next = ctx
        .execute_command_list(consumer, draws.lists[1], &draws.memory)
        .unwrap();
    assert_eq!(next.backend, backend);
    assert_eq!(next.draw_count, 1);
    completed.push(ctx.fence_completed_value(fence).unwrap());
    assert_eq!(next.pixels, expected(1));
    assert_eq!(ctx.resource_bytes(draws.targets[0]).unwrap(), expected(0));
    assert_eq!(ctx.resource_bytes(draws.targets[1]).unwrap(), expected(1));

    ctx.queue_signal(consumer, fence, 13).unwrap();
    ctx.wait_for_fence(fence, 13, DEADLINE).unwrap();
    completed.push(ctx.fence_completed_value(fence).unwrap());
    ctx.destroy_fence(fence).unwrap();
    assert_eq!(
        ctx.fence_completed_value(fence),
        Err(GraphicsError::InvalidFenceHandle)
    );
    completed
}

fn unreached_host_wait(ctx: &mut GraphicsContext) {
    let device = ctx.create_device().unwrap();
    let fence = ctx.create_fence(device, 9).unwrap();
    let deadline = Duration::from_millis(20);
    let start = Instant::now();
    assert_eq!(
        ctx.wait_for_fence(fence, 10, deadline),
        Err(GraphicsError::FenceTimeout {
            requested: 10,
            completed: 9
        })
    );
    assert!((Duration::from_millis(10)..DEADLINE).contains(&start.elapsed()));
    assert_eq!(
        ctx.wait_for_fence(fence, 10, ZERO),
        Err(GraphicsError::FenceTimeout {
            requested: 10,
            completed: 9
        })
    );
    ctx.wait_for_fence(fence, 9, ZERO).unwrap();
}

#[test]
fn software_ordering_readback_and_timeout() {
    for cross_queue in [false, true] {
        assert_eq!(
            two_draws(
                &mut GraphicsContext::new(),
                cross_queue,
                GraphicsBackendId::SoftwareReference
            ),
            [3, 7, 13]
        );
    }
    unreached_host_wait(&mut GraphicsContext::new());
}

#[test]
fn fence_values_handles_lifetimes_and_capacity_are_checked() {
    let mut ctx = GraphicsContext::new();
    let device = ctx.create_device().unwrap();
    let other = ctx.create_device().unwrap();
    let queue = ctx.create_command_queue(device, DIRECT).unwrap();
    let fence = ctx.create_fence(device, 5).unwrap();
    let foreign = ctx.create_fence(other, 0).unwrap();

    assert!(matches!(
        ctx.queue_signal(queue, fence, 4),
        Err(GraphicsError::Unsupported(_))
    ));
    assert!(matches!(
        ctx.queue_signal(queue, fence, u64::MAX),
        Err(GraphicsError::Unsupported(_))
    ));
    assert!(matches!(
        ctx.queue_wait(queue, foreign, 1),
        Err(GraphicsError::Unsupported(_))
    ));
    assert!(matches!(
        ctx.create_fence(device, u64::MAX),
        Err(GraphicsError::Unsupported(_))
    ));
    assert!(matches!(
        ctx.wait_for_fence(fence, 8, DEADLINE * 2),
        Err(GraphicsError::Unsupported(_))
    ));
    assert_eq!(
        ctx.queue_wait(queue, device, 1),
        Err(GraphicsError::InvalidFenceHandle)
    );
    assert_eq!(
        ctx.queue_signal(fence, fence, 5),
        Err(GraphicsError::InvalidQueueHandle)
    );
    assert_eq!(ctx.fence_completed_value(fence).unwrap(), 5);

    ctx.queue_wait(queue, fence, 8).unwrap();
    ctx.queue_wait(queue, fence, 7).unwrap();
    // A queue cannot satisfy its own prerequisite with a later signal.
    assert_eq!(
        ctx.queue_signal(queue, fence, 8),
        Err(GraphicsError::FenceTimeout {
            requested: 8,
            completed: 5
        })
    );
    assert_eq!(ctx.fence_completed_value(fence).unwrap(), 5);
    ctx.destroy_command_queue(queue).unwrap();
    assert_eq!(
        ctx.destroy_device(device),
        Err(GraphicsError::ResourceLifetimeViolation)
    );
    ctx.destroy_fence(fence).unwrap();
    assert_ne!(ctx.create_fence(device, 0).unwrap(), fence);

    let queue = ctx.create_command_queue(device, DIRECT).unwrap();
    for index in 0..14 {
        let fence = ctx.create_fence(device, 0).unwrap();
        let result = ctx.queue_wait(queue, fence, 0);
        if index < 8 {
            result.unwrap();
        } else {
            assert_eq!(result, Err(GraphicsError::FenceLimitExceeded));
        }
    }
    assert_eq!(
        ctx.create_fence(device, 0),
        Err(GraphicsError::FenceLimitExceeded)
    );

    let mut native_only = GraphicsContext::new();
    native_only.set_require_metal_without_backend();
    let device = native_only.create_device().unwrap();
    assert_eq!(
        native_only.create_fence(device, 0),
        Err(GraphicsError::NativeMetalRequired)
    );
}

#[cfg(feature = "gpu-tests")]
#[test]
fn metal_shared_event_ordering_readback_and_timeout() {
    use std::sync::Arc;
    use xenora_loader::graphics::{FenceOperation, NativeFence};
    use xenora_metal::NativeCompute;
    use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};

    let gpu = Arc::new(NativeCompute::new().unwrap());
    let msl = compile_graphics(
        Architecture::Gfx1030,
        include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin"),
        include_bytes!("../../../fixtures/rdna2/x9-constant-ps.bin"),
    )
    .unwrap()
    .msl;
    let mut native = GraphicsContext::new();
    let renderer = gpu.clone();
    native.set_metal_backend(move |vertices, (width, height), clear| {
        renderer
            .render_rgba8(
                &msl,
                [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
                vertices,
                &[[0.0; 4]; 3],
                [width, height],
                clear,
            )
            .map_err(|error| GraphicsError::BackendError(error.to_string()))
    });
    native
        .set_native_fence_factory(move |initial| {
            let fence = gpu
                .new_render_fence(initial)
                .map_err(|error| GraphicsError::BackendError(error.to_string()))?;
            let adapter: NativeFence = Box::new(move |operation| {
                match operation {
                    FenceOperation::CompletedValue => fence.completed_value(),
                    FenceOperation::Signal(value) => fence.signal(value),
                    FenceOperation::QueueWait { value, timeout } => {
                        fence.queue_wait(value, timeout)
                    }
                    FenceOperation::HostWait { value, timeout } => fence.host_wait(value, timeout),
                }
                .map_err(|error| GraphicsError::BackendError(error.to_string()))
            });
            Ok(adapter)
        })
        .unwrap();

    for cross_queue in [false, true] {
        let cpu = two_draws(
            &mut GraphicsContext::new(),
            cross_queue,
            GraphicsBackendId::SoftwareReference,
        );
        let gpu = two_draws(&mut native, cross_queue, GraphicsBackendId::MetalNative);
        assert_eq!(gpu, cpu);
    }
    unreached_host_wait(&mut native);
}
