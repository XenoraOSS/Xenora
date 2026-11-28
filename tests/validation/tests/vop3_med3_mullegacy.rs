// SPDX-License-Identifier: GPL-3.0-or-later

//! Independent Wave32 checks for V_MUL_LEGACY_F32 (VOP2/VOP3) and
//! V_MED3_F32 (VOP3), issue #975. Expectations are transcribed from the public
//! AMD RDNA 2 ISA guide: legacy multiply's DX9 zero-wins rule (12.7, page
//! 131), MED3's MIN3/MAX3 branch order (12.12, page 170), the VOP3 source and
//! modifier fields (13.3.4), and MIN/MAX signed-zero selection (pages 132-133).
//! Nothing here imports the production decoder or semantic registry.

#[path = "operand_domains/harness.rs"]
#[allow(dead_code)]
mod harness;
#[path = "../independent_ref.rs"]
mod independent_ref;

use harness::{Case, words};
use xenora_shader::{Architecture, compile};

const PZ: u32 = 0x0000_0000;
const NZ: u32 = 0x8000_0000;
const ONE: u32 = 0x3f80_0000;
const TWO: u32 = 0x4000_0000;
const THREE: u32 = 0x4040_0000;
const NEG_ONE: u32 = 0xbf80_0000;
const NEG_TWO: u32 = 0xc000_0000;
const NEG_THREE: u32 = 0xc040_0000;
const PINF: u32 = 0x7f80_0000;
const NINF: u32 = 0xff80_0000;
const QNAN_A: u32 = 0x7fc0_0001;
const QNAN_B: u32 = 0x7fc0_0002;
const QNAN_C: u32 = 0xffc0_0003;
const MIN_SUB: u32 = 0x0000_0001;
const SIGN: u32 = 0x8000_0000;

fn nan(x: u32) -> bool {
    x & 0x7fff_ffff > 0x7f80_0000
}

fn subnormal(x: u32) -> bool {
    x & 0x7f80_0000 == 0 && x & 0x007f_ffff != 0
}

fn modify(mut x: u32, abs: bool, neg: bool) -> u32 {
    if abs {
        x &= 0x7fff_ffff;
    }
    if neg {
        x ^= SIGN;
    }
    x
}

/// The DX9 rule. The guide does not pin the forced-zero sign, so this helper
/// is used only for nonzero products; zero-multiplication sign bits are
/// checked separately as implementation observations, never as specification.
fn legacy_nonzero_product(a: u32, b: u32) -> Option<u32> {
    if (a | b) & 0x7fff_ffff == 0 || a & 0x7fff_ffff == 0 || b & 0x7fff_ffff == 0 {
        return None;
    }
    if nan(a) || nan(b) || subnormal(a) || subnormal(b) {
        return None;
    }
    let product = f64::from(f32::from_bits(a)) * f64::from(f32::from_bits(b));
    if product.is_nan() || (product != 0.0 && product.abs() < 2f64.powi(-126)) {
        return None;
    }
    Some((product as f32).to_bits())
}

fn minmax(minimum: bool, a: u32, b: u32) -> u32 {
    if nan(a) {
        return b;
    }
    if nan(b) {
        return a;
    }
    if a & 0x7fff_ffff == 0 && b & 0x7fff_ffff == 0 {
        return if minimum { a | b } else { a & b };
    }
    let ordered = f32::from_bits(a) < f32::from_bits(b);
    if ordered == minimum { a } else { b }
}

/// Guide page 170. Quiet NaNs take the MIN3 branch; otherwise the numeric
/// comparisons against S0/S1 use binary32 equality, so +0 equals -0. A
/// signalling NaN is outside this verification domain because IEEE_MODE and
/// the guide's Quiet() payload are not pinned.
fn med3(a: u32, b: u32, c: u32) -> Option<u32> {
    if [a, b, c]
        .into_iter()
        .any(|x| nan(x) && x & 0x0040_0000 == 0)
    {
        return None;
    }
    let min3 = minmax(true, minmax(true, a, b), c);
    if nan(a) || nan(b) || nan(c) {
        return Some(min3);
    }
    let max3 = minmax(false, minmax(false, a, b), c);
    let equal = |x, y| f32::from_bits(x) == f32::from_bits(y);
    Some(if equal(max3, a) {
        minmax(false, b, c)
    } else if equal(max3, b) {
        minmax(false, a, c)
    } else {
        minmax(false, a, b)
    })
}

fn vgpr(n: u32) -> u32 {
    256 + n
}

fn vmov(reg: u32, value: u32) -> [u32; 2] {
    [0x7e00_02ff | reg << 17, value]
}

fn smov(reg: u32, value: u32) -> [u32; 2] {
    [0xbe80_03ff | reg << 16, value]
}

fn vop2(op: u32, dst: u32, src0: u32, vsrc1: u32) -> u32 {
    op << 25 | dst << 17 | vsrc1 << 9 | src0
}

fn vop3(op: u32, dst: u32, s0: u32, s1: u32, s2: u32, abs: u32, neg: u32) -> [u32; 2] {
    [
        0xd400_0000 | op << 16 | (abs & 7) << 8 | dst,
        s0 | s1 << 9 | s2 << 18 | (neg & 7) << 29,
    ]
}

fn legacy_vop3(dst: u32, s0: u32, s1: u32, abs: u32, neg: u32) -> [u32; 2] {
    vop3(263, dst, s0, s1, 128, abs, neg)
}

fn med3_vop3(dst: u32, s0: u32, s1: u32, s2: u32, abs: u32, neg: u32) -> [u32; 2] {
    vop3(343, dst, s0, s1, s2, abs, neg)
}

/// Load v1 = lane input, run setup and ALU words, move the result to v2 and
/// store one word per lane through the synthetic compute ABI.
fn program(setup: &[u32], alu: &[u32], result: u32) -> Vec<u8> {
    let mut code = vec![0xe030_2000, 0x8000_0100, 0xbf8c_3f70];
    code.extend(setup);
    code.extend(alu);
    if result != 2 {
        code.push(0x7e04_0300 | result);
    }
    code.extend([0xe070_2000, 0x8001_0200, 0xbbfd_0000, 0xbf81_0000]);
    words(&code)
}

fn case(
    form: &'static str,
    family: &'static str,
    label: String,
    code: Vec<u8>,
    input: Vec<u32>,
    expected: Vec<u32>,
) -> Case {
    Case {
        form,
        family,
        label,
        code,
        input,
        expected,
    }
}

fn legacy_cases() -> Vec<Case> {
    // Nonzero finite and infinite lanes. Zero lanes are deliberately absent
    // from exact cases because the forced-zero sign is not specified.
    let lanes = [
        0x3fc0_0000, // 1.5
        NEG_TWO,
        0x3f00_0000, // 0.5
        0xbe80_0000, // -0.25
        0x4080_0000, // 4
        0xc180_0000, // -16
        PINF,
        NINF,
        0x0080_0000, // minimum normal
        0x7f7f_ffff, // maximum finite
    ];
    let mut cases = Vec::new();
    for (b, tag) in [(TWO, "2"), (0xc080_0000, "-4"), (PINF, "+inf")] {
        let input: Vec<_> = lanes
            .into_iter()
            .filter(|&a| legacy_nonzero_product(a, b).is_some())
            .collect();
        let expected = input
            .iter()
            .map(|&a| legacy_nonzero_product(a, b).unwrap())
            .collect();
        let code = program(&vmov(3, b), &[vop2(7, 2, vgpr(1), 3)], 2);
        cases.push(case(
            "v_mul_legacy_f32",
            "legacy VOP2 exact",
            format!("VOP2 lanes times {tag}"),
            code,
            input,
            expected,
        ));
    }

    // VOP3 modifiers on both source positions. Bit 0 modifies SRC0 and bit 1
    // modifies SRC1; bit 2 is the unused SRC2 position and must stay clear.
    for (abs, neg, tag) in [
        (0, 0, "none"),
        (1, 0, "abs0"),
        (0, 1, "neg0"),
        (1, 1, "negabs0"),
        (2, 0, "abs1"),
        (0, 2, "neg1"),
        (2, 2, "negabs1"),
        (3, 3, "both"),
    ] {
        let b = NEG_TWO;
        let input = vec![0x3fc0_0000, NEG_ONE, TWO, PINF];
        let expected = input
            .iter()
            .map(|&x| {
                let a = modify(x, abs & 1 != 0, neg & 1 != 0);
                let b = modify(b, abs & 2 != 0, neg & 2 != 0);
                legacy_nonzero_product(a, b).unwrap()
            })
            .collect();
        let alu = legacy_vop3(2, vgpr(1), vgpr(3), abs, neg);
        let code = program(&vmov(3, b), &alu, 2);
        cases.push(case(
            "v_mul_legacy_f32",
            "legacy VOP3 modifiers",
            format!("VOP3 {tag}"),
            code,
            input,
            expected,
        ));
    }

    // A normal SGPR and a literal are accepted sources in the bounded VOP3 form.
    let input = vec![0x3fc0_0000, NEG_TWO];
    for (setup, alu, tag) in [
        (
            smov(8, TWO).to_vec(),
            legacy_vop3(2, 8, vgpr(1), 0, 0).to_vec(),
            "s8 SRC0",
        ),
        (
            Vec::new(),
            [legacy_vop3(2, 255, vgpr(1), 0, 0).as_slice(), &[TWO]].concat(),
            "literal SRC0",
        ),
    ] {
        let expected = input
            .iter()
            .map(|&x| legacy_nonzero_product(TWO, x).unwrap())
            .collect();
        cases.push(case(
            "v_mul_legacy_f32",
            "legacy VOP3 sources",
            format!("VOP3 {tag}"),
            program(&setup, &alu, 2),
            input.clone(),
            expected,
        ));
    }
    cases
}

fn uniform_med3_case(a: u32, b: u32, c: u32, family: &'static str, label: String) -> Case {
    let setup = [
        vmov(3, a).as_slice(),
        vmov(4, b).as_slice(),
        vmov(5, c).as_slice(),
    ]
    .concat();
    let alu = med3_vop3(2, vgpr(3), vgpr(4), vgpr(5), 0, 0);
    let expected = vec![med3(a, b, c).expect("accepted med3 case"); 32];
    case(
        "v_med3_f32",
        family,
        label,
        program(&setup, &alu, 2),
        vec![0; 32],
        expected,
    )
}

fn med3_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    // Ordinary numeric medians, including infinities and a subnormal. MED3 is
    // selection only, so the subnormal input is part of the bit-exact domain.
    for (triple, tag) in [
        ((ONE, TWO, THREE), "1,2,3"),
        ((THREE, ONE, TWO), "3,1,2"),
        ((NEG_ONE, NEG_THREE, NEG_TWO), "negative order"),
        ((NINF, ONE, PINF), "infinities"),
        ((MIN_SUB, ONE, NEG_ONE), "subnormal selection"),
    ] {
        cases.push(uniform_med3_case(
            triple.0,
            triple.1,
            triple.2,
            "med3 numeric",
            format!("numeric {tag}"),
        ));
    }

    // These tuples distinguish numeric equality from bit equality in the
    // guide's MAX3 source-position tests: +0 and -0 compare equal, and the
    // first matching source position decides which pair the final MAX reads.
    for (triple, tag) in [
        ((NZ, PZ, NZ), "-0,+0,-0"),
        ((NZ, PZ, NEG_ONE), "-0,+0,-1"),
        ((NZ, NZ, PZ), "-0,-0,+0"),
        ((NZ, NEG_ONE, PZ), "-0,-1,+0"),
        ((NEG_ONE, NZ, PZ), "-1,-0,+0"),
    ] {
        cases.push(uniform_med3_case(
            triple.0,
            triple.1,
            triple.2,
            "med3 signed zeros",
            format!("signed zero {tag}"),
        ));
    }

    // One quiet NaN gives the minimum of the two non-NaN sources; two quiet
    // NaNs leave the non-NaN source; three collapse through nested MIN to S2.
    for (triple, expected, tag) in [
        ((QNAN_A, TWO, ONE), ONE, "NaN in S0"),
        ((TWO, QNAN_A, ONE), ONE, "NaN in S1"),
        ((TWO, ONE, QNAN_A), ONE, "NaN in S2"),
        ((QNAN_A, QNAN_B, NEG_TWO), NEG_TWO, "two NaNs"),
        (
            (QNAN_A, QNAN_B, QNAN_C),
            QNAN_C,
            "three NaNs keep the S2 payload",
        ),
    ] {
        let case = uniform_med3_case(
            triple.0,
            triple.1,
            triple.2,
            "med3 quiet NaNs",
            format!("quiet {tag}"),
        );
        assert_eq!(case.expected[0], expected, "{tag}");
        cases.push(case);
    }

    // ABS/NEG on every source position. The expectation is computed from the
    // modified bit patterns, not by copying production modifier handling.
    for (abs, neg, tag) in [
        (1, 0, "abs0"),
        (0, 1, "neg0"),
        (2, 0, "abs1"),
        (0, 2, "neg1"),
        (4, 0, "abs2"),
        (0, 4, "neg2"),
        (7, 7, "negabs all"),
    ] {
        let (a, b, c) = (NEG_THREE, NEG_ONE, TWO);
        let x = modify(a, abs & 1 != 0, neg & 1 != 0);
        let y = modify(b, abs & 2 != 0, neg & 2 != 0);
        let z = modify(c, abs & 4 != 0, neg & 4 != 0);
        let setup = [
            vmov(3, a).as_slice(),
            vmov(4, b).as_slice(),
            vmov(5, c).as_slice(),
        ]
        .concat();
        let alu = med3_vop3(2, vgpr(3), vgpr(4), vgpr(5), abs, neg);
        cases.push(case(
            "v_med3_f32",
            "med3 modifiers",
            format!("med3 {tag}"),
            program(&setup, &alu, 2),
            vec![0; 32],
            vec![med3(x, y, z).unwrap(); 32],
        ));
    }

    // Scalar and literal sources in the narrowed VOP3 float domain.
    let setup = [
        smov(8, ONE).as_slice(),
        vmov(4, TWO).as_slice(),
        vmov(5, THREE).as_slice(),
    ]
    .concat();
    let alu = med3_vop3(2, 8, vgpr(4), vgpr(5), 0, 0);
    cases.push(case(
        "v_med3_f32",
        "med3 sources",
        "s8 SRC0".into(),
        program(&setup, &alu, 2),
        vec![0; 32],
        vec![TWO; 32],
    ));
    let alu = [med3_vop3(2, 255, vgpr(4), vgpr(5), 0, 0).as_slice(), &[ONE]].concat();
    cases.push(case(
        "v_med3_f32",
        "med3 sources",
        "literal SRC0".into(),
        program(&setup[2..], &alu, 2),
        vec![0; 32],
        vec![TWO; 32],
    ));
    cases
}

fn all_cases() -> Vec<Case> {
    let mut cases = legacy_cases();
    cases.extend(med3_cases());
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
                .then(|| format!("  {}: {output:?}, expected {:?}", case.label, case.expected))
        })
        .collect()
}

fn check_cpu_interpreters(cases: &[Case]) {
    let failed = failures(cases, |code, input| {
        xenora_ref::execute(code, input).map_err(|error| format!("{error:?}"))
    });
    assert!(
        failed.is_empty(),
        "reference disagrees:\n{}",
        failed.join("\n")
    );
    let failed = failures(cases, independent_ref::execute);
    assert!(
        failed.is_empty(),
        "independent interpreter disagrees:\n{}",
        failed.join("\n")
    );
}

#[test]
fn legacy_multiply_and_med3_match_the_guide_on_both_cpu_interpreters() {
    let cases = all_cases();
    assert_eq!(cases.len(), 3 + 8 + 2 + 5 + 5 + 5 + 7 + 2);
    check_cpu_interpreters(&cases);
    eprintln!(
        "vop3_med3_mullegacy: {} CPU cases per interpreter",
        cases.len()
    );
}

#[test]
fn compiler_accepts_the_legacy_multiply_and_med3_domain() {
    for case in all_cases() {
        compile(Architecture::Gfx1030, &case.code)
            .unwrap_or_else(|error| panic!("compiler rejected {}: {error:?}", case.label));
    }
}

/// Zero times NaN or infinity is the defining DX9 case. Both CPU interpreters
/// must produce a zero magnitude, but the sign is only an implementation
/// observation because the guide does not pin it.
#[test]
fn legacy_zero_times_nan_or_infinity_is_zero_without_claiming_a_spec_sign() {
    for (zero, other, tag) in [
        (PZ, QNAN_A, "+0*qNaN"),
        (NZ, QNAN_A, "-0*qNaN"),
        (PZ, PINF, "+0*+inf"),
        (NZ, NINF, "-0*-inf"),
        (PZ, MIN_SUB, "+0*subnormal"),
    ] {
        for reverse in [false, true] {
            let (a, b) = if reverse {
                (other, zero)
            } else {
                (zero, other)
            };
            let code = program(&vmov(3, b), &[vop2(7, 2, vgpr(1), 3)], 2);
            for (name, output) in [
                (
                    "reference",
                    xenora_ref::execute(&code, &[a]).map_err(|e| e.to_string()),
                ),
                ("independent", independent_ref::execute(&code, &[a])),
            ] {
                let output = output
                    .unwrap_or_else(|error| panic!("{tag} reverse={reverse} {name}: {error}"));
                assert_eq!(output.len(), 1);
                assert_eq!(
                    output[0] & 0x7fff_ffff,
                    0,
                    "{tag} reverse={reverse} {name}: {output:#x?}"
                );
                eprintln!(
                    "legacy forced zero {tag} reverse={reverse} {name}: sign bit {}",
                    output[0] >> 31
                );
            }
        }
    }
}

fn assert_rejected(code: &[u8], offset: usize, label: &str) {
    let compiler = compile(Architecture::Gfx1030, code).expect_err(label);
    let compiler_offset = match compiler {
        xenora_shader::CompileError::Verify { offset: at, .. } => at,
        xenora_shader::CompileError::Decode(error) => error.offset,
        other => panic!("{label}: unexpected compiler error {other:?}"),
    };
    assert_eq!(compiler_offset, offset, "{label}: compiler {compiler:?}");
    let error = xenora_ref::execute(code, &[0, 1]).expect_err(label);
    assert_eq!(error.offset, offset, "{label}: reference {error:?}");
    let error = independent_ref::execute(code, &[0, 1]).expect_err(label);
    assert!(
        error.starts_with(&format!("{offset:#x}:")),
        "{label}: independent {error}"
    );
}

#[test]
fn unsupported_vop3_modifier_and_source_combinations_are_refused() {
    let setup = [
        vmov(3, ONE).as_slice(),
        vmov(4, TWO).as_slice(),
        vmov(5, THREE).as_slice(),
    ]
    .concat();
    let offset = 12 + setup.len() * 4;
    let mut count = 0;
    let mut reject = |label: String, alu: &[u32]| {
        assert_rejected(&program(&setup, alu, 2), offset, &label);
        count += 1;
    };
    let legacy = legacy_vop3(2, vgpr(3), vgpr(4), 0, 0);
    // OP_SEL bits and CLAMP in the first word; OMOD in the second.
    for bit in 11..16 {
        reject(
            format!("legacy first modifier bit {bit}"),
            &[legacy[0] | 1 << bit, legacy[1]],
        );
    }
    for bit in [27, 28] {
        reject(
            format!("legacy OMOD bit {bit}"),
            &[legacy[0], legacy[1] | 1 << bit],
        );
    }
    // The unused legacy SRC2 must be inline zero and must not have modifiers.
    for src2 in [0, 127, 129, 255] {
        reject(
            format!("legacy SRC2 {src2}"),
            &[legacy[0], (legacy[1] & !(0x1ff << 18)) | src2 << 18],
        );
    }
    for (bit, value) in [(10, 4), (31, 4)] {
        let (head, tail) = if bit == 10 {
            (legacy[0] | value << 8, legacy[1])
        } else {
            (legacy[0], legacy[1] | value << 29)
        };
        reject(format!("legacy SRC2 modifier bit {bit}"), &[head, tail]);
    }
    // Inline integers, float inline constants, special sources and two literals.
    for selector in [107, 128, 160, 208, 240, 247, 251, 252, 253, 254] {
        reject(
            format!("legacy SRC0 selector {selector}"),
            &[legacy[0], (legacy[1] & !0x1ff) | selector],
        );
        reject(
            format!("legacy SRC1 selector {selector}"),
            &[legacy[0], (legacy[1] & !(0x1ff << 9)) | selector << 9],
        );
    }
    reject(
        "legacy two literals".into(),
        &[
            legacy[0],
            (legacy[1] & !(0x1ff | 0x1ff << 9)) | 255 | 255 << 9,
            ONE,
        ],
    );

    let med = med3_vop3(2, vgpr(3), vgpr(4), vgpr(5), 0, 0);
    for bit in 11..16 {
        reject(
            format!("med3 first modifier bit {bit}"),
            &[med[0] | 1 << bit, med[1]],
        );
    }
    for bit in [27, 28] {
        reject(format!("med3 OMOD bit {bit}"), &[med[0], med[1] | 1 << bit]);
    }
    for (shift, name) in [(0, "SRC0"), (9, "SRC1"), (18, "SRC2")] {
        for selector in [107, 128, 160, 208, 240, 247, 251, 252, 253, 254] {
            reject(
                format!("med3 {name} selector {selector}"),
                &[med[0], (med[1] & !(0x1ff << shift)) | selector << shift],
            );
        }
    }
    reject(
        "med3 two literals".into(),
        &[
            med[0],
            (med[1] & !(0x1ff | 0x1ff << 9)) | 255 | 255 << 9,
            ONE,
        ],
    );
    assert_eq!(count, 5 + 2 + 4 + 2 + 20 + 1 + 5 + 2 + 30 + 1);
    eprintln!("vop3_med3_mullegacy: {count} controlled rejections");
}

#[test]
#[ignore = "requires a native Metal device with API and shader validation"]
fn legacy_multiply_and_med3_match_the_guide_on_native_metal() {
    assert_eq!(std::env::var("MTL_DEBUG_LAYER").as_deref(), Ok("1"));
    assert_eq!(std::env::var("MTL_SHADER_VALIDATION").as_deref(), Ok("1"));
    let cases = all_cases();
    let (failures, programs) = harness::check_metal(&cases);
    failures.assert_none("native Metal disagrees with the guide");
    eprintln!(
        "vop3_med3_mullegacy: {} Metal cases in {programs} programs",
        cases.len()
    );
}

#[test]
#[ignore = "requires a native Metal device with API and shader validation"]
fn native_metal_legacy_zero_sign_is_reported_not_specified() {
    use xenora_shader::ENTRY_POINT;
    let metal = xenora_metal::NativeCompute::new().unwrap();
    for (zero, other, tag) in [(PZ, QNAN_A, "+0*qNaN"), (NZ, PINF, "-0*+inf")] {
        let code = program(&vmov(3, other), &[vop2(7, 2, vgpr(1), 3)], 2);
        let compiled = compile(Architecture::Gfx1030, &code).unwrap();
        let output = metal
            .run_words(&compiled.msl, ENTRY_POINT, &[zero])
            .unwrap();
        assert_eq!(output[0] & 0x7fff_ffff, 0, "{tag}: {output:#x?}");
        eprintln!(
            "native Metal forced zero {tag}: sign bit {}",
            output[0] >> 31
        );
    }
}
