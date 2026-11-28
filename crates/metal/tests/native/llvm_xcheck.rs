// SPDX-License-Identifier: GPL-3.0-or-later

use xenora_metal::NativeCompute;
use xenora_shader::{Architecture, ENTRY_POINT, compile};

const CODE: &[u8] = include_bytes!("../../../../fixtures/rdna2/llvm-xcheck.bin");
const INPUTS: [u32; 10] = [
    0x0000_0000,
    0x0000_0001,
    0x0000_0011,
    0x00ab_cdef,
    0x00ff_ffff,
    0x0100_0000,
    0x8000_0001,
    0xffff_ffff,
    0x0042_f6e9,
    0x00d1_5ea5,
];
// Independently derived by evaluating the integer recurrence in
// fixtures/llvm-xcheck/xcheck.ll with wrapping unsigned 32-bit arithmetic.
const EXPECTED: [u32; 10] = [
    0x00000000, 0xd6dee183, 0xf5d2d033, 0xdf5d9f59, 0x83b39969, 0x01000000, 0x56dee183, 0x7cb39969,
    0xcfe62f5b, 0x5c6db58f,
];

#[test]
fn llvm_gfx1030_integer_core_matches_known_answers_on_metal() {
    let shader =
        compile(Architecture::Gfx1030, CODE).expect("translate LLVM-derived gfx1030 fixture");
    for instruction in [
        "v_mul_u32_u24_e32 v4, 0x123456, v3",
        "v_xor_b32_e32 v4, v4, v3",
        "v_mul_u32_u24_e32 v3, 0x15944c, v4",
        "v_xor_b32_e32 v4, v3, v4",
    ] {
        assert!(
            shader.disassembly.contains(instruction),
            "{instruction}\n{}",
            shader.disassembly
        );
    }

    let context = NativeCompute::new().expect("native Metal device and queue");
    let output = context
        .run_words(&shader.msl, ENTRY_POINT, &INPUTS)
        .expect("complete translated Metal dispatch and readback");
    assert_eq!(output, EXPECTED);
}
