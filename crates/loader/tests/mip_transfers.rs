// SPDX-License-Identifier: GPL-3.0-or-later
//! Independent CPU service/transaction checks. The callback is not GPU evidence.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use xenora_loader::graphics::*;
use xenora_loader::graphics_service::graphics_call_v1;
use xenora_loader::{GuestMemory, MemoryPermissions};
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
        for (a, n) in [(DESC, 128), (OUT, 8), (DATA, 256)] {
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
        let s = self.ok(6, &[d, 5, 3, 3, 0x400]);
        let t = self.ok(6, &[d, 5, 3, 3, 0x400]);
        [d, q, a, l, s, t]
    }
    fn upload(&mut self, r: u64, b: &[u8]) {
        self.memory.write_bytes(DATA, b).unwrap();
        self.ok(9, &[r, 0, DATA, b.len() as u64]);
    }
    fn record(&mut self, l: u64, s: u64, t: u64) {
        self.ok(13, &[l, s, 0x400, 0x800]);
        self.ok(50, &[l, t, 0, 0, 3, 2, s, 0, 1, 1, 0, 1, 1]);
        self.ok(13, &[l, t, 0x400, 0x800]);
        self.ok(12, &[l]);
    }
}
fn expected() -> Vec<u8> {
    let mut b = vec![0x59; 72];
    b[52..56].copy_from_slice(&[17, 53, 127, 193]);
    b
}
fn install(h: &mut Harness, bad: usize, calls: Arc<AtomicUsize>) {
    h.ctx
        .set_metal_mip_copy_queued_backend(GraphicsTextureCapabilities::default(), move |work| {
            calls.fetch_add(1, Ordering::SeqCst);
            let GraphicsQueuedWorkRequest::ImageCopy(r) = work else {
                panic!("image copy")
            };
            assert_eq!(
                r.source_layout,
                GraphicsCopyLayout::Rgba8MipChain {
                    extent: (5, 3),
                    mip_levels: 3
                }
            );
            assert_eq!(r.destination_layout, r.source_layout);
            assert_eq!(
                r.source_location,
                GraphicsImageCopyLocation::Rgba8Mip {
                    mip: 1,
                    slice: 0,
                    origin: (1, 0)
                }
            );
            assert_eq!(
                r.destination_location,
                GraphicsImageCopyLocation::Rgba8Mip {
                    mip: 0,
                    slice: 0,
                    origin: (3, 2)
                }
            );
            let GraphicsCopyInput::Host(prior) = r.destination else {
                panic!("host prior")
            };
            assert_eq!(prior.as_ref(), &[0x59; 72]);
            let mut bytes = expected();
            if bad == 1 {
                bytes.pop();
            }
            Ok(Box::new(Ticket(Some(GraphicsQueuedWorkOutput::Copy {
                layout: if bad == 2 {
                    GraphicsCopyLayout::Rgba8 { extent: (6, 3) }
                } else {
                    r.destination_layout
                },
                bytes,
            }))))
        });
}
#[test]
fn cpu_mip_full_chain_publication_retirement_and_refund() {
    let mut h = Harness::new();
    let [_, q, a, l, s, t] = h.setup();
    let mut src = [0x27; 72];
    src[64..68].copy_from_slice(&[17, 53, 127, 193]);
    h.upload(s, &src);
    h.upload(t, &[0x59; 72]);
    h.record(l, s, t);
    let calls = Arc::new(AtomicUsize::new(0));
    install(&mut h, 0, calls.clone());
    let receipt = h.ok(35, &[q, l, 0, 0]);
    assert_eq!(h.ctx.queued_copy_allocation_bytes(), 288);
    h.ok(11, &[l, a]);
    h.ok(26, &[s]);
    assert_eq!(h.ok(32, &[receipt]), 1);
    h.ok(33, &[receipt]);
    assert_eq!(h.ctx.queued_copy_allocation_bytes(), 0);
    h.ok(10, &[t, 0, DATA, 72]);
    let mut actual = [0; 72];
    h.memory.read_bytes(DATA, &mut actual).unwrap();
    assert_eq!(actual.as_slice(), expected());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(h.ctx.last_presentation.lock().unwrap().frame_index, 0);
}
#[test]
fn cpu_mip_malformed_completion_rolls_back_and_recovery_uses_original_prior() {
    for bad in [1, 2] {
        let mut h = Harness::new();
        let [_, q, a, l, s, t] = h.setup();
        let mut src = [0x27; 72];
        src[64..68].copy_from_slice(&[17, 53, 127, 193]);
        h.upload(s, &src);
        h.upload(t, &[0x59; 72]);
        h.record(l, s, t);
        let calls = Arc::new(AtomicUsize::new(0));
        install(&mut h, bad, calls.clone());
        let receipt = h.ok(35, &[q, l, 0, 0]);
        assert_eq!(h.ok(32, &[receipt]), 2);
        h.ok(33, &[receipt]);
        assert_eq!(h.ctx.queued_copy_allocation_bytes(), 0);
        h.ok(11, &[l, a]);
        h.record(l, s, t);
        install(&mut h, 0, calls.clone());
        let receipt = h.ok(35, &[q, l, 0, 0]);
        assert_eq!(h.ok(32, &[receipt]), 1);
        h.ok(33, &[receipt]);
        h.ok(10, &[t, 0, DATA, 72]);
        let mut b = [0; 72];
        h.memory.read_bytes(DATA, &mut b).unwrap();
        assert_eq!(b.as_slice(), expected());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
#[test]
fn cpu_mip_backend_downgrade_rejects_before_dispatch() {
    let mut h = Harness::new();
    let [_, q, _, l, s, t] = h.setup();
    h.record(l, s, t);
    install(&mut h, 0, Arc::new(AtomicUsize::new(0)));
    h.ctx
        .set_metal_image_copy_queued_backend(GraphicsTextureCapabilities::default(), |_| {
            panic!("downgraded callback must not receive explicit mip work")
        });
    assert_eq!(h.call(35, &[q, l, 0, 0]), 2);
    assert_eq!(h.result(), SENTINEL);
    assert_eq!(h.ctx.queued_copy_allocation_bytes(), 0);
}
#[test]
fn cpu_mip_service_envelope_and_packed_buffer_subresource_reject_atomically() {
    let mut h = Harness::new();
    let [d, _, _, l, s, t] = h.setup();
    let buffer = h.ok(5, &[d, 24, 0x800]);
    let valid = [l, t, 0, 0, 0, 0, s, 0, 1, 0, 0, 1, 1];
    for len in [0, 127, 129] {
        h.prepare(&valid);
        assert_eq!(
            graphics_call_v1(&mut h.ctx, &mut h.memory, DESC, len, OUT).unwrap(),
            1
        );
        assert_eq!(h.result(), SENTINEL);
    }
    h.prepare(&valid);
    h.memory
        .write_bytes(DESC + 120, &1u64.to_le_bytes())
        .unwrap();
    assert_eq!(
        graphics_call_v1(&mut h.ctx, &mut h.memory, DESC, 128, OUT).unwrap(),
        1
    );
    assert_eq!(h.result(), SENTINEL);
    for packed in [1, 1 << 32, u64::MAX] {
        assert_eq!(
            h.call(50, &[l, t, 0, 0, 0, 0, buffer, 1, packed, 4, 12, 2, 2]),
            2
        );
        assert_eq!(h.result(), SENTINEL);
    }
    assert_eq!(
        h.call(50, &[l, t, 0, 1 << 32, 0, 0, s, 0, 1, 0, 0, 1, 1]),
        2
    );
    // Bounds use the selected 2x1 level, not the 5x3 root.
    assert_eq!(h.call(50, &[l, t, 0, 1, 2, 0, s, 0, 1, 0, 0, 1, 1]), 2);
    h.ok(50, &[l, t, 0, 0, 0, 0, buffer, 1, 0, 4, 12, 2, 2]);
}
