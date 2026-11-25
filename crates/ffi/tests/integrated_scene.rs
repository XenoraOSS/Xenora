// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded original indexed MRT scene through the production async Metal adapter.
//! Failure coverage is preflight rollback, not simulated or real GPU failure.
use xenora_ffi::graphics_backend::install_async_pipeline_backend;
use xenora_loader::graphics::*;
use xenora_loader::{GuestMemory, MemoryPermissions};
use xenora_texture::SamplerDesc;

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
const UV: [f32; 4] = [0.5, 0.5, 0.0, 1.0];
const CLEARS: [[u8; 4]; 2] = [[10, 20, 30, 255], [40, 50, 60, 255]];
type Rows = [[[u8; 4]; 8]; 2];

#[derive(Clone, Copy)]
struct Draw {
    sampled: bool,
    z: f32,
    color: [f32; 4],
    x: usize,
    width: usize,
    write: bool,
    blend: GraphicsBlendMode,
    mask: u8,
}
impl Draw {
    fn new(sampled: bool, z: f32, color: [f32; 4], x: usize, width: usize) -> Self {
        Self {
            sampled,
            z,
            color,
            x,
            width,
            write: true,
            blend: GraphicsBlendMode::Replace,
            mask: 15,
        }
    }
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
fn texture(width: u64, height: u32) -> ResourceDesc {
    ResourceDesc {
        dimension: ResourceDimension::Texture2D,
        width,
        height,
        depth_or_array_size: 1,
        mip_levels: 1,
        format_rgba8: true,
    }
}
struct Scene {
    ctx: GraphicsContext,
    memory: GuestMemory,
    queue: GraphicsHandle,
    allocator: GraphicsHandle,
    list: GraphicsHandle,
    programs: [GraphicsHandle; 2],
    vb: GraphicsHandle,
    cb: GraphicsHandle,
    ib: GraphicsHandle,
    sampled: GraphicsHandle,
    targets: [GraphicsHandle; 2],
    depth: GraphicsHandle,
}
impl Scene {
    fn new() -> Self {
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
        let programs = [
            include_bytes!("../../../fixtures/rdna2/x9-scene-varying-mrt-ps.bin").as_slice(),
            include_bytes!("../../../fixtures/rdna2/x9-scene-sampled-mrt-ps.bin").as_slice(),
        ]
        .map(|ps| {
            ctx.create_graphics_pipeline(
                device,
                include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin"),
                ps,
            )
            .unwrap()
        });
        let vb = ctx
            .create_committed_resource(device, buffer(256), ResourceStates::COPY_DEST, None)
            .unwrap();
        let cb = ctx
            .create_committed_resource(device, buffer(256), ResourceStates::COPY_DEST, None)
            .unwrap();
        let ib = ctx
            .create_committed_resource(device, buffer(24), ResourceStates::COPY_DEST, None)
            .unwrap();
        let sampled = ctx
            .create_committed_resource(device, texture(1, 1), ResourceStates::COPY_DEST, None)
            .unwrap();
        let targets = [0, 1].map(|_| {
            ctx.create_committed_resource(
                device,
                texture(8, 3),
                ResourceStates::RENDER_TARGET,
                None,
            )
            .unwrap()
        });
        let depth = ctx.create_depth_resource(device, (8, 3)).unwrap();
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x10000, 0x10000, MemoryPermissions::READ_WRITE)
            .unwrap();
        Self {
            ctx,
            memory,
            queue,
            allocator,
            list,
            programs,
            vb,
            cb,
            ib,
            sampled,
            targets,
            depth,
        }
    }

    fn record(&mut self, draws: &[Draw], texel: [u8; 4], load: bool, fail: bool) {
        assert!(draws.len() <= 4);
        self.ctx
            .reset_command_list(self.list, self.allocator)
            .unwrap();
        let positions: Vec<u8> = draws
            .iter()
            .flat_map(|d| {
                [
                    [-1.0, -1.0, d.z, 1.0],
                    [1.0, -1.0, d.z, 1.0],
                    [-1.0, 1.0, d.z, 1.0],
                    [1.0, 1.0, d.z, 1.0],
                ]
            })
            .flatten()
            .flat_map(f32::to_le_bytes)
            .collect();
        let colors: Vec<u8> = draws
            .iter()
            .flat_map(|d| [d.color; 4])
            .flatten()
            .flat_map(f32::to_le_bytes)
            .collect();
        // First six indices form a quad. The second group is selected only by
        // the failing final draw; preceding draws remain individually valid.
        let indices: Vec<u8> = [0u16, 1, 2, 2, 1, 3, 0, 1, 2, 2, 1, 65535]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        for (resource, bytes) in [
            (self.vb, positions.as_slice()),
            (self.cb, colors.as_slice()),
            (self.ib, indices.as_slice()),
            (self.sampled, texel.as_slice()),
        ] {
            self.memory.write_bytes(0x10000, bytes).unwrap();
            self.ctx
                .copy_guest_to_resource(resource, 0, 0x10000, bytes.len(), &self.memory)
                .unwrap();
        }
        // A previous successful frame leaves both colors readable. Failed work
        // must leave that state intact too, so recovery replays these barriers.
        if load {
            for target in self.targets {
                self.ctx
                    .cmd_resource_barrier(
                        self.list,
                        target,
                        ResourceStates::COPY_SOURCE,
                        ResourceStates::RENDER_TARGET,
                    )
                    .unwrap();
            }
        }
        for (resource, state) in [
            (self.vb, ResourceStates::VERTEX_AND_CONSTANT_BUFFER),
            (self.cb, ResourceStates::VERTEX_AND_CONSTANT_BUFFER),
            (self.ib, ResourceStates::INDEX_BUFFER),
            (self.sampled, ResourceStates::PIXEL_SHADER_RESOURCE),
        ] {
            self.ctx
                .cmd_resource_barrier(self.list, resource, ResourceStates::COPY_DEST, state)
                .unwrap();
        }
        let loads = if load && !fail {
            [GraphicsColorLoad::Load; 2]
        } else {
            CLEARS.map(GraphicsColorLoad::Clear)
        };
        self.ctx
            .cmd_set_color_targets_with_loads(self.list, &self.targets, &loads)
            .unwrap();
        self.ctx
            .cmd_set_depth_target(
                self.list,
                Some(self.depth),
                if fail {
                    GraphicsDepthLoad::Clear(0.0)
                } else if load {
                    GraphicsDepthLoad::Load
                } else {
                    GraphicsDepthLoad::Clear(1.0)
                },
            )
            .unwrap();
        self.ctx.cmd_set_viewport(self.list, 8, 3).unwrap();
        self.ctx
            .cmd_set_index_buffer(self.list, self.ib, 0, 57)
            .unwrap();
        for (index, draw) in draws.iter().enumerate() {
            self.ctx
                .cmd_set_graphics_pipeline(
                    self.list,
                    Some(self.programs[usize::from(draw.sampled)]),
                )
                .unwrap();
            self.ctx
                .cmd_set_sampled_texture(
                    self.list,
                    draw.sampled.then_some(self.sampled),
                    SamplerDesc::nearest_clamp(),
                )
                .unwrap();
            self.ctx
                .cmd_set_vertex_buffer(self.list, self.vb, index * 64, 16)
                .unwrap();
            self.ctx
                .cmd_set_vertex_colors(self.list, self.cb, index * 64, 16)
                .unwrap();
            self.ctx
                .cmd_set_draw_state(
                    self.list,
                    GraphicsDrawState {
                        depth_compare: Some(if fail {
                            GraphicsDepthCompare::Always
                        } else {
                            GraphicsDepthCompare::Less
                        }),
                        depth_write: draw.write,
                        blend: draw.blend,
                        color_write_mask: draw.mask,
                        scissor: Some([draw.x, 0, draw.width, 3]),
                        ..Default::default()
                    },
                )
                .unwrap();
            let start = if fail && index + 1 == draws.len() {
                6
            } else {
                0
            };
            self.ctx
                .cmd_draw_indexed_instanced(self.list, 6, 1, start, 0, 0)
                .unwrap();
        }
        for (resource, state) in [
            (self.vb, ResourceStates::VERTEX_AND_CONSTANT_BUFFER),
            (self.cb, ResourceStates::VERTEX_AND_CONSTANT_BUFFER),
            (self.ib, ResourceStates::INDEX_BUFFER),
            (self.sampled, ResourceStates::PIXEL_SHADER_RESOURCE),
        ] {
            self.ctx
                .cmd_resource_barrier(self.list, resource, state, ResourceStates::COPY_DEST)
                .unwrap();
        }
        for target in self.targets {
            self.ctx
                .cmd_resource_barrier(
                    self.list,
                    target,
                    ResourceStates::RENDER_TARGET,
                    ResourceStates::COPY_SOURCE,
                )
                .unwrap();
        }
        self.ctx.close_command_list(self.list).unwrap();
    }

    fn finish(&mut self, rows: Rows, frame: u64, draws: usize) {
        let receipt = self
            .ctx
            .submit_command_list(self.queue, self.list, &self.memory)
            .unwrap();
        assert_eq!(
            self.ctx.wait_submission(receipt).unwrap(),
            GraphicsSubmissionStatus::Complete
        );
        let out = self
            .ctx
            .submission_outcome(receipt)
            .unwrap()
            .unwrap()
            .clone();
        assert_eq!(out.backend, GraphicsBackendId::MetalNative);
        assert_eq!((out.width, out.height, out.draw_count), (8, 3, draws));
        assert_eq!(out.color_outputs.len(), 2);
        for (slot, row) in rows.iter().enumerate() {
            assert_eq!(
                out.color_outputs[slot],
                row.repeat(3).into_iter().flatten().collect::<Vec<_>>(),
                "output slot={slot}, frame={frame}"
            );
        }
        assert_eq!(out.pixels, out.color_outputs[0]);
        self.ctx.release_submission(receipt).unwrap();
        self.check_stored(rows, frame);
    }

    fn check_stored(&mut self, rows: Rows, frame: u64) {
        for (slot, target) in self.targets.into_iter().enumerate() {
            self.ctx
                .copy_resource_to_guest(target, 0, 0x10000, 96, &mut self.memory)
                .unwrap();
            let mut actual = [0; 96];
            self.memory.read_bytes(0x10000, &mut actual).unwrap();
            for (index, pixel) in actual.as_chunks::<4>().0.iter().enumerate() {
                assert_eq!(
                    *pixel,
                    rows[slot][index % 8],
                    "stored slot={slot}, pixel={index}, frame={frame}"
                );
            }
        }
        let shown = self.ctx.last_presentation.lock().unwrap();
        assert_eq!(shown.frame_index, frame);
        assert_eq!((shown.width, shown.height), (8, 3));
        assert_eq!(shown.backend, Some(GraphicsBackendId::MetalNative));
        assert_eq!(
            shown.pixels,
            rows[0].repeat(3).into_iter().flatten().collect::<Vec<_>>()
        );
    }
}

#[test]
#[cfg_attr(not(feature = "gpu-tests"), ignore = "requires native Metal")]
fn integrated_scene_preserves_mrt_depth_and_recovers_after_failure() {
    let mut scene = Scene::new();
    let first = [
        Draw::new(false, 0.5, BLUE, 0, 6),
        Draw::new(true, 0.25, UV, 2, 4),
        Draw {
            write: false,
            blend: GraphicsBlendMode::Additive,
            mask: 5,
            ..Draw::new(false, 0.75, GREEN, 0, 6)
        },
        Draw {
            blend: GraphicsBlendMode::Additive,
            mask: 1,
            ..Draw::new(false, 0.125, RED, 4, 2)
        },
    ];
    let first_rows = [
        [
            [0, 0, 255, 255],
            [0, 0, 255, 255],
            [64, 128, 192, 255],
            [64, 128, 192, 255],
            [255, 128, 192, 255],
            [255, 128, 192, 255],
            [10, 20, 30, 255],
            [10, 20, 30, 255],
        ],
        [
            [255, 0, 0, 255],
            [255, 0, 0, 255],
            [192, 64, 128, 255],
            [192, 64, 128, 255],
            [192, 64, 128, 255],
            [192, 64, 128, 255],
            [40, 50, 60, 255],
            [40, 50, 60, 255],
        ],
    ];
    scene.record(&first, [64, 128, 192, 255], false, false);
    scene.finish(first_rows, 1, 4);

    // The changed upload passes only where .375 is nearer than retained .5.
    // Green writes .0625; later .2 must fail. The last additive draw is read-only.
    let second = [
        Draw::new(true, 0.375, UV, 0, 6),
        Draw::new(false, 0.0625, GREEN, 3, 2),
        Draw::new(false, 0.2, RED, 4, 2),
        Draw {
            write: false,
            blend: GraphicsBlendMode::Additive,
            mask: 7,
            ..Draw::new(true, 0.75, UV, 6, 2)
        },
    ];
    let second_rows = [
        [
            [16, 32, 48, 255],
            [16, 32, 48, 255],
            [64, 128, 192, 255],
            [0, 255, 0, 255],
            [0, 255, 0, 255],
            [255, 128, 192, 255],
            [26, 52, 78, 255],
            [26, 52, 78, 255],
        ],
        [
            [48, 16, 32, 255],
            [48, 16, 32, 255],
            [192, 64, 128, 255],
            [0, 0, 255, 255],
            [0, 0, 255, 255],
            [192, 64, 128, 255],
            [88, 66, 92, 255],
            [88, 66, 92, 255],
        ],
    ];
    scene.record(&second, [16, 32, 48, 255], true, false);
    scene.finish(second_rows, 2, 4);

    // Valid preceding draws and clear operations must not publish when the final
    // indexed draw is invalid. This is a pre-GPU validation failure, not a GPU fault.
    let failed = [
        Draw::new(false, 0.01, RED, 0, 8),
        Draw::new(true, 0.02, UV, 0, 8),
    ];
    scene.record(&failed, [16, 32, 48, 255], true, true);
    assert_eq!(
        scene
            .ctx
            .submit_command_list(scene.queue, scene.list, &scene.memory)
            .unwrap_err(),
        GraphicsError::IndexOutOfRange
    );
    scene.check_stored(second_rows, 2);

    // Actual native recovery: .2 passes columns 0..2 but fails 3..5. Publishing
    // the failed clear (0) or .01 write would reject those first three columns.
    // .8 passes 6..7 only if the prior read-only .75 draw left depth at 1.
    let recovery = [
        Draw::new(false, 0.2, RED, 0, 6),
        Draw::new(true, 0.8, UV, 6, 2),
        Draw::new(false, 0.3, BLUE, 0, 3),
    ];
    let recovered_rows = [
        [
            [255, 0, 0, 255],
            [255, 0, 0, 255],
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 255, 0, 255],
            [255, 128, 192, 255],
            [16, 32, 48, 255],
            [16, 32, 48, 255],
        ],
        [
            [0, 255, 0, 255],
            [0, 255, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [0, 0, 255, 255],
            [192, 64, 128, 255],
            [48, 16, 32, 255],
            [48, 16, 32, 255],
        ],
    ];
    scene.record(&recovery, [16, 32, 48, 255], true, false);
    scene.finish(recovered_rows, 3, 3);

    // A separate Load frame proves the recovery writes persisted after collection:
    // .3 must now lose to .2 in columns 0..2, and .9 must lose to .8 in 6..7.
    // A changed sampled upload would be visible if either rejection were lost.
    let probe = [
        Draw {
            write: false,
            ..Draw::new(false, 0.3, BLUE, 0, 3)
        },
        Draw {
            write: false,
            ..Draw::new(true, 0.9, UV, 6, 2)
        },
    ];
    scene.record(&probe, [240, 224, 208, 255], true, false);
    scene.finish(recovered_rows, 4, 2);
}
