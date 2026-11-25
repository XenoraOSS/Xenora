// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU metadata/lifetime evidence; callback colors are not GPU sampling evidence.
use std::sync::{Arc, Mutex};
use xenora_loader::graphics::*;
use xenora_loader::graphics_service::graphics_call_v1;
use xenora_loader::{GuestMemory, MemoryPermissions};
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
        for (a, n) in [(DESC, 128), (OUT, 8), (DATA, 8192)] {
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
    fn setup(&mut self) -> [u64; 6] {
        let d = self.ok(1, &[]);
        let q = self.ok(2, &[d]);
        let a = self.ok(3, &[d]);
        let l = self.ok(4, &[d, a]);
        let s = self.ok(51, &[d, 5, 3, 3, 2, 0x400]);
        let t = self.ok(51, &[d, 5, 3, 3, 2, 0x400]);
        [d, q, a, l, s, t]
    }
    fn upload(&mut self, r: u64, b: &[u8]) {
        self.memory.write_bytes(DATA, b).unwrap();
        self.ok(9, &[r, 0, DATA, b.len() as u64]);
    }
}

fn record_draw(h: &mut Harness, d: u64, l: u64, texture: u64, view: u64) {
    let vs = include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin");
    let ps = include_bytes!("../../../fixtures/rdna2/x9-view-sample-ps.bin");
    h.memory.write_bytes(DATA, vs).unwrap();
    h.memory.write_bytes(DATA + 1024, ps).unwrap();
    let pipeline = h.ok(8, &[d, DATA, vs.len() as u64, DATA + 1024, ps.len() as u64]);
    let vb = h.ok(5, &[d, 48, 0x400]);
    let cb = h.ok(5, &[d, 48, 0x400]);
    let positions = [[-1f32, -1., 0., 1.], [3., -1., 0., 1.], [-1., 3., 0., 1.]];
    let colors = [[0.25f32, 0.5, 0., 1.]; 3];
    h.upload(
        vb,
        &positions
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    );
    h.upload(
        cb,
        &colors
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    );
    let target = h.ok(6, &[d, 2, 1, 1, 4]);
    let mut targets = [0u8; 16];
    targets[..8].copy_from_slice(&target.to_le_bytes());
    h.memory.write_bytes(DATA + 4096, &targets).unwrap();
    h.ok(15, &[l, 1, DATA + 4096]);
    h.ok(13, &[l, texture, 0x400, 0x80]);
    h.ok(13, &[l, vb, 0x400, 1]);
    h.ok(13, &[l, cb, 0x400, 1]);
    h.ok(17, &[l, 2, 1]);
    h.ok(14, &[l, pipeline]);
    h.ok(18, &[l, vb, 0, 16]);
    h.ok(19, &[l, cb, 0, 16]);
    let mut table = [0u64; 10];
    table[0] = view;
    table[2] = 1;
    h.memory
        .write_bytes(
            DATA + 4096,
            &table
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        )
        .unwrap();
    h.ok(39, &[l, DATA + 4096]);
    h.ok(23, &[l, 3, 1, 0, 0]);
    h.ok(13, &[l, texture, 0x80, 0x400]);
    h.ok(12, &[l]);
}
#[test]
fn cpu_array_views_nested_root_lifetime_bounds_and_service_envelope() {
    let mut h = Harness::new();
    let [d, _, _, list, root, _] = h.setup();
    let p = h.ok(52, &[d, root, 1, 1, 2, 1]);
    assert_eq!(h.call(37, &[d, root, 0, 3, 0]), 2);
    assert_eq!(h.call(21, &[list, root, 0, 0, 0]), 2);
    for args in [
        [d, root, 2, 0, 1, 0],
        [d, root, 1, 3, 1, 0],
        [d, root, 1, 0, 0, 0],
        [d, root, 1, 0, 1, 2],
        [d, root, 1u64 << 32, 0, 1, 0],
    ] {
        assert_eq!(h.call(52, &args), 2);
        assert_eq!(h.result(), SENTINEL);
    }
    for (base, count) in [(2, 1), (1, 2)] {
        assert_eq!(h.call(45, &[d, p, base, count, 0]), 2);
    }
    let child = h.ok(45, &[d, p, 1, 1, 0]);
    h.ok(38, &[p]);
    assert_eq!(h.call(26, &[root]), 2);
    let grandchild = h.ok(45, &[d, child, 0, 1, 1]);
    h.ok(38, &[child]);
    assert_eq!(h.call(26, &[root]), 2);
    h.ok(38, &[grandchild]);
    h.ok(26, &[root]);
    let ordinary = h.ok(6, &[d, 5, 3, 3, 0x400]);
    assert_eq!(h.call(52, &[d, ordinary, 0, 0, 1, 0]), 2);
    h.prepare(&[d, ordinary, 0, 0, 1, 0]);
    h.memory
        .write_bytes(DESC + 8, &52u64.to_le_bytes())
        .unwrap();
    h.memory
        .write_bytes(DESC + 64, &1u64.to_le_bytes())
        .unwrap();
    assert_eq!(
        graphics_call_v1(&mut h.ctx, &mut h.memory, DESC, 128, OUT).unwrap(),
        1
    );
    assert_eq!(h.result(), SENTINEL);
}
#[test]
fn cpu_array_views_full_immutable_snapshots_exclusive_metadata_and_array_one() {
    for layers in [1, 2] {
        let mut h = Harness::new();
        let [d, q, a, l, _, _] = h.setup();
        let root = h.ok(51, &[d, 5, 3, 3, layers, 0x400]);
        let parent = h.ok(52, &[d, root, layers - 1, 1, 2, 1]);
        let child = h.ok(45, &[d, parent, 1, 1, 0]);
        h.ok(38, &[parent]);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let capture = seen.clone();
        h.ctx.set_metal_array_view_attachment_backend(
            GraphicsTextureCapabilities::default(),
            move |draws, extent, _, _, _| {
                assert_eq!(extent, (2, 1));
                assert_eq!(draws.len(), 1);
                let b = &draws[0].sampled_bindings;
                assert!(b.views[0].is_none());
                assert!(b.array_views[1].is_none());
                let view = b.array_views[0].unwrap();
                assert_eq!(
                    (view.slice, view.base_mip, view.mip_count),
                    (layers as u32 - 1, 2, 1)
                );
                assert_eq!(view.format, GraphicsTextureViewFormat::Rgba8Unorm);
                let snapshot = b.textures[0].as_ref().unwrap();
                assert_eq!(
                    (
                        snapshot.width,
                        snapshot.height,
                        snapshot.mip_levels,
                        snapshot.array_layers
                    ),
                    (5, 3, 3, Some(layers as u32))
                );
                assert_eq!(snapshot.pixels.len(), 72 * layers as usize);
                capture.lock().unwrap().push(snapshot.pixels.clone());
                Ok(GraphicsPassOutput {
                    colors: vec![vec![19; 8]],
                    depth: None,
                })
            },
        );
        for value in [17, 31] {
            h.upload(root, &vec![value; 72 * layers as usize]);
            record_draw(&mut h, d, l, root, child);
            assert_eq!(h.ok(25, &[q, l]), 1);
            h.ok(11, &[l, a]);
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].as_ref(), vec![17; 72 * layers as usize]);
        assert_eq!(seen[1].as_ref(), vec![31; 72 * layers as usize]);
        assert!(!Arc::ptr_eq(&seen[0], &seen[1]));
    }
}
#[test]
fn cpu_array_views_capability_downgrade_rejects_before_callback() {
    let mut h = Harness::new();
    let [d, q, _, l, root, _] = h.setup();
    let view = h.ok(52, &[d, root, 0, 0, 3, 0]);
    h.upload(root, &[17; 144]);
    record_draw(&mut h, d, l, root, view);
    h.ctx.set_metal_array_view_attachment_backend(
        GraphicsTextureCapabilities::default(),
        |_, _, _, _, _| panic!("unused"),
    );
    h.ctx.set_metal_uniform_attachment_backend(
        GraphicsTextureCapabilities::default(),
        |_, _, _, _, _| panic!("array view dispatched through downgraded backend"),
    );
    assert_eq!(h.call(25, &[q, l]), 2);
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
}
