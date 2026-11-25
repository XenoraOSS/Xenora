// SPDX-License-Identifier: GPL-3.0-or-later
//! Independent CPU service/typed-publication checks, not GPU execution evidence.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use xenora_loader::graphics::*;
use xenora_loader::graphics_service::graphics_call_v1;
use xenora_loader::{GuestMemory, MemoryPermissions};
use xenora_texture::TextureFormat::{self, *};
const DESC: u64 = 0x1000;
const OUT: u64 = 0x2000;
const DATA: u64 = 0x3000;
const SENTINEL: u64 = 0x9182736455463728;
struct Ticket(Option<GraphicsQueuedWorkOutput>);
impl GraphicsQueuedPendingWork for Ticket {
    fn poll(&mut self) -> Option<Result<GraphicsQueuedWorkOutput, GraphicsError>> {
        self.0.take().map(Ok)
    }
    fn wait(&mut self) -> Result<GraphicsQueuedWorkOutput, GraphicsError> {
        self.poll().unwrap()
    }
    fn reject_dependency(&mut self, _: GraphicsQueuePoint) {}
}
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

fn setup(h: &mut Harness, code: u64) -> [u64; 6] {
    let d = h.ok(1, &[]);
    let q = h.ok(2, &[d]);
    let a = h.ok(3, &[d]);
    let l = h.ok(4, &[d, a]);
    let s = h.ok(53, &[d, 5, 3, 3, 2, code, 0x400]);
    let t = h.ok(53, &[d, 5, 3, 3, 2, code, 0x400]);
    [d, q, a, l, s, t]
}
fn record(h: &mut Harness, l: u64, s: u64, t: u64, bc: bool) {
    h.ok(13, &[l, s, 0x400, 0x800]);
    h.ok(
        54,
        &[
            l,
            t,
            0,
            (1u64 << 32) | if bc { 1 } else { 2 },
            0,
            0,
            s,
            0,
            1,
            u64::from(!bc),
            0,
            if bc { 2 } else { 1 },
            1,
        ],
    );
    h.ok(13, &[l, t, 0x400, 0x800]);
    h.ok(12, &[l]);
}
fn cases() -> [(TextureFormat, usize, usize, usize, &'static [u8]); 10] {
    [
        (Rgba8Unorm, 144, 64, 140, &[17, 53, 127, 193]),
        (
            Bc1RgbaUnorm,
            64,
            16,
            48,
            &[0, 248, 224, 7, 228, 27, 177, 78],
        ),
        (
            Bc3RgbaUnorm,
            128,
            32,
            96,
            &[
                231, 17, 136, 198, 250, 136, 198, 250, 31, 0, 0, 248, 228, 27, 177, 78,
            ],
        ),
        (R8Unorm, 36, 16, 35, &[127]),
        (Rg8Unorm, 72, 32, 70, &[17, 193]),
        (Bgra8Unorm, 144, 64, 140, &[17, 53, 127, 193]),
        (
            Rgba16Float,
            288,
            128,
            280,
            &[0, 188, 0, 64, 0, 128, 53, 126],
        ),
        (R32Float, 144, 64, 140, &[69, 35, 193, 127]),
        (R32Uint, 144, 64, 140, &[1, 0, 0, 1]),
        (Depth32Float, 144, 64, 140, &[69, 35, 193, 127]),
    ]
}
#[test]
fn cpu_typed_transfers_all_formats_exact_layout_bits_retirement_refund() {
    for (code, (format, size, source_offset, dest_offset, pattern)) in
        cases().into_iter().enumerate()
    {
        eprintln!("typed transfer format={format:?} guest_code={code} bytes={size}");
        let mut h = Harness::new();
        let [_, q, a, l, s, t] = setup(&mut h, code as u64);
        let mut source = vec![0x39; size];
        source[source_offset..source_offset + pattern.len()].copy_from_slice(pattern);
        let prior = vec![0xa5; size];
        let mut expected = prior.clone();
        expected[dest_offset..dest_offset + pattern.len()].copy_from_slice(pattern);
        h.upload(s, &source);
        h.upload(t, &prior);
        record(&mut h, l, s, t, code == 1 || code == 2);
        let answer = expected.clone();
        let descriptor = GraphicsTypedTextureDesc {
            format,
            extent: (5, 3),
            mip_levels: 3,
            array_layers: Some(2),
        };
        h.ctx.set_metal_typed_copy_queued_backend(
            GraphicsTextureCapabilities::default(),
            GraphicsTypedCopyCapabilities { formats: 0x3ff },
            move |work| {
                let GraphicsQueuedWorkRequest::ImageCopy(r) = work else {
                    panic!("image")
                };
                assert_eq!(
                    r.source_layout,
                    GraphicsCopyLayout::TypedTexture(descriptor)
                );
                assert_eq!(r.destination_layout, r.source_layout);
                let GraphicsCopyInput::Host(src) = r.source else {
                    panic!("host")
                };
                let GraphicsCopyInput::Host(dst) = r.destination else {
                    panic!("host")
                };
                assert_eq!(src.as_ref(), source);
                assert_eq!(dst.as_ref(), prior);
                Ok(Box::new(Ticket(Some(GraphicsQueuedWorkOutput::Copy {
                    layout: r.destination_layout,
                    bytes: answer.clone(),
                }))))
            },
        );
        let receipt = h.ok(35, &[q, l, 0, 0]);
        h.ok(11, &[l, a]);
        h.ok(26, &[s]);
        assert_eq!(h.ok(32, &[receipt]), 1);
        h.ok(33, &[receipt]);
        h.ok(10, &[t, 0, DATA, size as u64]);
        let mut bytes = vec![0; size];
        h.memory.read_bytes(DATA, &mut bytes).unwrap();
        assert_eq!(bytes, expected, "{format:?}");
        assert_eq!(h.ctx.queued_copy_allocation_bytes(), 0);
        assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
    }
}
#[test]
fn cpu_typed_transfers_equal_size_wrong_format_shape_and_length_rollback() {
    for bad in 0..3 {
        let mut h = Harness::new();
        let [_, q, a, l, s, t] = setup(&mut h, 8);
        let mut source = [0x39; 144];
        source[64..68].copy_from_slice(&[1, 0, 0, 1]);
        h.upload(s, &source);
        h.upload(t, &[0xa5; 144]);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        h.ctx.set_metal_typed_copy_queued_backend(
            GraphicsTextureCapabilities::default(),
            GraphicsTypedCopyCapabilities { formats: 0x100 },
            move |work| {
                let GraphicsQueuedWorkRequest::ImageCopy(r) = work else {
                    panic!("image")
                };
                let GraphicsCopyInput::Host(prior) = r.destination else {
                    panic!("prior")
                };
                assert_eq!(prior.as_ref(), &[0xa5; 144]);
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                let mut bytes = vec![0xa5; 144];
                bytes[140..144].copy_from_slice(&[1, 0, 0, 1]);
                let mut d = GraphicsTypedTextureDesc {
                    format: R32Uint,
                    extent: (5, 3),
                    mip_levels: 3,
                    array_layers: Some(2),
                };
                if attempt == 0 {
                    match bad {
                        0 => d.format = R32Float,
                        1 => {
                            d.extent = (6, 3);
                            d.mip_levels = 1;
                        }
                        _ => {
                            bytes.pop();
                        }
                    }
                }
                Ok(Box::new(Ticket(Some(GraphicsQueuedWorkOutput::Copy {
                    layout: GraphicsCopyLayout::TypedTexture(d),
                    bytes,
                }))))
            },
        );
        for expected in [2, 1] {
            record(&mut h, l, s, t, false);
            let receipt = h.ok(35, &[q, l, 0, 0]);
            assert_eq!(h.ok(32, &[receipt]), expected);
            h.ok(33, &[receipt]);
            assert_eq!(h.ctx.queued_copy_allocation_bytes(), 0);
            h.ok(11, &[l, a]);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
#[test]
fn cpu_typed_transfers_capability_mask_and_old_setter_revoke_before_dispatch() {
    for mask in [0, 1, 0x8000] {
        let mut h = Harness::new();
        let [_, q, _, l, s, t] = setup(&mut h, 8);
        record(&mut h, l, s, t, false);
        h.ctx.set_metal_typed_copy_queued_backend(
            GraphicsTextureCapabilities::default(),
            GraphicsTypedCopyCapabilities { formats: mask },
            |_| panic!("unsupported typed format dispatched"),
        );
        assert_eq!(h.call(35, &[q, l, 0, 0]), 2);
        assert_eq!(h.result(), SENTINEL);
        assert_eq!(h.ctx.queued_copy_allocation_bytes(), 0);
    }
    let mut h = Harness::new();
    let [_, q, _, l, s, t] = setup(&mut h, 8);
    record(&mut h, l, s, t, false);
    h.ctx.set_metal_typed_copy_queued_backend(
        GraphicsTextureCapabilities::default(),
        GraphicsTypedCopyCapabilities { formats: 0x3ff },
        |_| panic!("unused"),
    );
    h.ctx
        .set_metal_array_copy_queued_backend(GraphicsTextureCapabilities::default(), |_| {
            panic!("old backend cannot dispatch typed copy")
        });
    assert_eq!(h.call(35, &[q, l, 0, 0]), 2);
}
#[test]
fn cpu_typed_transfers_service_descriptor_and_location_rejections() {
    let mut h = Harness::new();
    let [d, _, _, l, s, t] = setup(&mut h, 8);
    for args in [
        [d, 5, 3, 3, 2, 10, 0x400],
        [d, 5, 3, 3, 257, 8, 0x400],
        [d, 5, 3, 3, 1u64 << 32, 8, 0x400],
        [d, 4096, 4096, 1, 2, 8, 0x400],
        [d, 5, 3, 3, 2, 8, 1u64 << 32],
    ] {
        assert_eq!(h.call(53, &args), 2);
        assert_eq!(h.result(), SENTINEL);
    }
    assert_eq!(h.call(50, &[l, t, 0, 0, 0, 0, s, 0, 0, 0, 0, 1, 1]), 2);
    let buffer = h.ok(5, &[d, 12, 0x800]);
    assert_eq!(h.call(54, &[l, t, 0, 1, 0, 0, buffer, 1, 1, 4, 8, 1, 1]), 2);
    assert_eq!(h.call(54, &[l, t, 0, 3, 0, 0, s, 0, 1, 0, 0, 1, 1]), 2);
    assert_eq!(
        h.call(54, &[l, s, 0, 1 << 32, 0, 0, s, 0, 1, 0, 0, 1, 1]),
        2
    );
    for (opcode, args, reserved) in [
        (53, vec![d, 5, 3, 3, 2, 8, 0x400], 72),
        (54, vec![l, t, 0, 1, 0, 0, s, 0, 1, 0, 0, 1, 1], 120),
    ] {
        h.prepare(&args);
        h.memory
            .write_bytes(DESC + 8, &(opcode as u64).to_le_bytes())
            .unwrap();
        h.memory
            .write_bytes(DESC + reserved, &1u64.to_le_bytes())
            .unwrap();
        assert_eq!(
            graphics_call_v1(&mut h.ctx, &mut h.memory, DESC, 128, OUT).unwrap(),
            1
        );
        assert_eq!(h.result(), SENTINEL);
    }
}
