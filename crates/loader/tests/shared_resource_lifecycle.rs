// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared CPU and GPU resource ownership under repeated submissions (#152).
//!
//! Two ownership interfaces exist today and both are exercised here.
//!
//! The guest graphics context (`GraphicsContext`) hands out handles that
//! carry a generation. It has no release call yet, so a stale or retired
//! handle can only be expressed as a live id with the wrong generation; the
//! tests check that every object kind rejects such handles and that a
//! rejected call changes nothing. The context also tracks each resource's
//! state across submissions, and its vertex data lives in guest memory that
//! the CPU keeps writing between submissions. These tests use the software
//! reference backend and run in every `cargo test`.
//!
//! The native Metal compute context (`NativeCompute`) owns each submission's
//! command buffer, pipeline and buffers until Metal reports completion. Those
//! tests need an Apple GPU and run only with the `gpu-tests` feature.
//!
//! Expected pixels and words are written out by hand or computed with plain
//! integer arithmetic below; none come from the code under test.

use xenora_loader::graphics::{
    CommandListType, GraphicsBackendId, GraphicsContext, GraphicsError, GraphicsHandle,
    ResourceDesc, ResourceDimension, ResourceStates,
};
use xenora_loader::memory::{GuestMemory, MemoryPermissions};

const WIDTH: usize = 4;
const HEIGHT: usize = 3;
const VERTICES: u64 = 0x0020_0000;
const VERTEX_BYTES: usize = 48;
const RED: [u8; 4] = [255, 0, 0, 255];

/// Covers every pixel centre of the 4×3 viewport.
const FULL: [[f32; 4]; 3] = [
    [-1.0, -1.0, 0.0, 1.0],
    [3.0, -1.0, 0.0, 1.0],
    [-1.0, 3.0, 0.0, 1.0],
];
/// Lies wholly outside the viewport, so only the clear colour shows.
const OFFSCREEN: [[f32; 4]; 3] = [
    [2.0, 2.0, 0.0, 1.0],
    [3.0, 2.0, 0.0, 1.0],
    [2.0, 3.0, 0.0, 1.0],
];

struct Rig {
    ctx: GraphicsContext,
    memory: GuestMemory,
    device: GraphicsHandle,
    queue: GraphicsHandle,
    alloc: GraphicsHandle,
    list: GraphicsHandle,
    vertices: GraphicsHandle,
    target: GraphicsHandle,
}

fn rig() -> Rig {
    let mut ctx = GraphicsContext::new();
    let device = ctx.create_device().unwrap();
    let queue = ctx
        .create_command_queue(device, CommandListType::Direct)
        .unwrap();
    let alloc = ctx
        .create_command_allocator(device, CommandListType::Direct)
        .unwrap();
    let list = ctx
        .create_command_list(device, alloc, CommandListType::Direct)
        .unwrap();
    ctx.close_command_list(list).unwrap();
    let mut memory = GuestMemory::new();
    memory
        .map_region(VERTICES, 0x1000, MemoryPermissions::READ_WRITE)
        .unwrap();
    let buffer = ResourceDesc {
        dimension: ResourceDimension::Buffer,
        width: VERTEX_BYTES as u64,
        height: 1,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: false,
    };
    let vertices = ctx
        .create_committed_resource(
            device,
            buffer,
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
            Some(VERTICES),
        )
        .unwrap();
    let texture = ResourceDesc {
        dimension: ResourceDimension::Texture2D,
        width: WIDTH as u64,
        height: HEIGHT as u32,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: true,
    };
    let target = ctx
        .create_committed_resource(device, texture, ResourceStates::PRESENT, None)
        .unwrap();
    Rig {
        ctx,
        memory,
        device,
        queue,
        alloc,
        list,
        vertices,
        target,
    }
}

fn write_triangle(memory: &mut GuestMemory, triangle: [[f32; 4]; 3]) {
    let bytes: Vec<u8> = triangle
        .iter()
        .flatten()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    memory.write_bytes(VERTICES, &bytes).unwrap();
}

fn solid(colour: [u8; 4]) -> Vec<u8> {
    colour.repeat(WIDTH * HEIGHT)
}

/// The same id with the next generation: what a handle looks like after its
/// slot has been reused.
fn next_generation(handle: GraphicsHandle) -> GraphicsHandle {
    GraphicsHandle {
        id: handle.id,
        generation: handle.generation.wrapping_add(1),
    }
}

/// Every forged form of `handle` the context must refuse.
fn forged(handle: GraphicsHandle) -> [GraphicsHandle; 4] {
    [
        next_generation(handle),
        GraphicsHandle {
            id: handle.id,
            generation: 0,
        },
        GraphicsHandle::NULL,
        GraphicsHandle {
            id: u32::MAX,
            generation: handle.generation,
        },
    ]
}

/// The transitions a frame records around its draw.
#[derive(Clone, Copy)]
struct Transitions {
    acquire_from: ResourceStates,
    release: bool,
}

const NORMAL: Transitions = Transitions {
    acquire_from: ResourceStates::PRESENT,
    release: true,
};

fn record(rig: &mut Rig, clear: [u8; 4], transitions: Transitions) -> Result<(), GraphicsError> {
    let ctx = &mut rig.ctx;
    ctx.reset_command_list(rig.list, rig.alloc)?;
    ctx.cmd_resource_barrier(
        rig.list,
        rig.target,
        transitions.acquire_from,
        ResourceStates::RENDER_TARGET,
    )?;
    ctx.cmd_set_viewport(rig.list, WIDTH, HEIGHT)?;
    ctx.cmd_set_render_targets(rig.list, Some(rig.target), clear)?;
    ctx.cmd_set_vertex_buffer(rig.list, rig.vertices, 0, 48)?;
    ctx.cmd_draw_instanced(rig.list, 3, 1, 0, 0)?;
    if transitions.release {
        ctx.cmd_resource_barrier(
            rig.list,
            rig.target,
            ResourceStates::RENDER_TARGET,
            ResourceStates::PRESENT,
        )?;
    }
    ctx.close_command_list(rig.list)
}

fn submit(rig: &mut Rig) -> Result<Vec<u8>, GraphicsError> {
    let outcome = rig
        .ctx
        .execute_command_list(rig.queue, rig.list, &rig.memory)?;
    assert_eq!(outcome.backend, GraphicsBackendId::SoftwareReference);
    assert_eq!((outcome.width, outcome.height), (WIDTH, HEIGHT));
    assert_eq!(outcome.draw_count, 1);
    Ok(outcome.pixels)
}

fn presented(rig: &Rig) -> (u64, Vec<u8>) {
    let presentation = rig.ctx.last_presentation.lock().unwrap();
    (presentation.frame_index, presentation.pixels.clone())
}

#[test]
fn live_handles_start_at_generation_one_and_round_trip_through_guest_words() {
    let rig = rig();
    for handle in [
        rig.device,
        rig.queue,
        rig.alloc,
        rig.list,
        rig.vertices,
        rig.target,
    ] {
        assert_eq!(handle.generation, 1);
        assert_ne!(handle, GraphicsHandle::NULL);
        let word = handle.to_u64();
        assert_eq!(word, (1u64 << 32) | u64::from(handle.id));
        assert_eq!(GraphicsHandle::from_u64(word), handle);
        // A guest that keeps an old word sees a different generation in the
        // upper half, which must decode to a different handle.
        assert_eq!(
            GraphicsHandle::from_u64(word + (1u64 << 32)),
            next_generation(handle)
        );
    }
}

#[test]
fn creation_rejects_forged_device_and_allocator_handles() {
    let mut rig = rig();
    let buffer = ResourceDesc {
        dimension: ResourceDimension::Buffer,
        width: 16,
        height: 1,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: false,
    };
    for device in forged(rig.device) {
        assert_eq!(
            rig.ctx
                .create_command_queue(device, CommandListType::Direct)
                .unwrap_err(),
            GraphicsError::InvalidDeviceHandle
        );
        assert_eq!(
            rig.ctx
                .create_command_allocator(device, CommandListType::Direct)
                .unwrap_err(),
            GraphicsError::InvalidDeviceHandle
        );
        assert_eq!(
            rig.ctx
                .create_command_list(device, rig.alloc, CommandListType::Direct)
                .unwrap_err(),
            GraphicsError::InvalidDeviceHandle
        );
        assert_eq!(
            rig.ctx
                .create_committed_resource(device, buffer.clone(), ResourceStates::COMMON, None)
                .unwrap_err(),
            GraphicsError::InvalidDeviceHandle
        );
    }
    for alloc in forged(rig.alloc) {
        assert_eq!(
            rig.ctx
                .create_command_list(rig.device, alloc, CommandListType::Direct)
                .unwrap_err(),
            GraphicsError::InvalidAllocatorHandle
        );
    }
    // The rejections allocated nothing: the next live object gets the id
    // right after the six the rig created.
    let fresh = rig
        .ctx
        .create_command_allocator(rig.device, CommandListType::Direct)
        .unwrap();
    assert_eq!(
        fresh,
        GraphicsHandle {
            id: 7,
            generation: 1
        }
    );
}

#[test]
fn forged_handles_during_recording_append_nothing_and_the_frame_still_renders() {
    let mut rig = rig();
    write_triangle(&mut rig.memory, FULL);
    let (list, alloc, target, vertices) = (rig.list, rig.alloc, rig.target, rig.vertices);

    for stale in forged(alloc) {
        assert_eq!(
            rig.ctx.reset_command_list(list, stale),
            Err(GraphicsError::InvalidAllocatorHandle)
        );
    }
    for stale in forged(list) {
        assert_eq!(
            rig.ctx.reset_command_list(stale, alloc),
            Err(GraphicsError::InvalidCommandListHandle)
        );
    }
    // Neither failed reset reopened the closed list.
    assert_eq!(
        rig.ctx.cmd_set_viewport(list, WIDTH, HEIGHT),
        Err(GraphicsError::CommandListNotRecording)
    );

    let ctx = &mut rig.ctx;
    ctx.reset_command_list(list, alloc).unwrap();
    ctx.cmd_resource_barrier(
        list,
        target,
        ResourceStates::PRESENT,
        ResourceStates::RENDER_TARGET,
    )
    .unwrap();
    for stale in forged(target) {
        // Each would leave the target in the wrong state, or bind nothing,
        // if it were recorded.
        assert_eq!(
            ctx.cmd_resource_barrier(
                list,
                stale,
                ResourceStates::RENDER_TARGET,
                ResourceStates::PRESENT
            ),
            Err(GraphicsError::InvalidResourceHandle)
        );
        assert_eq!(
            ctx.cmd_set_render_targets(list, Some(stale), [0, 0, 255, 255]),
            Err(GraphicsError::InvalidResourceHandle)
        );
    }
    ctx.cmd_set_viewport(list, WIDTH, HEIGHT).unwrap();
    ctx.cmd_set_render_targets(list, Some(target), [0, 0, 0, 255])
        .unwrap();
    for stale in forged(vertices) {
        assert_eq!(
            ctx.cmd_set_vertex_buffer(list, stale, 0, 48),
            Err(GraphicsError::InvalidResourceHandle)
        );
    }
    ctx.cmd_set_vertex_buffer(list, vertices, 0, 48).unwrap();
    ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
    ctx.cmd_resource_barrier(
        list,
        target,
        ResourceStates::RENDER_TARGET,
        ResourceStates::PRESENT,
    )
    .unwrap();
    for stale in forged(list) {
        assert_eq!(
            ctx.close_command_list(stale),
            Err(GraphicsError::InvalidCommandListHandle)
        );
        assert_eq!(
            ctx.cmd_draw_instanced(stale, 3, 1, 0, 0),
            Err(GraphicsError::InvalidCommandListHandle)
        );
    }
    ctx.close_command_list(list).unwrap();

    // Had any forged call been recorded, the draw count, clear colour or the
    // final target state would differ.
    assert_eq!(submit(&mut rig).unwrap(), solid(RED));
    write_triangle(&mut rig.memory, OFFSCREEN);
    assert_eq!(submit(&mut rig).unwrap(), solid([0, 0, 0, 255]));
}

#[test]
fn submission_rejects_forged_queue_and_list_handles_without_presenting() {
    let mut rig = rig();
    write_triangle(&mut rig.memory, FULL);
    record(&mut rig, [0, 255, 0, 255], NORMAL).unwrap();
    for queue in forged(rig.queue) {
        assert_eq!(
            rig.ctx
                .execute_command_list(queue, rig.list, &rig.memory)
                .unwrap_err(),
            GraphicsError::InvalidQueueHandle
        );
    }
    for list in forged(rig.list) {
        assert_eq!(
            rig.ctx
                .execute_command_list(rig.queue, list, &rig.memory)
                .unwrap_err(),
            GraphicsError::InvalidCommandListHandle
        );
    }
    assert_eq!(presented(&rig), (0, Vec::new()));
    assert_eq!(submit(&mut rig).unwrap(), solid(RED));
    assert_eq!(presented(&rig), (1, solid(RED)));
}

#[test]
fn a_list_still_recording_cannot_be_submitted() {
    let mut rig = rig();
    write_triangle(&mut rig.memory, FULL);
    rig.ctx.reset_command_list(rig.list, rig.alloc).unwrap();
    rig.ctx.cmd_set_viewport(rig.list, WIDTH, HEIGHT).unwrap();
    assert_eq!(
        rig.ctx
            .execute_command_list(rig.queue, rig.list, &rig.memory)
            .unwrap_err(),
        GraphicsError::CommandListNotRecording
    );
    assert_eq!(presented(&rig), (0, Vec::new()));
}

#[test]
fn repeated_submit_cycles_read_the_guest_memory_current_at_each_submission() {
    let mut rig = rig();
    for cycle in 0..64u8 {
        let clear = [cycle, 0, 255 - cycle, 255];
        let covers = cycle % 3 != 0;
        write_triangle(&mut rig.memory, if covers { FULL } else { OFFSCREEN });
        record(&mut rig, clear, NORMAL).unwrap();
        let expected = if covers { solid(RED) } else { solid(clear) };
        assert_eq!(submit(&mut rig).unwrap(), expected, "cycle {cycle}");
        assert_eq!(presented(&rig), (u64::from(cycle) + 1, expected));
    }
}

#[test]
fn resubmitting_a_closed_list_uses_the_vertices_written_after_recording() {
    let mut rig = rig();
    let clear = [0, 0, 255, 255];
    write_triangle(&mut rig.memory, FULL);
    record(&mut rig, clear, NORMAL).unwrap();
    for round in 0..8u64 {
        write_triangle(&mut rig.memory, FULL);
        assert_eq!(submit(&mut rig).unwrap(), solid(RED));
        // The CPU rewrites the shared buffer after the list was closed and
        // submitted; the next submission of the same list must see it.
        write_triangle(&mut rig.memory, OFFSCREEN);
        assert_eq!(submit(&mut rig).unwrap(), solid(clear));
        assert_eq!(presented(&rig).0, 2 * round + 2);
    }
}

fn refused(rig: &mut Rig, clear: [u8; 4], transitions: Transitions) {
    record(rig, clear, transitions).unwrap();
    assert_eq!(
        rig.ctx
            .execute_command_list(rig.queue, rig.list, &rig.memory)
            .unwrap_err(),
        GraphicsError::ResourceLifetimeViolation
    );
}

/// `PRESENT` and `COMMON` are both 0, and the context accepts any barrier
/// whose "before" state is `COMMON` without comparing it, so a frame that
/// acquires from `PRESENT` cannot detect a target still held as a render
/// target. These checks therefore use non-zero "before" states, which the
/// context does compare with the state left by the previous submission.
#[test]
fn target_state_carries_across_submissions_and_a_held_target_must_be_released() {
    let mut rig = rig();
    let held = Transitions {
        acquire_from: ResourceStates::PRESENT,
        release: false,
    };
    let from = |acquire_from| Transitions {
        acquire_from,
        release: true,
    };
    let mut frames = 0;
    for _ in 0..3 {
        // In PRESENT, claiming it is a render target is a mismatch.
        refused(
            &mut rig,
            [0, 0, 0, 255],
            from(ResourceStates::RENDER_TARGET),
        );
        assert_eq!(presented(&rig).0, frames);

        // This frame leaves the target in RENDER_TARGET.
        write_triangle(&mut rig.memory, FULL);
        record(&mut rig, [0, 0, 0, 255], held).unwrap();
        assert_eq!(submit(&mut rig).unwrap(), solid(RED));
        frames += 1;

        // The next submission sees that state: any other non-zero claim is
        // refused, and nothing is presented.
        for wrong in [
            ResourceStates::COPY_DEST,
            ResourceStates::PIXEL_SHADER_RESOURCE,
        ] {
            refused(&mut rig, [1, 1, 1, 255], from(wrong));
            assert_eq!(presented(&rig), (frames, solid(RED)));
        }

        // Acquiring from the state it is really in releases it to PRESENT.
        write_triangle(&mut rig.memory, OFFSCREEN);
        record(
            &mut rig,
            [9, 9, 9, 255],
            from(ResourceStates::RENDER_TARGET),
        )
        .unwrap();
        assert_eq!(submit(&mut rig).unwrap(), solid([9, 9, 9, 255]));
        frames += 1;

        // Ordinary frames work again.
        write_triangle(&mut rig.memory, FULL);
        record(&mut rig, [0, 0, 0, 255], NORMAL).unwrap();
        assert_eq!(submit(&mut rig).unwrap(), solid(RED));
        frames += 1;
        assert_eq!(presented(&rig), (frames, solid(RED)));
    }
}

#[cfg(feature = "gpu-tests")]
mod native_submissions {
    //! Completion-aware ownership in `NativeCompute`. The kernel is an
    //! original fixture; its expected words are `3x + 7` with wrapping, as
    //! computed by `expected` on the CPU.

    use xenora_metal::{MetalError, NativeCompute};

    const LIMIT: usize = 16;
    const SOURCE: &str = r"
#include <metal_stdlib>
using namespace metal;
kernel void lifecycle(device const uint *input [[buffer(0)]],
                      device uint *output [[buffer(1)]],
                      uint id [[thread_position_in_grid]]) {
    output[id] = input[id] * 3u + 7u;
}
";
    const ENTRY: &str = "lifecycle";

    fn native<T>(result: Result<T, MetalError>) -> T {
        result.unwrap_or_else(|error| {
            if error.is_capability_unavailable() {
                panic!(
                    "CAPABILITY UNAVAILABLE on this device, not an implementation failure: {error}"
                )
            }
            panic!("IMPLEMENTATION FAILURE: {error}")
        })
    }

    fn context() -> NativeCompute {
        native(NativeCompute::new())
    }

    fn input(round: u32, slot: u32) -> [u32; 3] {
        let base = round.wrapping_mul(0x0101_0101) ^ slot;
        [base, base.wrapping_add(1), u32::MAX - base]
    }

    fn expected(input: &[u32]) -> Vec<u32> {
        input
            .iter()
            .map(|word| word.wrapping_mul(3).wrapping_add(7))
            .collect()
    }

    fn assert_idle(metal: &NativeCompute) {
        assert_eq!((metal.in_flight(), metal.tracked()), (0, 0));
    }

    #[test]
    fn repeated_submit_and_retire_rounds_leave_nothing_tracked() {
        let metal = context();
        for round in 0..24u32 {
            let width = 1 + (round as usize % LIMIT);
            let tokens: Vec<_> = (0..width as u32)
                .map(|slot| {
                    let words = input(round, slot);
                    (words, native(metal.submit_words(SOURCE, ENTRY, &words)))
                })
                .collect();
            assert_eq!(metal.tracked(), width, "round {round}");
            // Collect odd rounds newest first.
            let ordered: Vec<_> = if round % 2 == 0 {
                tokens
            } else {
                tokens.into_iter().rev().collect()
            };
            for (words, token) in ordered {
                assert_eq!(native(metal.wait(token)).output, expected(&words));
            }
            assert_idle(&metal);
        }
    }

    #[test]
    fn dropped_tokens_hold_capacity_until_their_work_completes() {
        let metal = context();
        for round in 0..3u32 {
            let (first, gate) = native(metal.submit_words_gated(SOURCE, ENTRY, &input(round, 0)));
            drop(first);
            // The gated command buffer holds back every later one on the queue.
            for slot in 1..(LIMIT - 1) as u32 {
                drop(native(metal.submit_words(
                    SOURCE,
                    ENTRY,
                    &input(round, slot),
                )));
            }
            let words = input(round, 100);
            let last = native(metal.submit_words(SOURCE, ENTRY, &words));
            assert_eq!(metal.in_flight(), LIMIT);
            assert_eq!(metal.tracked(), LIMIT);
            let Err(error) = metal.submit_words(SOURCE, ENTRY, &input(round, 99)) else {
                panic!("a seventeenth submission was accepted while sixteen were in flight");
            };
            assert_eq!(error.operation, "in-flight limit");
            // The refusal took no slot.
            assert_eq!(metal.tracked(), LIMIT);

            gate.open();
            // The kept token is last on the queue; collecting it sweeps the
            // abandoned entries ahead of it.
            assert_eq!(native(metal.wait(last)).output, expected(&words));
            assert_idle(&metal);
        }
    }

    #[test]
    fn a_token_waited_on_another_context_stays_owned_by_its_issuer() {
        let owner = context();
        let other = context();
        let words = input(7, 7);
        let (token, gate) = native(owner.submit_words_gated(SOURCE, ENTRY, &words));
        let Err(error) = other.wait(token) else {
            panic!("another context collected a submission it does not own");
        };
        assert_eq!(error.operation, "submission");
        assert_idle(&other);
        // The rejected token was dropped; its issuer still owns the pending
        // work and keeps it until completion.
        assert_eq!((owner.in_flight(), owner.tracked()), (1, 1));
        gate.open();
        let next = input(8, 8);
        let token = native(owner.submit_words(SOURCE, ENTRY, &next));
        assert_eq!(native(owner.wait(token)).output, expected(&next));
        assert_idle(&owner);
    }

    #[test]
    fn failed_submissions_take_no_slot_at_the_limit() {
        let metal = context();
        let (first, gate) = native(metal.submit_words_gated(SOURCE, ENTRY, &input(0, 0)));
        let mut tokens = vec![(input(0, 0), first)];
        for slot in 1..(LIMIT - 1) as u32 {
            let words = input(0, slot);
            tokens.push((words, native(metal.submit_words(SOURCE, ENTRY, &words))));
        }
        assert_eq!(metal.in_flight(), LIMIT - 1);
        // One slot left. Each invalid request is refused before it takes it.
        let too_many = vec![0u32; 64 * 64 + 1];
        for (source, entry, words) in [
            ("not valid MSL", ENTRY, &[1u32][..]),
            (SOURCE, "missing_entry", &[1u32][..]),
            (SOURCE, ENTRY, &[][..]),
            (SOURCE, ENTRY, &too_many[..]),
        ] {
            assert!(metal.submit_words(source, entry, words).is_err());
            assert_eq!((metal.in_flight(), metal.tracked()), (LIMIT - 1, LIMIT - 1));
        }
        let words = input(0, 50);
        tokens.push((words, native(metal.submit_words(SOURCE, ENTRY, &words))));
        assert_eq!(metal.in_flight(), LIMIT);
        gate.open();
        for (words, token) in tokens {
            assert_eq!(native(metal.wait(token)).output, expected(&words));
        }
        assert_idle(&metal);
    }
}
