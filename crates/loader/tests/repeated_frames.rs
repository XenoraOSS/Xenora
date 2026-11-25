// SPDX-License-Identifier: GPL-3.0-or-later

//! Repeated-frame regression for the guest graphics context (issue #148).
//!
//! One device, queue, allocator and command list are driven through the
//! per-frame loop a D3D12 guest uses: reset, record, close, execute. Each
//! frame changes the guest-backed vertex data, the clear colour or the draw
//! parameters. Expected pixels are written out by hand below for triangles
//! chosen so their coverage of a 4×3 viewport is obvious; they are not
//! computed by the rasterizer under test. These tests use the software
//! reference backend only; no native Metal result is claimed here.

use xenora_loader::graphics::{
    CommandListType, GraphicsBackendId, GraphicsContext, GraphicsError, GraphicsHandle,
    ResourceDesc, ResourceDimension, ResourceStates,
};
use xenora_loader::memory::{GuestMemory, MemoryPermissions};

const WIDTH: usize = 4;
const HEIGHT: usize = 3;
const VERTICES: u64 = 0x0010_0000;
/// Two triangles of three float4 positions each.
const VERTEX_BYTES: usize = 96;
const RED: [u8; 4] = [255, 0, 0, 255];

/// Covers every pixel centre of the viewport.
const FULL: [[f32; 4]; 3] = [
    [-1.0, -1.0, 0.0, 1.0],
    [3.0, -1.0, 0.0, 1.0],
    [-1.0, 3.0, 0.0, 1.0],
];
/// Its right edge is x = 0 in clip space, so it covers the two left columns
/// (centres at x = -0.75 and -0.25) and none of the two right ones.
const LEFT_HALF: [[f32; 4]; 3] = [
    [0.0, 10.0, 0.0, 1.0],
    [0.0, -10.0, 0.0, 1.0],
    [-30.0, 0.0, 0.0, 1.0],
];
/// Lies wholly outside the viewport.
const OFFSCREEN: [[f32; 4]; 3] = [
    [2.0, 2.0, 0.0, 1.0],
    [3.0, 2.0, 0.0, 1.0],
    [2.0, 3.0, 0.0, 1.0],
];

struct Rig {
    ctx: GraphicsContext,
    memory: GuestMemory,
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
    // A new list starts open; close it so every frame begins with a reset.
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
        queue,
        alloc,
        list,
        vertices,
        target,
    }
}

fn write_triangle(memory: &mut GuestMemory, slot: u64, triangle: [[f32; 4]; 3]) {
    let bytes: Vec<u8> = triangle
        .iter()
        .flatten()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    memory.write_bytes(VERTICES + slot * 48, &bytes).unwrap();
}

/// Pixels with `columns` red from the left and the rest in `clear`.
fn expected(columns: usize, clear: [u8; 4]) -> Vec<u8> {
    (0..WIDTH * HEIGHT)
        .flat_map(|pixel| if pixel % WIDTH < columns { RED } else { clear })
        .collect()
}

struct Draw {
    clear: [u8; 4],
    offset: usize,
    vertex_count: u32,
    instance_count: u32,
    start_vertex: u32,
    start_instance: u32,
}

impl Draw {
    fn one(clear: [u8; 4]) -> Self {
        Self {
            clear,
            offset: 0,
            vertex_count: 3,
            instance_count: 1,
            start_vertex: 0,
            start_instance: 0,
        }
    }
}

/// Record one frame: target to RENDER_TARGET, draw, back to PRESENT.
fn record(rig: &mut Rig, draw: &Draw) -> Result<(), GraphicsError> {
    let ctx = &mut rig.ctx;
    ctx.reset_command_list(rig.list, rig.alloc)?;
    ctx.cmd_resource_barrier(
        rig.list,
        rig.target,
        ResourceStates::PRESENT,
        ResourceStates::RENDER_TARGET,
    )?;
    ctx.cmd_set_viewport(rig.list, WIDTH, HEIGHT)?;
    ctx.cmd_set_render_targets(rig.list, Some(rig.target), draw.clear)?;
    ctx.cmd_set_vertex_buffer(rig.list, rig.vertices, draw.offset, 48)?;
    ctx.cmd_draw_instanced(
        rig.list,
        draw.vertex_count,
        draw.instance_count,
        draw.start_vertex,
        draw.start_instance,
    )?;
    ctx.cmd_resource_barrier(
        rig.list,
        rig.target,
        ResourceStates::RENDER_TARGET,
        ResourceStates::PRESENT,
    )?;
    ctx.close_command_list(rig.list)
}

fn frame(rig: &mut Rig, draw: &Draw) -> Result<(Vec<u8>, usize), GraphicsError> {
    record(rig, draw)?;
    let outcome = rig
        .ctx
        .execute_command_list(rig.queue, rig.list, &rig.memory)?;
    assert_eq!(outcome.backend, GraphicsBackendId::SoftwareReference);
    assert_eq!((outcome.width, outcome.height), (WIDTH, HEIGHT));
    Ok((outcome.pixels, outcome.draw_count))
}

fn frame_index(rig: &Rig) -> u64 {
    rig.ctx.last_presentation.lock().unwrap().frame_index
}

#[test]
fn each_frame_reflects_its_own_guest_vertices_and_clear_colour() {
    let mut rig = rig();
    let frames = [
        (LEFT_HALF, [0, 0, 64, 255], 2),
        (FULL, [10, 20, 30, 255], WIDTH),
        (OFFSCREEN, [1, 2, 3, 4], 0),
        (LEFT_HALF, [9, 9, 9, 255], 2),
    ];
    for (index, (triangle, clear, columns)) in frames.into_iter().enumerate() {
        write_triangle(&mut rig.memory, 0, triangle);
        let (pixels, draws) = frame(&mut rig, &Draw::one(clear)).unwrap();
        let want = expected(columns, clear);
        assert_eq!(pixels, want, "frame {index}");
        assert_eq!(draws, 1, "frame {index}");
        assert_eq!(frame_index(&rig), index as u64 + 1);
        // The presentation holds this frame's pixels, not an earlier one's.
        let presented = rig.ctx.last_presentation.lock().unwrap().pixels.clone();
        assert_eq!(presented, want, "presented frame {index}");
    }
}

#[test]
fn a_zero_instance_or_zero_vertex_frame_draws_nothing() {
    let mut rig = rig();
    write_triangle(&mut rig.memory, 0, FULL);
    let clear = [5, 6, 7, 255];
    for (vertices, instances) in [(3, 0), (0, 1), (2, 1)] {
        let draw = Draw {
            vertex_count: vertices,
            instance_count: instances,
            ..Draw::one(clear)
        };
        let (pixels, draws) = frame(&mut rig, &draw).unwrap();
        assert_eq!(pixels, expected(0, clear), "{vertices} x {instances}");
        assert_eq!(draws, 0, "{vertices} x {instances}");
    }
    // The next ordinary frame still draws.
    let (pixels, draws) = frame(&mut rig, &Draw::one(clear)).unwrap();
    assert_eq!((pixels, draws), (expected(WIDTH, clear), 1));
}

#[test]
fn unsupported_draw_parameters_fail_and_the_next_frame_recovers() {
    let mut rig = rig();
    write_triangle(&mut rig.memory, 0, LEFT_HALF);
    write_triangle(&mut rig.memory, 1, FULL);
    let clear = [0, 0, 0, 255];
    frame(&mut rig, &Draw::one(clear)).unwrap();
    let rejected = [
        Draw {
            start_vertex: 3,
            ..Draw::one(clear)
        },
        Draw {
            instance_count: 2,
            ..Draw::one(clear)
        },
        Draw {
            start_instance: 1,
            ..Draw::one(clear)
        },
        Draw {
            vertex_count: 6,
            ..Draw::one(clear)
        },
    ];
    for draw in &rejected {
        // Only the draw is rejected. The barrier before it already ran, so
        // the target is left in RENDER_TARGET, as a guest would see it.
        let before = frame_index(&rig);
        let result = frame(&mut rig, draw);
        assert!(
            matches!(result, Err(GraphicsError::Unsupported(_))),
            "{result:?}"
        );
        assert_eq!(frame_index(&rig), before, "a failed frame is not presented");
        rig.ctx.reset_command_list(rig.list, rig.alloc).unwrap();
        rig.ctx
            .cmd_resource_barrier(
                rig.list,
                rig.target,
                ResourceStates::RENDER_TARGET,
                ResourceStates::PRESENT,
            )
            .unwrap();
        rig.ctx.close_command_list(rig.list).unwrap();
        rig.ctx
            .execute_command_list(rig.queue, rig.list, &rig.memory)
            .unwrap();
    }
    // The second triangle is still reachable through the view offset.
    let draw = Draw {
        offset: 48,
        ..Draw::one(clear)
    };
    let (pixels, _) = frame(&mut rig, &draw).unwrap();
    assert_eq!(pixels, expected(WIDTH, clear));
}

#[test]
fn a_vertex_view_outside_its_buffer_fails_without_reading_past_it() {
    let mut rig = rig();
    write_triangle(&mut rig.memory, 0, FULL);
    write_triangle(&mut rig.memory, 1, FULL);
    // Mapped guest memory continues past the buffer; the view must not.
    write_triangle(&mut rig.memory, 2, FULL);
    let clear = [0, 0, 0, 255];
    for offset in [VERTEX_BYTES - 47, VERTEX_BYTES, usize::MAX - 100] {
        let draw = Draw {
            offset,
            ..Draw::one(clear)
        };
        assert_eq!(
            frame(&mut rig, &draw),
            Err(GraphicsError::IndexOutOfRange),
            "offset {offset}"
        );
        rig.ctx.reset_command_list(rig.list, rig.alloc).unwrap();
        rig.ctx
            .cmd_resource_barrier(
                rig.list,
                rig.target,
                ResourceStates::RENDER_TARGET,
                ResourceStates::PRESENT,
            )
            .unwrap();
        rig.ctx.close_command_list(rig.list).unwrap();
        rig.ctx
            .execute_command_list(rig.queue, rig.list, &rig.memory)
            .unwrap();
    }
}
