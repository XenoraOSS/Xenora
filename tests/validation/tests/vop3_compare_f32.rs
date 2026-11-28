// SPDX-License-Identifier: GPL-3.0-or-later

//! Wave32 VOP3A binary32 compares with input modifiers (#976). The six
//! relations are guide 12.9 Table 61 offsets 1-6, re-encoded by 12.9.1 in the
//! VOP3A layout of 13.3.4. The expectations are transcribed here from the
//! guide: NaN makes every one of these six ordered relations false, +0 equals
//! -0, ABS clears the sign bit and NEG flips it before the comparison.
//! EXEC gates each lane's contribution (3.9), and the explicit scalar
//! destination is one Wave32 SGPR mask. Nothing is copied from the production
//! decoder, reference or shader compiler.

#[path = "operand_domains/harness.rs"]
#[allow(dead_code)]
mod harness;
#[path = "../independent_ref.rs"]
mod independent_ref;

use harness::{Case, words};
use xenora_shader::{Architecture, compile};

const FORMS: [(u32, &str); 6] = [
    (1, "v_cmp_lt_f32"),
    (2, "v_cmp_eq_f32"),
    (3, "v_cmp_le_f32"),
    (4, "v_cmp_gt_f32"),
    (5, "v_cmp_lg_f32"),
    (6, "v_cmp_ge_f32"),
];

const POS_ZERO: u32 = 0x0000_0000;
const NEG_ZERO: u32 = 0x8000_0000;
const POS_INF: u32 = 0x7f80_0000;
const NEG_INF: u32 = 0xff80_0000;
const QNAN: u32 = 0x7fc0_0000;
const NEG_QNAN: u32 = 0xffc0_0001;
const SNAN: u32 = 0x7f80_0001;
const NEG_SNAN: u32 = 0xffbf_ffff;
const ONE: u32 = 0x3f80_0000;
const TWO: u32 = 0x4000_0000;
const NEG_ONE: u32 = 0xbf80_0000;
const NEG_TWO: u32 = 0xc000_0000;

/// Thirty-two lane inputs, with finite nonzero values of both signs, signed
/// zeros, quiet and signalling NaNs, infinities, subnormals and normals.
const LANES: [u32; 32] = [
    POS_ZERO,
    NEG_ZERO,
    ONE,
    NEG_ONE,
    TWO,
    NEG_TWO,
    0x3f00_0000,
    0xbf00_0000,
    POS_INF,
    NEG_INF,
    QNAN,
    NEG_QNAN,
    SNAN,
    NEG_SNAN,
    0x0000_0001,
    0x8000_0001,
    0x007f_ffff,
    0x807f_ffff,
    0x0080_0000,
    0x8080_0000,
    0x7f7f_ffff,
    0xff7f_ffff,
    0x4049_0fdb,
    0xc049_0fdb,
    0x3f80_0001,
    0xbf80_0001,
    0x42c8_0000,
    0xc2c8_0000,
    0x4b00_0000,
    0xcb00_0000,
    0x3e80_0000,
    0xbe80_0000,
];

const EXEC_MASKS: [u32; 5] = [u32::MAX, 0x5555_5555, 0xaaaa_aaaa, 0x8000_0001, 0];
const STALE_VCC: u32 = 0xa5a5_c33c;
const VCC_LO: u32 = 106;
const EXEC_LO: u32 = 126;

/// Table 61 offsets 1-6. These are ordered comparisons: quiet and signalling
/// NaNs make every one false. The host binary64 comparison is exact for every
/// binary32 input, and +0/-0 already compare equal there.
#[allow(clippy::double_comparisons)]
fn passes(op: u32, a: u32, b: u32) -> bool {
    let (a, b) = (f64::from(f32::from_bits(a)), f64::from(f32::from_bits(b)));
    match op {
        1 => a < b,
        2 => a == b,
        3 => a <= b,
        4 => a > b,
        5 => a < b || a > b,
        6 => a >= b,
        _ => unreachable!("not one of the six ordered binary32 compares"),
    }
}

fn modified(value: u32, abs: bool, neg: bool) -> u32 {
    let mut value = value;
    if abs {
        value &= 0x7fff_ffff;
    }
    if neg {
        value ^= 0x8000_0000;
    }
    value
}

/// VOP3A word pair. Bit 8/9 of the first word are ABS for SRC0/SRC1; bits
/// 29/30 of the second word are the corresponding NEG bits. SRC2 is inline
/// zero, and OP_SEL, CLAMP and OMOD are clear.
fn vop3(op: u32, dst: u32, reverse: bool, abs: u32, neg: u32) -> [u32; 2] {
    let (src0, src1) = if reverse { (511, 257) } else { (257, 511) };
    [
        0xd400_0000 | op << 16 | (abs & 3) << 8 | dst,
        src0 | src1 << 9 | 128 << 18 | (neg & 3) << 29,
    ]
}

fn vmov_literal(reg: u32, value: u32) -> [u32; 2] {
    [0x7e00_02ff | reg << 17, value]
}

fn smov(dst: u32, value: u32) -> [u32; 2] {
    [0xbe80_03ff | dst << 16, value]
}

fn setup(bound: u32, exec: u32) -> Vec<u32> {
    let mut code = vec![0xe030_2000, 0x8000_0100, 0xbf8c_3f70]; // load v1
    code.extend(vmov_literal(255, bound));
    code.extend(smov(EXEC_LO, exec));
    code
}

fn store(mut code: Vec<u32>) -> Vec<u8> {
    code.extend([0xe070_2000, 0x8001_0200, 0xbbfd_0000, 0xbf81_0000]);
    words(&code)
}

fn mask(op: u32, exec: u32, wave: &[u32], bound: u32, reverse: bool, abs: u32, neg: u32) -> u32 {
    wave.iter().enumerate().fold(0, |mask, (lane, &input)| {
        // The physical source positions decide which modifier applies. v1 is
        // SRC0 in the direct form and SRC1 in the reversed form.
        let (raw0, raw1) = if reverse {
            (bound, input)
        } else {
            (input, bound)
        };
        let s0 = modified(raw0, abs & 1 != 0, neg & 1 != 0);
        let s1 = modified(raw1, abs & 2 != 0, neg & 2 != 0);
        mask | (exec & (u32::from(passes(op, s0, s1)) << lane))
    })
}

#[allow(clippy::too_many_arguments)]
fn program(
    op: u32,
    dst: u32,
    bound: u32,
    exec: u32,
    reverse: bool,
    abs: u32,
    neg: u32,
    read: u32,
) -> Vec<u8> {
    let mut code = setup(bound, exec);
    code.extend(smov(VCC_LO, STALE_VCC));
    code.extend(vop3(op, dst, reverse, abs, neg));
    code.extend([0xbefe_03c1, 0x7e04_0200 | read]); // full EXEC, v2 = s{read}
    store(code)
}

#[allow(clippy::too_many_arguments)]
fn expected(
    input: &[u32],
    op: u32,
    exec: u32,
    bound: u32,
    reverse: bool,
    abs: u32,
    neg: u32,
    transform: impl Fn(u32) -> u32,
) -> Vec<u32> {
    input
        .chunks(32)
        .flat_map(|wave| {
            let value = transform(mask(op, exec, wave, bound, reverse, abs, neg));
            std::iter::repeat_n(value, wave.len())
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn mask_case(
    op: u32,
    family: &'static str,
    label: String,
    dst: u32,
    bound: u32,
    exec: u32,
    reverse: bool,
    abs: u32,
    neg: u32,
    lanes: usize,
) -> Case {
    let input: Vec<_> = LANES.into_iter().cycle().take(lanes).collect();
    Case {
        form: FORMS[(op - 1) as usize].1,
        family,
        label,
        code: program(op, dst, bound, exec, reverse, abs, neg, dst),
        expected: expected(&input, op, exec, bound, reverse, abs, neg, |x| x),
        input,
    }
}

fn modifier_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    // Bit 0 modifies SRC0 and bit 1 SRC1. The nonzero finite lanes make each
    // result differ from the unmodified one for at least one relation/bound.
    let modifiers = [
        (0, 0, "none"),
        (1, 0, "abs0"),
        (0, 1, "neg0"),
        (1, 1, "negabs0"),
        (2, 0, "abs1"),
        (0, 2, "neg1"),
        (2, 2, "negabs1"),
        (3, 3, "absneg-both"),
    ];
    for &(op, name) in &FORMS {
        for &(bound, bound_name) in &[(ONE, "1.0"), (NEG_TWO, "-2.0")] {
            for &(abs, neg, modifier) in &modifiers {
                for reverse in [false, true] {
                    let label = format!("{name} {modifier}, bound {bound_name}, reverse={reverse}");
                    cases.push(mask_case(
                        op,
                        "f32 modifiers",
                        label,
                        VCC_LO,
                        bound,
                        u32::MAX,
                        reverse,
                        abs,
                        neg,
                        32,
                    ));
                }
            }
        }
    }
    cases
}

fn destination_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for &(op, name) in &FORMS {
        for dst in [20, 105, VCC_LO] {
            for exec in EXEC_MASKS {
                for lanes in [7, 32, 45] {
                    cases.push(mask_case(
                        op,
                        if dst == VCC_LO {
                            "vcc destination"
                        } else {
                            "sgpr destination"
                        },
                        format!("{name} s{dst}, EXEC={exec:#x}, {lanes} lanes"),
                        dst,
                        NEG_ONE,
                        exec,
                        false,
                        1,
                        1,
                        lanes,
                    ));
                }
            }
        }
    }
    cases
}

/// When the destination is not VCC_LO, read VCC_LO instead: the compare must
/// leave the stale scalar value exactly as it was.
fn vcc_preservation_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for &(op, name) in &FORMS {
        for exec in [u32::MAX, 0x5555_5555, 0] {
            let input: Vec<_> = LANES.into_iter().cycle().take(45).collect();
            cases.push(Case {
                form: name,
                family: "vcc preservation",
                label: format!("{name} s20 leaves VCC_LO, EXEC={exec:#x}"),
                code: program(op, 20, ONE, exec, false, 3, 3, VCC_LO),
                input,
                expected: vec![STALE_VCC; 45],
            });
        }
    }
    cases
}

fn all_cases() -> Vec<Case> {
    let mut cases = modifier_cases();
    cases.extend(destination_cases());
    cases.extend(vcc_preservation_cases());
    cases
}

/// The modifier cases are not vacuous: for every nonzero modifier combination
/// there is a finite nonzero pair whose answer changes if the modifier is
/// dropped. In particular `-abs(x)` differs from both `abs(x)` and `x`.
#[test]
fn modifier_vectors_are_not_vacuous() {
    for &(abs, neg, _) in &[(1, 0, "abs"), (0, 1, "neg"), (1, 1, "-abs")] {
        for op in 1..=6 {
            let changed = [ONE, NEG_ONE, TWO, NEG_TWO].into_iter().any(|a| {
                [ONE, NEG_TWO].into_iter().any(|b| {
                    passes(op, a, b) != passes(op, modified(a, abs & 1 != 0, neg & 1 != 0), b)
                })
            });
            assert!(changed, "op {op}, abs={abs}, neg={neg}");
        }
    }
    assert_ne!(
        modified(ONE, true, true),
        modified(ONE, true, false),
        "-abs differs from abs"
    );
    assert_ne!(modified(ONE, true, true), ONE, "-abs differs from none");
    assert_eq!(modified(POS_ZERO, true, true), NEG_ZERO);
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
                .then(|| format!("  {}: {output:?}, expected {:?}", case.label, case.expected))
        })
        .collect()
}

#[test]
fn vop3_binary32_compares_match_the_guide_on_the_reference() {
    let cases = all_cases();
    assert_eq!(cases.len(), 6 * 2 * 8 * 2 + 6 * 3 * 5 * 3 + 6 * 3);
    let failed = failures(&cases, |code, input| {
        xenora_ref::execute(code, input).map_err(|error| format!("{error:?}"))
    });
    assert!(
        failed.is_empty(),
        "reference disagrees:\n{}",
        failed.join("\n")
    );
    eprintln!("vop3_compare_f32: {} reference cases", cases.len());
}

#[test]
fn vop3_binary32_compares_match_the_guide_on_the_independent_interpreter() {
    let cases = all_cases();
    let failed = failures(&cases, independent_ref::execute);
    assert!(
        failed.is_empty(),
        "independent interpreter disagrees:\n{}",
        failed.join("\n")
    );
    eprintln!(
        "vop3_compare_f32: {} independent-interpreter cases",
        cases.len()
    );
}

#[test]
fn compiler_accepts_the_binary32_compare_modifier_domain() {
    for case in all_cases() {
        compile(Architecture::Gfx1030, &case.code)
            .unwrap_or_else(|error| panic!("compiler rejected {}: {error:?}", case.label));
    }
}

/// Unsupported output modifiers, SRC2 use, non-VGPR sources, bad destinations
/// and integer-input modifiers are controlled errors rather than being
/// dropped. Integer rejection is also covered exhaustively by vop3_compare.rs;
/// these checks pin the float boundary.
#[test]
fn unsupported_binary32_compare_encodings_are_refused() {
    let mut count = 0;
    for &(op, name) in &FORMS {
        let setup = setup(ONE, u32::MAX);
        let offset = setup.len() * 4;
        let [head, tail] = vop3(op, VCC_LO, false, 0, 0);
        let mut check = |words_in: &[u32]| {
            let mut code = setup.clone();
            code.extend(words_in);
            let code = if words_in.len() == 1 {
                words(&code)
            } else {
                code.push(0x7e04_026a);
                store(code)
            };
            let input = [1, 2];
            let error = xenora_ref::execute(&code, &input).expect_err(name);
            assert_eq!(error.offset, offset, "{name}: {words_in:08x?} {error:?}");
            let error = independent_ref::execute(&code, &input).expect_err(name);
            assert!(
                error.starts_with(&format!("{offset:#x}:")),
                "{name}: {words_in:08x?} {error}"
            );
            count += 1;
        };
        // ABS on unused SRC2, OP_SEL and CLAMP.
        for bit in 10..16 {
            check(&[head | 1 << bit, tail]);
        }
        // OMOD and NEG on unused SRC2.
        for bit in [27, 28, 31] {
            check(&[head, tail | 1 << bit]);
        }
        // SRC2 is not inline zero.
        for selector in [0, 127, 129, 255] {
            check(&[head, (tail & !(0x1ff << 18)) | selector << 18]);
        }
        // SGPR, VCC_LO, inline integer, inline float and literal sources.
        for selector in [4, 106, 128, 240, 255] {
            check(&[head, (tail & !0x1ff) | selector]);
            check(&[head, (tail & !(0x1ff << 9)) | selector << 9]);
        }
        // VCC_HI, TTMP, EXEC and the end of the destination byte.
        for dst in [107, 108, 124, 126, 255] {
            check(&[(head & !0xff) | dst, tail]);
        }
        check(&[head]);
    }
    assert_eq!(count, 6 * (6 + 3 + 4 + 10 + 5 + 1));
    eprintln!("vop3_compare_f32: {count} rejections per CPU interpreter");
}

#[test]
#[ignore = "requires a native Metal device with API and shader validation"]
fn vop3_binary32_compares_match_the_guide_on_native_metal() {
    assert_eq!(std::env::var("MTL_DEBUG_LAYER").as_deref(), Ok("1"));
    assert_eq!(std::env::var("MTL_SHADER_VALIDATION").as_deref(), Ok("1"));
    let cases = all_cases();
    let (failures, programs) = harness::check_metal(&cases);
    failures.assert_none("native Metal disagrees with the guide");
    eprintln!(
        "vop3_compare_f32: {} Metal cases in {programs} programs",
        cases.len()
    );
}
