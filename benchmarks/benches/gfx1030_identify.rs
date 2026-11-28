// SPDX-License-Identifier: GPL-3.0-or-later

//! CPU-only gfx1030 metadata benchmark.
//!
//! Run `cargo bench --locked -p xenora-benchmarks --bench gfx1030_identify`
//! from the repository root. Repeat the command to collect independent
//! first-pass samples. The first pass is the first identification in a fresh
//! process with the input already in memory. It does not measure process
//! startup or guarantee cold hardware caches. Repeated passes use the same
//! stream after the untimed correctness check.
//!
//! To verify the fixture bytes and instruction lengths independently, run:
//! ```sh
//! llvm-mc -triple=amdgcn-amd-amdhsa -mcpu=gfx1030 -mattr=+wavefrontsize32 \
//!     -show-encoding benchmarks/benches/fixtures/gfx1030_mixed.s
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use xenora_isa::meta::{EncodingFamily, Identified, identify};

const COPIES: usize = 4096;
const WARM_PASSES: usize = 9;

// Encodings and lengths come from llvm-mc, one row per instruction in
// fixtures/gfx1030_mixed.s. Each word is stored in little-endian order.
const FIXTURE: &[(&str, EncodingFamily, &[u32])] = {
    use EncodingFamily::*;
    &[
        ("S_MOV_B32", Sop1, &[0xbe8c03ff, 0x89abcdef]),
        ("V_ADD_F32", Vop2, &[0x06081305]),
        ("S_CMP_LT_U32", Sopc, &[0xbf0a0d0c]),
        ("DS_READ_B32", Ds, &[0xd8d8000c, 0x08000002]),
        ("V_MOV_B32", Vop1, &[0x7e1202fa, 0xff004e06]),
        ("S_ADD_U32", Sop2, &[0x80101211]),
        ("BUFFER_STORE_DWORD", Mubuf, &[0xe0700000, 0x80010700]),
        ("V_FMA_F32", Vop3, &[0xd54b0005, 0x03fe1508, 0x3f800003]),
        ("S_MOVK_I32", Sopk, &[0xb0132345]),
        ("GLOBAL_LOAD_DWORD", Global, &[0xdc308000, 0x037d000a]),
        ("V_CMP_LT_U32", Vopc, &[0x7d820f03]),
        ("S_LOAD_DWORD", Smem, &[0xf400050b, 0xfa000020]),
        ("V_PK_ADD_F16", Vop3p, &[0xcc0f4003, 0x1a020d04]),
        ("S_WAITCNT", Sopp, &[0xbf8c3f70]),
    ]
};

fn walk(stream: &[u8], mut observe: impl FnMut(usize, Identified)) -> usize {
    let mut offset = 0;
    let mut count = 0;
    while offset < stream.len() {
        let identified = identify(&stream[offset..])
            .unwrap_or_else(|error| panic!("identification failed at byte {offset}: {error}"));
        observe(offset, identified);
        offset += identified.len;
        count += 1;
    }
    count
}

fn verify(stream: &[u8]) {
    let mut expected_offset = 0;
    let mut expected = FIXTURE.iter().cycle();
    let count = walk(stream, |offset, identified| {
        let &(name, family, words) = expected.next().expect("fixture is nonempty");
        let len = std::mem::size_of_val(words);
        assert_eq!(offset, expected_offset, "byte boundary for {name}");
        assert_eq!(identified.len, len, "length of {name} at byte {offset}");
        assert_eq!(identified.form.name, name, "mnemonic at byte {offset}");
        assert_eq!(identified.form.family, family, "family at byte {offset}");
        expected_offset += len;
    });
    assert_eq!(expected_offset, stream.len());
    assert_eq!(count, COPIES * FIXTURE.len());
}

fn measure(stream: &[u8]) -> Duration {
    let start = Instant::now();
    let count = walk(black_box(stream), |_, identified| {
        black_box(identified);
    });
    let elapsed = start.elapsed();
    assert_eq!(count, COPIES * FIXTURE.len());
    elapsed
}

fn report(label: &str, elapsed: Duration, byte_count: usize) {
    let seconds = elapsed.as_secs_f64();
    let count = (COPIES * FIXTURE.len()) as f64;
    println!(
        "{label}: {:.3} ms, {:.2} ns/instruction, {:.2} MiB/s",
        seconds * 1e3,
        seconds * 1e9 / count,
        byte_count as f64 / (1024.0 * 1024.0) / seconds
    );
}

fn main() {
    let block: Vec<u8> = FIXTURE
        .iter()
        .flat_map(|(_, _, words)| words.iter().flat_map(|word| word.to_le_bytes()))
        .collect();
    let stream = block.repeat(COPIES);

    // 'cargo test --all-targets' checks the fixture without timing it.
    if !std::env::args().any(|arg| arg == "--bench") {
        verify(&stream);
        return;
    }

    // Keep the first pass ahead of any metadata lookups. Validate the entire
    // stream before reporting any timings. That validation also warms the path.
    let first = measure(&stream);
    verify(&stream);
    let mut warm = [Duration::ZERO; WARM_PASSES];
    for sample in &mut warm {
        *sample = measure(&stream);
    }
    warm.sort_unstable();

    println!(
        "gfx1030 metadata identification: {} instructions, {} bytes, {} repeated passes",
        COPIES * FIXTURE.len(),
        stream.len(),
        WARM_PASSES
    );
    report("First pass", first, stream.len());
    report("Repeated minimum", warm[0], stream.len());
    report("Repeated median", warm[WARM_PASSES / 2], stream.len());
    report("Repeated maximum", warm[WARM_PASSES - 1], stream.len());
}
