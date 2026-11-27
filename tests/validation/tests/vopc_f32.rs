// SPDX-License-Identifier: GPL-3.0-or-later

//! Independent per-lane expected values for the sixteen Wave32 VOPC binary32
//! compares (#786): `v_cmp_{f,lt,eq,le,gt,lg,ge,o,u,nge,nlg,ngt,nle,neq,nlt,tru}_f32`,
//! `_e32` only, writing VCC_LO under the contract in
//! `docs/shaders/vector-compare.md`.
//!
//! The expectations come from the public AMD "RDNA 2" Instruction Set
//! Architecture Reference Guide only: section 12.9 "VOPC Instructions", Table
//! 61 (pages 152-153: the per-opcode `D[threadId]` lines for opcodes 0-15),
//! section 3.9 (page 19: `VCC[n] = EXEC[n] & test(n)`, fully written) and the
//! VOPC word of section 13.3.3. `CLASS_MASKS` is written by hand from those
//! lines; `expect` transcribes them. Nothing here comes from the ISA crate,
//! the registry, the decoder or the shader compiler.
//!
//! Readings of the guide (not hardware evidence):
//! - U is `isNan(S0) || isNan(S1)` from the per-opcode table; the summary
//!   table's `!isNaN(S0) || !isNaN(S1)` contradicts it
//!   (`docs/research/rdna2-errata.md`).
//! - `isNan` is true for quiet and signalling NaNs of either sign; the guide
//!   does not distinguish them (errata, undefined functions).
//! - LG is `S0 <> S1`, less or greater, so false on unordered operands. The N*
//!   forms are the written negations and so true on unordered operands, as the
//!   table's "With NAN inputs this is not the same operation" notes say.
//! - +0 and -0 compare equal (IEEE 754). MODE pins input denormals allowed, so
//!   a subnormal compares by its value, not as zero.
//! - Refused: integer inline constants (selectors 128-208) as SRC0, as for the
//!   binary32 VOP1 forms, and float inline constants (240-248), which are
//!   outside the VOPC subset.

#[path = "operand_domains/harness.rs"]
#[allow(dead_code)]
mod harness;
#[path = "../independent_ref.rs"]
mod independent_ref;

use std::collections::BTreeMap;

use harness::{Case, words};
use xenora_shader::{Architecture, CompileError, compile};

/// The mnemonics in opcode order (Table 61, opcodes 0-15).
const FORMS: [&str; 16] = [
    "v_cmp_f_f32",
    "v_cmp_lt_f32",
    "v_cmp_eq_f32",
    "v_cmp_le_f32",
    "v_cmp_gt_f32",
    "v_cmp_lg_f32",
    "v_cmp_ge_f32",
    "v_cmp_o_f32",
    "v_cmp_u_f32",
    "v_cmp_nge_f32",
    "v_cmp_nlg_f32",
    "v_cmp_ngt_f32",
    "v_cmp_nle_f32",
    "v_cmp_neq_f32",
    "v_cmp_nlt_f32",
    "v_cmp_tru_f32",
];

/// Bit `op` set when opcode `op` holds, for each way two values can relate.
/// By hand from Table 61: S0 < S1 makes LT, LE, LG, O, NGE, NGT, NEQ and TRU
/// hold; S0 == S1 makes EQ, LE, GE, O, NLG, NGT, NLT and TRU hold; S0 > S1
/// makes GT, LG, GE, O, NLE, NEQ, NLT and TRU hold; an unordered pair makes U,
/// the six N* forms and TRU hold.
const LESS: u16 = 0xaaaa;
const EQUAL: u16 = 0xcccc;
const GREATER: u16 = 0xf0f0;
const UNORDERED: u16 = 0xff00;

const QNAN: u32 = 0x7fc0_0000;
const NEG_QNAN: u32 = 0xffc0_0001;
const SNAN: u32 = 0x7f80_0001;
const NEG_SNAN: u32 = 0xffbf_ffff;
const POS_ZERO: u32 = 0x0000_0000;
const NEG_ZERO: u32 = 0x8000_0000;
const POS_INF: u32 = 0x7f80_0000;
const NEG_INF: u32 = 0xff80_0000;
const ONE: u32 = 0x3f80_0000;

/// Hand-classified operand pairs.
#[rustfmt::skip]
const CLASS_MASKS: [(u32, u32, u16); 20] = [
    (ONE, 0x4000_0000, LESS),           // 1 < 2
    (0x4000_0000, ONE, GREATER),        // 2 > 1
    (ONE, ONE, EQUAL),
    (POS_ZERO, NEG_ZERO, EQUAL),        // +0 == -0
    (NEG_ZERO, POS_ZERO, EQUAL),
    (NEG_ZERO, 0x0000_0001, LESS),      // -0 < min subnormal
    (0x8000_0001, POS_ZERO, LESS),      // -min subnormal < +0
    (POS_INF, POS_INF, EQUAL),
    (NEG_INF, NEG_INF, EQUAL),
    (0x7f7f_ffff, POS_INF, LESS),       // max finite < +inf
    (NEG_INF, 0xff7f_ffff, LESS),       // -inf < -max finite
    (POS_INF, NEG_INF, GREATER),
    (QNAN, ONE, UNORDERED),
    (ONE, QNAN, UNORDERED),
    (SNAN, ONE, UNORDERED),
    (ONE, NEG_SNAN, UNORDERED),
    (QNAN, QNAN, UNORDERED),            // a NaN is not equal to itself
    (SNAN, POS_INF, UNORDERED),
    (NEG_QNAN, NEG_INF, UNORDERED),
    (NEG_SNAN, SNAN, UNORDERED),
];

/// Table 61 opcode `op` for S0 = `a`, S1 = `b`, transcribed line by line (the
/// lints' rewrites would change the result on a NaN).
#[allow(
    clippy::double_comparisons,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::nonminimal_bool
)]
fn expect(op: u32, a: u32, b: u32) -> bool {
    let (s0, s1) = (f64::from(f32::from_bits(a)), f64::from(f32::from_bits(b)));
    let lg = s0 < s1 || s0 > s1;
    match op {
        0 => false,
        1 => s0 < s1,
        2 => s0 == s1,
        3 => s0 <= s1,
        4 => s0 > s1,
        5 => lg,
        6 => s0 >= s1,
        7 => !s0.is_nan() && !s1.is_nan(),
        8 => s0.is_nan() || s1.is_nan(),
        9 => !(s0 >= s1),
        10 => !lg,
        11 => !(s0 > s1),
        12 => !(s0 <= s1),
        13 => !(s0 == s1),
        14 => !(s0 < s1),
        15 => true,
        _ => unreachable!("not a binary32 compare"),
    }
}

/// Operand values: NaNs, zeros, infinities, subnormals and ordinary values.
const VALUES: [u32; 16] = [
    POS_ZERO,
    NEG_ZERO,
    POS_INF,
    NEG_INF,
    QNAN,
    NEG_QNAN,
    SNAN,
    NEG_SNAN,
    0x0000_0001,
    0x8000_0001,
    ONE,
    0xbf80_0000,
    0x3f80_0001,
    0x7f7f_ffff,
    0xff7f_ffff,
    0x4049_0fdb,
];
/// More lane inputs so a wave holds 32 different values.
const MORE_LANES: [u32; 16] = [
    0x4000_0000,
    0xc000_0000,
    0x3f00_0000,
    0xbf00_0000,
    0x007f_ffff,
    0x807f_ffff,
    0x0080_0000,
    0x7fff_ffff,
    0xffff_ffff,
    0x7fa0_0000,
    0x4b00_0000,
    0xcb00_0000,
    0x3f7f_ffff,
    0x42c8_0000,
    0xc2c8_0000,
    0x3e80_0000,
];
const EXEC_MASKS: [u32; 5] = [0, 0x5555_5555, 0xaaaa_aaaa, 0x8000_0001, u32::MAX];
/// VCC_LO before the compare, so a partial update would show.
const STALE_VCC: u32 = 0xa5a5_c33c;
const UNIFORM: [u32; 4] = [QNAN, NEG_ZERO, POS_INF, ONE];

const LITERAL: u32 = 255;
const VCC_LO: u32 = 106;
const EXEC_LO: u32 = 126;

/// ENCODING 0b0111110 in bits 25-31, OP 17-24, VSRC1 9-16, SRC0 0-8 (13.3.3).
fn vopc(op: u32, src0: u32, vsrc1: u32) -> u32 {
    0b011_1110 << 25 | op << 17 | vsrc1 << 9 | src0
}

const fn vgpr(n: u32) -> u32 {
    256 + n
}

fn vmov(reg: u32, value: u32) -> [u32; 2] {
    [0x7e00_02ff | reg << 17, value]
}

fn smov(reg: u32, value: u32) -> [u32; 2] {
    [0xbe80_03ff | reg << 16, value]
}

const FULL_EXEC: u32 = 0xbefe_03c1;
/// `v_mov_b32 v2, vcc_lo`.
const READ_VCC: u32 = 0x7e04_026a;
const PREFIX: [u32; 3] = [0xe030_2000, 0x8000_0100, 0xbf8c_3f70];
const STORE_V2: [u32; 4] = [0xe070_2000, 0x8001_0200, 0xbbfd_0000, 0xbf81_0000];

/// Load v1 = lane input, `setup`, EXEC_LO = `exec` (unless `None`), the
/// compare words, EXEC_LO = -1, v2 = vcc_lo, store v2. Returns the program and
/// the compare's byte offset.
fn program(setup: &[u32], exec: Option<u32>, compare: &[u32]) -> (Vec<u8>, usize) {
    let mut code = PREFIX.to_vec();
    code.extend(setup);
    if let Some(exec) = exec {
        code.extend(smov(EXEC_LO, exec));
    }
    let offset = 4 * code.len();
    code.extend(compare);
    if exec.is_some() {
        code.push(FULL_EXEC);
    }
    code.push(READ_VCC);
    code.extend(STORE_V2);
    (words(&code), offset)
}

fn lanes() -> Vec<u32> {
    VALUES.iter().chain(&MORE_LANES).copied().collect()
}

/// Section 3.9: VCC_LO of the wave holding dispatch index `n`, the OR over its
/// existing lanes k with EXEC[k] of test(k) << k.
fn vcc(op: u32, exec: u32, input: &[u32], n: usize, operands: impl Fn(u32) -> (u32, u32)) -> u32 {
    let base = n / 32 * 32;
    (0..32)
        .filter(|&k| base + k < input.len() && exec >> k & 1 == 1)
        .filter(|&k| {
            let (s0, s1) = operands(input[base + k]);
            expect(op, s0, s1)
        })
        .fold(0, |mask, k| mask | 1 << k)
}

fn case(
    op: u32,
    family: &'static str,
    label: String,
    code: Vec<u8>,
    input: Vec<u32>,
    exec: u32,
    operands: impl Fn(u32) -> (u32, u32),
) -> Case {
    let name = FORMS[op as usize];
    let expected = (0..input.len())
        .map(|n| vcc(op, exec, &input, n, &operands))
        .collect();
    Case {
        form: name,
        family,
        label: format!("{name} {label}"),
        code,
        input,
        expected,
    }
}

fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for op in 0..16 {
        // Every value against all 32 lane inputs, in each slot.
        for b in VALUES {
            let setup = vmov(3, b);
            let (code, _) = program(&setup, None, &[vopc(op, vgpr(1), 3)]);
            let label = format!("v1, {b:#x}");
            cases.push(case(op, "f32 domain", label, code, lanes(), !0, |x| (x, b)));
            let (code, _) = program(&setup, None, &[vopc(op, vgpr(3), 1)]);
            let label = format!("{b:#x}, v1");
            cases.push(case(op, "f32 domain", label, code, lanes(), !0, |x| (b, x)));
        }
        // Both sources the same register: a NaN lane is unordered with itself.
        let (code, _) = program(&[], None, &[vopc(op, vgpr(1), 1)]);
        let label = "v1, v1".to_string();
        cases.push(case(op, "f32 aliasing", label, code, lanes(), !0, |x| {
            (x, x)
        }));
        // Uniform SRC0: literal, s8, s105 and vcc_lo.
        for value in UNIFORM {
            let (code, _) = program(&[], None, &[vopc(op, LITERAL, 1), value]);
            let label = format!("literal {value:#x}, v1");
            let f = move |x| (value, x);
            cases.push(case(op, "f32 uniform source", label, code, lanes(), !0, f));
            for reg in [8, 105, VCC_LO] {
                let (code, _) = program(&smov(reg, value), None, &[vopc(op, reg, 1)]);
                let label = format!("s{reg} = {value:#x}, v1");
                cases.push(case(op, "f32 uniform source", label, code, lanes(), !0, f));
            }
        }
        // EXEC: inactive lanes contribute 0 and the stale VCC_LO is replaced.
        for exec in EXEC_MASKS {
            let mut setup = vmov(3, QNAN).to_vec();
            setup.extend(smov(VCC_LO, STALE_VCC));
            let compare = [vopc(op, vgpr(3), 1)];
            let (code, _) = program(&setup, Some(exec), &compare);
            let label = format!("exec {exec:#x}");
            let f = |x| (QNAN, x);
            cases.push(case(op, "f32 exec", label, code, lanes(), exec, f));
            let mut setup = vmov(3, ONE).to_vec();
            setup.extend(smov(VCC_LO, STALE_VCC));
            let (code, _) = program(&setup, Some(exec), &compare);
            let label = format!("exec {exec:#x}, 1.0");
            cases.push(case(op, "f32 exec", label, code, lanes(), exec, |x| {
                (ONE, x)
            }));
        }
        // A partial wave: lanes that do not exist contribute 0.
        let (code, _) = program(&vmov(3, NEG_ZERO), None, &[vopc(op, vgpr(1), 3)]);
        let short = lanes()[..7].to_vec();
        let label = "7 lanes".to_string();
        let f = |x| (x, NEG_ZERO);
        cases.push(case(op, "f32 partial wave", label, code, short, !0, f));
        // Two waves.
        let mut two = lanes();
        two.extend(lanes().iter().rev());
        let label = "64 lanes".to_string();
        let f = |x| (x, NEG_ZERO);
        let (code, _) = program(&vmov(3, NEG_ZERO), None, &[vopc(op, vgpr(1), 3)]);
        cases.push(case(op, "f32 partial wave", label, code, two, !0, f));
    }
    cases
}

const COUNTS: [(&str, usize); 5] = [
    ("f32 aliasing", 16),
    ("f32 domain", 16 * 16 * 2),
    ("f32 exec", 16 * 5 * 2),
    ("f32 partial wave", 16 * 2),
    ("f32 uniform source", 16 * 4 * 4),
];

/// Programs the decoders refuse at the compare word: every integer inline
/// constant (81), every float inline constant (9), and a missing literal.
fn rejections() -> Vec<(String, Vec<u8>, usize)> {
    let mut all = Vec::new();
    for op in 0..16 {
        let name = FORMS[op as usize];
        for src0 in (128..=208).chain(240..=248) {
            let (code, offset) = program(&[], None, &[vopc(op, src0, 1)]);
            all.push((format!("{name} src0 {src0}"), code, offset));
        }
        let mut code = PREFIX.to_vec();
        let offset = 4 * code.len();
        code.push(vopc(op, LITERAL, 1));
        all.push((format!("{name} missing literal"), words(&code), offset));
    }
    all
}

const REJECTIONS: usize = 16 * (81 + 9 + 1);

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
fn transcription_reproduces_the_hand_classification() {
    for (a, b, mask) in CLASS_MASKS {
        for op in 0..16 {
            let name = FORMS[op as usize];
            assert_eq!(
                expect(op, a, b),
                mask >> op & 1 == 1,
                "{name}({a:#x}, {b:#x})"
            );
        }
    }
}

/// Negative controls: the expectations tell apart the readings that would be
/// wrong, so a translation that used them would fail the cases above.
#[test]
fn expectations_reject_the_wrong_readings() {
    // The summary table's U, `!isNaN(S0) || !isNaN(S1)`, is not opcode 8: they
    // disagree whenever both or neither operand is a NaN.
    let nan = |x: u32| f32::from_bits(x).is_nan();
    for (a, b) in [
        (QNAN, QNAN),
        (SNAN, NEG_QNAN),
        (ONE, ONE),
        (NEG_ZERO, POS_INF),
    ] {
        assert_ne!(expect(8, a, b), !nan(a) || !nan(b), "{a:#x} {b:#x}");
    }
    for (a, b) in [(QNAN, ONE), (ONE, SNAN), (NEG_SNAN, NEG_QNAN)] {
        // Each N* form differs from the ordered relation it resembles on NaN:
        // NGE/LT, NLG/EQ, NGT/LE, NLE/GT, NEQ/LG, NLT/GE.
        for (negated, ordered) in [(9, 1), (10, 2), (11, 3), (12, 4), (13, 5), (14, 6)] {
            assert_ne!(expect(negated, a, b), expect(ordered, a, b));
        }
    }
    // Comparing the raw bits as integers would call -0 less than +0 (signed)
    // or greater (unsigned), and a NaN equal to itself.
    assert!(expect(2, NEG_ZERO, POS_ZERO) && !expect(1, NEG_ZERO, POS_ZERO));
    assert!(!expect(2, QNAN, QNAN) && expect(13, QNAN, QNAN));
    // Flushing input denormals would make the smallest subnormal equal +0.
    assert!(expect(4, 0x0000_0001, POS_ZERO));
}

#[test]
fn f32_compares_match_the_guide_on_the_reference() {
    let cases = checked_cases();
    let failed = failures(&cases, |code, input| {
        xenora_ref::execute(code, input).map_err(|error| format!("{error:?}"))
    });
    assert!(
        failed.is_empty(),
        "reference disagrees:\n{}",
        failed.join("\n")
    );
    eprintln!("vopc_f32: {} reference cases", cases.len());
}

#[test]
fn f32_compares_match_the_guide_on_the_independent_interpreter() {
    let cases = checked_cases();
    let failed = failures(&cases, independent_ref::execute);
    assert!(
        failed.is_empty(),
        "independent interpreter disagrees:\n{}",
        failed.join("\n")
    );
    eprintln!("vopc_f32: {} independent-interpreter cases", cases.len());
}

#[test]
fn unsupported_f32_compare_encodings_are_refused() {
    let rejections = rejections();
    assert_eq!(rejections.len(), REJECTIONS);
    for (label, code, offset) in &rejections {
        assert_rejected_at(label, code, *offset, &lanes());
    }
    eprintln!("vopc_f32: {} rejected encodings", rejections.len());
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
fn compiler_matches_the_f32_compare_operand_domain() {
    for case in &checked_cases() {
        compile(Architecture::Gfx1030, &case.code)
            .unwrap_or_else(|error| panic!("compiler rejected {}: {error:?}", case.label));
    }
    check_compiler_rejections();
}

#[test]
#[ignore = "requires a native Metal device with API and shader validation"]
fn f32_compares_match_the_guide_on_native_metal() {
    assert_eq!(std::env::var("MTL_DEBUG_LAYER").as_deref(), Ok("1"));
    assert_eq!(std::env::var("MTL_SHADER_VALIDATION").as_deref(), Ok("1"));
    let cases = checked_cases();
    check_compiler_rejections();
    let (failures, programs) = harness::check_metal(&cases);
    failures.assert_none("native Metal disagrees with the guide");
    eprintln!(
        "vopc_f32: {} Metal cases in {programs} programs",
        cases.len()
    );
}
