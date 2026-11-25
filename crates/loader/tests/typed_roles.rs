// SPDX-License-Identifier: GPL-3.0-or-later
//! Independent CPU service/typed-publication checks, not GPU execution evidence.
use std::sync::Mutex;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use xenora_loader::graphics::*;
use xenora_loader::graphics_service::graphics_call_v1;
use xenora_loader::{GuestMemory, MemoryPermissions};
use xenora_texture::TextureFormat;
const DESC: u64 = 0x1000;
const OUT: u64 = 0x2000;
const DATA: u64 = 0x3000;
const SENTINEL: u64 = 0x9182736455463728;
struct Harness {
    ctx: GraphicsContext,
    memory: GuestMemory,
}
impl Harness {
    fn new() -> Self {
        let mut memory = GuestMemory::new();
        for (a, n) in [(DESC, 128), (OUT, 8), (DATA, 4096)] {
            memory
                .map_region(a, n, MemoryPermissions::READ_WRITE)
                .unwrap();
        }
        Self {
            ctx: GraphicsContext::new(),
            memory,
        }
    }
    fn prepare(&mut self, args: &[u64]) {
        let mut words = [0u64; 16];
        words[0] = 1;
        words[1] = 50;
        words[2..2 + args.len()].copy_from_slice(args);
        self.memory
            .write_bytes(
                DESC,
                &words
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        self.memory
            .write_bytes(OUT, &SENTINEL.to_le_bytes())
            .unwrap();
    }
    fn call(&mut self, op: u64, args: &[u64]) -> u64 {
        self.prepare(args);
        self.memory
            .write_bytes(DESC + 8, &op.to_le_bytes())
            .unwrap();
        graphics_call_v1(&mut self.ctx, &mut self.memory, DESC, 128, OUT).unwrap()
    }
    fn result(&self) -> u64 {
        self.memory.read_u64(OUT).unwrap()
    }
    fn ok(&mut self, op: u64, args: &[u64]) -> u64 {
        assert_eq!(self.call(op, args), 0, "opcode {op}");
        self.result()
    }
    fn upload(&mut self, r: u64, b: &[u8]) {
        self.memory.write_bytes(DATA, b).unwrap();
        self.ok(9, &[r, 0, DATA, b.len() as u64]);
    }
}

fn typed_records(h: &mut Harness, list: u64, targets: &[u64], invalid_second: bool) -> u64 {
    let mut words = Vec::new();
    for (i, target) in targets.iter().enumerate() {
        words.extend_from_slice(&[
            *target,
            0,
            0,
            if invalid_second && i == 1 { 1 } else { 0 },
            0,
            0,
            0,
            0,
        ]);
    }
    h.memory
        .write_bytes(
            DATA,
            &words
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect::<Vec<_>>(),
        )
        .unwrap();
    h.call(56, &[list, targets.len() as u64, DATA])
}
#[test]
fn cpu_typed_roles_equal_bytes_wrong_format_completion_atomic_retry() {
    let mut h = Harness::new();
    let d = h.ok(1, &[]);
    let q = h.ok(2, &[d]);
    let a = h.ok(3, &[d]);
    let l = h.ok(4, &[d, a]);
    let t0 = h.ok(53, &[d, 1, 1, 1, 0, 0, 0x400]);
    let t1 = h.ok(53, &[d, 1, 1, 1, 0, 5, 0x400]);
    let z = h.ok(53, &[d, 1, 1, 1, 0, 9, 0x400]);
    h.upload(t0, &[17, 53, 127, 193]);
    h.upload(t1, &[7, 11, 13, 19]);
    h.upload(z, &0.25f32.to_le_bytes());
    let calls = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    let prior = Arc::new(Mutex::new(None::<GraphicsDepthSnapshot>));
    let saved = prior.clone();
    h.ctx.set_metal_typed_attachment_backend(
        GraphicsTextureCapabilities::default(),
        move |_, extent, colors, _, depth| {
            assert_eq!(extent, (1, 1));
            assert_eq!(colors.len(), 2);
            let old = depth.unwrap().previous.unwrap();
            let mut saved = saved.lock().unwrap();
            if let Some(ref first) = *saved {
                assert!(Arc::ptr_eq(first, &old));
            } else {
                *saved = Some(old);
            }
            let attempt = called.fetch_add(1, Ordering::SeqCst);
            let mut layouts = vec![
                GraphicsTypedTextureDesc {
                    format: TextureFormat::Rgba8Unorm,
                    extent: (1, 1),
                    mip_levels: 1,
                    array_layers: None,
                },
                GraphicsTypedTextureDesc {
                    format: TextureFormat::Bgra8Unorm,
                    extent: (1, 1),
                    mip_levels: 1,
                    array_layers: None,
                },
            ];
            if attempt == 0 {
                layouts[1].format = TextureFormat::R32Float;
            }
            let mut depth_desc = GraphicsTypedTextureDesc {
                format: TextureFormat::Depth32Float,
                extent: (1, 1),
                mip_levels: 1,
                array_layers: None,
            };
            if attempt == 1 {
                depth_desc.format = TextureFormat::R32Float;
            }
            let bytes: Arc<[u8]> = if attempt == 2 {
                Arc::from([0u8; 3])
            } else {
                Arc::from([0u8, 0, 64, 63])
            };
            Ok(GraphicsPassOutput {
                colors: vec![vec![29, 31, 37, 41], vec![43, 47, 53, 59]],
                color_layouts: Some(layouts),
                depth: Some(Arc::new(GraphicsTypedDepthSnapshot {
                    desc: depth_desc,
                    bytes,
                })),
            })
        },
    );
    h.ctx
        .enable_resource_roles(GraphicsResourceRoleCapabilities {
            sampled_formats: 0x3ff,
            color_formats: 0x1f9,
        })
        .unwrap();
    h.ok(13, &[l, t0, 0x400, 4]);
    h.ok(13, &[l, t1, 0x400, 4]);
    assert_eq!(typed_records(&mut h, l, &[t0, t1], false), 0);
    h.ok(13, &[l, z, 0x400, 0x10]);
    h.ok(16, &[l, z, 1, 0]);
    h.ok(17, &[l, 1, 1]);
    h.ok(13, &[l, t0, 4, 0x800]);
    h.ok(13, &[l, t1, 4, 0x800]);
    h.ok(13, &[l, z, 0x10, 0x20]);
    h.ok(12, &[l]);
    for _ in 0..3 {
        assert_eq!(h.call(25, &[q, l]), 2);
        assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    }
    // Execute barriers from the original states, read all prior bytes, then
    // restore COPY_DEST before retrying the exact failed list.
    let a2 = h.ok(3, &[d]);
    let l2 = h.ok(4, &[d, a2]);
    for target in [t0, t1, z] {
        h.ok(13, &[l2, target, 0x400, 0x800]);
    }
    h.ok(12, &[l2]);
    h.ok(25, &[q, l2]);
    for (target, expected) in [
        (t0, [17, 53, 127, 193]),
        (t1, [7, 11, 13, 19]),
        (z, [0, 0, 128, 62]),
    ] {
        h.ok(10, &[target, 0, DATA, 4]);
        let mut actual = [0; 4];
        h.memory.read_bytes(DATA, &mut actual).unwrap();
        assert_eq!(actual, expected);
    }
    h.ok(11, &[l2, a2]);
    for target in [t0, t1, z] {
        h.ok(13, &[l2, target, 0x800, 0x400]);
    }
    h.ok(12, &[l2]);
    h.ok(25, &[q, l2]);
    let before_retry = h.ctx.last_presentation.lock().unwrap().frame_index;
    h.ok(25, &[q, l]);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        h.ctx.last_presentation.lock().unwrap().frame_index,
        before_retry + 1
    );
    for (target, expected) in [(t0, [29, 31, 37, 41]), (t1, [43, 47, 53, 59])] {
        h.ok(10, &[target, 0, DATA, 4]);
        let mut actual = [0; 4];
        h.memory.read_bytes(DATA, &mut actual).unwrap();
        assert_eq!(actual, expected);
    }
}

#[test]
fn cpu_typed_roles_invalid_second_record_atomic_and_backend_downgrade() {
    let mut h = Harness::new();
    let d = h.ok(1, &[]);
    let q = h.ok(2, &[d]);
    let a = h.ok(3, &[d]);
    let l = h.ok(4, &[d, a]);
    let targets = [
        h.ok(53, &[d, 1, 1, 1, 0, 0, 4]),
        h.ok(53, &[d, 1, 1, 1, 0, 5, 4]),
    ];
    let calls = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    h.ctx.set_metal_typed_attachment_backend(
        GraphicsTextureCapabilities::default(),
        move |_, _, colors, _, _| {
            called.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                colors.len(),
                1,
                "failed two-record binding replaced prior target"
            );
            assert_eq!(colors[0].typed.unwrap().format, TextureFormat::Bgra8Unorm);
            Ok(GraphicsPassOutput {
                colors: vec![vec![17, 53, 127, 193]],
                depth: None,
                color_layouts: Some(vec![GraphicsTypedTextureDesc {
                    format: TextureFormat::Bgra8Unorm,
                    extent: (1, 1),
                    mip_levels: 1,
                    array_layers: None,
                }]),
            })
        },
    );
    h.ctx
        .enable_resource_roles(GraphicsResourceRoleCapabilities {
            sampled_formats: 0x3ff,
            color_formats: 0x1f9,
        })
        .unwrap();
    assert_eq!(typed_records(&mut h, l, &targets[1..], false), 0);
    assert_eq!(typed_records(&mut h, l, &targets, true), 2);
    h.ok(17, &[l, 1, 1]);
    h.ok(12, &[l]);
    h.ok(25, &[q, l]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    h.ok(11, &[l, a]);
    assert_eq!(typed_records(&mut h, l, &targets[1..], false), 0);
    h.ok(17, &[l, 1, 1]);
    h.ok(12, &[l]);
    h.ctx.set_metal_typed_attachment_backend(
        GraphicsTextureCapabilities::default(),
        |_, _, _, _, _| panic!("downgraded role reached backend"),
    );
    assert_eq!(h.call(25, &[q, l]), 2);
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
}

#[test]
fn cpu_typed_roles_equal_size_view_casts_reject() {
    let mut h = Harness::new();
    let d = h.ok(1, &[]);
    for (format, valid, invalid) in [
        (0, 0, 12),
        (5, 8, 11),
        (7, 11, 12),
        (8, 12, 11),
        (9, 13, 12),
    ] {
        let r = h.ok(53, &[d, 1, 1, 1, 0, format, 0x400]);
        h.ok(37, &[d, r, 0, 1, valid]);
        assert_eq!(
            h.call(37, &[d, r, 0, 1, invalid]),
            2,
            "storage code {format} cast {invalid}"
        );
    }
}

#[test]
fn cpu_typed_roles_shader_resource_classes_reject_before_callback() {
    for shader_type in 0..3u64 {
        for (resource_type, format, view_code) in [(0, 7, 11), (1, 8, 12), (2, 9, 13)] {
            if shader_type == resource_type {
                continue;
            }
            let mut h = Harness::new();
            let d = h.ok(1, &[]);
            let q = h.ok(2, &[d]);
            let a = h.ok(3, &[d]);
            let l = h.ok(4, &[d, a]);
            h.ctx.set_metal_typed_attachment_backend(
                GraphicsTextureCapabilities::default(),
                |_, _, _, _, _| panic!("class mismatch reached callback"),
            );
            h.ctx
                .enable_resource_roles(GraphicsResourceRoleCapabilities {
                    sampled_formats: 0x3ff,
                    color_formats: 0x1f9,
                })
                .unwrap();
            let vs = include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin");
            let ps = include_bytes!("../../../fixtures/rdna2/x9-view-sample-ps.bin");
            h.memory.write_bytes(DATA, vs).unwrap();
            h.memory.write_bytes(DATA + 1024, ps).unwrap();
            let pipeline = h.ok(
                55,
                &[
                    d,
                    DATA,
                    vs.len() as u64,
                    DATA + 1024,
                    ps.len() as u64,
                    shader_type | if shader_type == 1 { 256 } else { 0 },
                ],
            );
            let texture = h.ok(53, &[d, 1, 1, 1, 0, format, 0x400]);
            h.upload(texture, &[0, 0, 128, 62]);
            let view = h.ok(37, &[d, texture, 0, 1, view_code]);
            let target = h.ok(
                53,
                &[d, 1, 1, 1, 0, if shader_type == 1 { 8 } else { 0 }, 4],
            );
            let words = [target, 0, 0, u64::from(shader_type == 1), 0, 0, 0, 0];
            h.memory
                .write_bytes(
                    DATA,
                    &words
                        .into_iter()
                        .flat_map(u64::to_le_bytes)
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            h.ok(56, &[l, 1, DATA]);
            let vb = h.ok(5, &[d, 48, 0x400]);
            let cb = h.ok(5, &[d, 48, 0x400]);
            let positions = [
                [-1f32, -1., 0.5, 1.],
                [3., -1., 0.5, 1.],
                [-1., 3., 0.5, 1.],
            ];
            h.upload(
                vb,
                &positions
                    .into_iter()
                    .flatten()
                    .flat_map(f32::to_le_bytes)
                    .collect::<Vec<_>>(),
            );
            h.upload(
                cb,
                &[[0.5f32, 0.5, 0., 1.]; 3]
                    .into_iter()
                    .flatten()
                    .flat_map(f32::to_le_bytes)
                    .collect::<Vec<_>>(),
            );
            for (r, state) in [(vb, 1), (cb, 1), (texture, 0x80)] {
                h.ok(13, &[l, r, 0x400, state]);
            }
            h.ok(14, &[l, pipeline]);
            h.ok(17, &[l, 1, 1]);
            h.ok(18, &[l, vb, 0, 16]);
            h.ok(19, &[l, cb, 0, 16]);
            let table = [view, 0, 1, 0, 0, 0, 0, 0, 0, 0];
            h.memory
                .write_bytes(
                    DATA,
                    &table
                        .into_iter()
                        .flat_map(u64::to_le_bytes)
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            h.ok(39, &[l, DATA]);
            h.ok(23, &[l, 3, 1, 0, 0]);
            h.ok(12, &[l]);
            assert_eq!(
                h.call(25, &[q, l]),
                2,
                "shader={shader_type} resource={resource_type}"
            );
            assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
        }
    }
}
