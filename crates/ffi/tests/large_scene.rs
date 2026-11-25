// SPDX-License-Identifier: GPL-3.0-or-later
//! Original indexed tile scene through the real asynchronous Metal adapter.
use xenora_ffi::graphics_backend::install_async_pipeline_backend;
use xenora_loader::graphics::*;
use xenora_loader::{GuestMemory, MemoryPermissions};

fn buffer(size: usize) -> ResourceDesc {
    ResourceDesc {
        dimension: ResourceDimension::Buffer,
        width: size as u64,
        height: 1,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: false,
    }
}
fn submit(
    ctx: &mut GraphicsContext,
    queue: GraphicsHandle,
    list: GraphicsHandle,
    memory: &GuestMemory,
) {
    ctx.close_command_list(list).unwrap();
    let receipt = ctx.submit_command_list(queue, list, memory).unwrap();
    assert_eq!(
        ctx.wait_submission(receipt).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    ctx.release_submission(receipt).unwrap();
}
fn verify(pixels: &[u8], frame: usize) {
    assert_eq!(pixels.len(), 512 * 512 * 4);
    for (n, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = (n % 512, n / 512);
        // Expected raster is a binary stripe/checker pattern in pixel coordinates.
        // This does not reuse vertex generation, index gathering or a rasterizer.
        let expected = [
            if ((x >> 4) ^ frame) & 1 != 0 { 255 } else { 0 },
            if y & 16 != 0 { 255 } else { 0 },
            if (x ^ y) & 32 != 0 { 255 } else { 0 },
            255,
        ];
        assert_eq!(*pixel, expected, "frame={frame}, pixel=({x},{y})");
    }
}

#[test]
#[cfg_attr(not(feature = "gpu-tests"), ignore = "requires native Metal")]
fn large_indexed_tiles_reuse_depth_and_colors_with_both_index_formats() {
    assert_eq!(
        MAX_NATIVE_RENDER_VERTICES,
        xenora_metal::MAX_RENDER_VERTICES
    );
    assert_eq!(MAX_NATIVE_RENDER_PIXELS, xenora_metal::MAX_RENDER_PIXELS);
    assert_eq!(MAX_RENDER_DIMENSION, xenora_metal::MAX_RENDER_DIMENSION);
    assert_eq!(MAX_NATIVE_RENDER_DRAWS, xenora_metal::MAX_RENDER_DRAWS);
    for format in [IndexFormat::R16Uint, IndexFormat::R32Uint] {
        let mut ctx = GraphicsContext::new();
        install_async_pipeline_backend(&mut ctx).unwrap();
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
        let pipeline = ctx
            .create_graphics_pipeline(
                device,
                include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin"),
                include_bytes!("../../../fixtures/rdna2/x9-varying-ps.bin"),
            )
            .unwrap();
        let mut positions = vec![0xa5; 16];
        let mut indices = vec![0xff; format.size() * 5]; // two offset indices, three start indices
        for y in 0..32 {
            for x in 0..32 {
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let pos: [f32; 4] = [
                        (x + dx) as f32 / 16. - 1.,
                        1. - (y + dy) as f32 / 16.,
                        0.25,
                        1.,
                    ];
                    positions.extend(pos.into_iter().flat_map(f32::to_le_bytes));
                    positions.extend_from_slice(&[0xa5; 16]);
                }
                let base = (y * 32 + x) * 4 + 1; // deliberately corrected by base_vertex=-1
                for index in [base, base + 2, base + 1, base + 1, base + 2, base + 3] {
                    match format {
                        IndexFormat::R16Uint => {
                            indices.extend_from_slice(&(index as u16).to_le_bytes())
                        }
                        IndexFormat::R32Uint => {
                            indices.extend_from_slice(&(index as u32).to_le_bytes())
                        }
                    }
                }
            }
        }
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x10000, 2 * 1024 * 1024, MemoryPermissions::READ_WRITE)
            .unwrap();
        let vb = ctx
            .create_committed_resource(
                device,
                buffer(positions.len()),
                ResourceStates::COPY_DEST,
                None,
            )
            .unwrap();
        let cb = ctx
            .create_committed_resource(
                device,
                buffer(32 + 4096 * 32),
                ResourceStates::COPY_DEST,
                None,
            )
            .unwrap();
        let ib = ctx
            .create_committed_resource(
                device,
                buffer(indices.len()),
                ResourceStates::COPY_DEST,
                None,
            )
            .unwrap();
        let target = ctx
            .create_committed_resource(
                device,
                ResourceDesc {
                    dimension: ResourceDimension::Texture2D,
                    width: 512,
                    height: 512,
                    depth_or_array_size: 1,
                    mip_levels: 1,
                    format_rgba8: true,
                },
                ResourceStates::RENDER_TARGET,
                None,
            )
            .unwrap();
        let depth = ctx.create_depth_resource(device, (512, 512)).unwrap();
        for (resource, bytes) in [(vb, positions.as_slice()), (ib, indices.as_slice())] {
            memory.write_bytes(0x10000, bytes).unwrap();
            ctx.copy_guest_to_resource(resource, 0, 0x10000, bytes.len(), &memory)
                .unwrap();
        }
        for frame in 0..32 {
            ctx.reset_command_list(list, allocator).unwrap();
            let mut colors = vec![0xa5; 32];
            for y in 0..32 {
                for x in 0..32 {
                    let color: [f32; 4] = [
                        ((x + frame) % 2) as f32,
                        (y % 2) as f32,
                        ((x / 2 + y / 2) % 2) as f32,
                        1.,
                    ];
                    for _ in 0..4 {
                        colors.extend(color.into_iter().flat_map(f32::to_le_bytes));
                        colors.extend_from_slice(&[0xa5; 16]);
                    }
                }
            }
            memory.write_bytes(0x10000, &colors).unwrap();
            ctx.copy_guest_to_resource(cb, 0, 0x10000, colors.len(), &memory)
                .unwrap();
            memory.write_bytes(0x10000, &vec![0; colors.len()]).unwrap();
            if frame == 0 {
                ctx.cmd_resource_barrier(
                    list,
                    vb,
                    ResourceStates::COPY_DEST,
                    ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
                )
                .unwrap();
                ctx.cmd_resource_barrier(
                    list,
                    ib,
                    ResourceStates::COPY_DEST,
                    ResourceStates::INDEX_BUFFER,
                )
                .unwrap();
            }
            ctx.cmd_resource_barrier(
                list,
                cb,
                ResourceStates::COPY_DEST,
                ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
            )
            .unwrap();
            ctx.cmd_set_graphics_pipeline(list, Some(pipeline)).unwrap();
            ctx.cmd_set_color_targets(list, &[target], &[[17, 31, 47, 255]])
                .unwrap();
            ctx.cmd_set_depth_target(list, Some(depth), GraphicsDepthLoad::Clear(1.))
                .unwrap();
            ctx.cmd_set_viewport(list, 512, 512).unwrap();
            ctx.cmd_set_vertex_buffer(list, vb, 16, 32).unwrap();
            ctx.cmd_set_vertex_colors(list, cb, 32, 32).unwrap();
            ctx.cmd_set_index_buffer(
                list,
                ib,
                2 * format.size(),
                if format == IndexFormat::R16Uint {
                    57
                } else {
                    42
                },
            )
            .unwrap();
            ctx.cmd_set_draw_state(
                list,
                GraphicsDrawState {
                    depth_compare: Some(GraphicsDepthCompare::Less),
                    depth_write: true,
                    scissor: Some([0, 0, 512, 256]),
                    ..Default::default()
                },
            )
            .unwrap();
            ctx.cmd_draw_indexed_instanced(list, 3072, 1, 3, -1, 0)
                .unwrap();
            ctx.cmd_set_draw_state(
                list,
                GraphicsDrawState {
                    depth_compare: Some(GraphicsDepthCompare::Less),
                    depth_write: true,
                    ..Default::default()
                },
            )
            .unwrap();
            ctx.cmd_draw_indexed_instanced(list, 3072, 1, 3075, -1, 0)
                .unwrap();
            ctx.cmd_resource_barrier(
                list,
                cb,
                ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
                ResourceStates::COPY_DEST,
            )
            .unwrap();
            submit(&mut ctx, queue, list, &memory);
            verify(&ctx.last_presentation.lock().unwrap().pixels, frame);
            ctx.reset_command_list(list, allocator).unwrap();
            ctx.cmd_set_color_targets_with_loads(list, &[target], &[GraphicsColorLoad::Load])
                .unwrap();
            ctx.cmd_set_depth_target(list, Some(depth), GraphicsDepthLoad::Load)
                .unwrap();
            // Zero colors would replace the pattern if loaded depth were cleared or lost.
            ctx.copy_guest_to_resource(cb, 0, 0x10000, colors.len(), &memory)
                .unwrap();
            ctx.cmd_resource_barrier(
                list,
                cb,
                ResourceStates::COPY_DEST,
                ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
            )
            .unwrap();
            ctx.cmd_set_graphics_pipeline(list, Some(pipeline)).unwrap();
            ctx.cmd_set_viewport(list, 512, 512).unwrap();
            ctx.cmd_set_vertex_buffer(list, vb, 16, 32).unwrap();
            ctx.cmd_set_vertex_colors(list, cb, 32, 32).unwrap();
            ctx.cmd_set_index_buffer(
                list,
                ib,
                2 * format.size(),
                if format == IndexFormat::R16Uint {
                    57
                } else {
                    42
                },
            )
            .unwrap();
            ctx.cmd_set_draw_state(
                list,
                GraphicsDrawState {
                    depth_compare: Some(GraphicsDepthCompare::Less),
                    depth_write: false,
                    ..Default::default()
                },
            )
            .unwrap();
            ctx.cmd_draw_indexed_instanced(list, 6144, 1, 3, -1, 0)
                .unwrap();
            ctx.cmd_resource_barrier(
                list,
                cb,
                ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
                ResourceStates::COPY_DEST,
            )
            .unwrap();
            submit(&mut ctx, queue, list, &memory);
            verify(&ctx.last_presentation.lock().unwrap().pixels, frame);
        }
        assert_eq!(ctx.last_presentation.lock().unwrap().frame_index, 64);
        ctx.reset_command_list(list, allocator).unwrap();
        ctx.cmd_set_color_targets_with_loads(list, &[target], &[GraphicsColorLoad::Load])
            .unwrap();
        ctx.cmd_resource_barrier(
            list,
            ib,
            ResourceStates::INDEX_BUFFER,
            ResourceStates::COPY_DEST,
        )
        .unwrap();
        submit(&mut ctx, queue, list, &memory);
        memory.write_bytes(0x10000, &[0xff; 4]).unwrap();
        ctx.copy_guest_to_resource(
            ib,
            indices.len() - format.size(),
            0x10000,
            format.size(),
            &memory,
        )
        .unwrap();
        ctx.reset_command_list(list, allocator).unwrap();
        ctx.cmd_set_color_targets_with_loads(list, &[target], &[GraphicsColorLoad::Load])
            .unwrap();
        ctx.cmd_set_graphics_pipeline(list, Some(pipeline)).unwrap();
        ctx.cmd_set_viewport(list, 512, 512).unwrap();
        ctx.cmd_set_vertex_buffer(list, vb, 16, 32).unwrap();
        ctx.cmd_resource_barrier(
            list,
            ib,
            ResourceStates::COPY_DEST,
            ResourceStates::INDEX_BUFFER,
        )
        .unwrap();
        ctx.cmd_set_index_buffer(
            list,
            ib,
            2 * format.size(),
            if format == IndexFormat::R16Uint {
                57
            } else {
                42
            },
        )
        .unwrap();
        ctx.cmd_draw_indexed_instanced(list, 6144, 1, 3, -1, 0)
            .unwrap();
        ctx.close_command_list(list).unwrap();
        assert_eq!(
            ctx.submit_command_list(queue, list, &memory),
            Err(GraphicsError::IndexOutOfRange)
        );
        assert_eq!(ctx.last_presentation.lock().unwrap().frame_index, 65);
        verify(&ctx.last_presentation.lock().unwrap().pixels, 31);
        ctx.reset_command_list(list, allocator).unwrap();
        ctx.destroy_graphics_pipeline(pipeline).unwrap();
        for resource in [vb, cb, ib, target, depth] {
            ctx.destroy_resource(resource).unwrap();
        }
    }
}
