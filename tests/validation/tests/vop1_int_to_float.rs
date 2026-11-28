// SPDX-License-Identifier: GPL-3.0-or-later

//! Independent per-lane expected values for the six 32-bit VOP1 integer to
//! binary32 conversions (#785): `v_cvt_f32_i32`, `v_cvt_f32_u32` and
//! `v_cvt_f32_ubyte0`-`3`, all `_e32`, Wave32.
//!
//! The expectations come from the public AMD "RDNA 2" Instruction Set
//! Architecture Reference Guide only: section 12.8 "VOP1 Instructions" (page
//! 138: `D.f = (float)S0.i` and `D.f = (float)S0.u`, "0.5ULP accuracy"; page
//! 139: `D.f = (float)(S0.u[7:0])` and the other three bytes), sections 3.5
//! and 6.4 (pages 15 and 47: MODE.FP_ROUND[1:0], 0 = nearest even), section
//! 13.3.2 for the VOP1 word and section 6.2 for SRC0. `KNOWN` is a table of
//! S0 and D written from those definitions; `expect` widens S0 exactly to
//! binary64 and rounds once to binary32. Nothing here comes from the ISA
//! crate, the registry, the decoder or the shader compiler.
//!
//! Readings of the guide (not hardware evidence):
//! - Pinned mode: MODE.FP_ROUND single precision = 0, round to nearest, ties
//!   to even. "0.5ULP accuracy" is that rounding. An I32 or U32 magnitude
//!   from 2^24 up can be inexact (2^24 + 1 gives 2^24). No MODE-writing
//!   instruction is in the subset, so the other three modes cannot be
//!   selected and are outside this contract. A byte is always exact.
//! - Integer inline constants (selectors 128-208) are integer sources and
//!   convert as their values (-16 gives -16.0). Float inline constants stay
//!   outside the VOP1 subset.
//! - No input is refused: every 32-bit integer has a binary32 result.
//! - None of the six writes VCC or SCC; inactive lanes keep their VGPRs
//!   (section 3.3).

#[path = "operand_domains/harness.rs"]
#[allow(dead_code)]
mod harness;
#[path = "../independent_ref.rs"]
mod independent_ref;

use std::collections::BTreeMap;

use harness::{Case, words};
use xenora_shader::{Architecture, CompileError, compile};

/// The six forms with their ENC_VOP1 opcodes (guide 12.8).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Form {
    I32 = 5,
    U32 = 6,
    Ubyte0 = 17,
    Ubyte1 = 18,
    Ubyte2 = 19,
    Ubyte3 = 20,
}

const FORMS: [(Form, &str); 6] = [
    (Form::I32, "v_cvt_f32_i32"),
    (Form::U32, "v_cvt_f32_u32"),
    (Form::Ubyte0, "v_cvt_f32_ubyte0"),
    (Form::Ubyte1, "v_cvt_f32_ubyte1"),
    (Form::Ubyte2, "v_cvt_f32_ubyte2"),
    (Form::Ubyte3, "v_cvt_f32_ubyte3"),
];

/// S0 and the expected D for I32, U32 and UBYTE0-3.
#[rustfmt::skip]
const KNOWN: [(u32, [u32; 6]); 21] = [
    (0x0000_0000, [0x0000_0000, 0x0000_0000, 0x0000_0000, 0x0000_0000, 0x0000_0000, 0x0000_0000]), // 0
    (0x0000_0001, [0x3f80_0000, 0x3f80_0000, 0x3f80_0000, 0x0000_0000, 0x0000_0000, 0x0000_0000]), // 1
    (0xffff_ffff, [0xbf80_0000, 0x4f80_0000, 0x437f_0000, 0x437f_0000, 0x437f_0000, 0x437f_0000]), // -1, UINT_MAX -> 2^32
    (0xffff_fff0, [0xc180_0000, 0x4f80_0000, 0x4370_0000, 0x437f_0000, 0x437f_0000, 0x437f_0000]), // -16
    (0x8000_0000, [0xcf00_0000, 0x4f00_0000, 0x0000_0000, 0x0000_0000, 0x0000_0000, 0x4300_0000]), // INT_MIN, 2^31
    (0x8000_0001, [0xcf00_0000, 0x4f00_0000, 0x3f80_0000, 0x0000_0000, 0x0000_0000, 0x4300_0000]), // INT_MIN + 1
    (0x7fff_ffff, [0x4f00_0000, 0x4f00_0000, 0x437f_0000, 0x437f_0000, 0x437f_0000, 0x42fe_0000]), // 2^31 - 1 -> 2^31
    (0x00ff_ffff, [0x4b7f_ffff, 0x4b7f_ffff, 0x437f_0000, 0x437f_0000, 0x437f_0000, 0x0000_0000]), // 2^24 - 1, exact
    (0x0100_0000, [0x4b80_0000, 0x4b80_0000, 0x0000_0000, 0x0000_0000, 0x0000_0000, 0x3f80_0000]), // 2^24
    (0x0100_0001, [0x4b80_0000, 0x4b80_0000, 0x3f80_0000, 0x0000_0000, 0x0000_0000, 0x3f80_0000]), // 2^24 + 1, tie down to even
    (0x0100_0003, [0x4b80_0002, 0x4b80_0002, 0x4040_0000, 0x0000_0000, 0x0000_0000, 0x3f80_0000]), // 2^24 + 3, tie up to even
    (0xff00_0000, [0xcb80_0000, 0x4f7f_0000, 0x0000_0000, 0x0000_0000, 0x0000_0000, 0x437f_0000]), // -2^24
    (0xfeff_ffff, [0xcb80_0000, 0x4f7f_0000, 0x437f_0000, 0x437f_0000, 0x437f_0000, 0x437e_0000]), // -(2^24 + 1)
    (0xff00_0001, [0xcb7f_ffff, 0x4f7f_0000, 0x3f80_0000, 0x0000_0000, 0x0000_0000, 0x437f_0000]), // -(2^24 - 1)
    (0x0080_0001, [0x4b00_0001, 0x4b00_0001, 0x3f80_0000, 0x0000_0000, 0x4300_0000, 0x0000_0000]), // 2^23 + 1
    (0x1234_5678, [0x4d91_a2b4, 0x4d91_a2b4, 0x42f0_0000, 0x42ac_0000, 0x4250_0000, 0x4190_0000]),
    (0x8080_8080, [0xcefe_feff, 0x4f00_8080, 0x4300_0000, 0x4300_0000, 0x4300_0000, 0x4300_0000]),
    (0x7f7f_7f7f, [0x4efe_feff, 0x4efe_feff, 0x42fe_0000, 0x42fe_0000, 0x42fe_0000, 0x42fe_0000]),
    (0x0000_ff00, [0x477f_0000, 0x477f_0000, 0x0000_0000, 0x437f_0000, 0x0000_0000, 0x0000_0000]),
    (0x00ff_0000, [0x4b7f_0000, 0x4b7f_0000, 0x0000_0000, 0x0000_0000, 0x437f_0000, 0x0000_0000]),
    (0xaabb_ccdd, [0xceaa_8866, 0x4f2a_bbcd, 0x435d_0000, 0x434c_0000, 0x433b_0000, 0x432a_0000]),
];

fn column(form: Form) -> usize {
    match form {
        Form::I32 => 0,
        Form::U32 => 1,
        _ => form as usize - 15,
    }
}

/// D for S0 = `bits`: the guide's integer, widened exactly to binary64 (every
/// 32-bit integer is a binary64 value), rounded once to binary32 to nearest,
/// ties to even.
fn expect(form: Form, bits: u32) -> u32 {
    let value = match form {
        Form::I32 => f64::from(bits as i32),
        Form::U32 => f64::from(bits),
        _ => f64::from((bits >> (8 * (form as u32 - 17))) & 0xff),
    };
    (value as f32).to_bits()
}

const LITERAL: u32 = 255;
const VCC_LO: u32 = 106;

/// ENCODING 0b0111111 in bits 25-31, VDST 17-24, OP 9-16, SRC0 0-8 (13.3.2).
fn vop1(op: u32, dst: u32, src0: u32) -> u32 {
    0x7e00_0000 | dst << 17 | op << 9 | src0
}

const fn vgpr(n: u32) -> u32 {
    256 + n
}

/// `v_mov_b32 v<reg>, literal`.
fn vmov(reg: u32, value: u32) -> [u32; 2] {
    [0x7e00_02ff | reg << 17, value]
}

/// `s_mov_b32 s<reg>, literal`.
fn smov(reg: u32, value: u32) -> [u32; 2] {
    [0xbe80_03ff | reg << 16, value]
}

/// `v_xor_b32 v<dst>, v<a>, v<b>`.
fn vxor(dst: u32, a: u32, b: u32) -> u32 {
    29 << 25 | dst << 17 | b << 9 | vgpr(a)
}

const FULL_EXEC: u32 = 0xbefe_03c1;
const SCC_SET: u32 = 0xbf06_8080;
const SCC_CLEAR: u32 = 0xbf06_8180;
/// v6 = vcc_lo; v2 ^= v6; `s_cbranch_scc0` over v2 ^= v1.
const FLAG_TAIL: [u32; 4] = [0x7e0c_026a, 0x3a04_0506, 0xbf84_0001, 0x3a04_0501];
/// `buffer_load_dword v3, v0, s[0:3], 0 idxen` without the wait.
const PENDING_LOAD_V3: [u32; 2] = [0xe030_2000, 0x8000_0300];
/// XORed with v1 to give each lane its own old VGPR before a masked write.
const OLD: u32 = 0x0bad_f00d;
const EXEC_MASKS: [u32; 5] = [0, 0x5555_5555, 0xaaaa_aaaa, 0x8000_0001, u32::MAX];

/// Load v1 = lane input, `setup`, the ALU words, `after`, v2 = v<result>, store v2.
fn program(setup: &[u32], alu: &[u32], after: &[u32], result: u32) -> Vec<u8> {
    let mut code = vec![0xe030_2000, 0x8000_0100, 0xbf8c_3f70];
    code.extend(setup);
    code.extend(alu);
    code.extend(after);
    if result != 2 {
        code.push(0x7e04_0300 | result);
    }
    code.extend([0xe070_2000, 0x8001_0200, 0xbbfd_0000, 0xbf81_0000]);
    words(&code)
}

fn alu_offset(setup: &[u32]) -> usize {
    12 + 4 * setup.len()
}

/// Lane inputs: the known values, then 2^k - 1, 2^k and 2^k + 1 for k 0..31
/// and their two's complement negations, the values 2^24..2^24 + 8 and 2^25
/// plus each remainder 0..4 (ties both ways), the halfway points of the top
/// binade, and every byte value in each lane of the word.
fn lanes() -> Vec<u32> {
    let mut lanes: Vec<u32> = KNOWN.iter().map(|&(s, _)| s).collect();
    for k in 0..32 {
        let p = 1_u32 << k;
        for bits in [p.wrapping_sub(1), p, p.wrapping_add(1)] {
            lanes.extend([bits, bits.wrapping_neg()]);
        }
    }
    lanes.extend((0..=8).map(|k| 0x0100_0000 + k));
    lanes.extend((0..=4).map(|k| 0x0200_0000 + k));
    lanes.extend([
        0x8000_0080,
        0x8000_0180,
        0xffff_ff80,
        0xffff_ff7f,
        0x7fff_ff80,
        0x7fff_ff7f,
    ]);
    lanes.extend((0..256).map(|b: u32| (b * 0x0101_0101) ^ (b << 8)));
    lanes
}

fn case(
    name: &'static str,
    family: &'static str,
    label: String,
    code: Vec<u8>,
    input: Vec<u32>,
    lane: impl Fn(usize, u32) -> u32,
) -> Case {
    let expected = input.iter().enumerate().map(|(n, &x)| lane(n, x)).collect();
    Case {
        form: name,
        family,
        label: format!("{name} {label}"),
        code,
        input,
        expected,
    }
}

fn masked(exec: u32, n: usize, d: u32, old: u32) -> u32 {
    if exec >> (n % 32) & 1 == 1 { d } else { old }
}

/// Source values for SGPR, vcc_lo and literal SRC0 cases.
const UNIFORM: [u32; 6] = [
    0xffff_ffff,
    0x8000_0000,
    0x7fff_ffff,
    0x0100_0001,
    0xfeff_ffff,
    0x1234_5678,
];

/// Integer inline constant selectors and their values (guide 6.2).
const INLINE: [(u32, u32); 5] = [
    (128, 0),
    (129, 1),
    (192, 64),
    (193, 0xffff_ffff),
    (208, 0xffff_fff0),
];

fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for (form, name) in FORMS {
        let op = form as u32;
        let f = move |s| expect(form, s);
        let all = lanes();
        let alu = [vop1(op, 2, vgpr(1))];
        let code = program(&[], &alu, &[], 2);
        let label = format!("{} lanes", all.len());
        cases.push(case(
            name,
            "cvt lanes",
            label,
            code.clone(),
            all.clone(),
            |_, x| f(x),
        ));
        let short = all[..7].to_vec();
        cases.push(case(
            name,
            "cvt lanes",
            "7 lanes".into(),
            code,
            short,
            |_, x| f(x),
        ));
        // VDST equal to SRC0, and v255 as VDST and as SRC0.
        let code = program(&[], &[vop1(op, 1, vgpr(1))], &[], 1);
        cases.push(case(
            name,
            "cvt registers",
            "v1 = v1".into(),
            code,
            all.clone(),
            |_, x| f(x),
        ));
        let code = program(&[], &[vop1(op, 255, vgpr(1))], &[], 255);
        cases.push(case(
            name,
            "cvt registers",
            "v255 = v1".into(),
            code,
            all.clone(),
            |_, x| f(x),
        ));
        let b = 0x8123_4567;
        let code = program(&vmov(255, b), &[vop1(op, 2, vgpr(255))], &[], 2);
        cases.push(case(
            name,
            "cvt registers",
            "v2 = v255".into(),
            code,
            all.clone(),
            |_, _| f(b),
        ));
        // Uniform SRC0: s8, s105, vcc_lo and a literal dword.
        for value in UNIFORM {
            for reg in [8, 105, VCC_LO] {
                let code = program(&smov(reg, value), &[vop1(op, 2, reg)], &[], 2);
                let label = format!("s{reg} = {value:#x}");
                cases.push(case(
                    name,
                    "cvt uniform source",
                    label,
                    code,
                    all.clone(),
                    |_, _| f(value),
                ));
            }
            let code = program(&[], &[vop1(op, 2, LITERAL), value], &[], 2);
            let label = format!("literal {value:#x}");
            cases.push(case(
                name,
                "cvt uniform source",
                label,
                code,
                all.clone(),
                |_, _| f(value),
            ));
        }
        for (selector, value) in INLINE {
            let code = program(&[], &[vop1(op, 2, selector)], &[], 2);
            let label = format!("inline {selector}");
            cases.push(case(
                name,
                "cvt inline integer",
                label,
                code,
                all.clone(),
                |_, _| f(value),
            ));
        }
        // EXEC: inactive lanes keep v2 = v1 ^ OLD.
        let mut setup = vmov(5, OLD).to_vec();
        setup.push(vxor(2, 5, 1));
        for exec in EXEC_MASKS {
            let mut before = setup.clone();
            before.extend(smov(126, exec));
            let code = program(&before, &alu, &[FULL_EXEC], 2);
            let label = format!("exec {exec:#x}");
            cases.push(case(name, "cvt exec", label, code, all.clone(), |n, x| {
                masked(exec, n, f(x), x ^ OLD)
            }));
        }
        // VCC_LO and SCC are not written: D ^ VCC_LO, XORed with S0 when SCC is set.
        for vcc in [0, u32::MAX] {
            for scc in [false, true] {
                let mut before = smov(VCC_LO, vcc).to_vec();
                before.push(if scc { SCC_SET } else { SCC_CLEAR });
                let code = program(&before, &alu, &FLAG_TAIL, 2);
                let label = format!("vcc_lo {vcc:#x}, scc {scc}");
                cases.push(case(
                    name,
                    "cvt no vcc or scc write",
                    label,
                    code,
                    all.clone(),
                    |_, x| f(x) ^ vcc ^ if scc { x } else { 0 },
                ));
            }
        }
    }
    cases
}

/// Per form: two lane counts, three register cases, 6 x 4 uniform sources,
/// five inline integers, five EXEC masks and four flag seeds.
const COUNTS: [(&str, usize); 6] = [
    ("cvt exec", 6 * 5),
    ("cvt inline integer", 6 * 5),
    ("cvt lanes", 6 * 2),
    ("cvt no vcc or scc write", 6 * 4),
    ("cvt registers", 6 * 3),
    ("cvt uniform source", 6 * 6 * 4),
];

/// Programs the decoders refuse at the ALU word: a missing literal, every
/// selector outside the subset (107-127 and 209-254, including the float
/// inline constants, with the SDWA and DPP extension dword where one
/// belongs), and a read and a write of a VGPR with a load outstanding.
fn rejections() -> Vec<(String, Vec<u8>, usize)> {
    let setup = vmov(3, 7);
    let offset = alu_offset(&setup);
    let extension = |src0: u32| match src0 {
        249 => vec![0x0606_0601],
        250 => vec![0xff00_e401],
        233 | 234 => vec![0xfac6_8801],
        _ => vec![],
    };
    let mut all = Vec::new();
    for (form, name) in FORMS {
        let op = form as u32;
        let mut code = vec![0xe030_2000, 0x8000_0100, 0xbf8c_3f70];
        code.extend(setup);
        code.push(vop1(op, 2, LITERAL));
        all.push((format!("{name} missing literal"), words(&code), offset));
        for src0 in (107..=127).chain(209..=254) {
            let mut alu = vec![vop1(op, 2, src0)];
            alu.extend(extension(src0));
            let code = program(&setup, &alu, &[], 2);
            all.push((format!("{name} src0 {src0}"), code, offset));
        }
        let mut pending = setup.to_vec();
        pending.extend(PENDING_LOAD_V3);
        let at = alu_offset(&pending);
        for (dst, src0, what) in [(2, 3, "read"), (3, 1, "write")] {
            let code = program(&pending, &[vop1(op, dst, vgpr(src0))], &[], 2);
            all.push((
                format!("{name} {what} of v3 with its load outstanding"),
                code,
                at,
            ));
        }
    }
    all
}

const REJECTIONS: usize = 6 * (1 + 21 + 46 + 2);

fn assert_rejected_at(label: &str, code: &[u8], offset: usize, input: &[u32]) {
    match xenora_ref::execute(code, input) {
        Ok(_) => panic!("reference accepted {label}"),
        Err(error) => assert_eq!(
            error.offset, offset,
            "reference rejected {label} elsewhere: {error:?}"
        ),
    }
    match independent_ref::execute(code, input) {
        Ok(_) => panic!("independent accepted {label}"),
        Err(error) => assert!(
            error.starts_with(&format!("{offset:#x}:")),
            "independent rejected {label} elsewhere: {error}"
        ),
    }
}

fn checked_cases() -> Vec<Case> {
    let cases = cases();
    let mut counts = BTreeMap::new();
    for case in &cases {
        *counts.entry(case.family).or_default() += 1;
    }
    assert_eq!(counts, BTreeMap::from(COUNTS));
    cases
}

fn failures(
    cases: &[Case],
    execute: impl Fn(&[u8], &[u32]) -> Result<Vec<u32>, String>,
) -> Vec<String> {
    cases
        .iter()
        .filter_map(|case| {
            let output = execute(&case.code, &case.input);
            (output.as_deref().ok() != Some(case.expected.as_slice()))
                .then(|| format!("  {}: {output:?}", case.label))
        })
        .collect()
}

#[test]
fn widened_rounding_reproduces_the_hand_table() {
    for (s, expected) in KNOWN {
        for (form, name) in FORMS {
            assert_eq!(expect(form, s), expected[column(form)], "{name}({s:#x})");
        }
    }
}

/// Negative control: the oracle is not truncation. Round toward zero of an
/// I32 or U32 magnitude (keeping its top 24 bits) differs from the table on
/// the known values that round up, so a converter that truncated, or a Metal
/// path in another round mode, cannot pass the case tables.
#[test]
fn rounding_up_known_values_are_not_truncated() {
    let toward_zero = |exact: i64| {
        let magnitude = exact.unsigned_abs();
        let shift = (64 - magnitude.leading_zeros()).saturating_sub(24);
        let kept = (magnitude >> shift << shift) as f64;
        if exact < 0 { -kept } else { kept }
    };
    let mut differ = Vec::new();
    for (s, expected) in KNOWN {
        for (exact, column) in [(i64::from(s as i32), 0), (i64::from(s), 1)] {
            if f64::from(f32::from_bits(expected[column])) != toward_zero(exact) {
                differ.push((s, column));
            }
        }
    }
    // UINT_MAX (U32), 2^31 - 1 (both), 2^24 + 3 (both) and 0xfeff_ffff (U32).
    for required in [
        (0xffff_ffff, 1),
        (0x7fff_ffff, 0),
        (0x7fff_ffff, 1),
        (0x0100_0003, 0),
        (0x0100_0003, 1),
        (0xfeff_ffff, 1),
    ] {
        assert!(
            differ.contains(&required),
            "{required:x?} is not a round-up case"
        );
    }
    // 2^24 + 1 is a tie that rounds down to even: it equals truncation.
    assert!(!differ.contains(&(0x0100_0001, 1)));
}

#[test]
fn conversions_match_the_guide_on_the_reference() {
    let cases = checked_cases();
    let failed = failures(&cases, |code, input| {
        xenora_ref::execute(code, input).map_err(|error| format!("{error:?}"))
    });
    assert!(
        failed.is_empty(),
        "reference disagrees with the guide:\n{}",
        failed.join("\n")
    );
    eprintln!("vop1_int_to_float: {} reference cases", cases.len());
}

#[test]
fn conversions_match_the_guide_on_the_independent_interpreter() {
    let cases = checked_cases();
    let failed = failures(&cases, independent_ref::execute);
    assert!(
        failed.is_empty(),
        "independent interpreter disagrees:\n{}",
        failed.join("\n")
    );
    eprintln!(
        "vop1_int_to_float: {} independent-interpreter cases",
        cases.len()
    );
}

/// Negative control: a wrong expectation is caught. Flipping the low bit of
/// every expected word must fail on both interpreters.
#[test]
fn a_corrupted_expectation_fails_on_both_interpreters() {
    let mut cases = checked_cases();
    cases.truncate(6);
    for case in &mut cases {
        for word in &mut case.expected {
            *word ^= 1;
        }
    }
    let reference = failures(&cases, |code, input| {
        xenora_ref::execute(code, input).map_err(|error| format!("{error:?}"))
    });
    assert_eq!(reference.len(), cases.len());
    assert_eq!(
        failures(&cases, independent_ref::execute).len(),
        cases.len()
    );
}

#[test]
fn unsupported_conversion_encodings_are_refused() {
    let rejections = rejections();
    assert_eq!(rejections.len(), REJECTIONS);
    let input = lanes();
    for (label, code, offset) in &rejections {
        assert_rejected_at(label, code, *offset, &input);
    }
    eprintln!("vop1_int_to_float: {} rejected encodings", rejections.len());
}

fn check_compiler_rejections() {
    for (label, code, offset) in rejections() {
        let at = match compile(Architecture::Gfx1030, &code) {
            Err(CompileError::Decode(error)) => error.offset,
            Err(CompileError::Verify { offset, .. }) => offset,
            other => panic!("compiler did not reject {label}: {other:?}"),
        };
        assert_eq!(at, offset, "compiler rejected {label} elsewhere");
    }
}

#[test]
fn compiler_matches_the_conversion_operand_domain() {
    for case in &checked_cases() {
        compile(Architecture::Gfx1030, &case.code)
            .unwrap_or_else(|error| panic!("compiler rejected {}: {error:?}", case.label));
    }
    check_compiler_rejections();
}

#[test]
#[ignore = "requires a native Metal device with API and shader validation"]
fn conversions_match_the_guide_on_native_metal() {
    assert_eq!(std::env::var("MTL_DEBUG_LAYER").as_deref(), Ok("1"));
    assert_eq!(std::env::var("MTL_SHADER_VALIDATION").as_deref(), Ok("1"));
    let cases = checked_cases();
    check_compiler_rejections();
    let (failures, programs) = harness::check_metal(&cases);
    failures.assert_none("native Metal disagrees with the guide");
    eprintln!(
        "vop1_int_to_float: {} Metal cases in {programs} programs",
        cases.len()
    );
}
