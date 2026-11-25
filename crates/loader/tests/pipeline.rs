// SPDX-License-Identifier: GPL-3.0-or-later

//! CPU-only command validation and callback-contract tests; no Metal execution.

use std::sync::Arc;
use xenora_loader::graphics::{
    CommandListType, GraphicsContext, GraphicsError, GraphicsHandle, GraphicsPassOutput,
    ResourceDesc, ResourceDimension, ResourceStates,
};
use xenora_loader::memory::{GuestMemory, MemoryPermissions};
use xenora_texture::SamplerDesc;

const VS: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin");
const PS: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-constant-ps.bin");
const VARYING: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin");
const ARITHMETIC: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-pixel-arithmetic-ps.bin");
const SAMPLED: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-sampled-arithmetic-ps.bin");
const MRT: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-mrt-ps.bin");
const R: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const G: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const B: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

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
        let vertices = ctx
            .create_committed_resource(
                device,
                buffer_desc(48),
                ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
                None,
            )
            .unwrap();
        Self {
            ctx,
            memory: GuestMemory::new(),
            device,
            queue,
            allocator,
            list,
            vertices,
        }
    }

    fn pipeline(&mut self, vs: &[u8], ps: &[u8]) -> GraphicsHandle {
        self.ctx
            .create_graphics_pipeline(self.device, vs, ps)
            .unwrap()
    }

    fn texture(&mut self, state: ResourceStates) -> GraphicsHandle {
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
                state,
                None,
            )
            .unwrap()
    }

    fn draw(&mut self) {
        self.ctx
            .cmd_set_vertex_buffer(self.list, self.vertices, 0, 16)
            .unwrap();
        self.ctx.cmd_draw_instanced(self.list, 3, 1, 0, 0).unwrap();
    }

    fn baseline(&mut self) {
        self.ctx
            .cmd_set_render_targets(self.list, None, [7, 11, 13, 255])
            .unwrap();
        self.ctx.close_command_list(self.list).unwrap();
        let out = self
            .ctx
            .execute_command_list(self.queue, self.list, &self.memory)
            .unwrap();
        assert_eq!(out.pixels, [7, 11, 13, 255].repeat(12));
        self.ctx
            .reset_command_list(self.list, self.allocator)
            .unwrap();
    }
}

fn buffer_desc(width: u64) -> ResourceDesc {
    ResourceDesc {
        dimension: ResourceDimension::Buffer,
        width,
        height: 1,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: false,
    }
}

#[test]
fn cpu_malformed_pipeline_creation_does_not_poison_valid_creation() {
    let mut h = Harness::new();
    for (vs, ps) in [(&[][..], PS), (VS, &[][..]), (VS, &[0xff][..])] {
        assert!(matches!(
            h.ctx.create_graphics_pipeline(h.device, vs, ps),
            Err(GraphicsError::BackendError(_))
        ));
    }
    let pipeline = h.pipeline(VS, PS);
    h.ctx
        .cmd_set_graphics_pipeline(h.list, Some(pipeline))
        .unwrap();
    h.draw();
    // CPU-only callback checks that the usable pipeline reaches dispatch.
    h.ctx
        .set_metal_stateful_backend(|draws, extent, _, _, depth| {
            assert_eq!(draws.len(), 1);
            assert_eq!(extent, (4, 3));
            let pipeline = draws[0].pipeline.as_ref().unwrap();
            assert_eq!(pipeline.color_targets, 1);
            assert!(!pipeline.uses_texture);
            assert!(depth.is_none());
            Ok(GraphicsPassOutput {
                colors: vec![[29, 31, 37, 255].repeat(12)],
                depth: None,
            })
        });
    h.ctx.close_command_list(h.list).unwrap();
    let out = h
        .ctx
        .execute_command_list(h.queue, h.list, &h.memory)
        .unwrap();
    assert_eq!(out.pixels, [29, 31, 37, 255].repeat(12));
}

#[test]
fn cpu_binding_only_commands_retain_superseded_pipelines_textures_and_colors() {
    let mut h = Harness::new();
    let pipelines = [h.pipeline(VS, PS), h.pipeline(VARYING, ARITHMETIC)];
    let textures = [
        h.texture(ResourceStates::PIXEL_SHADER_RESOURCE),
        h.texture(ResourceStates::PIXEL_SHADER_RESOURCE),
    ];
    let colors = [
        h.vertices,
        h.ctx
            .create_committed_resource(
                h.device,
                buffer_desc(48),
                ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
                None,
            )
            .unwrap(),
    ];
    let second = h
        .ctx
        .create_command_list(h.device, h.allocator, CommandListType::Direct)
        .unwrap();
    for list in [h.list, second] {
        for index in 0..2 {
            h.ctx
                .cmd_set_graphics_pipeline(list, Some(pipelines[index]))
                .unwrap();
            h.ctx
                .cmd_set_sampled_texture(list, Some(textures[index]), SamplerDesc::nearest_clamp())
                .unwrap();
            h.ctx
                .cmd_set_vertex_colors(list, colors[index], 0, 16)
                .unwrap();
        }
        h.ctx.cmd_set_graphics_pipeline(list, None).unwrap();
        h.ctx
            .cmd_set_sampled_texture(list, None, SamplerDesc::nearest_clamp())
            .unwrap();
    }
    for closed in [false, true] {
        if closed {
            h.ctx.close_command_list(h.list).unwrap();
        }
        for pipeline in pipelines {
            assert_eq!(
                h.ctx.destroy_graphics_pipeline(pipeline),
                Err(GraphicsError::ResourceLifetimeViolation)
            );
        }
        for resource in textures.into_iter().chain(colors) {
            assert_eq!(
                h.ctx.destroy_resource(resource),
                Err(GraphicsError::ResourceLifetimeViolation)
            );
        }
    }
    // Failed binding-only submissions keep all recorded references.
    h.ctx
        .set_metal_backend(|_, _, _| panic!("invalid binding-only list dispatched"));
    assert!(
        h.ctx
            .execute_command_list(h.queue, h.list, &h.memory)
            .is_err()
    );
    for pipeline in pipelines {
        assert_eq!(
            h.ctx.destroy_graphics_pipeline(pipeline),
            Err(GraphicsError::ResourceLifetimeViolation)
        );
    }
    for resource in textures.into_iter().chain(colors) {
        assert_eq!(
            h.ctx.destroy_resource(resource),
            Err(GraphicsError::ResourceLifetimeViolation)
        );
    }
    h.ctx.reset_command_list(second, h.allocator).unwrap();
    // The failed first list alone must still retain even superseded bindings.
    for pipeline in pipelines {
        assert_eq!(
            h.ctx.destroy_graphics_pipeline(pipeline),
            Err(GraphicsError::ResourceLifetimeViolation)
        );
    }
    for resource in textures.into_iter().chain(colors) {
        assert_eq!(
            h.ctx.destroy_resource(resource),
            Err(GraphicsError::ResourceLifetimeViolation)
        );
    }
    h.ctx.reset_command_list(h.list, h.allocator).unwrap();
    for pipeline in pipelines {
        h.ctx.destroy_graphics_pipeline(pipeline).unwrap();
    }
    for resource in textures.into_iter().chain(colors) {
        h.ctx.destroy_resource(resource).unwrap();
    }
}

#[test]
fn cpu_invalid_pipeline_draws_never_dispatch_or_replace_presentation() {
    for case in 0..5 {
        let mut h = Harness::new();
        h.baseline();
        let before = h.ctx.last_presentation.lock().unwrap().clone();
        // Both callback kinds are CPU-only rejection sentinels.
        if case == 4 {
            h.ctx
                .set_metal_backend(|_, _, _| panic!("explicit pipeline reached legacy callback"));
        } else {
            h.ctx.set_metal_stateful_backend(|_, _, _, _, _| {
                panic!("invalid draw reached callback")
            });
        }
        let pipeline = match case {
            0 | 1 => h.pipeline(VARYING, SAMPLED),
            2 => h.pipeline(VS, MRT),
            _ => h.pipeline(VS, PS),
        };
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        if case == 1 {
            let texture = h.texture(ResourceStates::COPY_DEST);
            h.ctx
                .cmd_set_sampled_texture(h.list, Some(texture), SamplerDesc::nearest_clamp())
                .unwrap();
        }
        h.draw();
        if case == 3 {
            // Pipeline switching is valid, but this later draw needs eight targets.
            let other = h.pipeline(VS, MRT);
            h.ctx
                .cmd_set_graphics_pipeline(h.list, Some(other))
                .unwrap();
            h.draw();
        }
        h.ctx.close_command_list(h.list).unwrap();
        let expected = if case == 1 {
            GraphicsError::ResourceLifetimeViolation
        } else {
            GraphicsError::InvalidDescriptor
        };
        assert_eq!(
            h.ctx
                .execute_command_list(h.queue, h.list, &h.memory)
                .unwrap_err(),
            expected,
            "case {case}"
        );
        let after = h.ctx.last_presentation.lock().unwrap();
        assert_eq!(after.pixels, before.pixels, "case {case}");
        assert_eq!(
            (after.width, after.height, after.frame_index, after.backend),
            (
                before.width,
                before.height,
                before.frame_index,
                before.backend
            ),
            "case {case}"
        );
    }
}

#[test]
fn cpu_indexed_draw_gathers_guest_vertex_colors_in_index_order() {
    let mut h = Harness::new();
    h.memory
        .map_region(0x1000, 48, MemoryPermissions::READ_WRITE)
        .unwrap();
    let bytes: Vec<_> = [R, G, B]
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    h.memory.write_bytes(0x1000, &bytes).unwrap();
    let colors = h
        .ctx
        .create_committed_resource(
            h.device,
            buffer_desc(48),
            ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
            Some(0x1000),
        )
        .unwrap();
    let indices = h
        .ctx
        .create_committed_resource(
            h.device,
            buffer_desc(12),
            ResourceStates::INDEX_BUFFER,
            Some(0x2000),
        )
        .unwrap();
    h.memory
        .map_region(0x2000, 12, MemoryPermissions::READ_WRITE)
        .unwrap();
    let index_bytes: Vec<_> = [2u32, 0, 1].iter().flat_map(|v| v.to_le_bytes()).collect();
    h.memory.write_bytes(0x2000, &index_bytes).unwrap();
    let pipeline = h.pipeline(VARYING, ARITHMETIC);
    h.ctx
        .cmd_set_graphics_pipeline(h.list, Some(pipeline))
        .unwrap();
    h.ctx
        .cmd_set_vertex_buffer(h.list, h.vertices, 0, 16)
        .unwrap();
    h.ctx.cmd_set_vertex_colors(h.list, colors, 0, 16).unwrap();
    h.ctx.cmd_set_index_buffer(h.list, indices, 0, 42).unwrap();
    h.ctx
        .cmd_draw_indexed_instanced(h.list, 3, 1, 0, 0, 0)
        .unwrap();
    // CPU-only: the callback checks gathering, not rasterization or shader execution.
    h.ctx.set_metal_stateful_backend(|draws, _, _, _, depth| {
        assert_eq!(draws.len(), 1);
        assert_eq!(draws[0].colors, [B, R, G]);
        assert_eq!(draws[0].pipeline.as_ref().unwrap().color_targets, 1);
        assert!(!draws[0].pipeline.as_ref().unwrap().uses_texture);
        assert!(depth.is_none());
        Ok(GraphicsPassOutput {
            colors: vec![[41, 43, 47, 255].repeat(12)],
            depth: None,
        })
    });
    h.ctx.close_command_list(h.list).unwrap();
    let out = h
        .ctx
        .execute_command_list(h.queue, h.list, &h.memory)
        .unwrap();
    assert_eq!(out.pixels, [41, 43, 47, 255].repeat(12));
}

#[test]
fn cpu_sampled_draws_share_pixel_snapshot_and_keep_distinct_samplers() {
    let mut h = Harness::new();
    let pipeline = h.pipeline(VARYING, SAMPLED);
    let texture = h.texture(ResourceStates::PIXEL_SHADER_RESOURCE);
    let samplers = [
        SamplerDesc::nearest_clamp(),
        SamplerDesc::nearest_clamp().with_lod_range(0.0, 0.0),
    ];
    h.ctx
        .cmd_set_graphics_pipeline(h.list, Some(pipeline))
        .unwrap();
    for sampler in samplers {
        h.ctx
            .cmd_set_sampled_texture(h.list, Some(texture), sampler)
            .unwrap();
        h.draw();
    }
    // CPU-only: compare immutable input snapshots and recorded descriptors.
    h.ctx
        .set_metal_stateful_backend(move |draws, _, _, _, depth| {
            assert_eq!(draws.len(), 2);
            let a = draws[0].sampled.as_ref().unwrap();
            let b = draws[1].sampled.as_ref().unwrap();
            assert!(Arc::ptr_eq(&a.pixels, &b.pixels));
            assert_eq!(&*a.pixels, &[0, 0, 0, 0]);
            assert_eq!((a.width, a.height, b.width, b.height), (1, 1, 1, 1));
            assert_eq!(a.sampler, samplers[0]);
            assert_eq!(b.sampler, samplers[1]);
            assert!(
                draws
                    .iter()
                    .all(|draw| draw.pipeline.as_ref().unwrap().uses_texture)
            );
            assert!(depth.is_none());
            Ok(GraphicsPassOutput {
                colors: vec![[59, 61, 67, 255].repeat(12)],
                depth: None,
            })
        });
    h.ctx.close_command_list(h.list).unwrap();
    let out = h
        .ctx
        .execute_command_list(h.queue, h.list, &h.memory)
        .unwrap();
    assert_eq!(out.draw_count, 2);
    assert_eq!(out.pixels, [59, 61, 67, 255].repeat(12));
}

#[test]
fn cpu_unbinding_after_draw_does_not_hide_unsupported_sampled_binding() {
    for (legacy, indexed) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut h = Harness::new();
        if legacy {
            h.ctx
                .set_metal_backend(|_, _, _| panic!("sampled draw reached legacy callback"));
        }
        let texture = h.texture(ResourceStates::PIXEL_SHADER_RESOURCE);
        h.ctx
            .cmd_set_sampled_texture(h.list, Some(texture), SamplerDesc::nearest_clamp())
            .unwrap();
        if indexed {
            h.ctx
                .cmd_set_vertex_buffer(h.list, h.vertices, 0, 16)
                .unwrap();
            h.ctx
                .cmd_set_index_buffer(h.list, h.vertices, 0, 42)
                .unwrap();
            h.ctx
                .cmd_draw_indexed_instanced(h.list, 3, 1, 0, 0, 0)
                .unwrap();
        } else {
            h.draw();
        }
        h.ctx
            .cmd_set_sampled_texture(h.list, None, SamplerDesc::nearest_clamp())
            .unwrap();
        h.ctx.close_command_list(h.list).unwrap();
        assert_eq!(
            h.ctx
                .execute_command_list(h.queue, h.list, &h.memory)
                .unwrap_err(),
            GraphicsError::InvalidDescriptor
        );
        let presentation = h.ctx.last_presentation.lock().unwrap();
        assert_eq!(presentation.frame_index, 0);
        assert!(presentation.pixels.is_empty());
    }
}

#[test]
fn cpu_sampled_texture_cannot_alias_legacy_color_target() {
    let mut h = Harness::new();
    h.baseline();
    let before = h.ctx.last_presentation.lock().unwrap().clone();
    let pipeline = h.pipeline(VARYING, SAMPLED);
    let texture = h.texture(ResourceStates::PIXEL_SHADER_RESOURCE);
    h.ctx
        .set_metal_stateful_backend(|_, _, _, _, _| panic!("aliased texture reached callback"));
    h.ctx
        .cmd_set_graphics_pipeline(h.list, Some(pipeline))
        .unwrap();
    h.ctx
        .cmd_set_render_targets(h.list, Some(texture), [0; 4])
        .unwrap();
    h.ctx.cmd_set_viewport(h.list, 1, 1).unwrap();
    h.ctx
        .cmd_set_sampled_texture(h.list, Some(texture), SamplerDesc::nearest_clamp())
        .unwrap();
    h.draw();
    h.ctx.close_command_list(h.list).unwrap();
    assert!(
        h.ctx
            .execute_command_list(h.queue, h.list, &h.memory)
            .is_err()
    );
    let after = h.ctx.last_presentation.lock().unwrap();
    assert_eq!(after.pixels, before.pixels);
    assert_eq!(
        (after.width, after.height, after.frame_index, after.backend),
        (
            before.width,
            before.height,
            before.frame_index,
            before.backend
        )
    );
}

#[test]
fn cpu_pipeline_target_state_is_checked_without_sampled_aliasing() {
    for indexed in [false, true] {
        let mut h = Harness::new();
        h.baseline();
        let before = h.ctx.last_presentation.lock().unwrap().clone();
        let pipeline = h.pipeline(VS, PS);
        let target = h.texture(ResourceStates::PIXEL_SHADER_RESOURCE);
        h.ctx.set_metal_stateful_backend(|_, _, _, _, _| {
            panic!("wrong target state reached backend")
        });
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        h.ctx
            .cmd_set_render_targets(h.list, Some(target), [0; 4])
            .unwrap();
        h.ctx.cmd_set_viewport(h.list, 1, 1).unwrap();
        h.ctx
            .cmd_set_vertex_buffer(h.list, h.vertices, 0, 16)
            .unwrap();
        if indexed {
            h.ctx
                .cmd_set_index_buffer(h.list, h.vertices, 0, 42)
                .unwrap();
            h.ctx
                .cmd_draw_indexed_instanced(h.list, 3, 1, 0, 0, 0)
                .unwrap();
        } else {
            h.ctx.cmd_draw_instanced(h.list, 3, 1, 0, 0).unwrap();
        }
        h.ctx.close_command_list(h.list).unwrap();
        assert_eq!(
            h.ctx
                .execute_command_list(h.queue, h.list, &h.memory)
                .unwrap_err(),
            GraphicsError::ResourceLifetimeViolation
        );
        assert_eq!(
            h.ctx.last_presentation.lock().unwrap().pixels,
            before.pixels
        );
        assert_eq!(
            h.ctx.last_presentation.lock().unwrap().frame_index,
            before.frame_index
        );
    }
}

#[test]
fn cpu_indexed_colors_honor_offset_stride_base_and_reject_final_fetch() {
    for short in [false, true] {
        let mut h = Harness::new();
        h.baseline();
        let before = h.ctx.last_presentation.lock().unwrap().clone();
        h.memory
            .map_region(0x1000, 88, MemoryPermissions::READ_WRITE)
            .unwrap();
        for (row, color) in [R, G, B].into_iter().enumerate() {
            let bytes: Vec<_> = color.into_iter().flat_map(f32::to_le_bytes).collect();
            h.memory
                .write_bytes(0x1008 + row as u64 * 32, &bytes)
                .unwrap();
        }
        let colors = h
            .ctx
            .create_committed_resource(
                h.device,
                buffer_desc(if short { 72 } else { 88 }),
                ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
                Some(0x1000),
            )
            .unwrap();
        h.memory
            .map_region(0x2000, 20, MemoryPermissions::READ_WRITE)
            .unwrap();
        let indices = if short {
            [99u32, 99, 1, 2, 3]
        } else {
            [99, 99, 3, 1, 2]
        };
        let bytes: Vec<_> = indices.into_iter().flat_map(u32::to_le_bytes).collect();
        h.memory.write_bytes(0x2000, &bytes).unwrap();
        let indices = h
            .ctx
            .create_committed_resource(
                h.device,
                buffer_desc(20),
                ResourceStates::INDEX_BUFFER,
                Some(0x2000),
            )
            .unwrap();
        let pipeline = h.pipeline(VARYING, ARITHMETIC);
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        h.ctx
            .cmd_set_vertex_buffer(h.list, h.vertices, 0, 16)
            .unwrap();
        h.ctx.cmd_set_vertex_colors(h.list, colors, 8, 32).unwrap();
        h.ctx.cmd_set_index_buffer(h.list, indices, 4, 42).unwrap();
        h.ctx
            .cmd_draw_indexed_instanced(h.list, 3, 1, 1, -1, 0)
            .unwrap();
        h.ctx.set_metal_stateful_backend(move |draws, _, _, _, _| {
            assert!(!short, "failed final color fetch reached callback");
            assert_eq!(draws[0].colors, [B, R, G]);
            Ok(GraphicsPassOutput {
                colors: vec![[23, 29, 31, 255].repeat(12)],
                depth: None,
            })
        });
        h.ctx.close_command_list(h.list).unwrap();
        let out = h.ctx.execute_command_list(h.queue, h.list, &h.memory);
        if short {
            assert_eq!(out.unwrap_err(), GraphicsError::IndexOutOfRange);
            assert_eq!(
                h.ctx.last_presentation.lock().unwrap().pixels,
                before.pixels
            );
            assert_eq!(
                h.ctx.last_presentation.lock().unwrap().frame_index,
                before.frame_index
            );
        } else {
            assert_eq!(out.unwrap().pixels, [23, 29, 31, 255].repeat(12));
        }
    }
}

fn assert_cpu_pipeline_switch_snapshots(draws: &[xenora_loader::graphics::GraphicsDraw]) {
    assert_eq!(draws.len(), 3);
    let a = draws[0].pipeline.as_ref().unwrap();
    let b = draws[1].pipeline.as_ref().unwrap();
    let again = draws[2].pipeline.as_ref().unwrap();
    assert!(Arc::ptr_eq(a, again));
    assert!(!Arc::ptr_eq(a, b));
    assert_eq!(
        (a.color_targets, b.color_targets, again.color_targets),
        (1, 1, 1)
    );
    assert!(!a.uses_texture);
    assert!(b.uses_texture);
    assert!(!again.uses_texture);
    assert!(draws[0].sampled.is_none());
    assert_eq!(&*draws[1].sampled.as_ref().unwrap().pixels, &[0, 0, 0, 0]);
    assert!(draws[2].sampled.is_none());
}

struct CpuPipelineSwitchPending(xenora_loader::graphics::GraphicsSubmissionRequest);
impl xenora_loader::graphics::GraphicsPendingSubmission for CpuPipelineSwitchPending {
    fn poll(&mut self) -> Option<Result<GraphicsPassOutput, GraphicsError>> {
        None
    }
    fn wait(&mut self) -> Result<GraphicsPassOutput, GraphicsError> {
        // CPU-only: inspect owned data after the originating handles were destroyed.
        assert_cpu_pipeline_switch_snapshots(&self.0.draws);
        Ok(GraphicsPassOutput {
            colors: vec![[17, 29, 43, 255].repeat(12)],
            depth: None,
        })
    }
}

#[test]
fn cpu_pipeline_switch_a_b_a_snapshots_survive_async_reset_and_destruction() {
    use xenora_loader::graphics::GraphicsSubmissionStatus;
    for asynchronous in [false, true] {
        let mut h = Harness::new();
        let a = h.pipeline(VS, PS);
        let b = h.pipeline(VARYING, SAMPLED);
        let texture = h.texture(ResourceStates::PIXEL_SHADER_RESOURCE);
        for (pipeline, sampled) in [(a, None), (b, Some(texture)), (a, None)] {
            h.ctx
                .cmd_set_graphics_pipeline(h.list, Some(pipeline))
                .unwrap();
            // The nonsampled third pipeline requires explicit sampled unbinding.
            h.ctx
                .cmd_set_sampled_texture(h.list, sampled, SamplerDesc::nearest_clamp())
                .unwrap();
            h.draw();
        }
        h.ctx.close_command_list(h.list).unwrap();
        if asynchronous {
            h.ctx
                .set_metal_async_backend(|request| Ok(Box::new(CpuPipelineSwitchPending(request))));
            let ticket = h
                .ctx
                .submit_command_list(h.queue, h.list, &h.memory)
                .unwrap();
            assert_eq!(
                h.ctx.poll_submission(ticket).unwrap(),
                GraphicsSubmissionStatus::Pending
            );
            h.ctx.reset_command_list(h.list, h.allocator).unwrap();
            h.ctx.destroy_graphics_pipeline(a).unwrap();
            h.ctx.destroy_graphics_pipeline(b).unwrap();
            h.ctx.destroy_resource(texture).unwrap();
            h.ctx.destroy_resource(h.vertices).unwrap();
            assert_eq!(
                h.ctx.wait_submission(ticket).unwrap(),
                GraphicsSubmissionStatus::Complete
            );
            let outcome = h.ctx.submission_outcome(ticket).unwrap().unwrap();
            assert_eq!(outcome.draw_count, 3);
            assert_eq!(outcome.pixels, [17, 29, 43, 255].repeat(12));
            h.ctx.release_submission(ticket).unwrap();
        } else {
            h.ctx.set_metal_stateful_backend(|draws, _, _, _, _| {
                assert_cpu_pipeline_switch_snapshots(draws);
                Ok(GraphicsPassOutput {
                    colors: vec![[17, 29, 43, 255].repeat(12)],
                    depth: None,
                })
            });
            let outcome = h
                .ctx
                .execute_command_list(h.queue, h.list, &h.memory)
                .unwrap();
            assert_eq!(outcome.draw_count, 3);
            assert_eq!(outcome.pixels, [17, 29, 43, 255].repeat(12));
        }
    }
}

#[test]
fn cpu_later_pipeline_metadata_errors_reject_whole_pass_before_dispatch() {
    for asynchronous in [false, true] {
        for case in 0..3 {
            let mut h = Harness::new();
            h.baseline();
            let first = if case == 2 {
                h.pipeline(VARYING, SAMPLED)
            } else {
                h.pipeline(VS, PS)
            };
            let later = match case {
                0 => h.pipeline(VS, MRT),
                1 => h.pipeline(VARYING, SAMPLED),
                _ => h.pipeline(VS, PS),
            };
            h.ctx
                .cmd_set_graphics_pipeline(h.list, Some(first))
                .unwrap();
            if case == 2 {
                let texture = h.texture(ResourceStates::PIXEL_SHADER_RESOURCE);
                h.ctx
                    .cmd_set_sampled_texture(h.list, Some(texture), SamplerDesc::nearest_clamp())
                    .unwrap();
            }
            h.draw();
            h.ctx
                .cmd_set_graphics_pipeline(h.list, Some(later))
                .unwrap();
            // Case 0: incompatible MRT count; case 1: missing required texture;
            // case 2: sampled binding was not explicitly unbound for nonsampled PS.
            h.draw();
            h.ctx.close_command_list(h.list).unwrap();
            if asynchronous {
                h.ctx.set_metal_async_backend(|_| {
                    panic!("invalid later pipeline reached CPU submitter")
                });
                assert_eq!(
                    h.ctx.submit_command_list(h.queue, h.list, &h.memory),
                    Err(GraphicsError::InvalidDescriptor)
                );
            } else {
                h.ctx.set_metal_stateful_backend(|_, _, _, _, _| {
                    panic!("invalid later pipeline reached CPU callback")
                });
                assert_eq!(
                    h.ctx.execute_command_list(h.queue, h.list, &h.memory),
                    Err(GraphicsError::InvalidDescriptor)
                );
            }
            let presentation = h.ctx.last_presentation.lock().unwrap();
            assert_eq!(presentation.frame_index, 1);
            assert_eq!(presentation.pixels, [7, 11, 13, 255].repeat(12));
        }
    }
}

// Original descriptor-field variants of the existing original sample fixture.
// Resource and sampler fields are independently encoded in four-SGPR units.
fn sampled_pairs_shader(pairs: &[(u32, u32)]) -> Vec<u8> {
    let mut code = SAMPLED[..16].to_vec();
    for &(texture, sampler) in pairs {
        code.extend_from_slice(&SAMPLED[16..20]);
        let operand = 0x0000_0402u32 | ((texture * 3) << 16) | ((2 + sampler * 3) << 21);
        code.extend_from_slice(&operand.to_le_bytes());
    }
    code.extend_from_slice(&SAMPLED[24..]);
    code
}

#[test]
fn cpu_multislot_all_pairs_have_exact_independent_snapshots() {
    for (texture_slot, sampler_slot, expected_pairs) in [(0, 0, 1), (0, 1, 2), (1, 0, 4), (1, 1, 8)]
    {
        let mut h = Harness::new();
        let pipeline = h.pipeline(
            VARYING,
            &sampled_pairs_shader(&[(texture_slot, sampler_slot)]),
        );
        let texture = h.texture(ResourceStates::PIXEL_SHADER_RESOURCE);
        h.ctx
            .set_metal_multislot_attachment_backend(move |draws, extent, _, _, _| {
                let draw = &draws[0];
                assert_eq!(
                    draw.pipeline.as_ref().unwrap().sampled_pairs,
                    expected_pairs
                );
                assert_eq!(draw.sampled.is_some(), expected_pairs == 1);
                for slot in 0..2 {
                    assert_eq!(
                        draw.sampled_bindings.textures[slot].is_some(),
                        slot == texture_slot as usize
                    );
                    assert_eq!(
                        draw.sampled_bindings.samplers[slot].is_some(),
                        slot == sampler_slot as usize
                    );
                }
                assert_eq!(
                    &*draw.sampled_bindings.textures[texture_slot as usize]
                        .as_ref()
                        .unwrap()
                        .pixels,
                    &[0, 0, 0, 0]
                );
                Ok(GraphicsPassOutput {
                    colors: vec![vec![0; extent.0 * extent.1 * 4]],
                    depth: None,
                })
            });
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        let mut textures = [None; 2];
        let mut samplers = [None; 2];
        textures[texture_slot as usize] = Some(texture);
        samplers[sampler_slot as usize] = Some(SamplerDesc::nearest_clamp());
        h.ctx
            .cmd_set_sampled_bindings(h.list, textures, samplers)
            .unwrap();
        h.draw();
        h.ctx.close_command_list(h.list).unwrap();
        h.ctx
            .execute_command_list(h.queue, h.list, &h.memory)
            .unwrap();
    }
}

#[test]
fn cpu_multislot_shared_texture_and_legacy_unbind_preserve_slot_one() {
    let mut h = Harness::new();
    let both = h.pipeline(VARYING, &sampled_pairs_shader(&[(0, 0), (1, 1)]));
    let one = h.pipeline(VARYING, &sampled_pairs_shader(&[(1, 1)]));
    let texture = h.texture(ResourceStates::PIXEL_SHADER_RESOURCE);
    h.ctx
        .set_metal_multislot_attachment_backend(|draws, extent, _, _, _| {
            let a = draws[0].sampled_bindings.textures[0].as_ref().unwrap();
            let b = draws[0].sampled_bindings.textures[1].as_ref().unwrap();
            let c = draws[1].sampled_bindings.textures[1].as_ref().unwrap();
            assert!(Arc::ptr_eq(&a.pixels, &b.pixels));
            assert!(Arc::ptr_eq(&b.pixels, &c.pixels));
            assert!(draws[1].sampled_bindings.textures[0].is_none());
            assert!(draws[1].sampled_bindings.samplers[0].is_none());
            Ok(GraphicsPassOutput {
                colors: vec![vec![0; extent.0 * extent.1 * 4]],
                depth: None,
            })
        });
    h.ctx.cmd_set_graphics_pipeline(h.list, Some(both)).unwrap();
    h.ctx
        .cmd_set_sampled_bindings(
            h.list,
            [Some(texture); 2],
            [Some(SamplerDesc::nearest_clamp()); 2],
        )
        .unwrap();
    h.draw();
    h.ctx
        .cmd_set_sampled_texture(h.list, None, SamplerDesc::nearest_clamp())
        .unwrap();
    h.ctx.cmd_set_graphics_pipeline(h.list, Some(one)).unwrap();
    h.draw();
    h.ctx.close_command_list(h.list).unwrap();
    h.ctx
        .execute_command_list(h.queue, h.list, &h.memory)
        .unwrap();
}

#[test]
fn cpu_multislot_legacy_callback_and_missing_binding_fail_before_dispatch() {
    for multislot in [false, true] {
        let mut h = Harness::new();
        let pipeline = h.pipeline(VARYING, &sampled_pairs_shader(&[(1, 1)]));
        let texture = h.texture(ResourceStates::PIXEL_SHADER_RESOURCE);
        if multislot {
            h.ctx
                .set_metal_multislot_attachment_backend(|_, _, _, _, _| {
                    panic!("CPU callback must not run")
                });
        } else {
            h.ctx
                .set_metal_attachment_backend(|_, _, _, _, _| panic!("CPU callback must not run"));
        }
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        h.ctx
            .cmd_set_sampled_bindings(
                h.list,
                [None, Some(texture)],
                [None, (!multislot).then(SamplerDesc::nearest_clamp)],
            )
            .unwrap();
        h.draw();
        h.ctx.close_command_list(h.list).unwrap();
        assert!(
            h.ctx
                .execute_command_list(h.queue, h.list, &h.memory)
                .is_err()
        );
    }
}

#[test]
fn cpu_multislot_superseded_tables_retain_both_textures_until_reset() {
    let mut h = Harness::new();
    let a = h.texture(ResourceStates::PIXEL_SHADER_RESOURCE);
    let b = h.texture(ResourceStates::PIXEL_SHADER_RESOURCE);
    h.ctx
        .cmd_set_sampled_bindings(h.list, [Some(a), Some(b)], [None; 2])
        .unwrap();
    h.ctx
        .cmd_set_sampled_bindings(h.list, [None; 2], [None; 2])
        .unwrap();
    assert!(h.ctx.destroy_resource(a).is_err());
    assert!(h.ctx.destroy_resource(b).is_err());
    h.ctx.reset_command_list(h.list, h.allocator).unwrap();
    h.ctx.destroy_resource(a).unwrap();
    h.ctx.destroy_resource(b).unwrap();
}

#[test]
fn cpu_multislot_checks_only_used_pairs_and_rejects_invalid_lod_state_alias() {
    for invalid in 0..4 {
        let mut h = Harness::new();
        // Cross pairs only: texture0 has mip1; texture1 has only mip0.
        let pipeline = h.pipeline(VARYING, &sampled_pairs_shader(&[(0, 1), (1, 0)]));
        let a = h
            .ctx
            .create_committed_resource(
                h.device,
                ResourceDesc {
                    dimension: ResourceDimension::Texture2D,
                    width: 2,
                    height: 2,
                    depth_or_array_size: 1,
                    mip_levels: 2,
                    format_rgba8: true,
                },
                ResourceStates::PIXEL_SHADER_RESOURCE,
                None,
            )
            .unwrap();
        let b = h.texture(if invalid == 2 {
            ResourceStates::COPY_DEST
        } else if invalid == 3 {
            ResourceStates::RENDER_TARGET
        } else {
            ResourceStates::PIXEL_SHADER_RESOURCE
        });
        h.ctx
            .set_metal_multislot_attachment_backend(move |_, extent, _, _, _| {
                assert_eq!(invalid, 0, "invalid inputs reached CPU callback");
                Ok(GraphicsPassOutput {
                    colors: vec![vec![0; extent.0 * extent.1 * 4]],
                    depth: None,
                })
            });
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        let mip1 = SamplerDesc {
            explicit_mip: Some(1),
            ..SamplerDesc::nearest_clamp()
        };
        h.ctx
            .cmd_set_sampled_bindings(
                h.list,
                [Some(a), Some(b)],
                [
                    Some(if invalid == 1 {
                        mip1
                    } else {
                        SamplerDesc::nearest_clamp()
                    }),
                    Some(mip1),
                ],
            )
            .unwrap();
        if invalid == 3 {
            h.ctx
                .cmd_set_color_targets(h.list, &[b], &[[0; 4]])
                .unwrap();
            h.ctx.cmd_set_viewport(h.list, 1, 1).unwrap();
        }
        h.draw();
        h.ctx.close_command_list(h.list).unwrap();
        assert_eq!(
            h.ctx
                .execute_command_list(h.queue, h.list, &h.memory)
                .is_ok(),
            invalid == 0
        );
    }
}

#[test]
fn cpu_views_share_full_backing_and_validate_relative_mips() {
    use xenora_loader::graphics::{GraphicsTextureViewDesc, GraphicsTextureViewFormat};
    for relative_mip in [0, 1, 2] {
        let mut h = Harness::new();
        let pipeline = h.pipeline(VARYING, &sampled_pairs_shader(&[(0, 0), (1, 1)]));
        let backing = h
            .ctx
            .create_committed_resource(
                h.device,
                ResourceDesc {
                    dimension: ResourceDimension::Texture2D,
                    width: 4,
                    height: 4,
                    depth_or_array_size: 1,
                    mip_levels: 3,
                    format_rgba8: true,
                },
                ResourceStates::COPY_DEST,
                None,
            )
            .unwrap();
        h.memory
            .map_region(0x9000, 84, MemoryPermissions::READ_WRITE)
            .unwrap();
        let bytes = [
            [17, 31, 47, 255].repeat(16),
            [71, 83, 97, 128].repeat(4),
            vec![109, 127, 139, 64],
        ]
        .concat();
        h.memory.write_bytes(0x9000, &bytes).unwrap();
        h.ctx
            .copy_guest_to_resource(backing, 0, 0x9000, 84, &h.memory)
            .unwrap();
        let linear = GraphicsTextureViewDesc {
            base_mip: 1,
            mip_count: 2,
            format: GraphicsTextureViewFormat::Rgba8Unorm,
        };
        let srgb = GraphicsTextureViewDesc {
            format: GraphicsTextureViewFormat::Rgba8UnormSrgb,
            ..linear
        };
        let a = h
            .ctx
            .create_texture_view(h.device, backing, linear)
            .unwrap();
        let b = h.ctx.create_texture_view(h.device, backing, srgb).unwrap();
        h.ctx
            .set_metal_view_attachment_backend(move |draws, extent, _, _, _| {
                assert!(
                    relative_mip < 2,
                    "invalid view-relative mip reached CPU callback"
                );
                let bindings = &draws[0].sampled_bindings;
                assert_eq!(bindings.views, [Some(linear), Some(srgb)]);
                let a = bindings.textures[0].as_ref().unwrap();
                let b = bindings.textures[1].as_ref().unwrap();
                assert_eq!((a.width, a.height, a.mip_levels), (4, 4, 3));
                assert_eq!(a.pixels.as_ref(), bytes.as_slice());
                assert!(Arc::ptr_eq(&a.pixels, &b.pixels));
                assert!(draws[0].sampled.is_none());
                Ok(GraphicsPassOutput {
                    colors: vec![vec![0; extent.0 * extent.1 * 4]],
                    depth: None,
                })
            });
        h.ctx
            .cmd_resource_barrier(
                h.list,
                backing,
                ResourceStates::COPY_DEST,
                ResourceStates::PIXEL_SHADER_RESOURCE,
            )
            .unwrap();
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        h.ctx
            .cmd_set_sampled_views(
                h.list,
                [Some(a), Some(b)],
                [Some(SamplerDesc::nearest_clamp().with_explicit_mip(relative_mip)); 2],
            )
            .unwrap();
        h.draw();
        h.ctx.close_command_list(h.list).unwrap();
        assert_eq!(
            h.ctx
                .execute_command_list(h.queue, h.list, &h.memory)
                .is_ok(),
            relative_mip < 2
        );
    }
}

#[test]
fn cpu_view_lifetime_invalid_ranges_and_atomic_later_binding() {
    use xenora_loader::graphics::{GraphicsTextureViewDesc, GraphicsTextureViewFormat};
    let mut h = Harness::new();
    let backing = h.texture(ResourceStates::PIXEL_SHADER_RESOURCE);
    let desc = GraphicsTextureViewDesc {
        base_mip: 0,
        mip_count: 1,
        format: GraphicsTextureViewFormat::Rgba8Unorm,
    };
    for (base_mip, mip_count) in [(0, 0), (1, 1), (u32::MAX, 2)] {
        assert!(
            h.ctx
                .create_texture_view(
                    h.device,
                    backing,
                    GraphicsTextureViewDesc {
                        base_mip,
                        mip_count,
                        ..desc
                    }
                )
                .is_err()
        );
    }
    let view = h.ctx.create_texture_view(h.device, backing, desc).unwrap();
    assert!(h.ctx.create_texture_view(h.device, view, desc).is_err());
    assert!(h.ctx.destroy_resource(backing).is_err());
    assert!(
        h.ctx
            .cmd_set_sampled_views(h.list, [Some(view), Some(backing)], [None; 2])
            .is_err()
    );
    h.ctx.destroy_texture_view(view).unwrap(); // Invalid second handle recorded nothing.
    let view = h.ctx.create_texture_view(h.device, backing, desc).unwrap();
    h.ctx
        .cmd_set_sampled_views(h.list, [None, Some(view)], [None; 2])
        .unwrap();
    h.ctx
        .cmd_set_sampled_bindings(h.list, [None; 2], [None; 2])
        .unwrap();
    assert!(h.ctx.destroy_texture_view(view).is_err()); // Superseded command still refers.
    h.ctx.reset_command_list(h.list, h.allocator).unwrap();
    h.ctx.destroy_texture_view(view).unwrap();
    h.ctx.destroy_resource(backing).unwrap();
}

#[test]
fn cpu_views_reject_old_callback_and_backing_output_alias() {
    use xenora_loader::graphics::{GraphicsTextureViewDesc, GraphicsTextureViewFormat};
    for alias in [false, true] {
        let mut h = Harness::new();
        let pipeline = h.pipeline(VARYING, SAMPLED);
        let backing = h.texture(if alias {
            ResourceStates::RENDER_TARGET
        } else {
            ResourceStates::PIXEL_SHADER_RESOURCE
        });
        let view = h
            .ctx
            .create_texture_view(
                h.device,
                backing,
                GraphicsTextureViewDesc {
                    base_mip: 0,
                    mip_count: 1,
                    format: GraphicsTextureViewFormat::Rgba8Unorm,
                },
            )
            .unwrap();
        if alias {
            h.ctx.set_metal_view_attachment_backend(|_, _, _, _, _| {
                panic!("CPU callback must not run")
            });
        } else {
            h.ctx
                .set_metal_multislot_attachment_backend(|_, _, _, _, _| {
                    panic!("old callback must not ignore view")
                });
        }
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        h.ctx
            .cmd_set_sampled_views(
                h.list,
                [Some(view), None],
                [Some(SamplerDesc::nearest_clamp()), None],
            )
            .unwrap();
        if alias {
            h.ctx
                .cmd_set_color_targets(h.list, &[backing], &[[0; 4]])
                .unwrap();
            h.ctx.cmd_set_viewport(h.list, 1, 1).unwrap();
        }
        h.draw();
        h.ctx.close_command_list(h.list).unwrap();
        assert!(
            h.ctx
                .execute_command_list(h.queue, h.list, &h.memory)
                .is_err()
        );
    }
}

#[test]
fn cpu_typed_bc_snapshots_keep_original_blocks_and_require_capability() {
    use xenora_loader::graphics::{
        GraphicsTextureCapabilities, GraphicsTextureDesc, GraphicsTextureViewDesc,
        GraphicsTextureViewFormat,
    };
    use xenora_texture::TextureFormat;
    for (format, length, view_format) in [
        (
            TextureFormat::Bc1RgbaUnorm,
            48,
            GraphicsTextureViewFormat::Bc1RgbaUnormSrgb,
        ),
        (
            TextureFormat::Bc3RgbaUnorm,
            96,
            GraphicsTextureViewFormat::Bc3RgbaUnormSrgb,
        ),
    ] {
        for enabled in [false, true] {
            let mut h = Harness::new();
            let pipeline = h.pipeline(VARYING, SAMPLED);
            let texture = h
                .ctx
                .create_texture_resource(
                    h.device,
                    GraphicsTextureDesc {
                        width: 5,
                        height: 5,
                        mip_levels: 3,
                        format,
                    },
                    ResourceStates::COPY_DEST,
                )
                .unwrap();
            // Literal 5x5 chain: four blocks, one block, one block (8/16 bytes each).
            let bytes: Vec<u8> = (0..length).map(|i| (i * 7 + 3) as u8).collect();
            h.memory
                .map_region(0xa000, length, MemoryPermissions::READ_WRITE)
                .unwrap();
            h.memory.write_bytes(0xa000, &bytes).unwrap();
            h.ctx
                .copy_guest_to_resource(texture, 0, 0xa000, length, &h.memory)
                .unwrap();
            let view = h
                .ctx
                .create_texture_view(
                    h.device,
                    texture,
                    GraphicsTextureViewDesc {
                        base_mip: 1,
                        mip_count: 2,
                        format: view_format,
                    },
                )
                .unwrap();
            h.ctx.set_metal_typed_attachment_backend(
                GraphicsTextureCapabilities {
                    bc1: enabled,
                    bc3: enabled,
                },
                move |draws, extent, _, _, _| {
                    assert!(enabled, "unsupported BC reached CPU callback");
                    let draw = &draws[0];
                    let snapshot = draw.sampled_bindings.textures[0].as_ref().unwrap();
                    assert_eq!(snapshot.format, format);
                    assert_eq!(
                        (snapshot.width, snapshot.height, snapshot.mip_levels),
                        (5, 5, 3)
                    );
                    assert_eq!(&*snapshot.pixels, bytes.as_slice());
                    assert!(draw.sampled.is_none());
                    assert_eq!(draw.sampled_bindings.views[0].unwrap().format, view_format);
                    Ok(GraphicsPassOutput {
                        colors: vec![vec![0; extent.0 * extent.1 * 4]],
                        depth: None,
                    })
                },
            );
            h.ctx
                .cmd_resource_barrier(
                    h.list,
                    texture,
                    ResourceStates::COPY_DEST,
                    ResourceStates::PIXEL_SHADER_RESOURCE,
                )
                .unwrap();
            h.ctx
                .cmd_set_graphics_pipeline(h.list, Some(pipeline))
                .unwrap();
            h.ctx
                .cmd_set_sampled_views(
                    h.list,
                    [Some(view), None],
                    [
                        Some(SamplerDesc::nearest_clamp().with_explicit_mip(1)),
                        None,
                    ],
                )
                .unwrap();
            h.draw();
            h.ctx.close_command_list(h.list).unwrap();
            assert_eq!(
                h.ctx
                    .execute_command_list(h.queue, h.list, &h.memory)
                    .is_ok(),
                enabled
            );
        }
    }
}

#[test]
fn cpu_typed_bc_rejects_attachment_incompatible_view_and_invalid_shape() {
    use xenora_loader::graphics::{
        GraphicsTextureDesc, GraphicsTextureViewDesc, GraphicsTextureViewFormat,
    };
    use xenora_texture::TextureFormat;
    let mut h = Harness::new();
    let desc = GraphicsTextureDesc {
        width: 4,
        height: 4,
        mip_levels: 1,
        format: TextureFormat::Bc1RgbaUnorm,
    };
    for bad in [
        GraphicsTextureDesc { width: 0, ..desc },
        GraphicsTextureDesc {
            mip_levels: 4,
            ..desc
        },
        GraphicsTextureDesc {
            width: usize::MAX,
            ..desc
        },
    ] {
        assert!(
            h.ctx
                .create_texture_resource(h.device, bad, ResourceStates::COPY_DEST)
                .is_err()
        );
    }
    assert!(
        h.ctx
            .create_texture_resource(h.device, desc, ResourceStates::RENDER_TARGET)
            .is_err()
    );
    let texture = h
        .ctx
        .create_texture_resource(h.device, desc, ResourceStates::PIXEL_SHADER_RESOURCE)
        .unwrap();
    for format in [
        GraphicsTextureViewFormat::Rgba8Unorm,
        GraphicsTextureViewFormat::Bc3RgbaUnorm,
    ] {
        assert!(
            h.ctx
                .create_texture_view(
                    h.device,
                    texture,
                    GraphicsTextureViewDesc {
                        base_mip: 0,
                        mip_count: 1,
                        format
                    }
                )
                .is_err()
        );
    }
    assert!(
        h.ctx
            .cmd_set_color_targets(h.list, &[texture], &[[0; 4]])
            .is_err()
    );
}

#[test]
fn cpu_typed_capabilities_are_independent_and_old_installers_reset_them() {
    use xenora_loader::graphics::{GraphicsTextureCapabilities, GraphicsTextureDesc};
    use xenora_texture::TextureFormat;
    for (format, bc1, bc3, reset, succeeds) in [
        (TextureFormat::Bc1RgbaUnorm, true, false, false, true),
        (TextureFormat::Bc3RgbaUnorm, true, false, false, false),
        (TextureFormat::Bc3RgbaUnorm, false, true, false, true),
        (TextureFormat::Bc1RgbaUnorm, false, true, false, false),
        (TextureFormat::Bc1RgbaUnorm, true, true, true, false),
    ] {
        let mut h = Harness::new();
        let pipeline = h.pipeline(VARYING, SAMPLED);
        let texture = h
            .ctx
            .create_texture_resource(
                h.device,
                GraphicsTextureDesc {
                    width: 4,
                    height: 4,
                    mip_levels: 1,
                    format,
                },
                ResourceStates::PIXEL_SHADER_RESOURCE,
            )
            .unwrap();
        h.ctx.set_metal_typed_attachment_backend(
            GraphicsTextureCapabilities { bc1, bc3 },
            move |draws, extent, _, _, _| {
                assert!(succeeds);
                assert!(draws[0].sampled.is_none());
                assert_eq!(
                    draws[0].sampled_bindings.textures[0]
                        .as_ref()
                        .unwrap()
                        .format,
                    format
                );
                Ok(GraphicsPassOutput {
                    colors: vec![vec![0; extent.0 * extent.1 * 4]],
                    depth: None,
                })
            },
        );
        if reset {
            h.ctx.set_metal_view_attachment_backend(|_, _, _, _, _| {
                panic!("old installer must reset BC capability")
            });
        }
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        h.ctx
            .cmd_set_sampled_texture(h.list, Some(texture), SamplerDesc::nearest_clamp())
            .unwrap();
        h.draw();
        h.ctx.close_command_list(h.list).unwrap();
        assert_eq!(
            h.ctx
                .execute_command_list(h.queue, h.list, &h.memory)
                .is_ok(),
            succeeds
        );
    }
}

#[test]
fn cpu_legacy_bc_targets_reject_clear_only_and_draw_before_dispatch_preserving_frame() {
    use xenora_loader::graphics::{GraphicsTextureCapabilities, GraphicsTextureDesc};
    use xenora_texture::TextureFormat;
    for format in [TextureFormat::Bc1RgbaUnorm, TextureFormat::Bc3RgbaUnorm] {
        for draw in [false, true] {
            let mut h = Harness::new();
            h.baseline();
            // One compressed block is larger than a 1x1 RGBA output: a byte-size
            // check cannot accidentally stand in for the required format check.
            let target = h
                .ctx
                .create_texture_resource(
                    h.device,
                    GraphicsTextureDesc {
                        width: 1,
                        height: 1,
                        mip_levels: 1,
                        format,
                    },
                    ResourceStates::PIXEL_SHADER_RESOURCE,
                )
                .unwrap();
            h.ctx.set_metal_typed_attachment_backend(
                GraphicsTextureCapabilities {
                    bc1: true,
                    bc3: true,
                },
                |_, _, _, _, _| panic!("BC attachment reached CPU callback"),
            );
            h.ctx
                .cmd_resource_barrier(
                    h.list,
                    target,
                    ResourceStates::PIXEL_SHADER_RESOURCE,
                    ResourceStates::RENDER_TARGET,
                )
                .unwrap();
            h.ctx
                .cmd_set_render_targets(h.list, Some(target), [101, 103, 107, 109])
                .unwrap();
            h.ctx.cmd_set_viewport(h.list, 1, 1).unwrap();
            if draw {
                h.draw();
            }
            h.ctx.close_command_list(h.list).unwrap();
            assert!(
                h.ctx
                    .execute_command_list(h.queue, h.list, &h.memory)
                    .is_err()
            );
            {
                let presentation = h.ctx.last_presentation.lock().unwrap();
                assert_eq!(presentation.frame_index, 1);
                assert_eq!((presentation.width, presentation.height), (4, 3));
                assert_eq!(presentation.pixels, [7, 11, 13, 255].repeat(12));
            }
            // A fresh barrier must still see the pre-failure state.
            h.ctx.reset_command_list(h.list, h.allocator).unwrap();
            h.ctx
                .cmd_resource_barrier(
                    h.list,
                    target,
                    ResourceStates::PIXEL_SHADER_RESOURCE,
                    ResourceStates::COPY_SOURCE,
                )
                .unwrap();
            h.ctx.close_command_list(h.list).unwrap();
            h.ctx.set_metal_attachment_backend(|_, extent, _, _, _| {
                Ok(GraphicsPassOutput {
                    colors: vec![vec![0; extent.0 * extent.1 * 4]],
                    depth: None,
                })
            });
            h.ctx
                .execute_command_list(h.queue, h.list, &h.memory)
                .unwrap();
        }
    }
}

#[test]
fn cpu_old_async_installers_reset_typed_bc_capabilities_before_submit() {
    use xenora_loader::graphics::{GraphicsTextureCapabilities, GraphicsTextureDesc};
    use xenora_texture::TextureFormat;
    for format in [TextureFormat::Bc1RgbaUnorm, TextureFormat::Bc3RgbaUnorm] {
        for installer in 0..3 {
            let mut h = Harness::new();
            h.baseline();
            let pipeline = h.pipeline(VARYING, SAMPLED);
            let texture = h
                .ctx
                .create_texture_resource(
                    h.device,
                    GraphicsTextureDesc {
                        width: 1,
                        height: 1,
                        mip_levels: 1,
                        format,
                    },
                    ResourceStates::PIXEL_SHADER_RESOURCE,
                )
                .unwrap();
            h.ctx.set_metal_typed_async_backend(
                GraphicsTextureCapabilities {
                    bc1: true,
                    bc3: true,
                },
                |_| panic!("replaced typed CPU submitter called"),
            );
            match installer {
                0 => h
                    .ctx
                    .set_metal_async_backend(|_| panic!("old async submitter received BC")),
                1 => h.ctx.set_metal_multislot_async_backend(|_| {
                    panic!("multislot async submitter received BC")
                }),
                _ => h
                    .ctx
                    .set_metal_view_async_backend(|_| panic!("view async submitter received BC")),
            }
            h.ctx
                .cmd_set_graphics_pipeline(h.list, Some(pipeline))
                .unwrap();
            h.ctx
                .cmd_set_sampled_texture(h.list, Some(texture), SamplerDesc::nearest_clamp())
                .unwrap();
            h.draw();
            h.ctx.close_command_list(h.list).unwrap();
            assert!(
                h.ctx
                    .submit_command_list(h.queue, h.list, &h.memory)
                    .is_err()
            );
            let presentation = h.ctx.last_presentation.lock().unwrap();
            assert_eq!(presentation.frame_index, 1);
            assert_eq!(presentation.pixels, [7, 11, 13, 255].repeat(12));
        }
    }
}

#[test]
fn cpu_old_queued_installers_reset_typed_bc_capabilities_before_submit() {
    use xenora_loader::graphics::{GraphicsTextureCapabilities, GraphicsTextureDesc};
    use xenora_texture::TextureFormat;
    for format in [TextureFormat::Bc1RgbaUnorm, TextureFormat::Bc3RgbaUnorm] {
        for view_installer in [false, true] {
            let mut h = Harness::new();
            h.baseline();
            let pipeline = h.pipeline(VARYING, SAMPLED);
            let texture = h
                .ctx
                .create_texture_resource(
                    h.device,
                    GraphicsTextureDesc {
                        width: 1,
                        height: 1,
                        mip_levels: 1,
                        format,
                    },
                    ResourceStates::PIXEL_SHADER_RESOURCE,
                )
                .unwrap();
            h.ctx.set_metal_typed_queued_backend(
                GraphicsTextureCapabilities {
                    bc1: true,
                    bc3: true,
                },
                |_| panic!("replaced typed CPU submitter called"),
            );
            if view_installer {
                h.ctx
                    .set_metal_view_queued_backend(|_| panic!("view queued submitter received BC"));
            } else {
                h.ctx
                    .set_metal_queued_backend(|_| panic!("old queued submitter received BC"));
            }
            h.ctx
                .cmd_set_graphics_pipeline(h.list, Some(pipeline))
                .unwrap();
            h.ctx
                .cmd_set_sampled_texture(h.list, Some(texture), SamplerDesc::nearest_clamp())
                .unwrap();
            h.draw();
            h.ctx.close_command_list(h.list).unwrap();
            assert!(
                h.ctx
                    .submit_command_list_after(h.queue, h.list, &[], &h.memory)
                    .is_err()
            );
            let presentation = h.ctx.last_presentation.lock().unwrap();
            assert_eq!(presentation.frame_index, 1);
            assert_eq!(presentation.pixels, [7, 11, 13, 255].repeat(12));
        }
    }
}

#[test]
fn cpu_runtime_uniform_snapshots_are_per_draw_and_missing_later_data_is_atomic() {
    use xenora_loader::graphics::GraphicsTextureCapabilities;
    for missing_last in [false, true] {
        let mut h = Harness::new();
        h.baseline();
        let before = h.ctx.last_presentation.lock().unwrap().clone();
        let pipeline = h.pipeline(
            include_bytes!("../../../fixtures/rdna2/x9-runtime-uniform-vs.bin"),
            include_bytes!("../../../fixtures/rdna2/x9-runtime-uniform-ps.bin"),
        );
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        for value in [
            Some([0; 8]),
            Some([1; 8]),
            if missing_last { None } else { Some([0; 8]) },
        ] {
            h.ctx.cmd_set_shader_uniforms(h.list, value).unwrap();
            h.draw();
        }
        h.ctx.set_metal_uniform_attachment_backend(
            GraphicsTextureCapabilities::default(),
            move |draws, extent, _, _, _| {
                assert!(
                    !missing_last,
                    "missing later uniform dispatched a partial batch"
                );
                assert_eq!(
                    draws.iter().map(|d| d.uniforms).collect::<Vec<_>>(),
                    vec![Some([0; 8]), Some([1; 8]), Some([0; 8])]
                );
                assert!(Arc::ptr_eq(
                    draws[0].pipeline.as_ref().unwrap(),
                    draws[2].pipeline.as_ref().unwrap()
                ));
                assert_eq!(draws[0].pipeline.as_ref().unwrap().required_uniform_mask, 3);
                Ok(GraphicsPassOutput {
                    colors: vec![[41, 43, 47, 255].repeat(extent.0 * extent.1)],
                    depth: None,
                })
            },
        );
        h.ctx.close_command_list(h.list).unwrap();
        let result = h.ctx.execute_command_list(h.queue, h.list, &h.memory);
        if missing_last {
            assert_eq!(result.unwrap_err(), GraphicsError::InvalidDescriptor);
            let after = h.ctx.last_presentation.lock().unwrap();
            assert_eq!(after.pixels, before.pixels);
            assert_eq!(after.frame_index, before.frame_index);
        } else {
            assert_eq!(result.unwrap().pixels, [41, 43, 47, 255].repeat(12));
        }
    }
}

#[test]
fn cpu_runtime_uniforms_reset_with_list_and_old_backend_rejects_even_explicit_zeroes() {
    use xenora_loader::graphics::GraphicsTextureCapabilities;
    for old_backend in [false, true] {
        let mut h = Harness::new();
        let pipeline = h.pipeline(
            include_bytes!("../../../fixtures/rdna2/x9-runtime-uniform-vs.bin"),
            include_bytes!("../../../fixtures/rdna2/x9-runtime-uniform-ps.bin"),
        );
        h.ctx.set_metal_uniform_attachment_backend(
            GraphicsTextureCapabilities::default(),
            |_, _, _, _, _| panic!("must reject"),
        );
        h.ctx.cmd_set_shader_uniforms(h.list, Some([0; 8])).unwrap();
        if old_backend {
            h.ctx.set_metal_typed_attachment_backend(
                GraphicsTextureCapabilities::default(),
                |_, _, _, _, _| panic!("old backend dispatched"),
            );
        } else {
            h.ctx.reset_command_list(h.list, h.allocator).unwrap();
        }
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        h.draw();
        h.ctx.close_command_list(h.list).unwrap();
        assert_eq!(
            h.ctx
                .execute_command_list(h.queue, h.list, &h.memory)
                .unwrap_err(),
            GraphicsError::InvalidDescriptor
        );
    }
}

#[test]
fn cpu_uniform_missing_binding_is_ignored_by_empty_draws_before_valid_draw() {
    use xenora_loader::graphics::GraphicsTextureCapabilities;
    for (indexed, zero_instances) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut h = Harness::new();
        let pipeline = h.pipeline(
            include_bytes!("../../../fixtures/rdna2/x9-runtime-uniform-vs.bin"),
            include_bytes!("../../../fixtures/rdna2/x9-runtime-uniform-ps.bin"),
        );
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        // No uniforms, VB or IB are bound for this no-op.
        let (count, instances) = if zero_instances { (3, 0) } else { (0, 1) };
        if indexed {
            h.ctx
                .cmd_draw_indexed_instanced(h.list, count, instances, 0, 0, 0)
                .unwrap();
        } else {
            h.ctx
                .cmd_draw_instanced(h.list, count, instances, 0, 0)
                .unwrap();
        }
        h.ctx.cmd_set_shader_uniforms(h.list, Some([0; 8])).unwrap();
        h.draw();
        h.ctx.set_metal_uniform_attachment_backend(
            GraphicsTextureCapabilities::default(),
            |draws, extent, _, _, _| {
                // CPU-only callback: exactly the nonempty draw reaches dispatch.
                assert_eq!(draws.len(), 1);
                assert_eq!(draws[0].uniforms, Some([0; 8]));
                assert_eq!(draws[0].vertices.len(), 3);
                Ok(GraphicsPassOutput {
                    colors: vec![[19, 23, 29, 255].repeat(extent.0 * extent.1)],
                    depth: None,
                })
            },
        );
        h.ctx.close_command_list(h.list).unwrap();
        assert_eq!(
            h.ctx
                .execute_command_list(h.queue, h.list, &h.memory)
                .unwrap()
                .pixels,
            [19, 23, 29, 255].repeat(12)
        );
    }
}

struct CpuUniformReady(Option<GraphicsPassOutput>);
impl xenora_loader::graphics::GraphicsPendingSubmission for CpuUniformReady {
    fn poll(&mut self) -> Option<Result<GraphicsPassOutput, GraphicsError>> {
        self.0.take().map(Ok)
    }
    fn wait(&mut self) -> Result<GraphicsPassOutput, GraphicsError> {
        self.poll().expect("CPU-only completion consumed once")
    }
}

#[test]
fn cpu_async_uniform_downgrades_and_missing_later_draw_do_not_allocate_receipts() {
    use xenora_loader::graphics::{GraphicsSubmissionStatus, GraphicsTextureCapabilities};
    for mode in 0..5 {
        let mut h = Harness::new();
        h.baseline();
        let before = h.ctx.last_presentation.lock().unwrap().clone();
        let pipeline = h.pipeline(
            include_bytes!("../../../fixtures/rdna2/x9-runtime-uniform-vs.bin"),
            include_bytes!("../../../fixtures/rdna2/x9-runtime-uniform-ps.bin"),
        );
        h.ctx
            .set_metal_uniform_async_backend(GraphicsTextureCapabilities::default(), |_| {
                panic!("CPU-only: invalid batch reached uniform async backend")
            });
        match mode {
            0 => {} // Valid first draw, missing binding on the later draw.
            1 => h
                .ctx
                .set_metal_async_backend(|_| panic!("downgraded async backend called")),
            2 => h.ctx.set_metal_multislot_async_backend(|_| {
                panic!("downgraded multislot backend called")
            }),
            3 => h
                .ctx
                .set_metal_view_async_backend(|_| panic!("downgraded view backend called")),
            _ => h
                .ctx
                .set_metal_typed_async_backend(GraphicsTextureCapabilities::default(), |_| {
                    panic!("downgraded typed backend called")
                }),
        }
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        h.ctx.cmd_set_shader_uniforms(h.list, Some([0; 8])).unwrap();
        h.draw();
        if mode == 0 {
            h.ctx.cmd_set_shader_uniforms(h.list, None).unwrap();
            h.draw();
        }
        h.ctx.close_command_list(h.list).unwrap();
        let marker = h.ctx.create_device().unwrap();
        assert_eq!(
            h.ctx
                .submit_command_list(h.queue, h.list, &h.memory)
                .unwrap_err(),
            GraphicsError::InvalidDescriptor
        );
        assert_eq!(
            h.ctx.create_device().unwrap().id,
            marker.id + 1,
            "rejected submission allocated a receipt handle"
        );
        let after = h.ctx.last_presentation.lock().unwrap().clone();
        assert_eq!(after.frame_index, before.frame_index);
        assert_eq!(after.pixels, before.pixels);
        // A valid follow-up proves no hidden pending/unreleased receipt remains.
        h.ctx.reset_command_list(h.list, h.allocator).unwrap();
        h.ctx
            .cmd_set_graphics_pipeline(h.list, Some(pipeline))
            .unwrap();
        h.ctx.cmd_set_shader_uniforms(h.list, Some([7; 8])).unwrap();
        h.draw();
        h.ctx.close_command_list(h.list).unwrap();
        h.ctx
            .set_metal_uniform_async_backend(GraphicsTextureCapabilities::default(), |request| {
                assert_eq!(request.draws.len(), 1);
                assert_eq!(request.draws[0].uniforms, Some([7; 8]));
                Ok(Box::new(CpuUniformReady(Some(GraphicsPassOutput {
                    colors: vec![[31, 37, 41, 255].repeat(request.extent.0 * request.extent.1)],
                    depth: None,
                }))))
            });
        let receipt = h
            .ctx
            .submit_command_list(h.queue, h.list, &h.memory)
            .unwrap();
        assert_eq!(
            h.ctx.wait_submission(receipt).unwrap(),
            GraphicsSubmissionStatus::Complete
        );
        h.ctx.release_submission(receipt).unwrap();
        assert_eq!(
            h.ctx.last_presentation.lock().unwrap().pixels,
            [31, 37, 41, 255].repeat(12)
        );
    }
}

#[test]
fn cpu_runtime_sampling_requires_both_feasible_pairs_for_every_uniform_value() {
    use xenora_loader::graphics::GraphicsTextureCapabilities;
    // missing0/1: texture slot0/1; missing2/3: sampler slot0/1; 4: all present.
    // Test both branch selections, including when the absent binding belongs to
    // the arm that the current runtime value would not execute.
    for selector in [0u32, 1] {
        for missing in 0..5 {
            let mut h = Harness::new();
            h.baseline();
            let before = h.ctx.last_presentation.lock().unwrap().clone();
            let pipeline = h.pipeline(
                VARYING,
                include_bytes!("../../../fixtures/rdna2/x9-runtime-sampled-ps.bin"),
            );
            let a = h.texture(ResourceStates::COPY_DEST);
            let b = h.texture(ResourceStates::COPY_DEST);
            let texels = [[17, 31, 47, 63], [71, 89, 101, 127]];
            for (slot, texture) in [a, b].into_iter().enumerate() {
                let address = 0xa000 + slot as u64 * 0x1000;
                h.memory
                    .map_region_with_data(address, 4, MemoryPermissions::READ_ONLY, &texels[slot])
                    .unwrap();
                h.ctx
                    .copy_guest_to_resource(texture, 0, address, 4, &h.memory)
                    .unwrap();
                h.ctx
                    .cmd_resource_barrier(
                        h.list,
                        texture,
                        ResourceStates::COPY_DEST,
                        ResourceStates::PIXEL_SHADER_RESOURCE,
                    )
                    .unwrap();
            }
            let mut textures = [Some(a), Some(b)];
            let mut samplers = [Some(SamplerDesc::nearest_clamp()); 2];
            if missing < 2 {
                textures[missing] = None;
            } else if missing < 4 {
                samplers[missing - 2] = None;
            }
            let mut uniforms = [0; 8];
            uniforms[0] = selector;
            h.ctx
                .cmd_set_graphics_pipeline(h.list, Some(pipeline))
                .unwrap();
            h.ctx
                .cmd_set_shader_uniforms(h.list, Some(uniforms))
                .unwrap();
            h.ctx
                .cmd_set_sampled_bindings(h.list, textures, samplers)
                .unwrap();
            h.draw();
            h.ctx.set_metal_uniform_attachment_backend(
                GraphicsTextureCapabilities::default(),
                move |draws, extent, _, _, _| {
                    // CPU-only contract check; this does not simulate texture sampling.
                    assert_eq!(
                        missing, 4,
                        "incomplete runtime resource union reached callback"
                    );
                    assert_eq!(draws.len(), 1);
                    let draw = &draws[0];
                    assert_eq!(draw.pipeline.as_ref().unwrap().sampled_pairs, 0x09);
                    assert_eq!(draw.pipeline.as_ref().unwrap().required_uniform_mask, 1);
                    assert_eq!(draw.uniforms, Some(uniforms));
                    assert!(
                        draw.sampled.is_none(),
                        "multislot draw has no legacy slot0 projection"
                    );
                    for (slot, expected) in texels.iter().enumerate() {
                        let snapshot = draw.sampled_bindings.textures[slot].as_ref().unwrap();
                        assert_eq!(
                            (snapshot.width, snapshot.height, snapshot.mip_levels),
                            (1, 1, 1)
                        );
                        assert_eq!(&*snapshot.pixels, expected);
                        assert_eq!(
                            draw.sampled_bindings.samplers[slot],
                            Some(SamplerDesc::nearest_clamp())
                        );
                    }
                    Ok(GraphicsPassOutput {
                        colors: vec![[131, 137, 139, 149].repeat(extent.0 * extent.1)],
                        depth: None,
                    })
                },
            );
            h.ctx.close_command_list(h.list).unwrap();
            let result = h.ctx.execute_command_list(h.queue, h.list, &h.memory);
            if missing == 4 {
                assert_eq!(result.unwrap().pixels, [131, 137, 139, 149].repeat(12));
                assert_eq!(
                    h.ctx.last_presentation.lock().unwrap().frame_index,
                    before.frame_index + 1
                );
            } else {
                assert_eq!(result.unwrap_err(), GraphicsError::InvalidDescriptor);
                let after = h.ctx.last_presentation.lock().unwrap();
                assert_eq!(after.pixels, before.pixels);
                assert_eq!(after.frame_index, before.frame_index);
            }
        }
    }
}

#[test]
fn cpu_nested_views_normalize_relative_ranges_and_survive_parent_destruction() {
    use xenora_loader::graphics::{GraphicsTextureViewDesc, GraphicsTextureViewFormat};
    let mut h = Harness::new();
    let root = h
        .ctx
        .create_committed_resource(
            h.device,
            ResourceDesc {
                dimension: ResourceDimension::Texture2D,
                width: 9,
                height: 5,
                depth_or_array_size: 1,
                mip_levels: 4,
                format_rgba8: true,
            },
            ResourceStates::PIXEL_SHADER_RESOURCE,
            None,
        )
        .unwrap();
    let desc = GraphicsTextureViewDesc {
        base_mip: 1,
        mip_count: 3,
        format: GraphicsTextureViewFormat::Rgba8UnormSrgb,
    };
    let parent = h.ctx.create_texture_view(h.device, root, desc).unwrap();
    let child = h
        .ctx
        .create_child_texture_view(
            h.device,
            parent,
            GraphicsTextureViewDesc {
                base_mip: 1,
                mip_count: 2,
                ..desc
            },
        )
        .unwrap();
    let leaf = h
        .ctx
        .create_child_texture_view(
            h.device,
            child,
            GraphicsTextureViewDesc {
                base_mip: 1,
                mip_count: 1,
                format: GraphicsTextureViewFormat::Rgba8Unorm,
            },
        )
        .unwrap();
    h.ctx.destroy_texture_view(parent).unwrap();
    h.ctx.destroy_texture_view(child).unwrap();
    assert!(h.ctx.destroy_resource(root).is_err());
    let pipeline = h.pipeline(VARYING, SAMPLED);
    let target = h.texture(ResourceStates::RENDER_TARGET);
    h.ctx
        .set_metal_view_attachment_backend(|draws, _, _, _, _| {
            // CPU-only descriptor oracle: no rendering or GPU validation is claimed.
            let binding = &draws[0].sampled_bindings;
            assert_eq!(
                binding.views[0],
                Some(GraphicsTextureViewDesc {
                    base_mip: 3,
                    mip_count: 1,
                    format: GraphicsTextureViewFormat::Rgba8Unorm
                })
            );
            let backing = binding.textures[0].as_ref().unwrap();
            assert_eq!(
                (
                    backing.width,
                    backing.height,
                    backing.mip_levels,
                    backing.pixels.len()
                ),
                (9, 5, 4, 224)
            );
            Ok(GraphicsPassOutput {
                colors: vec![vec![17, 31, 47, 255]],
                depth: None,
            })
        });
    h.ctx
        .cmd_set_graphics_pipeline(h.list, Some(pipeline))
        .unwrap();
    h.ctx
        .cmd_set_color_targets(h.list, &[target], &[[0; 4]])
        .unwrap();
    h.ctx.cmd_set_viewport(h.list, 1, 1).unwrap();
    h.ctx
        .cmd_set_sampled_views(
            h.list,
            [Some(leaf), None],
            [
                Some(SamplerDesc::nearest_clamp().with_explicit_mip(0)),
                None,
            ],
        )
        .unwrap();
    h.draw();
    h.ctx.close_command_list(h.list).unwrap();
    assert!(h.ctx.destroy_texture_view(leaf).is_err());
    h.ctx
        .execute_command_list(h.queue, h.list, &h.memory)
        .unwrap();
    h.ctx.reset_command_list(h.list, h.allocator).unwrap();
    h.ctx.destroy_texture_view(leaf).unwrap();
    h.ctx.destroy_resource(root).unwrap();
}

#[test]
fn cpu_nested_view_ranges_formats_and_superseded_references_are_checked() {
    use xenora_loader::graphics::{GraphicsTextureViewDesc, GraphicsTextureViewFormat};
    let mut h = Harness::new();
    let root = h
        .ctx
        .create_committed_resource(
            h.device,
            ResourceDesc {
                dimension: ResourceDimension::Texture2D,
                width: 8,
                height: 8,
                depth_or_array_size: 1,
                mip_levels: 4,
                format_rgba8: true,
            },
            ResourceStates::PIXEL_SHADER_RESOURCE,
            None,
        )
        .unwrap();
    let desc = GraphicsTextureViewDesc {
        base_mip: 1,
        mip_count: 2,
        format: GraphicsTextureViewFormat::Rgba8Unorm,
    };
    let parent = h.ctx.create_texture_view(h.device, root, desc).unwrap();
    for (base_mip, mip_count) in [(0, 0), (2, 1), (1, 2), (u32::MAX, 1), (1, u32::MAX)] {
        assert!(
            h.ctx
                .create_child_texture_view(
                    h.device,
                    parent,
                    GraphicsTextureViewDesc {
                        base_mip,
                        mip_count,
                        ..desc
                    }
                )
                .is_err()
        );
    }
    assert!(
        h.ctx
            .create_child_texture_view(h.device, root, desc)
            .is_err()
    );
    assert!(
        h.ctx
            .create_child_texture_view(
                h.device,
                parent,
                GraphicsTextureViewDesc {
                    base_mip: 0,
                    mip_count: 1,
                    format: GraphicsTextureViewFormat::Bc1RgbaUnorm
                }
            )
            .is_err()
    );
    let child = h
        .ctx
        .create_child_texture_view(
            h.device,
            parent,
            GraphicsTextureViewDesc {
                base_mip: 1,
                mip_count: 1,
                ..desc
            },
        )
        .unwrap();
    h.ctx
        .cmd_set_sampled_views(h.list, [Some(child), None], [None; 2])
        .unwrap();
    h.ctx
        .cmd_set_sampled_views(h.list, [None; 2], [None; 2])
        .unwrap();
    h.ctx.destroy_texture_view(parent).unwrap();
    assert!(h.ctx.destroy_texture_view(child).is_err());
    assert!(h.ctx.destroy_resource(root).is_err());
    h.ctx.reset_command_list(h.list, h.allocator).unwrap();
    h.ctx.destroy_texture_view(child).unwrap();
    h.ctx.destroy_resource(root).unwrap();
}

#[test]
fn cpu_nested_view_cannot_hide_sampled_output_backing_alias() {
    use xenora_loader::graphics::{GraphicsTextureViewDesc, GraphicsTextureViewFormat};
    let mut h = Harness::new();
    let root = h.texture(ResourceStates::RENDER_TARGET);
    let desc = GraphicsTextureViewDesc {
        base_mip: 0,
        mip_count: 1,
        format: GraphicsTextureViewFormat::Rgba8Unorm,
    };
    let parent = h.ctx.create_texture_view(h.device, root, desc).unwrap();
    let child = h
        .ctx
        .create_child_texture_view(h.device, parent, desc)
        .unwrap();
    h.ctx.destroy_texture_view(parent).unwrap();
    let pipeline = h.pipeline(VARYING, SAMPLED);
    h.ctx.set_metal_view_attachment_backend(|_, _, _, _, _| {
        panic!("aliased child view must reject before CPU callback")
    });
    h.ctx
        .cmd_set_graphics_pipeline(h.list, Some(pipeline))
        .unwrap();
    h.ctx
        .cmd_set_color_targets(h.list, &[root], &[[0; 4]])
        .unwrap();
    h.ctx.cmd_set_viewport(h.list, 1, 1).unwrap();
    h.ctx
        .cmd_set_sampled_views(
            h.list,
            [Some(child), None],
            [Some(SamplerDesc::nearest_clamp()), None],
        )
        .unwrap();
    h.draw();
    h.ctx.close_command_list(h.list).unwrap();
    assert!(
        h.ctx
            .execute_command_list(h.queue, h.list, &h.memory)
            .is_err()
    );
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
}

struct CpuSamplerSnapshotPending(xenora_loader::graphics::GraphicsSubmissionRequest);
impl xenora_loader::graphics::GraphicsPendingSubmission for CpuSamplerSnapshotPending {
    fn poll(&mut self) -> Option<Result<GraphicsPassOutput, GraphicsError>> {
        None
    }
    fn wait(&mut self) -> Result<GraphicsPassOutput, GraphicsError> {
        assert_eq!(self.0.draws.len(), 2);
        for (draw, mip) in self.0.draws.iter().zip([0, 1]) {
            assert_eq!(
                draw.sampled_bindings.samplers[0].unwrap().explicit_mip,
                Some(mip)
            );
            assert!(draw.sampled_bindings.samplers[1].is_none());
        }
        Ok(GraphicsPassOutput {
            colors: vec![vec![23, 41, 59, 255]],
            depth: None,
        })
    }
}

#[test]
fn cpu_sampler_handles_snapshot_per_draw_and_outlive_destroy_reset() {
    use xenora_loader::graphics::{
        GraphicsSubmissionStatus, GraphicsTextureViewDesc, GraphicsTextureViewFormat,
    };
    let mut h = Harness::new();
    let root = h
        .ctx
        .create_committed_resource(
            h.device,
            ResourceDesc {
                dimension: ResourceDimension::Texture2D,
                width: 2,
                height: 2,
                depth_or_array_size: 1,
                mip_levels: 2,
                format_rgba8: true,
            },
            ResourceStates::PIXEL_SHADER_RESOURCE,
            None,
        )
        .unwrap();
    let view = h
        .ctx
        .create_texture_view(
            h.device,
            root,
            GraphicsTextureViewDesc {
                base_mip: 0,
                mip_count: 2,
                format: GraphicsTextureViewFormat::Rgba8Unorm,
            },
        )
        .unwrap();
    let pipeline = h.pipeline(VARYING, SAMPLED);
    let target = h.texture(ResourceStates::RENDER_TARGET);
    h.ctx
        .set_metal_view_async_backend(|request| Ok(Box::new(CpuSamplerSnapshotPending(request))));
    h.ctx
        .cmd_set_graphics_pipeline(h.list, Some(pipeline))
        .unwrap();
    h.ctx
        .cmd_set_color_targets(h.list, &[target], &[[0; 4]])
        .unwrap();
    h.ctx.cmd_set_viewport(h.list, 1, 1).unwrap();
    let first = h
        .ctx
        .create_sampler(h.device, SamplerDesc::nearest_clamp().with_explicit_mip(0))
        .unwrap();
    let second = h
        .ctx
        .create_sampler(h.device, SamplerDesc::nearest_clamp().with_explicit_mip(1))
        .unwrap();
    h.ctx
        .cmd_set_sampled_view_handles(h.list, [Some(view), None], [Some(first), None])
        .unwrap();
    h.draw();
    h.ctx.destroy_sampler(first).unwrap();
    assert!(
        h.ctx
            .cmd_set_sampled_view_handles(h.list, [Some(view), None], [Some(first), None])
            .is_err()
    );
    h.ctx
        .cmd_set_sampled_view_handles(h.list, [Some(view), None], [Some(second), None])
        .unwrap();
    h.draw();
    h.ctx.close_command_list(h.list).unwrap();
    let receipt = h
        .ctx
        .submit_command_list(h.queue, h.list, &h.memory)
        .unwrap();
    h.ctx.destroy_sampler(second).unwrap();
    h.ctx.reset_command_list(h.list, h.allocator).unwrap();
    h.ctx.destroy_texture_view(view).unwrap();
    h.ctx.destroy_resource(root).unwrap();
    assert_eq!(
        h.ctx.wait_submission(receipt).unwrap(),
        GraphicsSubmissionStatus::Complete
    );
    h.ctx.release_submission(receipt).unwrap();
    assert_eq!(
        h.ctx.last_presentation.lock().unwrap().pixels,
        [23, 41, 59, 255]
    );
}

#[test]
fn cpu_sampler_handles_invalid_later_slot_is_atomic_and_aliases_reuse_descriptor() {
    use xenora_loader::graphics::{GraphicsTextureViewDesc, GraphicsTextureViewFormat};
    let mut h = Harness::new();
    let root = h.texture(ResourceStates::PIXEL_SHADER_RESOURCE);
    let view = h
        .ctx
        .create_texture_view(
            h.device,
            root,
            GraphicsTextureViewDesc {
                base_mip: 0,
                mip_count: 1,
                format: GraphicsTextureViewFormat::Rgba8Unorm,
            },
        )
        .unwrap();
    let sampler = h
        .ctx
        .create_sampler(h.device, SamplerDesc::nearest_clamp())
        .unwrap();
    assert!(
        h.ctx
            .cmd_set_sampled_view_handles(h.list, [Some(view), None], [Some(sampler), Some(root)])
            .is_err()
    );
    h.ctx.destroy_texture_view(view).unwrap(); // Failed bind recorded no first-view reference.
    let view = h
        .ctx
        .create_texture_view(
            h.device,
            root,
            GraphicsTextureViewDesc {
                base_mip: 0,
                mip_count: 1,
                format: GraphicsTextureViewFormat::Rgba8Unorm,
            },
        )
        .unwrap();
    let pipeline = h.pipeline(
        VARYING,
        include_bytes!("../../../fixtures/rdna2/x9-multislot-ps.bin"),
    );
    let target = h.texture(ResourceStates::RENDER_TARGET);
    h.ctx
        .set_metal_view_attachment_backend(|draws, _, _, _, _| {
            let bindings = &draws[0].sampled_bindings;
            assert_eq!(bindings.samplers, [Some(SamplerDesc::nearest_clamp()); 2]);
            assert!(Arc::ptr_eq(
                &bindings.textures[0].as_ref().unwrap().pixels,
                &bindings.textures[1].as_ref().unwrap().pixels
            ));
            Ok(GraphicsPassOutput {
                colors: vec![vec![0; 4]],
                depth: None,
            })
        });
    h.ctx
        .cmd_set_graphics_pipeline(h.list, Some(pipeline))
        .unwrap();
    h.ctx
        .cmd_set_color_targets(h.list, &[target], &[[0; 4]])
        .unwrap();
    h.ctx.cmd_set_viewport(h.list, 1, 1).unwrap();
    h.ctx
        .cmd_set_sampled_view_handles(h.list, [Some(view); 2], [Some(sampler); 2])
        .unwrap();
    assert!(
        h.ctx
            .cmd_set_sampled_view_handles(h.list, [Some(view), Some(root)], [Some(sampler); 2])
            .is_err()
    );
    h.ctx.destroy_sampler(sampler).unwrap();
    h.draw();
    h.ctx.close_command_list(h.list).unwrap();
    h.ctx
        .execute_command_list(h.queue, h.list, &h.memory)
        .unwrap();
}

#[test]
fn cpu_sampler_creation_does_not_skip_draw_time_view_lod_validation() {
    use xenora_loader::graphics::{GraphicsTextureViewDesc, GraphicsTextureViewFormat};
    let mut h = Harness::new();
    let root = h.texture(ResourceStates::PIXEL_SHADER_RESOURCE);
    let view = h
        .ctx
        .create_texture_view(
            h.device,
            root,
            GraphicsTextureViewDesc {
                base_mip: 0,
                mip_count: 1,
                format: GraphicsTextureViewFormat::Rgba8Unorm,
            },
        )
        .unwrap();
    let sampler = h
        .ctx
        .create_sampler(h.device, SamplerDesc::nearest_clamp().with_explicit_mip(1))
        .unwrap();
    let pipeline = h.pipeline(VARYING, SAMPLED);
    let target = h.texture(ResourceStates::RENDER_TARGET);
    h.ctx.set_metal_view_attachment_backend(|_, _, _, _, _| {
        panic!("invalid relative LOD reached CPU callback")
    });
    h.ctx
        .cmd_set_graphics_pipeline(h.list, Some(pipeline))
        .unwrap();
    h.ctx
        .cmd_set_color_targets(h.list, &[target], &[[0; 4]])
        .unwrap();
    h.ctx.cmd_set_viewport(h.list, 1, 1).unwrap();
    h.ctx
        .cmd_set_sampled_view_handles(h.list, [Some(view), None], [Some(sampler), None])
        .unwrap();
    h.ctx.destroy_sampler(sampler).unwrap();
    h.draw();
    h.ctx.close_command_list(h.list).unwrap();
    assert!(
        h.ctx
            .execute_command_list(h.queue, h.list, &h.memory)
            .is_err()
    );
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
}
