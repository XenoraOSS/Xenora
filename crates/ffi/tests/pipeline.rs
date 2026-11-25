// SPDX-License-Identifier: GPL-3.0-or-later
//! Original shaders/resources through the production loader-to-Metal adapter.
use xenora_ffi::graphics_backend::install_pipeline_backend;
use xenora_loader::graphics::*;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};
use xenora_texture::SamplerDesc;

#[test]
#[cfg_attr(not(feature = "gpu-tests"), ignore = "requires native Metal")]
fn selected_pipelines_sample_uploads_preserve_depth_and_switch_to_mrt() {
    let mut context = GraphicsContext::new();
    install_pipeline_backend(&mut context).unwrap();
    let device = context.create_device().unwrap();
    let queue = context
        .create_command_queue(device, CommandListType::Direct)
        .unwrap();
    let allocator = context
        .create_command_allocator(device, CommandListType::Direct)
        .unwrap();
    let list = context
        .create_command_list(device, allocator, CommandListType::Direct)
        .unwrap();
    let sample = context
        .create_graphics_pipeline(
            device,
            include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin"),
            include_bytes!("../../../fixtures/rdna2/x9-sampled-arithmetic-ps.bin"),
        )
        .unwrap();
    let mrt = context
        .create_graphics_pipeline(
            device,
            include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin"),
            include_bytes!("../../../fixtures/rdna2/x9-mrt-ps.bin"),
        )
        .unwrap();
    let buffer_desc = ResourceDesc {
        dimension: ResourceDimension::Buffer,
        width: 96,
        height: 1,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: false,
    };
    let positions = context
        .create_committed_resource(
            device,
            buffer_desc.clone(),
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
            Some(0x10000),
        )
        .unwrap();
    let colors = context
        .create_committed_resource(
            device,
            buffer_desc,
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
            Some(0x10100),
        )
        .unwrap();
    let desc = ResourceDesc {
        dimension: ResourceDimension::Texture2D,
        width: 2,
        height: 2,
        depth_or_array_size: 1,
        mip_levels: 2,
        format_rgba8: true,
    };
    let a = context
        .create_committed_resource(device, desc.clone(), ResourceStates::COPY_DEST, None)
        .unwrap();
    let b = context
        .create_committed_resource(device, desc.clone(), ResourceStates::COPY_DEST, None)
        .unwrap();
    let target_desc = ResourceDesc {
        width: 4,
        height: 3,
        mip_levels: 1,
        ..desc
    };
    let target = context
        .create_committed_resource(
            device,
            target_desc.clone(),
            ResourceStates::RENDER_TARGET,
            None,
        )
        .unwrap();
    let depth = context.create_depth_resource(device, (4, 3)).unwrap();
    let mut memory = GuestMemory::new();
    memory
        .map_region(0x10000, 4096, MemoryPermissions::READ_WRITE)
        .unwrap();
    let uv: Vec<u8> = [[0.25f32, 0.25, 0.0, 1.0]; 3]
        .into_iter()
        .chain([[0.75, 0.75, 0.0, 1.0]; 3])
        .flatten()
        .flat_map(f32::to_le_bytes)
        .collect();
    memory.write_bytes(0x10100, &uv).unwrap();
    let clear = [17, 31, 47, 255];
    for frame in 0..8 {
        context.reset_command_list(list, allocator).unwrap();
        let phase = frame % 4;
        let load = phase == 1 || phase == 2;
        let a_color = if frame % 4 < 2 {
            [200u8, 40, 60, 255]
        } else {
            [40, 120, 200, 255]
        };
        let b_color = if frame % 4 < 2 {
            [80u8, 40, 200, 255]
        } else {
            [160, 200, 40, 255]
        };
        let mip_color = if frame < 4 {
            [10u8, 220, 30, 255]
        } else {
            [60, 80, 180, 255]
        };
        let mut a_texels = [9u8; 20];
        a_texels[..4].copy_from_slice(&a_color);
        a_texels[16..20].copy_from_slice(&mip_color);
        let mut b_texels = [13u8; 20];
        b_texels[12..16].copy_from_slice(&b_color);
        memory.write_bytes(0x10200, &a_texels).unwrap();
        memory.write_bytes(0x10220, &b_texels).unwrap();
        context
            .copy_guest_to_resource(a, 0, 0x10200, 20, &memory)
            .unwrap();
        context
            .copy_guest_to_resource(b, 0, 0x10220, 20, &memory)
            .unwrap();
        let z = match phase {
            1 => [0.5, 0.5],
            2 => [0.625, 0.625],
            _ => [0.25, 0.75],
        };
        let vertices: Vec<u8> = z
            .into_iter()
            .flat_map(|z| {
                [
                    [-1.0f32, -1.0, z, 1.0],
                    [3.0, -1.0, z, 1.0],
                    [-1.0, 3.0, z, 1.0],
                ]
            })
            .flatten()
            .flat_map(f32::to_le_bytes)
            .collect();
        memory.write_bytes(0x10000, &vertices).unwrap();
        if frame > 0 {
            context
                .cmd_resource_barrier(
                    list,
                    target,
                    ResourceStates::COPY_SOURCE,
                    ResourceStates::RENDER_TARGET,
                )
                .unwrap();
        }
        for texture in [a, b] {
            context
                .cmd_resource_barrier(
                    list,
                    texture,
                    ResourceStates::COPY_DEST,
                    ResourceStates::PIXEL_SHADER_RESOURCE,
                )
                .unwrap();
        }
        context
            .cmd_set_graphics_pipeline(list, Some(sample))
            .unwrap();
        context
            .cmd_set_color_targets(list, &[target], &[clear])
            .unwrap();
        context
            .cmd_set_depth_target(
                list,
                Some(depth),
                if load {
                    GraphicsDepthLoad::Load
                } else {
                    GraphicsDepthLoad::Clear(1.0)
                },
            )
            .unwrap();
        context
            .cmd_set_vertex_buffer(list, positions, 0, 16)
            .unwrap();
        context.cmd_set_vertex_colors(list, colors, 0, 16).unwrap();
        context
            .cmd_set_draw_state(
                list,
                GraphicsDrawState {
                    depth_compare: Some(GraphicsDepthCompare::Less),
                    depth_write: true,
                    scissor: Some([0, 0, 2, 3]),
                    ..Default::default()
                },
            )
            .unwrap();
        context
            .cmd_set_sampled_texture(list, Some(a), SamplerDesc::nearest_clamp())
            .unwrap();
        context.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
        context
            .cmd_set_draw_state(
                list,
                GraphicsDrawState {
                    depth_compare: Some(GraphicsDepthCompare::Less),
                    depth_write: true,
                    ..Default::default()
                },
            )
            .unwrap();
        context
            .cmd_set_sampled_texture(
                list,
                Some(b),
                SamplerDesc::nearest_clamp().with_explicit_mip(0),
            )
            .unwrap();
        context.cmd_draw_instanced(list, 3, 1, 3, 0).unwrap();
        // Reuse A's uploaded texture with a different sampler selecting mip one.
        context
            .cmd_set_draw_state(
                list,
                GraphicsDrawState {
                    depth_compare: Some(GraphicsDepthCompare::LessEqual),
                    depth_write: true,
                    scissor: Some([0, 0, 1, 1]),
                    ..Default::default()
                },
            )
            .unwrap();
        let mut mip_sampler = SamplerDesc::nearest_clamp();
        if frame % 2 == 0 {
            mip_sampler.min_lod = 1.0;
            mip_sampler.max_lod = 1.0;
        } else {
            mip_sampler = mip_sampler.with_explicit_mip(1);
        }
        context
            .cmd_set_sampled_texture(list, Some(a), mip_sampler)
            .unwrap();
        context.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();

        for texture in [a, b] {
            context
                .cmd_resource_barrier(
                    list,
                    texture,
                    ResourceStates::PIXEL_SHADER_RESOURCE,
                    ResourceStates::COPY_DEST,
                )
                .unwrap();
        }
        context
            .cmd_resource_barrier(
                list,
                target,
                ResourceStates::RENDER_TARGET,
                ResourceStates::COPY_SOURCE,
            )
            .unwrap();
        context.close_command_list(list).unwrap();
        let out = context.execute_command_list(queue, list, &memory).unwrap();
        assert_eq!(out.backend, GraphicsBackendId::MetalNative);
        assert_eq!(out.draw_count, 3);
        for (index, pixel) in out.pixels.as_chunks::<4>().0.iter().enumerate() {
            let mut expected = if index % 4 < 2 { a_color } else { b_color };
            expected[0] /= 2;
            if phase == 2 || (phase == 1 && index % 4 < 2) {
                expected = clear;
            }
            if !load && index == 0 {
                expected = mip_color;
                expected[0] /= 2;
            }
            assert_eq!(pixel, &expected, "frame {frame}, pixel {index}");
        }
        context
            .copy_resource_to_guest(target, 0, 0x10300, 48, &mut memory)
            .unwrap();
        let mut copied = [0; 48];
        memory.read_bytes(0x10300, &mut copied).unwrap();
        assert_eq!(copied.as_slice(), out.pixels);
        assert_eq!(
            context.destroy_graphics_pipeline(sample),
            Err(GraphicsError::ResourceLifetimeViolation)
        );
        assert_eq!(
            context.destroy_resource(a),
            Err(GraphicsError::ResourceLifetimeViolation)
        );
    }
    // A different shader pipeline reaches all eight outputs through the same adapter.
    context.reset_command_list(list, allocator).unwrap();
    context.destroy_graphics_pipeline(sample).unwrap();
    let targets: Vec<_> = (0..8)
        .map(|_| {
            context
                .create_committed_resource(
                    device,
                    target_desc.clone(),
                    ResourceStates::RENDER_TARGET,
                    None,
                )
                .unwrap()
        })
        .collect();
    context.cmd_set_graphics_pipeline(list, Some(mrt)).unwrap();
    context
        .cmd_set_color_targets(list, &targets, &[clear; 8])
        .unwrap();
    context
        .cmd_set_depth_target(list, Some(depth), GraphicsDepthLoad::Clear(1.0))
        .unwrap();
    context
        .cmd_set_vertex_buffer(list, positions, 0, 16)
        .unwrap();
    context
        .cmd_set_draw_state(
            list,
            GraphicsDrawState {
                depth_compare: Some(GraphicsDepthCompare::Less),
                depth_write: true,
                ..Default::default()
            },
        )
        .unwrap();
    context.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
    context.close_command_list(list).unwrap();
    let out = context.execute_command_list(queue, list, &memory).unwrap();
    assert_eq!(out.color_outputs.len(), 8);
    let palette = [
        [0, 0, 0, 255],
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 0, 255],
        [255, 0, 255, 255],
        [0, 255, 255, 255],
        [255, 255, 255, 255],
    ];
    for (slot, color) in palette.into_iter().enumerate() {
        assert_eq!(out.color_outputs[slot], color.repeat(12));
    }
    context.reset_command_list(list, allocator).unwrap();
    context.destroy_graphics_pipeline(mrt).unwrap();
    for resource in targets
        .into_iter()
        .chain([target, depth, a, b, positions, colors])
    {
        context.destroy_resource(resource).unwrap();
    }
}

fn mixed_pipeline_pass(asynchronous: bool) {
    let mut ctx = GraphicsContext::new();
    if asynchronous {
        xenora_ffi::graphics_backend::install_async_pipeline_backend(&mut ctx).unwrap();
    } else {
        install_pipeline_backend(&mut ctx).unwrap();
    }
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
    let vs = include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin");
    let a = ctx
        .create_graphics_pipeline(
            device,
            vs,
            include_bytes!("../../../fixtures/rdna2/x9-varying-ps.bin"),
        )
        .unwrap();
    let b = ctx
        .create_graphics_pipeline(
            device,
            include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin"),
            include_bytes!("../../../fixtures/rdna2/x9-constant-ps.bin"),
        )
        .unwrap();
    let c = ctx
        .create_graphics_pipeline(
            device,
            vs,
            include_bytes!("../../../fixtures/rdna2/x9-sampled-arithmetic-ps.bin"),
        )
        .unwrap();
    let buffer = ResourceDesc {
        dimension: ResourceDimension::Buffer,
        width: 48,
        height: 1,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: false,
    };
    let positions = ctx
        .create_committed_resource(
            device,
            buffer.clone(),
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
            Some(0x10000),
        )
        .unwrap();
    let colors = ctx
        .create_committed_resource(
            device,
            buffer,
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
            Some(0x10100),
        )
        .unwrap();
    let desc = ResourceDesc {
        dimension: ResourceDimension::Texture2D,
        width: 1,
        height: 1,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: true,
    };
    let texture = ctx
        .create_committed_resource(device, desc.clone(), ResourceStates::COPY_DEST, None)
        .unwrap();
    let target = ctx
        .create_committed_resource(
            device,
            ResourceDesc {
                width: 8,
                height: 3,
                ..desc
            },
            ResourceStates::RENDER_TARGET,
            None,
        )
        .unwrap();
    let mut memory = GuestMemory::new();
    memory
        .map_region(0x10000, 4096, MemoryPermissions::READ_WRITE)
        .unwrap();
    let vertices: Vec<u8> = [
        [-1.0f32, -1., 0.25, 1.],
        [3., -1., 0.25, 1.],
        [-1., 3., 0.25, 1.],
    ]
    .into_iter()
    .flatten()
    .flat_map(f32::to_le_bytes)
    .collect();
    let blue: Vec<u8> = [[0.0f32, 0., 1., 1.]; 3]
        .into_iter()
        .flatten()
        .flat_map(f32::to_le_bytes)
        .collect();
    memory.write_bytes(0x10000, &vertices).unwrap();
    memory.write_bytes(0x10100, &blue).unwrap();
    memory.write_bytes(0x10200, &[0, 255, 0, 255]).unwrap();
    ctx.copy_guest_to_resource(texture, 0, 0x10200, 4, &memory)
        .unwrap();
    // This adapter requires explicit shaders even though the loader can also
    // serve legacy pipeline-less backends. Rejection must not publish a frame.
    ctx.cmd_set_color_targets(list, &[target], &[[17, 31, 47, 255]])
        .unwrap();
    ctx.cmd_set_viewport(list, 8, 3).unwrap();
    ctx.cmd_set_vertex_buffer(list, positions, 0, 16).unwrap();
    ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
    ctx.close_command_list(list).unwrap();
    assert_eq!(
        ctx.execute_command_list(queue, list, &memory).unwrap_err(),
        GraphicsError::InvalidDescriptor
    );
    assert_eq!(ctx.last_presentation.lock().unwrap().frame_index, 0);
    ctx.reset_command_list(list, allocator).unwrap();
    ctx.cmd_resource_barrier(
        list,
        texture,
        ResourceStates::COPY_DEST,
        ResourceStates::PIXEL_SHADER_RESOURCE,
    )
    .unwrap();
    ctx.cmd_set_color_targets(list, &[target], &[[17, 31, 47, 255]])
        .unwrap();
    ctx.cmd_set_viewport(list, 8, 3).unwrap();
    ctx.cmd_set_vertex_buffer(list, positions, 0, 16).unwrap();
    ctx.cmd_set_vertex_colors(list, colors, 0, 16).unwrap();
    // Same blend/write-mask for all draws: shader identity must distinguish the
    // native pipeline cache. Overlap and A/B/C/A order make switch-back observable.
    for (pipeline, scissor, sampled) in [
        (a, [0, 0, 8, 3], None),
        (b, [1, 0, 7, 3], None),
        (c, [2, 0, 5, 3], Some(texture)),
        (a, [3, 0, 3, 3], None),
    ] {
        ctx.cmd_set_graphics_pipeline(list, Some(pipeline)).unwrap();
        ctx.cmd_set_sampled_texture(list, sampled, SamplerDesc::nearest_clamp())
            .unwrap();
        ctx.cmd_set_draw_state(
            list,
            GraphicsDrawState {
                scissor: Some(scissor),
                ..Default::default()
            },
        )
        .unwrap();
        ctx.cmd_draw_instanced(list, 3, 1, 0, 0).unwrap();
    }
    ctx.close_command_list(list).unwrap();
    let out = if asynchronous {
        let receipt = ctx.submit_command_list(queue, list, &memory).unwrap();
        // Reset and logical destruction happen before collection. The pending
        // transaction must own every program, sampled resource and input snapshot.
        ctx.reset_command_list(list, allocator).unwrap();
        for pipeline in [a, b, c] {
            ctx.destroy_graphics_pipeline(pipeline).unwrap();
        }
        for resource in [positions, colors, texture, target] {
            ctx.destroy_resource(resource).unwrap();
        }
        drop(memory);
        assert_eq!(
            ctx.wait_submission(receipt).unwrap(),
            GraphicsSubmissionStatus::Complete
        );
        let out = ctx.submission_outcome(receipt).unwrap().unwrap().clone();
        ctx.release_submission(receipt).unwrap();
        out
    } else {
        ctx.execute_command_list(queue, list, &memory).unwrap()
    };
    assert_eq!(out.backend, GraphicsBackendId::MetalNative);
    assert_eq!(out.draw_count, 4);
    let row = [
        [0, 0, 255, 255],
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [0, 0, 255, 255],
        [0, 0, 255, 255],
        [0, 255, 0, 255],
        [255, 0, 0, 255],
    ];
    assert_eq!(
        out.pixels,
        row.repeat(3).into_iter().flatten().collect::<Vec<_>>()
    );
    assert_eq!(ctx.last_presentation.lock().unwrap().frame_index, 1);
}

#[test]
#[cfg_attr(not(feature = "gpu-tests"), ignore = "requires native Metal")]
fn mixed_pipelines_switch_programs_and_sampling_in_one_pass() {
    mixed_pipeline_pass(false);
}

#[test]
#[cfg_attr(not(feature = "gpu-tests"), ignore = "requires native Metal")]
fn mixed_pipelines_survive_async_reset_and_destruction() {
    mixed_pipeline_pass(true);
}
