// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU-only attachment contract tests. Callbacks never execute GPU work.

use xenora_loader::graphics::{
    CommandListType, GraphicsColorLoad as Load, GraphicsContext, GraphicsError, GraphicsHandle,
    GraphicsPassOutput, ResourceDesc, ResourceDimension, ResourceStates,
};
use xenora_loader::{GuestMemory, MemoryPermissions};

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
        memory
            .map_region(0x1000, 48, MemoryPermissions::READ_WRITE)
            .unwrap();
        let coords: [f32; 12] = [-1., -1., 0., 1., 1., -1., 0., 1., 0., 1., 0., 1.];
        memory
            .write_bytes(
                0x1000,
                &coords
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let vertices = ctx
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
                Some(0x1000),
            )
            .unwrap();
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

    fn bind(&mut self, targets: &[GraphicsHandle], loads: &[Load]) {
        self.ctx
            .cmd_set_color_targets_with_loads(self.list, targets, loads)
            .unwrap();
    }

    fn draw(&mut self) {
        self.ctx.cmd_set_viewport(self.list, 1, 1).unwrap();
        self.ctx
            .cmd_set_vertex_buffer(self.list, self.vertices, 0, 16)
            .unwrap();
        self.ctx.cmd_draw_instanced(self.list, 3, 1, 0, 0).unwrap();
    }

    fn execute(&mut self) -> Result<Vec<u8>, GraphicsError> {
        self.ctx.close_command_list(self.list).unwrap();
        self.ctx
            .execute_command_list(self.queue, self.list, &self.memory)
            .map(|out| out.pixels)
    }

    fn reset(&mut self) {
        self.ctx
            .reset_command_list(self.list, self.allocator)
            .unwrap();
    }

    fn seed(&mut self, target: GraphicsHandle, color: [u8; 4]) {
        // Exercise the old clear-only wrapper too.
        self.ctx
            .cmd_set_color_targets(self.list, &[target], &[color])
            .unwrap();
        assert_eq!(self.execute().unwrap(), color);
        self.reset();
    }
}

#[test]
fn zero_and_prior_contents_load_without_draw_or_backend() {
    let mut h = Harness::new();
    let target = h.target();
    h.bind(&[target], &[Load::Load]);
    assert_eq!(h.execute().unwrap(), [0, 0, 0, 0]);
    h.reset();
    h.seed(target, [17, 31, 47, 255]);
    h.ctx.set_require_metal_without_backend();
    h.bind(&[target], &[Load::Load]);
    assert_eq!(h.execute().unwrap(), [17, 31, 47, 255]);
}

#[test]
fn mixed_attachments_receive_literal_committed_snapshots() {
    let mut h = Harness::new();
    let a = h.target();
    let b = h.target();
    h.seed(a, [1, 2, 3, 255]);
    h.seed(b, [11, 22, 33, 44]);
    h.ctx
        .set_metal_attachment_backend(|draws, extent, bindings, _, depth| {
            assert_eq!(draws.len(), 1);
            assert_eq!(extent, (1, 1));
            assert!(depth.is_none());
            assert_eq!(bindings.len(), 2);
            assert_eq!(bindings[0].clear, [9, 8, 7, 6]);
            assert!(bindings[0].previous.is_none());
            assert_eq!(
                bindings[1].previous.as_deref(),
                Some([11, 22, 33, 44].as_slice())
            );
            Ok(GraphicsPassOutput {
                colors: vec![vec![9, 8, 7, 6], vec![55, 66, 77, 88]],
                depth: None,
            })
        });
    h.bind(&[a, b], &[Load::Clear([9, 8, 7, 6]), Load::Load]);
    h.draw();
    assert_eq!(h.execute().unwrap(), [9, 8, 7, 6]);
    h.reset();
    h.bind(&[b], &[Load::Load]);
    assert_eq!(h.execute().unwrap(), [55, 66, 77, 88]);
}

#[test]
fn malformed_output_preserves_all_colors_barriers_and_presentation() {
    let mut h = Harness::new();
    let a = h.target();
    let b = h.target();
    h.seed(a, [1, 2, 3, 4]);
    h.seed(b, [5, 6, 7, 8]);
    h.ctx.set_metal_attachment_backend(|_, _, bindings, _, _| {
        assert_eq!(
            bindings[0].previous.as_deref(),
            Some([1, 2, 3, 4].as_slice())
        );
        Ok(GraphicsPassOutput {
            colors: vec![vec![99; 4], vec![88; 3]],
            depth: None,
        })
    });
    h.bind(&[a, b], &[Load::Load, Load::Clear([10; 4])]);
    h.draw();
    h.ctx
        .cmd_resource_barrier(
            h.list,
            a,
            ResourceStates::RENDER_TARGET,
            ResourceStates::COPY_SOURCE,
        )
        .unwrap();
    assert!(matches!(h.execute(), Err(GraphicsError::BackendError(_))));
    {
        let presentation = h.ctx.last_presentation.lock().unwrap();
        assert_eq!(presentation.frame_index, 2);
        assert_eq!(presentation.pixels, [5, 6, 7, 8]);
    }
    h.reset();
    for (target, expected) in [(a, [1, 2, 3, 4]), (b, [5, 6, 7, 8])] {
        // Binding requires RT, proving the failed COPY_SOURCE transition was not committed.
        h.bind(&[target], &[Load::Load]);
        assert_eq!(h.execute().unwrap(), expected);
        h.reset();
    }
}

#[test]
fn post_draw_target_or_load_changes_reject_before_callback() {
    for change in 0..3 {
        let mut h = Harness::new();
        let a = h.target();
        let b = h.target();
        h.ctx
            .set_metal_attachment_backend(|_, _, _, _, _| panic!("invalid pass dispatched"));
        h.bind(&[a], &[Load::Load]);
        h.draw();
        match change {
            0 => h.bind(&[b], &[Load::Load]),
            1 => h.bind(&[a], &[Load::Clear([0; 4])]),
            _ => h
                .ctx
                .cmd_set_render_targets(h.list, Some(a), [0; 4])
                .unwrap(),
        }
        assert_eq!(h.execute(), Err(GraphicsError::InvalidDescriptor));
        assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    }
}

#[test]
fn old_backends_reject_drawn_load_before_dispatch() {
    for backend in 0..3 {
        let mut h = Harness::new();
        let target = h.target();
        match backend {
            0 => h
                .ctx
                .set_metal_stateful_backend(|_, _, _, _, _| panic!("old callback received Load")),
            1 => h
                .ctx
                .set_metal_backend(|_, _, _| panic!("legacy callback received Load")),
            _ => {} // Software reference is also unsupported for drawn Load.
        }
        h.bind(&[target], &[Load::Load]);
        h.draw();
        assert_eq!(h.execute(), Err(GraphicsError::InvalidDescriptor));
        assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    }
}

#[test]
fn superseded_load_bindings_retain_resources_until_reset() {
    let mut h = Harness::new();
    let a = h.target();
    let b = h.target();
    h.bind(&[a], &[Load::Load]);
    h.bind(&[b], &[Load::Load]);
    assert!(h.ctx.destroy_resource(a).is_err());
    assert!(h.ctx.destroy_resource(b).is_err());
    assert_eq!(h.execute().unwrap(), [0; 4]);
    assert!(h.ctx.destroy_resource(a).is_err());
    assert!(h.ctx.destroy_resource(b).is_err());
    h.reset();
    h.ctx.destroy_resource(a).unwrap();
    h.ctx.destroy_resource(b).unwrap();
}

#[test]
fn color_load_and_depth_snapshot_commit_together() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use xenora_loader::graphics::GraphicsDepthLoad;

    let mut h = Harness::new();
    let target = h.target();
    h.seed(target, [13, 17, 19, 23]);
    let depth = h.ctx.create_depth_resource(h.device, (1, 1)).unwrap();
    let phase = Arc::new(AtomicUsize::new(0));
    let callback_phase = phase.clone();
    h.ctx
        .set_metal_attachment_backend(move |draws, _, colors, _, depth| {
            assert!(draws.is_empty());
            assert_eq!(
                colors[0].previous.as_deref(),
                Some([13, 17, 19, 23].as_slice())
            );
            let depth = depth.unwrap();
            let phase = callback_phase.load(Ordering::SeqCst);
            if phase == 0 {
                assert!(depth.previous.is_none());
            } else {
                assert_eq!(*depth.previous.unwrap().downcast_ref::<u32>().unwrap(), 37);
            }
            Ok(GraphicsPassOutput {
                colors: vec![if phase == 1 {
                    vec![99; 3]
                } else {
                    vec![13, 17, 19, 23]
                }],
                depth: Some(Arc::new(if phase == 1 { 91u32 } else { 37u32 })),
            })
        });
    for step in 0..3 {
        phase.store(step, Ordering::SeqCst);
        h.bind(&[target], &[Load::Load]);
        h.ctx
            .cmd_set_depth_target(
                h.list,
                Some(depth),
                if step == 0 {
                    GraphicsDepthLoad::Clear(1.0)
                } else {
                    GraphicsDepthLoad::Load
                },
            )
            .unwrap();
        if step == 1 {
            assert!(matches!(h.execute(), Err(GraphicsError::BackendError(_))));
            assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 2);
        } else {
            assert_eq!(h.execute().unwrap(), [13, 17, 19, 23]);
        }
        h.reset();
    }
}

#[test]
fn load_requires_render_target_state_and_matching_actions() {
    let mut h = Harness::new();
    let target = h.target();
    assert_eq!(
        h.ctx
            .cmd_set_color_targets_with_loads(h.list, &[target], &[]),
        Err(GraphicsError::InvalidDescriptor)
    );
    assert_eq!(
        h.ctx
            .cmd_set_color_targets_with_loads(h.list, &[target, target], &[Load::Load; 2]),
        Err(GraphicsError::InvalidDescriptor)
    );
    h.ctx
        .cmd_resource_barrier(
            h.list,
            target,
            ResourceStates::RENDER_TARGET,
            ResourceStates::COPY_SOURCE,
        )
        .unwrap();
    h.bind(&[target], &[Load::Load]);
    assert_eq!(h.execute(), Err(GraphicsError::ResourceLifetimeViolation));
    h.reset();
    h.bind(&[target], &[Load::Load]);
    assert_eq!(h.execute().unwrap(), [0; 4]);
}

#[test]
fn recorded_load_reads_latest_base_level_and_preserves_mip_tail() {
    let mut h = Harness::new();
    let target = h
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
            ResourceStates::COPY_DEST,
            None,
        )
        .unwrap();
    h.memory
        .map_region(0x2000, 20, MemoryPermissions::READ_WRITE)
        .unwrap();
    h.memory
        .map_region(0x3000, 20, MemoryPermissions::READ_WRITE)
        .unwrap();
    let original = [1u8; 20];
    h.memory.write_bytes(0x2000, &original).unwrap();
    h.ctx
        .copy_guest_to_resource(target, 0, 0x2000, 20, &h.memory)
        .unwrap();
    let allocator = h
        .ctx
        .create_command_allocator(h.device, CommandListType::Direct)
        .unwrap();
    let transfer = h
        .ctx
        .create_command_list(h.device, allocator, CommandListType::Direct)
        .unwrap();
    h.ctx
        .cmd_resource_barrier(
            transfer,
            target,
            ResourceStates::COPY_DEST,
            ResourceStates::RENDER_TARGET,
        )
        .unwrap();
    h.ctx.close_command_list(transfer).unwrap();
    h.ctx
        .execute_command_list(h.queue, transfer, &h.memory)
        .unwrap();

    // Record and close before changing the committed bytes. Snapshotting here
    // would incorrectly capture the original all-ones image.
    h.bind(&[target], &[Load::Load]);
    h.ctx.cmd_set_viewport(h.list, 2, 2).unwrap();
    h.ctx
        .cmd_set_vertex_buffer(h.list, h.vertices, 0, 16)
        .unwrap();
    h.ctx.cmd_draw_instanced(h.list, 3, 1, 0, 0).unwrap();
    h.ctx
        .cmd_resource_barrier(
            h.list,
            target,
            ResourceStates::RENDER_TARGET,
            ResourceStates::COPY_SOURCE,
        )
        .unwrap();
    h.ctx.close_command_list(h.list).unwrap();

    h.ctx.reset_command_list(transfer, allocator).unwrap();
    h.ctx
        .cmd_resource_barrier(
            transfer,
            target,
            ResourceStates::RENDER_TARGET,
            ResourceStates::COPY_DEST,
        )
        .unwrap();
    h.ctx.close_command_list(transfer).unwrap();
    h.ctx
        .execute_command_list(h.queue, transfer, &h.memory)
        .unwrap();
    let updated = [
        11, 12, 13, 14, 21, 22, 23, 24, 31, 32, 33, 34, 41, 42, 43, 44, 201, 202, 203, 204,
    ];
    h.memory.write_bytes(0x2000, &updated).unwrap();
    h.ctx
        .copy_guest_to_resource(target, 0, 0x2000, 20, &h.memory)
        .unwrap();
    h.ctx.reset_command_list(transfer, allocator).unwrap();
    h.ctx
        .cmd_resource_barrier(
            transfer,
            target,
            ResourceStates::COPY_DEST,
            ResourceStates::RENDER_TARGET,
        )
        .unwrap();
    h.ctx.close_command_list(transfer).unwrap();
    h.ctx
        .execute_command_list(h.queue, transfer, &h.memory)
        .unwrap();

    h.ctx
        .set_metal_attachment_backend(|draws, extent, bindings, _, depth| {
            // CPU-only adapter: exact 16-byte base-level snapshot, excluding mip 1.
            assert_eq!(draws.len(), 1);
            assert_eq!(extent, (2, 2));
            assert!(depth.is_none());
            assert_eq!(bindings.len(), 1);
            assert_eq!(
                bindings[0].previous.as_deref(),
                Some(
                    [
                        11, 12, 13, 14, 21, 22, 23, 24, 31, 32, 33, 34, 41, 42, 43, 44,
                    ]
                    .as_slice()
                )
            );
            Ok(GraphicsPassOutput {
                colors: vec![vec![
                    91, 92, 93, 94, 21, 22, 23, 24, 31, 32, 33, 34, 41, 42, 43, 44,
                ]],
                depth: None,
            })
        });
    let outcome = h
        .ctx
        .execute_command_list(h.queue, h.list, &h.memory)
        .unwrap();
    assert_eq!(
        outcome.pixels,
        [
            91, 92, 93, 94, 21, 22, 23, 24, 31, 32, 33, 34, 41, 42, 43, 44,
        ]
    );
    h.ctx
        .copy_resource_to_guest(target, 0, 0x3000, 20, &mut h.memory)
        .unwrap();
    let mut readback = [0; 20];
    h.memory.read_bytes(0x3000, &mut readback).unwrap();
    assert_eq!(
        readback,
        [
            91, 92, 93, 94, 21, 22, 23, 24, 31, 32, 33, 34, 41, 42, 43, 44, 201, 202, 203, 204,
        ]
    );
}
