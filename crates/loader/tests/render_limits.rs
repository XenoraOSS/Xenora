// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU-only scene-limit tests. Native-labelled callbacks are fakes and perform no GPU work.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use xenora_loader::graphics::*;
use xenora_loader::{GuestMemory, MemoryPermissions};

struct Ready(Option<GraphicsPassOutput>);
impl GraphicsPendingSubmission for Ready {
    fn poll(&mut self) -> Option<Result<GraphicsPassOutput, GraphicsError>> {
        self.0.take().map(Ok)
    }
    fn wait(&mut self) -> Result<GraphicsPassOutput, GraphicsError> {
        Ok(self.0.take().unwrap())
    }
}

fn desc(size: u64) -> ResourceDesc {
    ResourceDesc {
        dimension: ResourceDimension::Buffer,
        width: size,
        height: 1,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: false,
    }
}

struct Harness {
    ctx: GraphicsContext,
    memory: GuestMemory,
    device: GraphicsHandle,
    queue: GraphicsHandle,
    allocator: GraphicsHandle,
    list: GraphicsHandle,
    vertices: GraphicsHandle,
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
        let mut memory = GuestMemory::new();
        let rows: Vec<f32> = (0..65535)
            .flat_map(|i| [i as f32, 0.25, 0.5, 1.0])
            .collect();
        let bytes: Vec<u8> = rows.iter().flat_map(|v| v.to_le_bytes()).collect();
        memory
            .map_region(0x1000, bytes.len(), MemoryPermissions::READ_WRITE)
            .unwrap();
        memory.write_bytes(0x1000, &bytes).unwrap();
        let vertices = ctx
            .create_committed_resource(
                device,
                desc(bytes.len() as u64),
                ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
                Some(0x1000),
            )
            .unwrap();
        ctx.cmd_set_viewport(list, 1, 1).unwrap();
        ctx.cmd_set_vertex_buffer(list, vertices, 0, 16).unwrap();
        Self {
            ctx,
            memory,
            device,
            queue,
            allocator,
            list,
            vertices,
        }
    }

    fn indices(&mut self, format: u32) {
        let bytes: Vec<u8> = if format == 57 {
            (0..65535u16).flat_map(u16::to_le_bytes).collect()
        } else {
            (0..65535u32).flat_map(u32::to_le_bytes).collect()
        };
        self.memory
            .map_region(0x200000, bytes.len(), MemoryPermissions::READ_WRITE)
            .unwrap();
        self.memory.write_bytes(0x200000, &bytes).unwrap();
        let resource = self
            .ctx
            .create_committed_resource(
                self.device,
                desc(bytes.len() as u64),
                ResourceStates::INDEX_BUFFER,
                Some(0x200000),
            )
            .unwrap();
        self.ctx
            .cmd_set_index_buffer(self.list, resource, 0, format)
            .unwrap();
    }

    fn draw(&mut self, indexed: bool, count: u32, start: u32) {
        if indexed {
            self.ctx
                .cmd_draw_indexed_instanced(self.list, count, 1, start, 0, 0)
                .unwrap();
        } else {
            self.ctx
                .cmd_draw_instanced(self.list, count, 1, start, 0)
                .unwrap();
        }
    }

    fn execute(&mut self) -> Result<ExecutionOutcome, GraphicsError> {
        self.ctx.close_command_list(self.list).unwrap();
        self.ctx
            .execute_command_list(self.queue, self.list, &self.memory)
    }

    fn target(&mut self, width: u64, height: u32) -> GraphicsHandle {
        self.ctx
            .create_committed_resource(
                self.device,
                ResourceDesc {
                    dimension: ResourceDimension::Texture2D,
                    width,
                    height,
                    depth_or_array_size: 1,
                    mip_levels: 1,
                    format_rgba8: true,
                },
                ResourceStates::RENDER_TARGET,
                None,
            )
            .unwrap()
    }
}

fn snapshot_output(
    draws: &[GraphicsDraw],
    extent: (usize, usize),
    expected: usize,
) -> GraphicsPassOutput {
    let count: usize = draws.iter().map(|draw| draw.vertices.len()).sum();
    assert_eq!(count, expected);
    assert_eq!(draws[0].vertices[0], [0.0, 0.25, 0.5, 1.0]);
    assert_eq!(
        *draws.last().unwrap().vertices.last().unwrap(),
        [65534.0, 0.25, 0.5, 1.0]
    );
    GraphicsPassOutput {
        color_layouts: None,
        colors: vec![[7, 11, 13, 255].repeat(extent.0 * extent.1)],
        depth: None,
    }
}

#[test]
fn native_sync_and_async_snapshot_exact_vertex_limit_for_both_index_widths() {
    assert_eq!(MAX_NATIVE_RENDER_VERTICES, 65535);
    assert_eq!(MAX_NATIVE_RENDER_PIXELS, 1048576);
    assert_eq!(MAX_RENDER_DIMENSION, 4096);
    for asynchronous in [false, true] {
        for format in [0, 57, 42] {
            let mut h = Harness::new();
            if format != 0 {
                h.indices(format);
            }
            if asynchronous {
                h.ctx.set_metal_async_backend(|request| {
                    Ok(Box::new(Ready(Some(snapshot_output(
                        &request.draws,
                        request.extent,
                        65535,
                    )))))
                });
            } else {
                h.ctx
                    .set_metal_attachment_backend(|draws, extent, _, _, _| {
                        Ok(snapshot_output(draws, extent, 65535))
                    });
            }
            // Exact aggregate bound, with snapshots spanning two draws.
            h.draw(format != 0, 32766, 0);
            h.draw(format != 0, 32769, 32766);
            assert_eq!(h.execute().unwrap().pixels, [7, 11, 13, 255]);
        }
    }
}

#[test]
fn native_overlimit_counts_and_aggregate_never_reach_backend() {
    for asynchronous in [false, true] {
        for indexed in [false, true] {
            for counts in [&[65538u32][..], &[u32::MAX][..], &[32769, 32769][..]] {
                let mut h = Harness::new();
                if indexed {
                    h.indices(42);
                }
                let calls = Arc::new(AtomicUsize::new(0));
                let callback_calls = calls.clone();
                if asynchronous {
                    h.ctx.set_metal_async_backend(move |_| {
                        callback_calls.fetch_add(1, Ordering::SeqCst);
                        Err(GraphicsError::BackendError(
                            "unexpected CPU dispatch".into(),
                        ))
                    });
                } else {
                    h.ctx.set_metal_attachment_backend(move |_, _, _, _, _| {
                        callback_calls.fetch_add(1, Ordering::SeqCst);
                        Err(GraphicsError::BackendError(
                            "unexpected CPU dispatch".into(),
                        ))
                    });
                }
                for &count in counts {
                    h.draw(indexed, count, 0);
                }
                assert_eq!(h.execute(), Err(GraphicsError::InvalidDescriptor));
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
            }
        }
    }
}

#[test]
fn vertex_address_and_viewport_origin_overflow_reject_before_dispatch() {
    for indexed in [false, true] {
        let mut h = Harness::new();
        if indexed {
            h.indices(57);
        }
        h.ctx
            .set_metal_attachment_backend(|_, _, _, _, _| panic!("invalid address dispatched"));
        h.ctx
            .cmd_set_vertex_buffer(h.list, h.vertices, usize::MAX - 16, 16)
            .unwrap();
        h.draw(indexed, 3, 0);
        assert_eq!(h.execute(), Err(GraphicsError::IndexOutOfRange));
    }
    let mut h = Harness::new();
    h.ctx
        .set_metal_attachment_backend(|_, _, _, _, _| panic!("invalid viewport dispatched"));
    let state = GraphicsDrawState {
        viewport_origin: [usize::MAX, 0],
        ..GraphicsDrawState::default()
    };
    h.ctx.cmd_set_draw_state(h.list, state).unwrap();
    h.draw(false, 3, 0);
    assert_eq!(h.execute(), Err(GraphicsError::InvalidViewport));
}

#[test]
fn software_keeps_768_vertex_and_4096_pixel_limits() {
    for indexed in [false, true] {
        let mut h = Harness::new();
        if indexed {
            h.indices(42);
        }
        h.draw(indexed, 771, 0);
        assert_eq!(h.execute(), Err(GraphicsError::InvalidDescriptor));
    }
    let mut h = Harness::new();
    h.ctx.cmd_set_viewport(h.list, 64, 64).unwrap();
    assert_eq!(h.execute().unwrap().pixels.len(), 16384);
    h.ctx.reset_command_list(h.list, h.allocator).unwrap();
    h.ctx.cmd_set_viewport(h.list, 65, 64).unwrap();
    assert_eq!(h.execute(), Err(GraphicsError::InvalidViewport));
    assert!(
        GraphicsContext::execute_software_reference_triangles(
            &[[0., 0., 0., 1.]; 3],
            (usize::MAX, 2),
            [0; 4]
        )
        .is_err()
    );
}

#[test]
fn target_pixel_and_axis_limits_apply_before_native_dispatch() {
    for (width, height, accepted) in [
        (1024, 1024, true),
        (4096, 256, true),
        (1025, 1024, false),
        (4097, 1, false),
    ] {
        let mut h = Harness::new();
        let target = h.target(width, height);
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        h.ctx
            .set_metal_attachment_backend(move |_, extent, _, _, _| {
                callback_calls.fetch_add(1, Ordering::SeqCst);
                Ok(GraphicsPassOutput {
                    color_layouts: None,
                    colors: vec![[2, 3, 5, 255].repeat(extent.0 * extent.1)],
                    depth: None,
                })
            });
        h.ctx
            .cmd_set_color_targets(h.list, &[target], &[[0; 4]])
            .unwrap();
        h.draw(false, 3, 0);
        let result = h.execute();
        if accepted {
            let result = result.unwrap();
            assert_eq!(result.pixels.len(), 4194304);
            assert_eq!(&result.pixels[..4], &[2, 3, 5, 255]);
            assert_eq!(&result.pixels[result.pixels.len() - 4..], &[2, 3, 5, 255]);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        } else {
            assert_eq!(result, Err(GraphicsError::InvalidViewport));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }
}

#[test]
fn depth_accepts_one_megapixel_and_rejects_overlimit_axes_or_products() {
    let mut ctx = GraphicsContext::new();
    let device = ctx.create_device().unwrap();
    for extent in [(1024, 1024), (4096, 256), (256, 4096)] {
        let depth = ctx.create_depth_resource(device, extent).unwrap();
        ctx.destroy_resource(depth).unwrap();
    }
    for extent in [
        (1025, 1024),
        (4097, 1),
        (1, 4097),
        (0, 1024),
        (usize::MAX, 2),
    ] {
        assert_eq!(
            ctx.create_depth_resource(device, extent),
            Err(GraphicsError::InvalidDescriptor)
        );
    }
    // Depth storage is budgeted by dimensions even before a backend snapshot exists.
    let mut depths = Vec::new();
    for _ in 0..64 {
        depths.push(ctx.create_depth_resource(device, (1024, 1024)).unwrap());
    }
    assert_eq!(
        ctx.create_depth_resource(device, (1, 1)),
        Err(GraphicsError::Memory(
            xenora_loader::memory::MemoryError::AllocationFailed
        ))
    );
    ctx.destroy_resource(depths.pop().unwrap()).unwrap();
    ctx.create_depth_resource(device, (1024, 1024)).unwrap();
}

#[test]
fn exact_single_draw_native_and_software_vertex_boundaries() {
    for indexed in [false, true] {
        let mut h = Harness::new();
        if indexed {
            h.indices(42);
        }
        h.ctx
            .set_metal_attachment_backend(|draws, extent, _, _, _| {
                assert_eq!(draws.len(), 1);
                Ok(snapshot_output(draws, extent, 65535))
            });
        h.draw(indexed, 65535, 0);
        h.execute().unwrap();

        let mut software = Harness::new();
        if indexed {
            software.indices(57);
        }
        software.draw(indexed, 768, 0);
        assert_eq!(software.execute().unwrap().pixels.len(), 4);
        software
            .ctx
            .reset_command_list(software.list, software.allocator)
            .unwrap();
        software.ctx.cmd_set_viewport(software.list, 1, 1).unwrap();
        software
            .ctx
            .cmd_set_vertex_buffer(software.list, software.vertices, 0, 16)
            .unwrap();
        // Nonindexed aggregate uses the same shared software limit.
        software.draw(false, 384, 0);
        software.draw(false, 387, 0);
        assert_eq!(software.execute(), Err(GraphicsError::InvalidDescriptor));
    }
}

#[test]
fn async_native_pixel_boundary_matches_synchronous_preparation() {
    for (width, accepted) in [(1024, true), (1025, false)] {
        let mut h = Harness::new();
        let target = h.target(width, 1024);
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        h.ctx.set_metal_async_backend(move |request| {
            callback_calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.extent, (1024, 1024));
            Ok(Box::new(Ready(Some(GraphicsPassOutput {
                color_layouts: None,
                colors: vec![vec![17; 4194304]],
                depth: None,
            }))))
        });
        h.ctx
            .cmd_set_color_targets(h.list, &[target], &[[0; 4]])
            .unwrap();
        h.draw(false, 3, 0);
        let result = h.execute();
        if accepted {
            assert_eq!(result.unwrap().pixels.len(), 4194304);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        } else {
            assert_eq!(result, Err(GraphicsError::InvalidViewport));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }
}

#[test]
fn public_software_helper_checks_dimensions_before_allocation() {
    let degenerate = [[0.0, 0.0, 0.0, 1.0]; 3];
    for extent in [(64, 64), (4096, 1), (1, 4096)] {
        let pixels = GraphicsContext::execute_software_reference_triangles(
            &degenerate,
            extent,
            [17, 31, 47, 255],
        )
        .unwrap();
        assert_eq!(pixels, [17, 31, 47, 255].repeat(4096));
    }
    for extent in [
        (0, 1),
        (1, 0),
        (0, 0),
        (4097, 1),
        (1, 4097),
        (65, 64),
        (4096, 4096),
        (usize::MAX, 2),
        (2, usize::MAX),
    ] {
        assert_eq!(
            GraphicsContext::execute_software_reference_triangles(&degenerate, extent, [0; 4],),
            Err(GraphicsError::InvalidViewport)
        );
        assert_eq!(
            GraphicsContext::execute_software_reference_draw(&degenerate, extent, [0; 4],),
            Err(GraphicsError::InvalidViewport)
        );
    }
    assert_eq!(
        GraphicsContext::execute_software_reference_triangles(
            &[[0.0, 0.0, 0.0, 1.0]; 771],
            (1, 1),
            [0; 4],
        ),
        Err(GraphicsError::InvalidDescriptor)
    );
}

#[test]
fn native_draw_count_accepts_256_and_rejects_257_before_fetch_or_dispatch() {
    assert_eq!(MAX_NATIVE_RENDER_DRAWS, 256);
    for asynchronous in [false, true] {
        for indexed in [false, true] {
            for excess in [false, true] {
                let mut h = Harness::new();
                if indexed {
                    h.indices(42);
                }
                let calls = Arc::new(AtomicUsize::new(0));
                let callback_calls = calls.clone();
                if asynchronous {
                    h.ctx.set_metal_async_backend(move |request| {
                        callback_calls.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(request.draws.len(), 256);
                        assert!(request.draws.iter().all(|draw| draw.vertices.len() == 3));
                        Ok(Box::new(Ready(Some(GraphicsPassOutput {
                            color_layouts: None,
                            colors: vec![vec![2, 3, 5, 255]],
                            depth: None,
                        }))))
                    });
                } else {
                    h.ctx
                        .set_metal_attachment_backend(move |draws, _, _, _, _| {
                            callback_calls.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(draws.len(), 256);
                            assert!(draws.iter().all(|draw| draw.vertices.len() == 3));
                            Ok(GraphicsPassOutput {
                                color_layouts: None,
                                colors: vec![vec![2, 3, 5, 255]],
                                depth: None,
                            })
                        });
                }
                for _ in 0..256 {
                    h.draw(indexed, 0, 0); // Empty commands do not consume the pass limit.
                    h.draw(indexed, 3, 0);
                }
                h.draw(indexed, 0, 0);
                if excess {
                    // Invalid fetch address distinguishes count rejection before fetching
                    // from a later IndexOutOfRange error on the 257th nonempty draw.
                    h.draw(indexed, 3, u32::MAX);
                }
                let result = h.execute();
                if excess {
                    assert_eq!(result, Err(GraphicsError::InvalidDescriptor));
                    assert_eq!(calls.load(Ordering::SeqCst), 0);
                    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
                } else {
                    let result = result.unwrap();
                    assert_eq!(result.draw_count, 256);
                    assert_eq!(result.pixels, [2, 3, 5, 255]);
                    assert_eq!(calls.load(Ordering::SeqCst), 1);
                }
            }
        }
    }
}
