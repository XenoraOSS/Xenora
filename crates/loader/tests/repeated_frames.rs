// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-driven submissions through the loader graphics API, not a new guest ABI.
//! Pixel expectations come from full coverage or complete clipping, never the
//! production rasterizer. Native checks fail if Metal is unavailable.

use xenora_loader::graphics::{
    CommandListType, ExecutionOutcome, GraphicsBackendId, GraphicsContext, GraphicsError,
    GraphicsHandle, ResourceDesc, ResourceDimension, ResourceStates,
};
use xenora_loader::memory::{GuestMemory, MemoryError, MemoryPermissions};

const VERTICES: u64 = 0x10000;
const COVERED: [[f32; 4]; 3] = [
    [-1.0, -1.0, 0.0, 1.0],
    [3.0, -1.0, 0.0, 1.0],
    [-1.0, 3.0, 0.0, 1.0],
];
const CLIPPED: [[f32; 4]; 3] = [
    [2.0, 2.0, 0.0, 1.0],
    [3.0, 2.0, 0.0, 1.0],
    [2.0, 3.0, 0.0, 1.0],
];

struct Frames {
    ctx: GraphicsContext,
    memory: GuestMemory,
    queue: GraphicsHandle,
    allocator: GraphicsHandle,
    list: GraphicsHandle,
    buffer: GraphicsHandle,
}

impl Frames {
    fn new(mut ctx: GraphicsContext) -> Self {
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
        let buffer = ctx
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
                Some(VERTICES),
            )
            .unwrap();
        let mut memory = GuestMemory::new();
        memory
            .map_region(VERTICES, 4096, MemoryPermissions::READ_WRITE)
            .unwrap();
        Self {
            ctx,
            memory,
            queue,
            allocator,
            list,
            buffer,
        }
    }

    fn record(&mut self, frame: u8) -> Vec<u8> {
        let visible = frame.is_multiple_of(2);
        let vertices = if visible { COVERED } else { CLIPPED };
        let bytes: Vec<_> = vertices
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        self.memory.write_bytes(VERTICES, &bytes).unwrap();
        let (width, height) = if frame.is_multiple_of(3) {
            (4, 3)
        } else {
            (3, 2)
        };
        let clear = [frame, 73, 151, 255];
        self.ctx
            .reset_command_list(self.list, self.allocator)
            .unwrap();
        self.ctx.cmd_set_viewport(self.list, width, height).unwrap();
        self.ctx
            .cmd_set_render_targets(self.list, None, clear)
            .unwrap();
        self.ctx
            .cmd_set_vertex_buffer(self.list, self.buffer, 0, 16)
            .unwrap();
        self.ctx.cmd_draw_instanced(self.list, 3, 1, 0, 0).unwrap();
        self.ctx.close_command_list(self.list).unwrap();
        let color = if visible { [255, 0, 0, 255] } else { clear };
        color.repeat(width * height)
    }

    fn submit(&mut self) -> Result<ExecutionOutcome, GraphicsError> {
        self.ctx
            .execute_command_list(self.queue, self.list, &self.memory)
    }

    fn assert_presented(&self, count: u64, outcome: &ExecutionOutcome) {
        let last = self.ctx.last_presentation.lock().unwrap();
        assert_eq!(last.frame_index, count);
        assert_eq!(last.width, outcome.width);
        assert_eq!(last.height, outcome.height);
        assert_eq!(last.pixels, outcome.pixels);
        assert_eq!(last.backend, Some(outcome.backend));
    }
}

fn sequence(ctx: GraphicsContext, backend: GraphicsBackendId) -> Vec<ExecutionOutcome> {
    let mut frames = Frames::new(ctx);
    let mut snapshots = Vec::new();
    for frame in 0..48u8 {
        let expected = frames.record(frame);
        // The same buffer handle must read newly written guest bytes each time.
        let outcome = frames.submit().expect("frame submission");
        let dimensions = if frame.is_multiple_of(3) {
            (4, 3)
        } else {
            (3, 2)
        };
        assert_eq!((outcome.width, outcome.height), dimensions);
        assert_eq!(outcome.backend, backend);
        assert_eq!(outcome.draw_count, 1, "reset must discard prior draws");
        assert_eq!(outcome.pixels, expected, "frame {frame}");
        frames.assert_presented(u64::from(frame) + 1, &outcome);

        if frame == 23 {
            // Guest backing disappears: the resource handle alone cannot make
            // this submission valid. A failed frame must not be presented.
            assert_eq!(
                frames
                    .ctx
                    .execute_command_list(frames.queue, frames.list, &GuestMemory::new()),
                Err(GraphicsError::Memory(MemoryError::UnmappedAddress(
                    VERTICES
                )))
            );
            frames.assert_presented(24, &outcome);

            // Reset must also discard the old vertex-buffer binding.
            frames
                .ctx
                .reset_command_list(frames.list, frames.allocator)
                .unwrap();
            frames
                .ctx
                .cmd_draw_instanced(frames.list, 3, 1, 0, 0)
                .unwrap();
            frames.ctx.close_command_list(frames.list).unwrap();
            assert_eq!(frames.submit(), Err(GraphicsError::InvalidResourceHandle));
            frames.assert_presented(24, &outcome);
        }
        snapshots.push(outcome);
    }
    // Retained readback buffers remain owned snapshots after subsequent frames
    // and destruction of the graphics context.
    drop(frames);
    for (frame, outcome) in snapshots.iter().enumerate() {
        let count = if frame.is_multiple_of(3) { 12 } else { 6 };
        let color = if frame.is_multiple_of(2) {
            [255, 0, 0, 255]
        } else {
            [frame as u8, 73, 151, 255]
        };
        assert_eq!(
            outcome.pixels,
            color.repeat(count),
            "retained frame {frame}"
        );
    }
    snapshots
}

#[test]
fn repeated_software_frames_update_recover_and_replay() {
    let first = sequence(GraphicsContext::new(), GraphicsBackendId::SoftwareReference);
    let second = sequence(GraphicsContext::new(), GraphicsBackendId::SoftwareReference);
    assert_eq!(first, second);
}

#[test]
fn backend_failure_preserves_presentation_and_allows_retry() {
    let mut frames = Frames::new(GraphicsContext::new());
    let expected = frames.record(1);
    let previous = frames.submit().unwrap();
    assert_eq!(previous.pixels, expected);
    let expected = frames.record(2);
    // This is fault injection only, not evidence of native GPU execution.
    frames.ctx.set_metal_backend(|_, _, _| {
        Err(GraphicsError::BackendError(
            "injected submission failure".into(),
        ))
    });
    assert_eq!(
        frames.submit(),
        Err(GraphicsError::BackendError(
            "injected submission failure".into()
        ))
    );
    frames.assert_presented(1, &previous);
    frames
        .ctx
        .set_metal_backend(GraphicsContext::execute_software_reference_triangles);
    let recovered = frames.submit().unwrap();
    assert_eq!(recovered.pixels, expected);
    frames.assert_presented(2, &recovered);
}

#[test]
#[cfg_attr(
    not(feature = "gpu-tests"),
    ignore = "needs native Metal; run scripts/test-gpu.sh"
)]
fn repeated_native_frames_update_recover_and_replay() {
    use xenora_metal::NativeCompute;
    use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};
    let make_context = || {
        let msl = compile_graphics(
            Architecture::Gfx1030,
            include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin"),
            include_bytes!("../../../fixtures/rdna2/x9-constant-ps.bin"),
        )
        .expect("translated fixture shaders")
        .msl;
        let gpu = NativeCompute::new().expect("native Metal device required");
        let mut ctx = GraphicsContext::new();
        ctx.set_metal_pass_backend(move |draws, viewport, clear| {
            use xenora_loader::graphics::{GraphicsBlendMode as B, GraphicsDepthCompare as D};
            use xenora_metal::{
                BlendFactor as F, BlendOperation as O, BlendState, ColorWriteMask, DepthCompare,
                DepthState, TriangleState,
            };
            let clear_depth = draws
                .iter()
                .any(|d| d.state.depth_compare.is_some())
                .then_some(1.0);
            let states: Vec<_> = draws
                .iter()
                .map(|draw| {
                    let state = draw.state;
                    let compare = match state.depth_compare {
                        None | Some(D::Always) => DepthCompare::Always,
                        Some(D::Never) => DepthCompare::Never,
                        Some(D::Less) => DepthCompare::Less,
                        Some(D::Equal) => DepthCompare::Equal,
                        Some(D::LessEqual) => DepthCompare::LessEqual,
                        Some(D::Greater) => DepthCompare::Greater,
                        Some(D::NotEqual) => DepthCompare::NotEqual,
                        Some(D::GreaterEqual) => DepthCompare::GreaterEqual,
                    };
                    let blend = match state.blend {
                        B::Replace => None,
                        B::Alpha => Some(BlendState {
                            source_rgb: F::SourceAlpha,
                            destination_rgb: F::OneMinusSourceAlpha,
                            rgb_operation: O::Add,
                            source_alpha: F::One,
                            destination_alpha: F::OneMinusSourceAlpha,
                            alpha_operation: O::Add,
                        }),
                        B::Additive => Some(BlendState {
                            source_rgb: F::One,
                            destination_rgb: F::One,
                            rgb_operation: O::Add,
                            source_alpha: F::One,
                            destination_alpha: F::One,
                            alpha_operation: O::Add,
                        }),
                    };
                    TriangleState {
                        viewport_origin: state.viewport_origin,
                        scissor: state.scissor,
                        depth: DepthState {
                            compare,
                            write: state.depth_write,
                        },
                        blend,
                        write_mask: ColorWriteMask {
                            red: state.color_write_mask & 1 != 0,
                            green: state.color_write_mask & 2 != 0,
                            blue: state.color_write_mask & 4 != 0,
                            alpha: state.color_write_mask & 8 != 0,
                        },
                    }
                })
                .collect();
            let colors: Vec<_> = draws
                .iter()
                .map(|d| vec![[0.0; 4]; d.vertices.len()])
                .collect();
            let draws: Vec<_> = draws
                .iter()
                .zip(&colors)
                .map(|(d, colors)| xenora_metal::TriangleDraw {
                    vertices: &d.vertices,
                    colors,
                    viewport: [d.viewport.0, d.viewport.1],
                })
                .collect();
            gpu.render_triangle_pass_with_state_rgba8(
                &msl,
                [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
                &xenora_metal::TrianglePass {
                    draws: &draws,
                    states: &states,
                    extent: [viewport.0, viewport.1],
                    clear,
                    clear_depth,
                },
            )
            .map_err(|e| GraphicsError::BackendError(e.to_string()))
        });
        ctx
    };
    bound_texture_defines_target_extent(make_context());
    resource_transfer_render(make_context());
    triangle_pair(make_context(), false);
    triangle_pair(make_context(), true);
    indexed_triangle_lists(make_context());
    viewport_pass(make_context());
    depth_blend_pass(make_context());
    strided_draws(make_context());
    let first = sequence(make_context(), GraphicsBackendId::MetalNative);
    let second = sequence(make_context(), GraphicsBackendId::MetalNative);
    assert_eq!(first, second);
}

// DrawInstanced start vertex and buffer-view stride follow the public contracts:
// https://learn.microsoft.com/windows/win32/api/d3d12/nf-d3d12-id3d12graphicscommandlist-drawinstanced
// https://learn.microsoft.com/windows/win32/api/d3d12/ns-d3d12-d3d12_vertex_buffer_view
fn strided_draws(ctx: GraphicsContext) {
    let mut frames = Frames::new(ctx);
    let device = frames.ctx.create_device().unwrap();
    frames.buffer = frames
        .ctx
        .create_committed_resource(
            device,
            ResourceDesc {
                dimension: ResourceDimension::Buffer,
                width: 160,
                height: 1,
                depth_or_array_size: 1,
                mip_levels: 1,
                format_rgba8: false,
            },
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
            Some(VERTICES),
        )
        .unwrap();
    for (offset, stride, start) in [(16usize, 32usize, 2u32), (0, 48, 1), (32, 32, 0)] {
        frames.memory.write_bytes(VERTICES, &[0; 160]).unwrap();
        for (i, vertex) in COVERED.iter().enumerate() {
            let bytes: Vec<_> = vertex.iter().flat_map(|v| v.to_le_bytes()).collect();
            let address = VERTICES + (offset + (start as usize + i) * stride) as u64;
            frames.memory.write_bytes(address, &bytes).unwrap();
        }
        frames
            .ctx
            .reset_command_list(frames.list, frames.allocator)
            .unwrap();
        frames.ctx.cmd_set_viewport(frames.list, 4, 3).unwrap();
        frames
            .ctx
            .cmd_set_vertex_buffer(frames.list, frames.buffer, offset, stride)
            .unwrap();
        frames
            .ctx
            .cmd_draw_instanced(frames.list, 3, 1, start, 0)
            .unwrap();
        frames.ctx.close_command_list(frames.list).unwrap();
        let result = frames.submit().unwrap();
        assert_eq!(result.draw_count, 1);
        assert_eq!(
            result.pixels,
            [255, 0, 0, 255].repeat(12),
            "offset={offset}, stride={stride}, start={start}"
        );
    }
}

#[test]
fn nonindexed_draw_honors_vertex_start_and_stride() {
    strided_draws(GraphicsContext::new());
}

#[test]
fn nonindexed_draw_rejects_unsupported_forms_and_out_of_bounds_reads() {
    let mut frames = Frames::new(GraphicsContext::new());
    // The mapped guest page is larger than the 48-byte vertex resource. Reads
    // must obey the resource boundary even when guest memory remains readable.
    for (count, instances, start, first_instance, stride, expected) in [
        (3, 1, 1, 0, 16, GraphicsError::IndexOutOfRange),
        (3, 1, u32::MAX, 0, 16, GraphicsError::IndexOutOfRange),
        (3, 1, 0, 0, 8, GraphicsError::InvalidDescriptor),
        (4, 1, 0, 0, 16, GraphicsError::InvalidDescriptor),
        (771, 1, 0, 0, 16, GraphicsError::InvalidDescriptor),
        (6, 1, 0, 0, 16, GraphicsError::IndexOutOfRange),
        (3, 2, 0, 0, 16, GraphicsError::InvalidDescriptor),
        (3, 1, 0, 1, 16, GraphicsError::InvalidDescriptor),
    ] {
        frames
            .ctx
            .reset_command_list(frames.list, frames.allocator)
            .unwrap();
        frames
            .ctx
            .cmd_set_vertex_buffer(frames.list, frames.buffer, 0, stride)
            .unwrap();
        frames
            .ctx
            .cmd_draw_instanced(frames.list, count, instances, start, first_instance)
            .unwrap();
        frames.ctx.close_command_list(frames.list).unwrap();
        assert_eq!(
            frames.submit(),
            Err(expected),
            "draw={count},{instances},{start},{first_instance}; stride={stride}"
        );
        assert_eq!(frames.ctx.last_presentation.lock().unwrap().frame_index, 0);
    }
}

#[test]
fn zero_instance_draw_does_not_fetch_vertices_or_render() {
    let mut frames = Frames::new(GraphicsContext::new());
    // No buffer is bound, so a mistakenly attempted draw fails.
    frames
        .ctx
        .cmd_draw_instanced(frames.list, 3, 0, 0, 0)
        .unwrap();
    frames.ctx.close_command_list(frames.list).unwrap();
    let result = frames.submit().unwrap();
    assert_eq!(result.draw_count, 0);
    assert_eq!(result.pixels, [0, 0, 0, 255].repeat(12));
}

fn triangle_pair(ctx: GraphicsContext, split: bool) {
    let mut frames = Frames::new(ctx);
    let device = frames.ctx.create_device().unwrap();
    frames.buffer = frames
        .ctx
        .create_committed_resource(
            device,
            ResourceDesc {
                dimension: ResourceDimension::Buffer,
                width: 96,
                height: 1,
                depth_or_array_size: 1,
                mip_levels: 1,
                format_rgba8: false,
            },
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
            Some(VERTICES),
        )
        .unwrap();
    // Two halves of the clip-space square: neither alone covers the viewport.
    let vertices: [[f32; 4]; 6] = [
        [-1.0, -1.0, 0.0, 1.0],
        [1.0, -1.0, 0.0, 1.0],
        [-1.0, 1.0, 0.0, 1.0],
        [1.0, -1.0, 0.0, 1.0],
        [1.0, 1.0, 0.0, 1.0],
        [-1.0, 1.0, 0.0, 1.0],
    ];
    let bytes: Vec<_> = vertices
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    frames.memory.write_bytes(VERTICES, &bytes).unwrap();
    frames.ctx.cmd_set_viewport(frames.list, 4, 3).unwrap();
    frames
        .ctx
        .cmd_set_vertex_buffer(frames.list, frames.buffer, 0, 16)
        .unwrap();
    if split {
        frames
            .ctx
            .cmd_draw_instanced(frames.list, 3, 1, 0, 0)
            .unwrap();
        frames
            .ctx
            .cmd_draw_instanced(frames.list, 0, 1, 0, 0)
            .unwrap();
        frames
            .ctx
            .cmd_draw_instanced(frames.list, 3, 1, 3, 0)
            .unwrap();
    } else {
        frames
            .ctx
            .cmd_draw_instanced(frames.list, 6, 1, 0, 0)
            .unwrap();
    }
    frames.ctx.close_command_list(frames.list).unwrap();
    let result = frames.submit().expect("two triangles share a target");
    assert_eq!(result.draw_count, if split { 2 } else { 1 });
    assert_eq!(result.pixels, [255, 0, 0, 255].repeat(12));
    frames.assert_presented(1, &result);
}

#[test]
fn one_draw_renders_both_triangles() {
    triangle_pair(GraphicsContext::new(), false);
}

#[test]
fn separate_draws_preserve_both_triangles() {
    triangle_pair(GraphicsContext::new(), true);
}

#[test]
fn combined_draw_limit_is_enforced() {
    let mut frames = Frames::new(GraphicsContext::new());
    frames.record(0);
    for count in [256, 257] {
        frames
            .ctx
            .reset_command_list(frames.list, frames.allocator)
            .unwrap();
        frames
            .ctx
            .cmd_set_vertex_buffer(frames.list, frames.buffer, 0, 16)
            .unwrap();
        for _ in 0..count {
            frames
                .ctx
                .cmd_draw_instanced(frames.list, 3, 1, 0, 0)
                .unwrap();
        }
        frames.ctx.close_command_list(frames.list).unwrap();
        if count == 256 {
            let out = frames.submit().unwrap();
            assert_eq!(out.draw_count, 256);
            assert_eq!(out.pixels, [255, 0, 0, 255].repeat(12));
        } else {
            assert_eq!(frames.submit(), Err(GraphicsError::InvalidDescriptor));
            assert_eq!(frames.ctx.last_presentation.lock().unwrap().frame_index, 1);
        }
    }
}

#[test]
fn invalid_later_commands_do_not_render_or_replace_presentation() {
    let mut frames = Frames::new(GraphicsContext::new());
    frames.record(0);
    let previous = frames.submit().unwrap();
    // A callback that panics proves validation completes before rendering.
    // This checks dispatch ordering only, not GPU behavior.
    frames
        .ctx
        .set_metal_backend(|_, _, _| panic!("invalid pass reached renderer"));
    for case in 0..4 {
        frames
            .ctx
            .reset_command_list(frames.list, frames.allocator)
            .unwrap();
        frames
            .ctx
            .cmd_set_vertex_buffer(frames.list, frames.buffer, 0, 16)
            .unwrap();
        frames
            .ctx
            .cmd_draw_instanced(frames.list, 3, 1, 0, 0)
            .unwrap();
        let expected = match case {
            0 => {
                frames
                    .ctx
                    .cmd_draw_instanced(frames.list, 3, 1, 3, 0)
                    .unwrap();
                GraphicsError::IndexOutOfRange
            }
            1 => {
                frames.ctx.cmd_set_viewport(frames.list, 2, 2).unwrap();
                GraphicsError::InvalidDescriptor
            }
            2 => {
                frames
                    .ctx
                    .cmd_set_render_targets(frames.list, None, [1, 2, 3, 255])
                    .unwrap();
                GraphicsError::InvalidDescriptor
            }
            _ => {
                frames
                    .ctx
                    .cmd_set_render_targets(frames.list, Some(frames.buffer), [0, 0, 0, 255])
                    .unwrap();
                GraphicsError::InvalidDescriptor
            }
        };
        frames.ctx.close_command_list(frames.list).unwrap();
        assert_eq!(frames.submit(), Err(expected));
        frames.assert_presented(1, &previous);
    }
}

fn indexed_triangle_lists(ctx: GraphicsContext) {
    let mut f = Frames::new(ctx);
    let device = f.ctx.create_device().unwrap();
    let mut buffer = |bytes, address| {
        f.ctx
            .create_committed_resource(
                device,
                ResourceDesc {
                    dimension: ResourceDimension::Buffer,
                    width: bytes,
                    height: 1,
                    depth_or_array_size: 1,
                    mip_levels: 1,
                    format_rgba8: false,
                },
                ResourceStates::COMMON,
                Some(address),
            )
            .unwrap()
    };
    f.buffer = buffer(96, VERTICES);
    let indices = buffer(3080, VERTICES + 512);
    let vertices: [[f32; 4]; 6] = [
        [-1.0, -1.0, 0.0, 1.0],
        [1.0, -1.0, 0.0, 1.0],
        [-1.0, 1.0, 0.0, 1.0],
        [1.0, -1.0, 0.0, 1.0],
        [1.0, 1.0, 0.0, 1.0],
        [-1.0, 1.0, 0.0, 1.0],
    ];
    let bytes: Vec<_> = vertices
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    f.memory.write_bytes(VERTICES, &bytes).unwrap();
    let mut presented = 0;
    for (format, size) in [(57, 2usize), (42, 4usize)] {
        for count in [6u32, 768] {
            // Both view offset and start index skip a poison entry. A negative
            // base vertex maps indices 1..6 to the six square vertices.
            let mut bytes = vec![0xff; size * 2];
            for i in 0..count {
                // Only the last six indices cover the square, so truncating
                // the maximum-size draw cannot accidentally pass.
                let index = if i < count - 6 { 1 } else { i % 6 + 1 };
                bytes.extend_from_slice(&index.to_le_bytes()[..size]);
            }
            f.memory.write_bytes(VERTICES + 512, &bytes).unwrap();
            f.ctx.reset_command_list(f.list, f.allocator).unwrap();
            f.ctx
                .cmd_set_vertex_buffer(f.list, f.buffer, 0, 16)
                .unwrap();
            f.ctx
                .cmd_set_index_buffer(f.list, indices, size, format)
                .unwrap();
            f.ctx
                .cmd_draw_indexed_instanced(f.list, count, 1, 1, -1, 0)
                .unwrap();
            f.ctx.close_command_list(f.list).unwrap();
            let out = f.submit().expect("indexed triangle list");
            assert_eq!(out.draw_count, 1);
            assert_eq!(out.pixels, [255, 0, 0, 255].repeat(12));
            presented += 1;
            f.assert_presented(presented, &out);
            // A bad final index must reject the whole submission, even after
            // all preceding triangles have valid indices.
            let last = VERTICES + 512 + ((count as usize + 1) * size) as u64;
            f.memory.write_bytes(last, &vec![0; size]).unwrap();
            assert_eq!(f.submit(), Err(GraphicsError::IndexOutOfRange));
            f.assert_presented(presented, &out);
        }
    }
}

#[test]
fn indexed_lists_honor_offsets_base_and_last_index_bounds() {
    indexed_triangle_lists(GraphicsContext::new());
}

#[test]
fn pass_backend_receives_ordered_viewports_and_fixed_extent() {
    use std::sync::{Arc, Mutex};
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let mut f = Frames::new(GraphicsContext::new());
    f.ctx.set_metal_pass_backend(move |draws, extent, clear| {
        assert_eq!(extent, (4, 3));
        assert_eq!(draws.len(), 2);
        assert_eq!(draws[0].viewport, (4, 3));
        assert_eq!(draws[1].viewport, (2, 2));
        assert_eq!(draws[0].vertices, COVERED);
        assert_eq!(draws[1].vertices, COVERED);
        captured.lock().unwrap().push(draws.len());
        Ok(clear.repeat(extent.0 * extent.1))
    });
    f.record(0);
    f.ctx.reset_command_list(f.list, f.allocator).unwrap();
    f.ctx.cmd_set_viewport(f.list, 4, 3).unwrap();
    f.ctx
        .cmd_set_vertex_buffer(f.list, f.buffer, 0, 16)
        .unwrap();
    f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
    f.ctx.cmd_set_viewport(f.list, 2, 2).unwrap();
    f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
    f.ctx.close_command_list(f.list).unwrap();
    let out = f.submit().unwrap();
    assert_eq!((out.width, out.height, out.draw_count), (4, 3, 2));
    assert_eq!(*seen.lock().unwrap(), [2]);
}

fn viewport_pass(ctx: GraphicsContext) {
    let mut f = Frames::new(ctx);
    let device = f.ctx.create_device().unwrap();
    f.buffer = f
        .ctx
        .create_committed_resource(
            device,
            ResourceDesc {
                dimension: ResourceDimension::Buffer,
                width: 144,
                height: 1,
                depth_or_array_size: 1,
                mip_levels: 1,
                format_rgba8: false,
            },
            ResourceStates::COMMON,
            Some(VERTICES),
        )
        .unwrap();
    let vertices = [
        [-1.0, -1.0, 0.0, 1.0],
        [0.0, -1.0, 0.0, 1.0],
        [-1.0, 1.0, 0.0, 1.0],
        [0.0, -1.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 1.0],
        [-1.0, 1.0, 0.0, 1.0],
        COVERED[0],
        COVERED[1],
        COVERED[2],
    ];
    let bytes: Vec<_> = vertices
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    f.memory.write_bytes(VERTICES, &bytes).unwrap();
    f.ctx.cmd_set_viewport(f.list, 4, 4).unwrap();
    f.ctx
        .cmd_set_vertex_buffer(f.list, f.buffer, 0, 16)
        .unwrap();
    f.ctx.cmd_draw_instanced(f.list, 6, 1, 0, 0).unwrap();
    f.ctx.cmd_set_viewport(f.list, 4, 2).unwrap();
    f.ctx.cmd_draw_instanced(f.list, 3, 1, 6, 0).unwrap();
    f.ctx.close_command_list(f.list).unwrap();
    let out = f.submit().unwrap();
    assert_eq!((out.width, out.height, out.draw_count), (4, 4, 2));
    for (i, &pixel) in out.pixels.as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(
            pixel,
            if i % 4 < 2 || i / 4 < 2 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 0, 255]
            }
        );
    }
    f.assert_presented(1, &out);
    f.ctx.reset_command_list(f.list, f.allocator).unwrap();
    f.ctx.cmd_set_viewport(f.list, 4, 4).unwrap();
    f.ctx
        .cmd_set_vertex_buffer(f.list, f.buffer, 0, 16)
        .unwrap();
    f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
    f.ctx.cmd_set_viewport(f.list, 5, 4).unwrap();
    f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
    f.ctx.close_command_list(f.list).unwrap();
    assert_eq!(f.submit(), Err(GraphicsError::InvalidViewport));
    f.assert_presented(1, &out);
}

#[test]
fn malformed_backend_readback_preserves_previous_frame() {
    let mut f = Frames::new(GraphicsContext::new());
    f.record(0);
    let previous = f.submit().unwrap();
    for bytes in [0, 47, 49] {
        f.ctx
            .set_metal_pass_backend(move |_, _, _| Ok(vec![0; bytes]));
        assert!(matches!(f.submit(), Err(GraphicsError::BackendError(_))));
        f.assert_presented(1, &previous);
    }
}

#[test]
fn undersized_target_is_rejected_before_rendering() {
    let mut f = Frames::new(GraphicsContext::new());
    f.record(0);
    let previous = f.submit().unwrap();
    f.ctx
        .set_metal_pass_backend(|_, _, _| panic!("undersized target reached renderer"));
    f.ctx.reset_command_list(f.list, f.allocator).unwrap();
    f.ctx.cmd_set_viewport(f.list, 4, 4).unwrap();
    f.ctx
        .cmd_set_render_targets(f.list, Some(f.buffer), [0; 4])
        .unwrap();
    f.ctx
        .cmd_set_vertex_buffer(f.list, f.buffer, 0, 16)
        .unwrap();
    f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
    f.ctx.close_command_list(f.list).unwrap();
    assert_eq!(f.submit(), Err(GraphicsError::IndexOutOfRange));
    f.assert_presented(1, &previous);
}

#[test]
fn loader_preserves_draw_state_and_rejects_legacy_state_loss() {
    use xenora_loader::graphics::{GraphicsBlendMode, GraphicsDepthCompare, GraphicsDrawState};
    let state = GraphicsDrawState {
        depth_compare: Some(GraphicsDepthCompare::Less),
        depth_write: true,
        blend: GraphicsBlendMode::Additive,
        color_write_mask: 5,
        ..GraphicsDrawState::default()
    };
    let mut f = Frames::new(GraphicsContext::new());
    f.record(0);
    f.ctx.reset_command_list(f.list, f.allocator).unwrap();
    f.ctx
        .cmd_set_vertex_buffer(f.list, f.buffer, 0, 16)
        .unwrap();
    f.ctx.cmd_set_draw_state(f.list, state).unwrap();
    f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
    f.ctx
        .cmd_set_draw_state(f.list, GraphicsDrawState::default())
        .unwrap();
    f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
    f.ctx.close_command_list(f.list).unwrap();
    assert_eq!(f.submit(), Err(GraphicsError::InvalidDescriptor));
    f.ctx.set_metal_pass_backend(move |draws, extent, clear| {
        assert_eq!(draws[0].state, state);
        assert_eq!(draws[1].state, GraphicsDrawState::default());
        Ok(clear.repeat(extent.0 * extent.1))
    });
    assert_eq!(f.submit().unwrap().draw_count, 2);
}

fn depth_blend_pass(ctx: GraphicsContext) {
    use xenora_loader::graphics::{GraphicsBlendMode, GraphicsDepthCompare, GraphicsDrawState};
    let mut f = Frames::new(ctx);
    let device = f.ctx.create_device().unwrap();
    f.buffer = f
        .ctx
        .create_committed_resource(
            device,
            ResourceDesc {
                dimension: ResourceDimension::Buffer,
                width: 96,
                height: 1,
                depth_or_array_size: 1,
                mip_levels: 1,
                format_rgba8: false,
            },
            ResourceStates::COMMON,
            Some(VERTICES),
        )
        .unwrap();
    let vertices: Vec<_> = [0.25, 0.75]
        .into_iter()
        .flat_map(|z| COVERED.map(|[x, y, _, w]| [x, y, z, w]))
        .collect();
    f.memory
        .write_bytes(
            VERTICES,
            &vertices
                .iter()
                .flatten()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        )
        .unwrap();
    for write in [false, true] {
        f.ctx.reset_command_list(f.list, f.allocator).unwrap();
        f.ctx
            .cmd_set_render_targets(f.list, None, [0, 0, 255, 255])
            .unwrap();
        f.ctx
            .cmd_set_vertex_buffer(f.list, f.buffer, 0, 16)
            .unwrap();
        f.ctx
            .cmd_set_draw_state(
                f.list,
                GraphicsDrawState {
                    depth_compare: Some(GraphicsDepthCompare::Less),
                    depth_write: write,
                    color_write_mask: 0,
                    ..GraphicsDrawState::default()
                },
            )
            .unwrap();
        f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
        f.ctx
            .cmd_set_draw_state(
                f.list,
                GraphicsDrawState {
                    depth_compare: Some(GraphicsDepthCompare::Less),
                    ..GraphicsDrawState::default()
                },
            )
            .unwrap();
        f.ctx.cmd_draw_instanced(f.list, 3, 1, 3, 0).unwrap();
        f.ctx.close_command_list(f.list).unwrap();
        assert_eq!(
            f.submit().unwrap().pixels,
            if write {
                [0, 0, 255, 255]
            } else {
                [255, 0, 0, 255]
            }
            .repeat(12)
        );
    }
    f.ctx.reset_command_list(f.list, f.allocator).unwrap();
    f.ctx
        .cmd_set_render_targets(f.list, None, [0, 0, 255, 255])
        .unwrap();
    f.ctx
        .cmd_set_vertex_buffer(f.list, f.buffer, 0, 16)
        .unwrap();
    f.ctx
        .cmd_set_draw_state(
            f.list,
            GraphicsDrawState {
                blend: GraphicsBlendMode::Additive,
                ..GraphicsDrawState::default()
            },
        )
        .unwrap();
    f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
    f.ctx.close_command_list(f.list).unwrap();
    assert_eq!(f.submit().unwrap().pixels, [255, 0, 255, 255].repeat(12));
    let expected = f.record(0);
    assert_eq!(
        f.submit().unwrap().pixels,
        expected,
        "reset clears depth/blend state"
    );
}

#[test]
fn malformed_draw_state_is_rejected_at_recording() {
    use xenora_loader::graphics::GraphicsDrawState;
    let mut f = Frames::new(GraphicsContext::new());
    for state in [
        GraphicsDrawState {
            color_write_mask: 16,
            ..GraphicsDrawState::default()
        },
        GraphicsDrawState {
            depth_write: true,
            ..GraphicsDrawState::default()
        },
    ] {
        assert_eq!(
            f.ctx.cmd_set_draw_state(f.list, state),
            Err(GraphicsError::InvalidDescriptor)
        );
    }
    f.ctx.close_command_list(f.list).unwrap();
    assert_eq!(
        f.ctx
            .cmd_set_draw_state(f.list, GraphicsDrawState::default()),
        Err(GraphicsError::CommandListNotRecording)
    );
}

#[test]
fn invalid_draw_regions_do_not_reach_backend_or_replace_frame() {
    use xenora_loader::graphics::GraphicsDrawState;
    let mut f = Frames::new(GraphicsContext::new());
    f.record(0);
    let previous = f.submit().unwrap();
    f.ctx
        .set_metal_pass_backend(|_, _, _| panic!("invalid draw region reached renderer"));
    for state in [
        GraphicsDrawState {
            viewport_origin: [1, 0],
            ..GraphicsDrawState::default()
        },
        GraphicsDrawState {
            viewport_origin: [usize::MAX, 0],
            ..GraphicsDrawState::default()
        },
        GraphicsDrawState {
            scissor: Some([3, 0, 2, 1]),
            ..GraphicsDrawState::default()
        },
        GraphicsDrawState {
            scissor: Some([0, usize::MAX, 1, 2]),
            ..GraphicsDrawState::default()
        },
    ] {
        f.ctx.reset_command_list(f.list, f.allocator).unwrap();
        f.ctx
            .cmd_set_vertex_buffer(f.list, f.buffer, 0, 16)
            .unwrap();
        f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
        f.ctx.cmd_set_draw_state(f.list, state).unwrap();
        f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
        f.ctx.close_command_list(f.list).unwrap();
        assert_eq!(f.submit(), Err(GraphicsError::InvalidViewport));
        f.assert_presented(1, &previous);
    }
}

fn resource_transfer_render(ctx: GraphicsContext) {
    let mut f = Frames::new(ctx);
    let device = f.ctx.create_device().unwrap();
    let desc = ResourceDesc {
        dimension: ResourceDimension::Buffer,
        width: 48,
        height: 1,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: false,
    };
    let buffer = f
        .ctx
        .create_committed_resource(device, desc.clone(), ResourceStates::COPY_DEST, None)
        .unwrap();
    let target = f
        .ctx
        .create_committed_resource(
            device,
            ResourceDesc {
                dimension: ResourceDimension::Texture2D,
                width: 4,
                height: 3,
                format_rgba8: true,
                ..desc
            },
            ResourceStates::RENDER_TARGET,
            None,
        )
        .unwrap();
    let bytes: Vec<u8> = COVERED
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    f.memory.write_bytes(VERTICES, &bytes).unwrap();
    f.ctx
        .copy_guest_to_resource(buffer, 0, VERTICES, 48, &f.memory)
        .unwrap();
    // Overwrite the upload source: rendering must now use the independent host copy.
    f.memory.write_bytes(VERTICES, &[0; 48]).unwrap();
    f.ctx
        .cmd_resource_barrier(
            f.list,
            buffer,
            ResourceStates::COPY_DEST,
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
        )
        .unwrap();
    f.ctx.cmd_set_viewport(f.list, 4, 3).unwrap();
    f.ctx
        .cmd_set_render_targets(f.list, Some(target), [0, 0, 0, 255])
        .unwrap();
    f.ctx.cmd_set_vertex_buffer(f.list, buffer, 0, 16).unwrap();
    f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
    f.ctx
        .cmd_resource_barrier(
            f.list,
            target,
            ResourceStates::RENDER_TARGET,
            ResourceStates::COPY_SOURCE,
        )
        .unwrap();
    f.ctx.close_command_list(f.list).unwrap();
    f.submit().unwrap();
    f.memory.write_bytes(VERTICES + 128, &[77; 50]).unwrap();
    f.ctx
        .copy_resource_to_guest(target, 0, VERTICES + 129, 48, &mut f.memory)
        .unwrap();
    let mut result = [0; 50];
    f.memory.read_bytes(VERTICES + 128, &mut result).unwrap();
    assert_eq!(result[0], 77);
    assert_eq!(result[49], 77);
    assert_eq!(result[1..49], [255, 0, 0, 255].repeat(12));
    let presentation = f.ctx.last_presentation.clone();
    for resource in [buffer, target] {
        assert_eq!(
            f.ctx.destroy_resource(resource),
            Err(GraphicsError::ResourceLifetimeViolation)
        );
    }
    f.ctx.reset_command_list(f.list, f.allocator).unwrap();
    f.ctx.destroy_resource(buffer).unwrap();
    f.ctx.destroy_resource(target).unwrap();
    assert_eq!(
        presentation.lock().unwrap().pixels,
        [255, 0, 0, 255].repeat(12)
    );
    assert_eq!(
        f.ctx
            .copy_resource_to_guest(target, 0, VERTICES, 48, &mut f.memory),
        Err(GraphicsError::InvalidResourceHandle)
    );
}

#[test]
fn host_resource_upload_render_readback() {
    resource_transfer_render(GraphicsContext::new());
}

#[test]
fn failed_submission_keeps_resource_references_until_reset() {
    let mut f = Frames::new(GraphicsContext::new());
    f.record(0);
    f.ctx
        .set_metal_backend(|_, _, _| Err(GraphicsError::BackendError("injected".into())));
    assert!(matches!(f.submit(), Err(GraphicsError::BackendError(_))));
    assert_eq!(
        f.ctx.destroy_resource(f.buffer),
        Err(GraphicsError::ResourceLifetimeViolation)
    );
    f.ctx.reset_command_list(f.list, f.allocator).unwrap();
    f.ctx.destroy_resource(f.buffer).unwrap();
    // Destroying a guest-backed resource does not unmap or alter caller-owned memory.
    let mut bytes = [0; 48];
    f.memory.read_bytes(VERTICES, &mut bytes).unwrap();
    let expected: Vec<u8> = COVERED
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    assert_eq!(bytes.as_slice(), expected);
    assert_eq!(f.ctx.last_presentation.lock().unwrap().frame_index, 0);
}

#[test]
fn depth_clear_is_per_pass_validated_and_reset() {
    use std::sync::{Arc, Mutex};
    let mut f = Frames::new(GraphicsContext::new());
    f.record(0);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let capture = seen.clone();
    f.ctx
        .set_metal_pass_with_depth_backend(move |_, extent, clear, depth| {
            capture.lock().unwrap().push(depth);
            Ok(clear.repeat(extent.0 * extent.1))
        });
    for (before, after, succeeds) in [
        (Some(0.0), None, true),
        (Some(0.5), Some(0.0), false),
        (None, None, true),
    ] {
        f.ctx.reset_command_list(f.list, f.allocator).unwrap();
        for invalid in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
            assert_eq!(
                f.ctx.cmd_set_depth_clear(f.list, invalid),
                Err(GraphicsError::InvalidDescriptor)
            );
        }
        if let Some(depth) = before {
            f.ctx.cmd_set_depth_clear(f.list, depth).unwrap();
        }
        f.ctx
            .cmd_set_vertex_buffer(f.list, f.buffer, 0, 16)
            .unwrap();
        f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
        if let Some(depth) = after {
            f.ctx.cmd_set_depth_clear(f.list, depth).unwrap();
        }
        f.ctx.close_command_list(f.list).unwrap();
        if succeeds {
            f.submit().unwrap();
        } else {
            assert_eq!(f.submit(), Err(GraphicsError::InvalidDescriptor));
            assert_eq!(f.ctx.last_presentation.lock().unwrap().frame_index, 1);
        }
    }
    assert_eq!(*seen.lock().unwrap(), [0.0, 1.0]);
    f.ctx
        .set_metal_pass_backend(|_, _, _| panic!("legacy adapter must reject custom depth"));
    f.ctx.reset_command_list(f.list, f.allocator).unwrap();
    f.ctx.cmd_set_depth_clear(f.list, 0.0).unwrap();
    f.ctx
        .cmd_set_vertex_buffer(f.list, f.buffer, 0, 16)
        .unwrap();
    f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
    f.ctx.close_command_list(f.list).unwrap();
    assert_eq!(f.submit(), Err(GraphicsError::InvalidDescriptor));
}

fn bound_texture_defines_target_extent(ctx: GraphicsContext) {
    let mut f = Frames::new(ctx);
    f.record(0);
    let device = f.ctx.create_device().unwrap();
    let target = f
        .ctx
        .create_committed_resource(
            device,
            ResourceDesc {
                dimension: ResourceDimension::Texture2D,
                width: 8,
                height: 4,
                depth_or_array_size: 1,
                mip_levels: 1,
                format_rgba8: true,
            },
            ResourceStates::RENDER_TARGET,
            None,
        )
        .unwrap();
    f.ctx.reset_command_list(f.list, f.allocator).unwrap();
    f.ctx
        .cmd_set_render_targets(f.list, Some(target), [0, 0, 255, 255])
        .unwrap();
    f.ctx.cmd_set_viewport(f.list, 2, 2).unwrap();
    f.ctx
        .cmd_set_vertex_buffer(f.list, f.buffer, 0, 16)
        .unwrap();
    f.ctx.cmd_draw_instanced(f.list, 3, 1, 0, 0).unwrap();
    f.ctx
        .cmd_resource_barrier(
            f.list,
            target,
            ResourceStates::RENDER_TARGET,
            ResourceStates::COPY_SOURCE,
        )
        .unwrap();
    f.ctx.close_command_list(f.list).unwrap();
    let output = f.submit().unwrap();
    assert_eq!((output.width, output.height), (8, 4));
    for y in 0..4 {
        for x in 0..8 {
            let expected = if x < 2 && y < 2 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
            assert_eq!(
                &output.pixels[(y * 8 + x) * 4..(y * 8 + x + 1) * 4],
                &expected
            );
        }
    }
    f.ctx
        .copy_resource_to_guest(target, 0, VERTICES + 128, 128, &mut f.memory)
        .unwrap();
    let mut readback = [0; 128];
    f.memory.read_bytes(VERTICES + 128, &mut readback).unwrap();
    assert_eq!(readback.as_slice(), output.pixels);
}

#[test]
fn texture_extent_reaches_pass_adapter_and_clear_only_readback() {
    let mut f = Frames::new(GraphicsContext::new());
    let device = f.ctx.create_device().unwrap();
    let target = f
        .ctx
        .create_committed_resource(
            device,
            ResourceDesc {
                dimension: ResourceDimension::Texture2D,
                width: 8,
                height: 4,
                depth_or_array_size: 1,
                mip_levels: 1,
                format_rgba8: true,
            },
            ResourceStates::RENDER_TARGET,
            None,
        )
        .unwrap();
    f.ctx
        .cmd_set_render_targets(f.list, Some(target), [1, 2, 3, 4])
        .unwrap();
    f.ctx.cmd_set_viewport(f.list, 2, 2).unwrap();
    f.ctx.close_command_list(f.list).unwrap();
    let output = f.submit().unwrap();
    assert_eq!((output.width, output.height), (8, 4));
    assert_eq!(output.pixels, [1, 2, 3, 4].repeat(32));
}

#[test]
#[cfg_attr(
    not(feature = "gpu-tests"),
    ignore = "needs native Metal; run scripts/test-gpu.sh"
)]
fn native_multiple_targets_render_transfer_and_reset() {
    use xenora_metal::{NativeCompute, TriangleDraw, TriangleState, TriangleTargetsPass};
    use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};
    let shader = compile_graphics(
        Architecture::Gfx1030,
        include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin"),
        include_bytes!("../../../fixtures/rdna2/x9-mrt-ps.bin"),
    )
    .unwrap();
    let gpu = NativeCompute::new().expect("native Metal required");
    let mut ctx = GraphicsContext::new();
    ctx.set_metal_targets_backend(move |draws, extent, clears, depth| {
        assert_eq!(depth, 1.0);
        let colors: Vec<_> = draws
            .iter()
            .map(|d| vec![[0.0; 4]; d.vertices.len()])
            .collect();
        let native: Vec<_> = draws
            .iter()
            .zip(&colors)
            .map(|(draw, colors)| {
                assert_eq!(draw.state, Default::default());
                TriangleDraw {
                    vertices: &draw.vertices,
                    colors,
                    viewport: [draw.viewport.0, draw.viewport.1],
                }
            })
            .collect();
        gpu.render_triangle_targets_rgba8(
            &shader.msl,
            [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
            &TriangleTargetsPass {
                draws: &native,
                states: &vec![TriangleState::default(); native.len()],
                extent: [extent.0, extent.1],
                clears,
                clear_depth: None,
            },
        )
        .map_err(|e| GraphicsError::BackendError(e.to_string()))
    });
    let mut frames = Frames::new(ctx);
    let device = frames.ctx.create_device().unwrap();
    let targets: Vec<_> = (0..8)
        .map(|_| {
            frames
                .ctx
                .create_committed_resource(
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
        })
        .collect();
    let expected = [
        [0, 0, 0, 255],
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 0, 255],
        [255, 0, 255, 255],
        [0, 255, 255, 255],
        [255, 255, 255, 255],
    ];
    let clears: [[u8; 4]; 8] = std::array::from_fn(|slot| [17 + slot as u8, 31, 47, 255]);
    for frame in 0..8 {
        frames
            .ctx
            .reset_command_list(frames.list, frames.allocator)
            .unwrap();
        if frame > 0 {
            for target in &targets {
                frames
                    .ctx
                    .cmd_resource_barrier(
                        frames.list,
                        *target,
                        ResourceStates::COPY_SOURCE,
                        ResourceStates::RENDER_TARGET,
                    )
                    .unwrap();
            }
        }
        let vertices = if frame % 2 == 0 { COVERED } else { CLIPPED };
        let bytes: Vec<_> = vertices
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        frames.memory.write_bytes(VERTICES, &bytes).unwrap();
        frames
            .ctx
            .cmd_set_color_targets(frames.list, &targets, &clears)
            .unwrap();
        frames.ctx.cmd_set_viewport(frames.list, 4, 3).unwrap();
        frames
            .ctx
            .cmd_set_vertex_buffer(frames.list, frames.buffer, 0, 16)
            .unwrap();
        frames
            .ctx
            .cmd_draw_instanced(frames.list, 3, 1, 0, 0)
            .unwrap();
        for target in &targets {
            frames
                .ctx
                .cmd_resource_barrier(
                    frames.list,
                    *target,
                    ResourceStates::RENDER_TARGET,
                    ResourceStates::COPY_SOURCE,
                )
                .unwrap();
        }
        frames.ctx.close_command_list(frames.list).unwrap();
        let out = frames
            .ctx
            .execute_command_list(frames.queue, frames.list, &frames.memory)
            .unwrap();
        assert_eq!(out.backend, GraphicsBackendId::MetalNative);
        assert_eq!(out.color_outputs.len(), 8);
        for (slot, target) in targets.iter().enumerate() {
            let pixels = if frame % 2 == 0 {
                expected[slot]
            } else {
                clears[slot]
            }
            .repeat(12);
            assert_eq!(
                out.color_outputs[slot], pixels,
                "frame {frame}, slot {slot}"
            );
            let address = VERTICES + 128 + slot as u64 * 64;
            frames.memory.write_bytes(address, &[99; 64]).unwrap();
            frames
                .ctx
                .copy_resource_to_guest(*target, 0, address + 8, 48, &mut frames.memory)
                .unwrap();
            let mut copied = [0; 64];
            frames.memory.read_bytes(address, &mut copied).unwrap();
            assert_eq!(&copied[..8], &[99; 8]);
            assert_eq!(&copied[8..56], pixels.as_slice());
            assert_eq!(&copied[56..], &[99; 8]);
            assert_eq!(
                frames.ctx.destroy_resource(*target),
                Err(GraphicsError::ResourceLifetimeViolation)
            );
        }
        assert_eq!(out.pixels, out.color_outputs[0]);
        frames.assert_presented(frame + 1, &out);
    }
    frames
        .ctx
        .reset_command_list(frames.list, frames.allocator)
        .unwrap();
    for target in targets {
        frames.ctx.destroy_resource(target).unwrap();
    }
}

#[test]
#[cfg_attr(
    not(feature = "gpu-tests"),
    ignore = "needs native Metal; run scripts/test-gpu.sh"
)]
fn native_loader_depth_rejects_color_without_advancing_depth() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use xenora_loader::graphics::{
        GraphicsDepthCompare, GraphicsDepthLoad as Load, GraphicsDrawState, GraphicsPassOutput,
    };
    use xenora_metal::{
        DepthCompare, DepthLoad, DepthSnapshot, DepthState, NativeCompute, TriangleDraw,
        TriangleState, TriangleTargetsPass,
    };
    use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};
    let gpu = NativeCompute::new().unwrap();
    let shader = compile_graphics(
        Architecture::Gfx1030,
        include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin"),
        include_bytes!("../../../fixtures/rdna2/x9-constant-ps.bin"),
    )
    .unwrap();
    let fail = Arc::new(AtomicBool::new(false));
    let inject = fail.clone();
    let mut ctx = GraphicsContext::new();
    ctx.set_metal_stateful_backend(move |draws, extent, clears, _, binding| {
        let binding = binding.ok_or(GraphicsError::InvalidDescriptor)?;
        let mut depth = match binding.previous {
            Some(snapshot) => gpu
                .restore_depth(
                    snapshot
                        .downcast_ref::<DepthSnapshot>()
                        .ok_or(GraphicsError::InvalidDescriptor)?,
                )
                .map_err(|e| GraphicsError::BackendError(e.to_string()))?,
            None => gpu
                .create_depth_target([extent.0, extent.1])
                .map_err(|e| GraphicsError::BackendError(e.to_string()))?,
        };
        let colors: Vec<_> = draws
            .iter()
            .map(|d| vec![[0.0; 4]; d.vertices.len()])
            .collect();
        let native: Vec<_> = draws
            .iter()
            .zip(&colors)
            .map(|(d, colors)| TriangleDraw {
                vertices: &d.vertices,
                colors,
                viewport: [d.viewport.0, d.viewport.1],
            })
            .collect();
        let states: Vec<_> = draws
            .iter()
            .map(|d| {
                assert_eq!(d.state.depth_compare, Some(GraphicsDepthCompare::Less));
                TriangleState {
                    depth: DepthState {
                        compare: DepthCompare::Less,
                        write: d.state.depth_write,
                    },
                    ..Default::default()
                }
            })
            .collect();
        let mut colors = gpu
            .render_triangle_targets_with_depth_rgba8(
                &shader.msl,
                [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
                &TriangleTargetsPass {
                    draws: &native,
                    states: &states,
                    extent: [extent.0, extent.1],
                    clears,
                    clear_depth: None,
                },
                &mut depth,
                match binding.load {
                    Load::Clear(v) => DepthLoad::Clear(v),
                    Load::Load => DepthLoad::Load,
                },
            )
            .map_err(|e| GraphicsError::BackendError(e.to_string()))?;
        if inject.load(Ordering::SeqCst) {
            colors[0].pop();
        }
        Ok(GraphicsPassOutput {
            color_layouts: None,
            colors,
            depth: Some(Arc::new(
                gpu.snapshot_depth(&depth)
                    .map_err(|e| GraphicsError::BackendError(e.to_string()))?,
            )),
        })
    });
    let mut frames = Frames::new(ctx);
    let device = frames.ctx.create_device().unwrap();
    let depth = frames.ctx.create_depth_resource(device, (4, 3)).unwrap();
    let clear = [17, 31, 47, 255];
    let mut presented = 0;
    for (index, (z, load, malformed, expected)) in [
        (0.25, Load::Clear(1.0), false, [255, 0, 0, 255]),
        (0.75, Load::Clear(1.0), true, [0; 4]),
        (0.5, Load::Load, false, clear),
        (0.125, Load::Load, false, [255, 0, 0, 255]),
    ]
    .into_iter()
    .enumerate()
    {
        frames
            .ctx
            .reset_command_list(frames.list, frames.allocator)
            .unwrap();
        let positions = COVERED.map(|[x, y, _, w]| [x, y, z, w]);
        let bytes: Vec<_> = positions
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        frames.memory.write_bytes(VERTICES, &bytes).unwrap();
        frames
            .ctx
            .cmd_set_render_targets(frames.list, None, clear)
            .unwrap();
        frames
            .ctx
            .cmd_set_depth_target(frames.list, Some(depth), load)
            .unwrap();
        frames
            .ctx
            .cmd_set_draw_state(
                frames.list,
                GraphicsDrawState {
                    depth_compare: Some(GraphicsDepthCompare::Less),
                    depth_write: true,
                    ..Default::default()
                },
            )
            .unwrap();
        frames
            .ctx
            .cmd_set_vertex_buffer(frames.list, frames.buffer, 0, 16)
            .unwrap();
        frames
            .ctx
            .cmd_draw_instanced(frames.list, 3, 1, 0, 0)
            .unwrap();
        frames.ctx.close_command_list(frames.list).unwrap();
        fail.store(malformed, Ordering::SeqCst);
        let outcome = frames
            .ctx
            .execute_command_list(frames.queue, frames.list, &frames.memory);
        if malformed {
            assert!(matches!(outcome, Err(GraphicsError::BackendError(_))));
            assert_eq!(
                frames.ctx.last_presentation.lock().unwrap().frame_index,
                presented
            );
        } else {
            let out = outcome.unwrap();
            assert_eq!(out.pixels, expected.repeat(12), "submission {index}");
            presented += 1;
            frames.assert_presented(presented, &out);
        }
    }
    // DEPTH_READ permits tests without writes; returning to WRITE must retain
    // the old .125 rather than the nearer .0625 read-only fragment.
    for (before, after, z, write) in [
        (
            ResourceStates::DEPTH_WRITE,
            ResourceStates::DEPTH_READ,
            0.0625,
            false,
        ),
        (
            ResourceStates::DEPTH_READ,
            ResourceStates::DEPTH_WRITE,
            0.1,
            true,
        ),
    ] {
        frames
            .ctx
            .reset_command_list(frames.list, frames.allocator)
            .unwrap();
        frames
            .ctx
            .cmd_resource_barrier(frames.list, depth, before, after)
            .unwrap();
        frames
            .ctx
            .cmd_set_depth_target(frames.list, Some(depth), Load::Load)
            .unwrap();
        frames
            .ctx
            .cmd_set_draw_state(
                frames.list,
                GraphicsDrawState {
                    depth_compare: Some(GraphicsDepthCompare::Less),
                    depth_write: write,
                    ..Default::default()
                },
            )
            .unwrap();
        let positions = COVERED.map(|[x, y, _, w]| [x, y, z, w]);
        let bytes: Vec<_> = positions
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        frames.memory.write_bytes(VERTICES, &bytes).unwrap();
        frames
            .ctx
            .cmd_set_vertex_buffer(frames.list, frames.buffer, 0, 16)
            .unwrap();
        frames
            .ctx
            .cmd_draw_instanced(frames.list, 3, 1, 0, 0)
            .unwrap();
        frames.ctx.close_command_list(frames.list).unwrap();
        let out = frames
            .ctx
            .execute_command_list(frames.queue, frames.list, &frames.memory)
            .unwrap();
        assert_eq!(out.pixels, [255, 0, 0, 255].repeat(12));
    }
    // Clear-only native depth initializes attachment contents with no geometry.
    frames
        .ctx
        .reset_command_list(frames.list, frames.allocator)
        .unwrap();
    frames
        .ctx
        .cmd_set_depth_target(frames.list, Some(depth), Load::Clear(0.0))
        .unwrap();
    frames.ctx.close_command_list(frames.list).unwrap();
    let clear_only = frames
        .ctx
        .execute_command_list(frames.queue, frames.list, &frames.memory)
        .unwrap();
    assert_eq!(clear_only.draw_count, 0);
    assert_eq!(clear_only.pixels, [0, 0, 0, 255].repeat(12));
    for clear_depth in [0.0, 1.0] {
        if clear_depth == 1.0 {
            frames
                .ctx
                .reset_command_list(frames.list, frames.allocator)
                .unwrap();
            frames
                .ctx
                .cmd_set_depth_target(frames.list, Some(depth), Load::Clear(1.0))
                .unwrap();
            frames.ctx.close_command_list(frames.list).unwrap();
            frames
                .ctx
                .execute_command_list(frames.queue, frames.list, &frames.memory)
                .unwrap();
        }
        frames
            .ctx
            .reset_command_list(frames.list, frames.allocator)
            .unwrap();
        frames
            .ctx
            .cmd_set_depth_target(frames.list, Some(depth), Load::Load)
            .unwrap();
        frames
            .ctx
            .cmd_set_draw_state(
                frames.list,
                GraphicsDrawState {
                    depth_compare: Some(GraphicsDepthCompare::Less),
                    depth_write: false,
                    ..Default::default()
                },
            )
            .unwrap();
        let positions = COVERED.map(|[x, y, _, w]| [x, y, 0.0625, w]);
        let bytes: Vec<_> = positions
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        frames.memory.write_bytes(VERTICES, &bytes).unwrap();
        frames
            .ctx
            .cmd_set_vertex_buffer(frames.list, frames.buffer, 0, 16)
            .unwrap();
        frames
            .ctx
            .cmd_draw_instanced(frames.list, 3, 1, 0, 0)
            .unwrap();
        frames.ctx.close_command_list(frames.list).unwrap();
        let out = frames
            .ctx
            .execute_command_list(frames.queue, frames.list, &frames.memory)
            .unwrap();
        assert_eq!(
            out.pixels,
            if clear_depth == 0.0 {
                [0, 0, 0, 255]
            } else {
                [255, 0, 0, 255]
            }
            .repeat(12)
        );
    }

    assert_eq!(
        frames.ctx.destroy_resource(depth),
        Err(GraphicsError::ResourceLifetimeViolation)
    );
    frames
        .ctx
        .reset_command_list(frames.list, frames.allocator)
        .unwrap();
    frames.ctx.destroy_resource(depth).unwrap();
}
