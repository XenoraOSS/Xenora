// SPDX-License-Identifier: GPL-3.0-or-later

//! Independent per-lane expected values for the sixteen Wave32 VOPC binary32
//! compares (#786): `v_cmp_{f,lt,eq,le,gt,lg,ge,o,u,nge,nlg,ngt,nle,neq,nlt,tru}_f32`
//! (ENC_VOPC opcodes 0-15), `_e32` only, under the contract in
//! `docs/shaders/vector-compare.md` (sections 2-7).
//!
//! The expectations are written below from the public AMD "RDNA 2" Instruction
//! Set Architecture Reference Guide only: 12.9 (Table 59 float half and the
//! float per-opcode table, printed pages 150-153), 3.9, 6.2 (Table 20) and the
//! errata record in docs/research/rdna2-errata.md (the summary table's
//! complement definition of U is a defect; the per-opcode table rules).
//! Nothing here comes from the ISA crate, the semantic registry, the decoder
//! or the shader compiler. Every case runs on the reference interpreter
//! `xenora_ref` and on the second interpreter `independent_ref`. The native
//! Metal gate runs every distinct program.
//!
//! Readings of the guide (not hardware evidence):
//! - The ordered relations LT, EQ, LE, GT, LG, GE are the IEEE 754 ordered
//!   predicates: false when either input is a NaN.
//! - +0 and -0 compare equal, and the infinities order as numbers.
//! - O is "neither input is a NaN", U is "either input is a NaN" - the
//!   per-opcode table's reading, against the contradictory summary table
//!   (errata). The N forms are the unordered complements of the ordered tests.
//! - Section 3.9: `VCC[n] = EXEC[n] & (test passed for thread[n])`, "VCC is
//!   fully written"; inactive and absent lanes give 0.
//! - Table 20: inline integer selectors 128-208 are rejected for the float
//!   forms, as the guide does not say how a float operation reads them (the
//!   reading the registry's vop1_f32 rule already records); one 32-bit
//!   literal is the constant form. Inline float constants stay outside the
//!   supported subset as well.

#[path = "operand_domains/harness.rs"]
#[allow(dead_code)]
mod harness;
#[path = "../independent_ref.rs"]
mod independent_ref;

use harness::{Case, Rejection, words};

/// One of the sixteen forms: ENC_VOPC opcode, which is the float-table offset.
#[derive(Clone, Copy, Debug)]
struct Form {
    op: u32,
    name: &'static str,
    offset: u32,
}

const NAMES: [&str; 16] = [
    "f", "lt", "eq", "le", "gt", "lg", "ge", "o", "u", "nge", "nlg", "ngt", "nle", "neq", "nlt",
    "tru",
];

fn forms() -> Vec<Form> {
    (0..16u32)
        .map(|offset| Form {
            op: offset,
            name: Box::leak(format!("v_cmp_{}_f32", NAMES[offset as usize]).into_boxed_str()),
            offset,
        })
        .collect()
}

fn form(name: &str) -> Form {
    forms().into_iter().find(|form| form.name == name).unwrap()
}

// ---- Binary32 domain bit patterns ----
const P_ZERO: u32 = 0x0000_0000;
const N_ZERO: u32 = 0x8000_0000;
const P_ONE: u32 = 0x3f80_0000;
const N_ONE: u32 = 0xbf80_0000;
const P_TWO: u32 = 0x4000_0000;
const P_HALF: u32 = 0x3f00_0000;
const P_INF: u32 = 0x7f80_0000;
const N_INF: u32 = 0xff80_0000;
const Q_NAN: u32 = 0x7fc0_0000;
const NQ_NAN: u32 = 0xffc0_0000;
const S_NAN: u32 = 0x7f80_0001;
const MIN_SUB: u32 = 0x0000_0001;
const MAX_SUB: u32 = 0x007f_ffff;
const MAX_FIN: u32 = 0x7f7f_ffff;
const MIN_FIN: u32 = 0xff7f_ffff;

/// Lane inputs (v1): the domain spread over the wave - zeros of both signs,
/// normals of both signs and several magnitudes, the infinities, quiet and
/// signaling NaNs, subnormals and the finite extrema.
const LANES: [u32; 32] = [
    P_ZERO,
    N_ZERO,
    P_ONE,
    N_ONE,
    P_TWO,
    P_HALF,
    0xc000_0000,
    0xbf00_0000,
    P_INF,
    N_INF,
    Q_NAN,
    NQ_NAN,
    S_NAN,
    0xff80_0001,
    MIN_SUB,
    MAX_SUB,
    0x8000_0001,
    0x807f_ffff,
    MAX_FIN,
    MIN_FIN,
    0x3f80_0001,
    0xbf80_0001,
    0x7e97_8942,
    0xfe97_8942,
    0x0080_0000,
    0x8080_0000,
    0x42f6_e979,
    0xc2f6_e979,
    P_ONE,
    N_INF,
    Q_NAN,
    P_ZERO,
];

/// The IEEE relation of a form for binary32 bit patterns, written from the
/// float per-opcode table alone.
fn test(form: Form, s0: u32, s1: u32) -> bool {
    let (a, b) = (f32::from_bits(s0), f32::from_bits(s1));
    match form.offset {
        0 => false,                      // F: D = 0
        1 => a < b,                      // LT: ordered
        2 => a == b,                     // EQ: ordered, signed zeros equal
        3 => a <= b,                     // LE
        4 => a > b,                      // GT
        5 => a < b || a > b,             // LG: ordered inequality
        6 => a >= b,                     // GE
        7 => !a.is_nan() && !b.is_nan(), // O: ordered pair
        8 => a.is_nan() || b.is_nan(),   // U: unordered (per-opcode table)
        9 => !(a >= b),                  // NGE
        10 => !(a < b || a > b),         // NLG
        11 => !(a > b),                  // NGT
        12 => !(a <= b),                 // NLE
        13 => !(a == b),                 // NEQ: unordered inequality
        14 => !(a < b),                  // NLT
        _ => true,                       // TRU: D = 1
    }
}

/// Section 3.9 and contract section 3: VCC_LO = OR over the lanes n that
/// exist (`present`) of (EXEC[n] & test(n)) << n.
fn vcc(form: Form, exec: u32, present: u32, operands: impl Fn(usize) -> (u32, u32)) -> u32 {
    (0..32)
        .filter(|&n| (present & exec) >> n & 1 == 1)
        .filter(|&n| {
            let (s0, s1) = operands(n);
            test(form, s0, s1)
        })
        .fold(0, |mask, n| mask | 1 << n)
}

// ---- Program building, the hand-assembled encodings of vopc_wide ----

/// `v_mov_b32 v<dst>, v<src>`.
fn vcopy(dst: u32, src: u32) -> u32 {
    0x7e00_0200 | dst << 17 | (256 + src)
}

/// `s_mov_b32 s<reg>, literal`.
fn smov(reg: u32, value: u32) -> [u32; 2] {
    [0xbe80_03ff | reg << 16, value]
}

const FULL_EXEC: u32 = 0xbefe_03c1;
const READ_VCC: u32 = 0x7e04_026a;
const VCCZ_OVER_ONE: u32 = 0xbf86_0001;
/// `v_xor_b32 v2, v1, v2`.
const XOR_V2_V1: u32 = 29 << 25 | 2 << 17 | 2 << 9 | (256 + 1);
const SMOV_S8_0: u32 = 0xbe88_0380;
const SMOV_S8_1: u32 = 0xbe88_0381;
const V2_FROM_S8: u32 = 0x7e04_0208;

const PREFIX: [u32; 3] = [0xe030_2000, 0x8000_0100, 0xbf8c_3f70];
const STORE_V2: [u32; 4] = [0xe070_2000, 0x8001_0200, 0xbbfd_0000, 0xbf81_0000];

const EXEC_LO: u32 = 126;
const LITERAL: u32 = 255;
const VCC_LO_SEL: u32 = 106;

/// ENCODING 0b0111110 in bits 25-31, OP 17-24, VSRC1 9-16, SRC0 0-8 (guide
/// 13.3.3); SRC0 256 + n is VGPR n.
fn vopc(op: u32, src0: u32, vsrc1: u32) -> u32 {
    0b011_1110 << 25 | op << 17 | vsrc1 << 9 | src0
}

const fn vgpr(n: u32) -> u32 {
    256 + n
}

/// Load v1 = lane input, `setup`, EXEC_LO = `exec` (unless `None`), the compare
/// words, EXEC_LO = -1 again, v2 = vcc_lo, `tail`, then store v2. Returns the
/// program and the compare's byte offset.
fn program(setup: &[u32], exec: Option<u32>, compare: &[u32], tail: &[u32]) -> (Vec<u8>, usize) {
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
    code.extend(tail);
    code.extend(STORE_V2);
    (words(&code), offset)
}

fn code(setup: &[u32], exec: Option<u32>, compare: &[u32]) -> Vec<u8> {
    program(setup, exec, compare, &[]).0
}

fn present(input: &[u32], n: usize) -> u32 {
    let in_wave = (input.len() - n / 32 * 32).min(32);
    if in_wave == 32 {
        u32::MAX
    } else {
        (1 << in_wave) - 1
    }
}

fn case(
    form: Form,
    family: &'static str,
    label: String,
    code: Vec<u8>,
    input: Vec<u32>,
    lane: impl Fn(usize, u32, u32) -> u32,
) -> Case {
    let expected = input
        .iter()
        .enumerate()
        .map(|(n, &x)| lane(n, x, present(&input, n)))
        .collect();
    Case {
        form: form.name,
        family,
        label: format!("{} {label}", form.name),
        code,
        input,
        expected,
    }
}

fn full(form: Form, operands: impl Fn(usize) -> (u32, u32)) -> impl Fn(usize, u32, u32) -> u32 {
    move |_, _, present| vcc(form, u32::MAX, present, &operands)
}

fn v1(n: usize) -> u32 {
    LANES[n % 32]
}

/// The uniform S1 values every form faces as a literal: a positive normal,
/// both zeros, both infinities, a quiet and a signaling NaN.
const UNIFORMS: [u32; 7] = [P_ONE, P_ZERO, N_ZERO, P_INF, N_INF, Q_NAN, S_NAN];

fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for form in forms() {
        let op = form.op;
        // ---- domain: the lane input as S0 (v1 copied to v3? no - direct)
        // against each uniform literal. v3 reads the lane input as VSRC1 for
        // the reversed cases; here SRC0 = literal, VSRC1 = v3 = lane input.
        for uniform in UNIFORMS {
            // v3 = v1; v_cmp_x_f32 literal, v3
            let setup = [vcopy(3, 1)];
            let compare = [vopc(op, LITERAL, 3), uniform];
            let lane = full(form, move |n| (v1(n), uniform));
            // NOTE: S0 is the literal here, so the operands are (uniform, lane).
            let lane_rev = full(form, move |n| (uniform, v1(n)));
            let _ = lane;
            cases.push(case(
                form,
                "vopcf32 domain",
                format!("literal {uniform:#x} vs lane input"),
                code(&setup, None, &compare),
                LANES.to_vec(),
                lane_rev,
            ));
        }
        // ---- lane input as S0 proper: v4 = v1 copy in SRC0 position ----
        for uniform in [P_ONE, Q_NAN, N_ZERO, P_INF] {
            let mut setup = vec![vcopy(3, 1)];
            setup.push(vcopy(4, 3));
            // v_cmp_x_f32 v4, v<dummy 3> reads v4 as S0... use S0=v4, VSRC1=v5 with uniform in v5
            let mut setup2 = vec![vcopy(4, 1)];
            setup2.extend({
                let mut m = Vec::new();
                m.extend([0x7e00_02ff | 5 << 17, uniform]); // v_mov v5, uniform
                m
            });
            let compare = [vopc(op, vgpr(4), 5)];
            let lane = full(form, move |n| (v1(n), uniform));
            cases.push(case(
                form,
                "vopcf32 lane as s0",
                format!("lane input vs v5 = {uniform:#x}"),
                code(&setup2, None, &compare),
                LANES.to_vec(),
                lane,
            ));
            let _ = setup;
        }
        // ---- SGPR source: s8 as S0 against the lane input ----
        for uniform in [P_ONE, N_INF, NQ_NAN] {
            let mut setup = smov(8, uniform).to_vec();
            setup.push(vcopy(3, 1));
            let compare = [vopc(op, 8, 3)];
            let lane = full(form, move |n| (uniform, v1(n)));
            cases.push(case(
                form,
                "vopcf32 sgpr source",
                format!("s8 = {uniform:#x} vs lane input"),
                code(&setup, None, &compare),
                LANES.to_vec(),
                lane,
            ));
        }
        // ---- exec masks ----
        for exec in [0x5555_5555u32, 0xaaaa_aaaa, 0x8000_0001, 0x0000_ffff] {
            let setup = [vcopy(3, 1)];
            let compare = [vopc(op, LITERAL, 3), P_ONE];
            let lane = move |_, _, present| vcc(form, exec, present, |n| (P_ONE, v1(n)));
            cases.push(case(
                form,
                "vopcf32 exec",
                format!("exec {exec:#x}"),
                code(&setup, Some(exec), &compare),
                LANES.to_vec(),
                lane,
            ));
        }
        // ---- empty exec: the mask is 0 and nothing is read ----
        {
            let setup = [vcopy(3, 1)];
            let compare = [vopc(op, LITERAL, 3), P_ONE];
            cases.push(case(
                form,
                "vopcf32 empty exec",
                "exec 0 gives a zero mask".to_string(),
                code(&setup, Some(0), &compare),
                LANES.to_vec(),
                |_, _, _| 0,
            ));
        }
        // ---- partial waves: 7 and 45 lanes ----
        for (input, label) in [
            (LANES[..7].to_vec(), "7 lanes"),
            (
                LANES
                    .iter()
                    .chain(LANES.iter().rev().take(13))
                    .copied()
                    .collect::<Vec<u32>>(),
                "45 lanes",
            ),
        ] {
            let setup = [vcopy(3, 1)];
            let compare = [vopc(op, LITERAL, 3), P_ONE];
            let copy = input.clone();
            cases.push(case(
                form,
                "vopcf32 partial wave",
                label.to_string(),
                code(&setup, None, &compare),
                input,
                move |n, _, p| vcc(form, u32::MAX, p, |k| (P_ONE, copy[n / 32 * 32 + k])),
            ));
        }
        // ---- vccz branch: after the compare, s_cbranch_vccz skips the xor
        // when the mask is zero, so v2 keeps vcc_lo; otherwise v2 ^= v1 ----
        for exec in [u32::MAX, 0x0000_8000, 0] {
            let setup = [vcopy(3, 1)];
            let compare = [vopc(op, LITERAL, 3), P_ONE];
            let (program, _) = program(&setup, Some(exec), &compare, &[VCCZ_OVER_ONE, XOR_V2_V1]);
            cases.push(case(
                form,
                "vopcf32 vccz branch",
                format!("exec {exec:#x}, then s_cbranch_vccz"),
                program,
                LANES.to_vec(),
                move |_, x, p| {
                    let mask = vcc(form, exec, p, |n| (P_ONE, v1(n)));
                    if mask == 0 { 0 } else { mask ^ x }
                },
            ));
        }
    }
    cases
}

fn rejections() -> Vec<Rejection> {
    let mut rejections = Vec::new();
    for form in forms() {
        let op = form.op;
        let family = "vopcf32 rejection";
        // Integer inline constants as SRC0: selectors 128-208 across the range.
        for selector in [128, 160, 192, 193, 200, 208] {
            let compare = [vopc(op, selector, 3)];
            let (bytes, offset) = program(&[vcopy(3, 1)], None, &compare, &[]);
            rejections.push(Rejection {
                family,
                truncated: false,
                label: format!("{} inline integer selector {selector}", form.name),
                code: bytes,
                offset,
            });
        }
        // Inline float constants (240-248) stay outside the subset.
        for selector in [240, 242, 248] {
            let compare = [vopc(op, selector, 3)];
            let (bytes, offset) = program(&[vcopy(3, 1)], None, &compare, &[]);
            rejections.push(Rejection {
                family,
                truncated: false,
                label: format!("{} inline float selector {selector}", form.name),
                code: bytes,
                offset,
            });
        }
        // A literal without its dword truncates: the stream ends right after
        // the compare word, and both implementations locate the error at the
        // instruction that needs the missing word.
        {
            let mut code = PREFIX.to_vec();
            code.extend([vcopy(3, 1)]);
            let offset = 4 * code.len();
            code.push(vopc(op, LITERAL, 3));
            rejections.push(Rejection {
                family,
                truncated: true,
                label: format!("{} truncated literal", form.name),
                code: words(&code),
                offset,
            });
        }
        // Special selectors: vcc_hi, exec, m0, ttmp, null, sdwa/dpp ranges.
        for selector in [107, 124, 125, 233, 249, 250, 251] {
            let compare = [vopc(op, selector, 3)];
            let (bytes, offset) = program(&[vcopy(3, 1)], None, &compare, &[]);
            rejections.push(Rejection {
                family,
                truncated: false,
                label: format!("{} special selector {selector}", form.name),
                code: bytes,
                offset,
            });
        }
    }
    rejections
}

fn checked_cases() -> Vec<Case> {
    let cases = cases();
    assert!(!cases.is_empty());
    cases
}

#[test]
fn expectations_follow_the_guide_examples() {
    // Worked by hand from the float per-opcode table and IEEE 754.
    let lt = form("v_cmp_lt_f32");
    let eq = form("v_cmp_eq_f32");
    let lg = form("v_cmp_lg_f32");
    let ge = form("v_cmp_ge_f32");
    let o = form("v_cmp_o_f32");
    let u = form("v_cmp_u_f32");
    let neq = form("v_cmp_neq_f32");
    let nlt = form("v_cmp_nlt_f32");
    let tru = form("v_cmp_tru_f32");
    let f = form("v_cmp_f_f32");
    // Ordered relations are false on any NaN input.
    for nan in [Q_NAN, NQ_NAN, S_NAN] {
        assert!(!test(lt, nan, P_ONE));
        assert!(!test(lt, P_ONE, nan));
        assert!(!test(eq, nan, nan));
        assert!(!test(lg, nan, P_ONE));
        assert!(!test(ge, nan, nan));
        assert!(test(u, nan, P_ONE));
        assert!(test(u, P_ONE, nan));
        assert!(test(u, nan, nan));
        assert!(!test(o, nan, P_ONE));
        assert!(test(neq, nan, P_ONE));
        assert!(test(nlt, nan, P_ONE));
        assert!(test(tru, nan, nan));
        assert!(!test(f, nan, nan));
    }
    // No NaN: U is false and O is true.
    assert!(!test(u, P_ONE, N_INF));
    assert!(test(o, P_ZERO, MAX_FIN));
    // Signed zeros compare equal in every equality-based relation.
    assert!(test(eq, P_ZERO, N_ZERO));
    assert!(!test(lt, P_ZERO, N_ZERO));
    assert!(!test(lt, N_ZERO, P_ZERO));
    assert!(test(ge, P_ZERO, N_ZERO));
    assert!(!test(lg, P_ZERO, N_ZERO));
    assert!(test(form("v_cmp_le_f32"), N_ZERO, P_ZERO));
    // Infinities order as numbers.
    assert!(test(lt, N_INF, P_INF));
    assert!(test(lt, MAX_FIN, P_INF));
    assert!(test(lt, N_INF, MIN_FIN));
    assert!(!test(lt, P_INF, MAX_FIN));
    assert!(test(eq, P_INF, P_INF));
    assert!(test(form("v_cmp_gt_f32"), P_INF, N_INF));
    // Subnormals order as numbers.
    assert!(test(lt, MIN_SUB, MAX_SUB));
    assert!(test(lt, P_ZERO, MIN_SUB));
    assert!(!test(eq, MIN_SUB, P_ZERO));
    // LG is ordered inequality: true for distinct ordered values.
    assert!(test(lg, P_ONE, P_TWO));
    assert!(test(lg, P_TWO, P_ONE));
    assert!(!test(lg, P_ONE, P_ONE));
    // The N forms are unordered complements.
    assert!(test(form("v_cmp_nge_f32"), P_ONE, P_TWO));
    assert!(!test(form("v_cmp_nge_f32"), P_TWO, P_ONE));
    assert!(test(form("v_cmp_nlg_f32"), P_ONE, P_ONE));
    assert!(!test(form("v_cmp_nlg_f32"), P_ONE, P_TWO));
    assert!(test(form("v_cmp_ngt_f32"), P_ONE, P_TWO));
    assert!(test(form("v_cmp_nle_f32"), P_TWO, P_ONE));
    // Section 3.9: inactive and absent lanes give 0.
    assert_eq!(vcc(lt, 0x0000_00f0, 0x1ff, |_| (P_ZERO, P_ONE)), 0xf0);
    assert_eq!(vcc(lt, u32::MAX, 0x7f, |_| (P_ZERO, P_ONE)), 0x7f);
    assert_eq!(vcc(lt, 0, u32::MAX, |_| (P_ZERO, P_ONE)), 0);
}

#[test]
fn vopcf32_match_the_guide_on_the_reference() {
    let cases = checked_cases();
    let failures = harness::check_reference(&cases);
    failures.assert_none("the reference interpreter disagrees with the guide");
    eprintln!("vopcf32: {} reference cases", cases.len());
}

#[test]
fn vopcf32_match_the_guide_on_the_independent() {
    let cases = checked_cases();
    let mut failures = 0;
    let mut first = String::new();
    for case in &cases {
        match independent_ref::execute(&case.code, &case.input) {
            Ok(output) if output == case.expected => {}
            other => {
                if failures == 0 {
                    first = format!("{}: {:?}, expected {:?}", case.label, other, case.expected);
                }
                failures += 1;
            }
        }
    }
    assert_eq!(
        failures, 0,
        "the independent interpreter disagrees: {first}"
    );
    eprintln!("vopcf32: {} independent cases", cases.len());
}

#[test]
fn vopcf32_rejections_at_the_same_byte() {
    let rejections = rejections();
    assert!(!rejections.is_empty());
    let failures = harness::check_rejected(&rejections);
    failures.assert_none("rejections disagree");
    eprintln!("vopcf32: {} rejections", rejections.len());
}

#[test]
fn vopcf32_match_metal_natively() {
    let cases = checked_cases();
    let (failures, programs) = harness::check_metal(&cases);
    failures.assert_none("native Metal disagrees with the guide");
    eprintln!(
        "vopcf32: {} Metal cases in {programs} programs",
        cases.len()
    );
}
