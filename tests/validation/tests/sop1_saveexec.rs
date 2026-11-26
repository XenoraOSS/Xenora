// SPDX-License-Identifier: GPL-3.0-or-later

//! Independent expected values for the twelve 32-bit SOP1 SAVEEXEC and
//! WREXEC forms (#740): `s_and_saveexec_b32`, `s_or_saveexec_b32`,
//! `s_xor_saveexec_b32`, `s_andn2_saveexec_b32`, `s_orn2_saveexec_b32`,
//! `s_nand_saveexec_b32`, `s_nor_saveexec_b32`, `s_xnor_saveexec_b32`,
//! `s_andn1_saveexec_b32`, `s_orn1_saveexec_b32`, `s_andn1_wrexec_b32` and
//! `s_andn2_wrexec_b32`.
//!
//! The expectations are written below from the public AMD "RDNA 2" Instruction
//! Set Architecture Reference Guide only: section 12.3 "SOP1 Instructions"
//! (the pseudocode on printed pages 120-122), section 3.3 (printed page 13)
//! for the width of EXEC under Wave32 and for the rule that the mask does not
//! affect scalar (ALU or memory) execution or branches, and section 13.1.3 for
//! the SOP1 word. Nothing here comes from the ISA crate, the semantic
//! registry, the decoder or the shader compiler, and no expected value is
//! taken from either interpreter. Every case runs on the reference
//! interpreter `xenora_ref` and on the second interpreter `independent_ref`;
//! the ignored test also runs the translated MSL on native Metal.
//!
//! Each case observes three things:
//!
//! * the destination, read back as `s10` into a VGPR under a full mask,
//! * SCC, which the guide sets to `(EXEC_LO != 0)` and which a
//!   `s_cbranch_scc0` turns into the low bit of every lane's output,
//! * the new EXEC_LO, seen through the lanes a later vector instruction runs
//!   under.

#[path = "operand_domains/harness.rs"]
#[allow(dead_code)]
mod harness;
#[path = "../independent_ref.rs"]
mod independent_ref;

use std::collections::BTreeMap;

use harness::{Case, words};
use xenora_shader::{Architecture, CompileError, compile};

/// The twelve forms with their ENC_SOP1 opcodes (guide 12.3).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Form {
    AndSaveExec = 60,
    OrSaveExec = 61,
    XorSaveExec = 62,
    Andn2SaveExec = 63,
    Orn2SaveExec = 64,
    NandSaveExec = 65,
    NorSaveExec = 66,
    XnorSaveExec = 67,
    Andn1SaveExec = 68,
    Orn1SaveExec = 69,
    Andn1Wrexec = 70,
    Andn2Wrexec = 71,
}

const FORMS: [(Form, &str); 12] = [
    (Form::AndSaveExec, "s_and_saveexec_b32"),
    (Form::OrSaveExec, "s_or_saveexec_b32"),
    (Form::XorSaveExec, "s_xor_saveexec_b32"),
    (Form::Andn2SaveExec, "s_andn2_saveexec_b32"),
    (Form::Orn2SaveExec, "s_orn2_saveexec_b32"),
    (Form::NandSaveExec, "s_nand_saveexec_b32"),
    (Form::NorSaveExec, "s_nor_saveexec_b32"),
    (Form::XnorSaveExec, "s_xnor_saveexec_b32"),
    (Form::Andn1SaveExec, "s_andn1_saveexec_b32"),
    (Form::Orn1SaveExec, "s_orn1_saveexec_b32"),
    (Form::Andn1Wrexec, "s_andn1_wrexec_b32"),
    (Form::Andn2Wrexec, "s_andn2_wrexec_b32"),
];

/// The guide's pseudocode for the twelve forms (section 12.3, printed pages
/// 120-122), transcribed here: the destination value, the new EXEC_LO and the
/// SCC the form sets. The ten SAVEEXEC forms write the *previous* EXEC_LO to
/// the destination and then update EXEC_LO; the two WREXEC forms update
/// EXEC_LO first and write the *new* value.
fn expect(form: Form, s0: u32, exec: u32) -> (u32, u32, bool) {
    let updated = match form {
        // 60 S_AND_SAVEEXEC_B32: EXEC_LO = S0.u32 & EXEC_LO.
        Form::AndSaveExec => s0 & exec,
        // 61 S_OR_SAVEEXEC_B32: EXEC_LO = S0.u32 | EXEC_LO.
        Form::OrSaveExec => s0 | exec,
        // 62 S_XOR_SAVEEXEC_B32: EXEC_LO = S0.u32 ^ EXEC_LO.
        Form::XorSaveExec => s0 ^ exec,
        // 63 S_ANDN2_SAVEEXEC_B32: EXEC_LO = S0.u32 & ~EXEC_LO.
        Form::Andn2SaveExec => s0 & !exec,
        // 64 S_ORN2_SAVEEXEC_B32: EXEC_LO = S0.u32 | ~EXEC_LO.
        Form::Orn2SaveExec => s0 | !exec,
        // 65 S_NAND_SAVEEXEC_B32: EXEC_LO = ~(S0.u32 & EXEC_LO).
        Form::NandSaveExec => !(s0 & exec),
        // 66 S_NOR_SAVEEXEC_B32: EXEC_LO = ~(S0.u32 | EXEC_LO).
        Form::NorSaveExec => !(s0 | exec),
        // 67 S_XNOR_SAVEEXEC_B32: EXEC_LO = ~(S0.u32 ^ EXEC_LO).
        Form::XnorSaveExec => !(s0 ^ exec),
        // 68 S_ANDN1_SAVEEXEC_B32: EXEC_LO = ~S0.u32 & EXEC_LO.
        Form::Andn1SaveExec => !s0 & exec,
        // 69 S_ORN1_SAVEEXEC_B32: EXEC_LO = ~S0.u32 | EXEC_LO.
        Form::Orn1SaveExec => !s0 | exec,
        // 70 S_ANDN1_WREXEC_B32: EXEC_LO = ~S0.u32 & EXEC_LO.
        Form::Andn1Wrexec => !s0 & exec,
        // 71 S_ANDN2_WREXEC_B32: EXEC_LO = S0.u32 & ~EXEC_LO.
        Form::Andn2Wrexec => s0 & !exec,
    };
    let destination = match form {
        Form::Andn1Wrexec | Form::Andn2Wrexec => updated,
        _ => exec,
    };
    // SCC = (EXEC_LO != 0), with the new EXEC_LO.
    (destination, updated, updated != 0)
}

/// Values read by hand from the guide's pseudocode: the destination, the new
/// EXEC_LO and SCC for a source and a starting mask. They are the check on
/// the transcription in `expect`.
/// One hand-written row: the form, the source, the starting mask and the
/// destination, the new EXEC_LO and SCC the guide's pseudocode gives.
type GuideRow = (Form, u32, u32, (u32, u32, bool));

const GUIDE_VALUES: [GuideRow; 20] = [
    // S0 = 0xF0F0F0F0, EXEC_LO = 0x0FF00FF0: the AND keeps 0x00F000F0.
    (
        Form::AndSaveExec,
        0xf0f0_f0f0,
        0x0ff0_0ff0,
        (0x0ff0_0ff0, 0x00f0_00f0, true),
    ),
    // S0 = 0x000000FF, EXEC_LO = 0xFFFF0000: the OR is 0xFFFF00FF.
    (
        Form::OrSaveExec,
        0x0000_00ff,
        0xffff_0000,
        (0xffff_0000, 0xffff_00ff, true),
    ),
    // S0 = 0xFFFFFFFF, EXEC_LO = 0x0F0F0F0F: the XOR is 0xF0F0F0F0.
    (
        Form::XorSaveExec,
        0xffff_ffff,
        0x0f0f_0f0f,
        (0x0f0f_0f0f, 0xf0f0_f0f0, true),
    ),
    // S0 = 0x12345678, EXEC_LO = 0x000000FF: the ANDN2 is 0x12345600.
    (
        Form::Andn2SaveExec,
        0x1234_5678,
        0x0000_00ff,
        (0x0000_00ff, 0x1234_5600, true),
    ),
    // S0 = 0x000000FF, EXEC_LO = 0x000000FF: the ORN2 is 0xFFFFFFFF.
    (
        Form::Orn2SaveExec,
        0x0000_00ff,
        0x0000_00ff,
        (0x0000_00ff, 0xffff_ffff, true),
    ),
    // S0 = 0xFFFFFFFF, EXEC_LO = 0xFFFFFFFF: the NAND is zero.
    (
        Form::NandSaveExec,
        0xffff_ffff,
        0xffff_ffff,
        (0xffff_ffff, 0x0000_0000, false),
    ),
    // S0 = 0x0F0F0F0F, EXEC_LO = 0x0F0F0F0F: the NOR is 0xF0F0F0F0.
    (
        Form::NorSaveExec,
        0x0f0f_0f0f,
        0x0f0f_0f0f,
        (0x0f0f_0f0f, 0xf0f0_f0f0, true),
    ),
    // S0 = 0xAAAAAAAA, EXEC_LO = 0x55555555: the XNOR is zero.
    (
        Form::XnorSaveExec,
        0xaaaa_aaaa,
        0x5555_5555,
        (0x5555_5555, 0x0000_0000, false),
    ),
    // S0 = 0xAAAAAAAA, EXEC_LO = 0xFFFFFFFF: the ANDN1 is 0x55555555.
    (
        Form::Andn1SaveExec,
        0xaaaa_aaaa,
        0xffff_ffff,
        (0xffff_ffff, 0x5555_5555, true),
    ),
    // S0 = 0, EXEC_LO = 0: the ORN1 is 0xFFFFFFFF.
    (
        Form::Orn1SaveExec,
        0x0000_0000,
        0x0000_0000,
        (0x0000_0000, 0xffff_ffff, true),
    ),
    // S0 = 0x0F0F0F0F, EXEC_LO = 0x000000FF: the ANDN1 WREXEC is 0x000000F0.
    (
        Form::Andn1Wrexec,
        0x0f0f_0f0f,
        0x0000_00ff,
        (0x0000_00f0, 0x0000_00f0, true),
    ),
    // S0 = 0xF0F0F0F0, EXEC_LO = 0x0F0F0F0F: the ANDN2 WREXEC is 0xF0F0F0F0.
    (
        Form::Andn2Wrexec,
        0xf0f0_f0f0,
        0x0f0f_0f0f,
        (0xf0f0_f0f0, 0xf0f0_f0f0, true),
    ),
    // EXEC_LO = 0: the form still runs, and the AND leaves EXEC_LO zero.
    (
        Form::AndSaveExec,
        0x1234_5678,
        0x0000_0000,
        (0x0000_0000, 0x0000_0000, false),
    ),
    // EXEC_LO = 0: the XOR still runs and sets EXEC_LO to the source.
    (
        Form::XorSaveExec,
        0x1234_5678,
        0x0000_0000,
        (0x0000_0000, 0x1234_5678, true),
    ),
    // EXEC_LO = 0: the OR still runs and sets EXEC_LO to the source.
    (
        Form::OrSaveExec,
        0x1234_5678,
        0x0000_0000,
        (0x0000_0000, 0x1234_5678, true),
    ),
    // EXEC_LO = 0: the ANDN1 WREXEC still runs and leaves EXEC_LO zero.
    (
        Form::Andn1Wrexec,
        0x1234_5678,
        0x0000_0000,
        (0x0000_0000, 0x0000_0000, false),
    ),
    // EXEC_LO = 0: the ANDN2 WREXEC still runs and sets EXEC_LO to the source.
    (
        Form::Andn2Wrexec,
        0x1234_5678,
        0x0000_0000,
        (0x1234_5678, 0x1234_5678, true),
    ),
    // S0 = 1, EXEC_LO = 2: the AND of two disjoint masks is zero.
    (
        Form::AndSaveExec,
        0x0000_0001,
        0x0000_0002,
        (0x0000_0002, 0x0000_0000, false),
    ),
    // S0 = 1, EXEC_LO = 2: the ORN1 is 0xFFFFFFFE.
    (
        Form::Orn1SaveExec,
        0x0000_0001,
        0x0000_0002,
        (0x0000_0002, 0xffff_fffe, true),
    ),
    // S0 = 0x000000FF, EXEC_LO = 0xFFFFFF00: the NAND is 0xFFFFFFFF.
    (
        Form::NandSaveExec,
        0x0000_00ff,
        0xffff_ff00,
        (0xffff_ff00, 0xffff_ffff, true),
    ),
];

/// The starting masks and source values, each with a label. They include a
/// full mask, an empty mask, single-lane masks and masks whose bits do not
/// overlap the source.
const PAIRS: [(&str, u32, u32); 12] = [
    ("full mask", 0xffff_ffff, 0xffff_ffff),
    ("full mask, zero source", 0x0000_0000, 0xffff_ffff),
    ("single lane", 0x0000_0001, 0x0000_0001),
    ("single lane, other lane", 0x0000_0001, 0x0000_0002),
    ("no overlap", 0x0000_00ff, 0xffff_ff00),
    ("same bits", 0x0000_00ff, 0x0000_00ff),
    ("alternating", 0xaaaa_aaaa, 0xaaaa_aaaa),
    ("complementary", 0x0f0f_0f0f, 0xf0f0_f0f0),
    ("sign bit", 0x8000_0000, 0x0000_0001),
    ("empty mask, full source", 0xffff_ffff, 0x0000_0000),
    ("empty mask, zero source", 0x0000_0000, 0x0000_0000),
    ("empty mask, mixed source", 0x1234_5678, 0x0000_0000),
];

/// ENCODING 0b10_1111101 in bits 23-31, SDST 16-22, OP 8-15, SSRC0 0-7
/// (guide 13.1.3).
fn sop1(op: u32, dst: u32, src0: u32) -> u32 {
    0xbe80_0000 | dst << 16 | op << 8 | src0
}

/// `s_mov_b32 s<dst>, <value>` with a literal dword.
fn smov_literal(dst: u32, value: u32) -> [u32; 2] {
    [0xbe80_03ff | dst << 16, value]
}

/// One wave of 32 lanes: 0 and !0 for the destination family's XOR trick, and
/// values that are neither 0 nor 1 for the rest, so that a lane the new mask
/// excludes is told apart from one it includes.
fn input() -> Vec<u32> {
    let mut input = vec![0, u32::MAX];
    input.extend((2..32).map(|lane| 0x0100_0000 | lane));
    input
}

/// The program that reads the destination and SCC back: load v1, set s8 to
/// the source and EXEC_LO to the starting mask, run the form, restore a full
/// EXEC_LO, copy s10 into v2, and let `s_cbranch_scc0` decide whether v2 is
/// exclusive-ored with the lane input. With the lane input [0, !0] the
/// output is [D, D] when SCC is clear and [D, !D] when it is set.
fn destination_program(op: u32, s0: u32, exec: u32) -> Vec<u8> {
    let mut code = vec![0xe030_2000, 0x8000_0100, 0xbf8c_3f70];
    code.extend(smov_literal(8, s0));
    code.extend(smov_literal(126, exec));
    code.push(sop1(op, 10, 8));
    code.push(0xbefe_03c1); // s_mov_b32 exec_lo, -1: a full mask again
    code.push(0x7e04_020a); // v_mov_b32_e32 v2, s10
    code.push(0xbf84_0001); // s_cbranch_scc0 over the next instruction
    code.push(0x3a04_0501); // v_xor_b32 v2, v1, v2
    code.extend([0xe070_2000, 0x8001_0200, 0xbbfd_0000, 0xbf81_0000]);
    words(&code)
}

/// The program that reads the new EXEC_LO back: load the lane input into v2,
/// set s9 to 1, run the form, and let a later vector instruction write 1
/// into the lanes the new mask covers. A lane the new mask excludes keeps the
/// lane input, so the output is 1 in the covered lanes and the input in the
/// others.
fn mask_program(op: u32, s0: u32, exec: u32) -> Vec<u8> {
    let mut code = vec![0xe030_2000, 0x8000_0200, 0xbf8c_3f70];
    code.extend(smov_literal(8, s0));
    code.extend(smov_literal(9, 1));
    code.extend(smov_literal(126, exec));
    code.push(sop1(op, 10, 8));
    code.push(0x7e04_0209); // v_mov_b32_e32 v2, s9
    code.push(0xbefe_03c1); // s_mov_b32 exec_lo, -1: a full mask again
    code.extend([0xe070_2000, 0x8001_0200, 0xbbfd_0000, 0xbf81_0000]);
    words(&code)
}

/// The SOP1 word's byte offset in `destination_program`.
const ALU_OFFSET: usize = 7 * 4;

fn cases() -> Vec<Case> {
    let input = input();
    let mut cases = Vec::new();
    for (form, name) in FORMS {
        let op = form as u32;
        for (pair, s0, exec) in PAIRS {
            // The families with an empty starting mask carry the guide's rule
            // (section 3.3, printed page 13) that the mask does not affect
            // scalar execution: the form still runs and still writes.
            let family = if exec == 0 {
                "saveexec exec zero"
            } else {
                "saveexec destination"
            };
            let (destination, _updated, scc) = expect(form, s0, exec);
            let expected: Vec<u32> = input
                .iter()
                .map(|&value| {
                    if scc {
                        value ^ destination
                    } else {
                        destination
                    }
                })
                .collect();
            cases.push(Case {
                form: name,
                family,
                label: format!("{name} {pair}, s0 {s0:#x}, exec {exec:#x}"),
                code: destination_program(op, s0, exec),
                input: input.clone(),
                expected,
            });
            let (_destination, updated, _scc) = expect(form, s0, exec);
            let expected: Vec<u32> = input
                .iter()
                .enumerate()
                .map(
                    |(lane, &value)| {
                        if updated >> lane & 1 == 1 { 1 } else { value }
                    },
                )
                .collect();
            cases.push(Case {
                form: name,
                family: if exec == 0 {
                    "saveexec exec zero"
                } else {
                    "saveexec mask"
                },
                label: format!("{name} {pair}, s0 {s0:#x}, exec {exec:#x}"),
                code: mask_program(op, s0, exec),
                input: input.clone(),
                expected,
            });
        }
    }
    cases
}

fn tally(cases: &[Case], key: fn(&Case) -> &'static str) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for case in cases {
        *counts.entry(key(case)).or_default() += 1;
    }
    counts
}

/// Runs `execute` on every case and returns one line per failing family: the
/// number of failures and the first one.
fn failures(
    cases: &[Case],
    execute: impl Fn(&[u8], &[u32]) -> Result<Vec<u32>, String>,
) -> Vec<String> {
    let mut failed: BTreeMap<&str, (usize, String)> = BTreeMap::new();
    for case in cases {
        let output = execute(&case.code, &case.input);
        if output.as_deref().ok() != Some(case.expected.as_slice()) {
            let first = format!("{}: {output:?}, expected {:?}", case.label, case.expected);
            failed.entry(case.family).or_insert((0, first)).0 += 1;
        }
    }
    failed
        .into_iter()
        .map(|(family, (n, first))| format!("  {family}: {n} failed, first: {first}"))
        .collect()
}

fn checked_cases() -> Vec<Case> {
    let cases = cases();
    assert_eq!(
        tally(&cases, |case| case.family),
        BTreeMap::from([
            ("saveexec destination", 12 * 9),
            ("saveexec exec zero", 12 * 3 * 2),
            ("saveexec mask", 12 * 9),
        ])
    );
    cases
}

#[test]
fn expectations_reproduce_the_hand_written_values() {
    for (form, s0, exec, (destination, updated, scc)) in GUIDE_VALUES {
        assert_eq!(
            expect(form, s0, exec),
            (destination, updated, scc),
            "{form:?}({s0:#x}, {exec:#x})"
        );
    }
}

#[test]
fn saveexec_forms_match_the_guide_on_the_reference() {
    let cases = checked_cases();
    let failed = failures(&cases, |code, input| {
        xenora_ref::execute(code, input).map_err(|error| format!("{error:?}"))
    });
    assert!(
        failed.is_empty(),
        "reference disagrees with the guide:\n{}",
        failed.join("\n")
    );
    eprintln!("sop1_saveexec: {} reference cases", cases.len());
    for (form, n) in tally(&cases, |case| case.form) {
        eprintln!("  form {form}: {n}");
    }
}

#[test]
fn saveexec_forms_match_the_guide_on_the_independent_interpreter() {
    let cases = checked_cases();
    let failed = failures(&cases, independent_ref::execute);
    assert!(
        failed.is_empty(),
        "independent interpreter disagrees with the guide:\n{}",
        failed.join("\n")
    );
    eprintln!(
        "sop1_saveexec: {} independent-interpreter cases",
        cases.len()
    );
}

/// The programs both interpreters must refuse at the SOP1 word.
fn rejections() -> Vec<(String, Vec<u8>)> {
    let mut all = Vec::new();
    for (form, name) in FORMS {
        let op = form as u32;
        // VCC_LO, VCC_HI, M0, TTMP, NULL, EXEC_LO and EXEC_HI as SDST.
        for dst in [106, 107, 124, 125, 126, 127] {
            let mut code = destination_program(op, 0x1234_5678, 0xffff_ffff);
            // Replace the SOP1 word with the refused destination.
            code[ALU_OFFSET..ALU_OFFSET + 4].copy_from_slice(&sop1(op, dst, 8).to_le_bytes());
            all.push((format!("{name} destination {dst}"), code));
        }
        // VCC_LO, VCC_HI, M0, TTMP, EXEC_LO, EXEC_HI, the unused selector 253
        // and the binary32 inline constants 240-248 as SSRC0.
        for src in [106, 107, 124, 125, 126, 127, 253]
            .into_iter()
            .chain(240..=248)
        {
            let mut code = destination_program(op, 0x1234_5678, 0xffff_ffff);
            code[ALU_OFFSET..ALU_OFFSET + 4].copy_from_slice(&sop1(op, 10, src).to_le_bytes());
            all.push((format!("{name} source {src}"), code));
        }
    }
    all
}

#[test]
fn unsupported_saveexec_operands_are_rejected() {
    for (label, code) in rejections() {
        match xenora_ref::execute(&code, &input()) {
            Ok(_) => panic!("reference accepted {label}"),
            Err(error) => assert_eq!(
                error.offset, ALU_OFFSET,
                "reference rejected {label} elsewhere"
            ),
        }
        match independent_ref::execute(&code, &input()) {
            Ok(_) => panic!("independent accepted {label}"),
            Err(error) => assert!(
                error.starts_with(&format!("{ALU_OFFSET:#x}:")),
                "independent rejected {label} elsewhere: {error}"
            ),
        }
        match compile(Architecture::Gfx1030, &code) {
            Err(CompileError::Decode(error)) => assert_eq!(
                error.offset, ALU_OFFSET,
                "compiler rejected {label} elsewhere: {error:?}"
            ),
            Err(CompileError::Verify { offset, .. }) => {
                assert_eq!(offset, ALU_OFFSET, "compiler rejected {label} elsewhere")
            }
            other => panic!("compiler did not reject {label}: {other:?}"),
        }
    }
}

#[test]
#[ignore = "requires a native Metal device with API and shader validation"]
fn saveexec_forms_match_the_guide_on_native_metal() {
    assert_eq!(std::env::var("MTL_DEBUG_LAYER").as_deref(), Ok("1"));
    assert_eq!(std::env::var("MTL_SHADER_VALIDATION").as_deref(), Ok("1"));
    let cases = checked_cases();
    let (failures, programs) = harness::check_metal(&cases);
    failures.assert_none("native Metal disagrees with the guide");
    eprintln!(
        "sop1_saveexec: {} Metal cases in {programs} programs",
        cases.len()
    );
}
