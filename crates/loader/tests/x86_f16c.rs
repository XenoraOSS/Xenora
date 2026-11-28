// SPDX-License-Identifier: GPL-3.0-or-later

//! F16C conversions against independent bit-exact vectors.
//!
//! `tests/x86/f16c.txt` pairs LLVM encodings with Intel-syntax text.
//! `tests/x86/f16c_vectors.txt` comes from `tests/x86/f16c_reference.py`, which
//! rounds exact `Fraction` values by definition and shares no code with
//! `softfloat.rs`, the registry, the decoder or the executor. Every vector is
//! run on each matching register and memory form in both engines.

use std::collections::BTreeMap;
use xenora_loader::cpu::{CpuState, ExecutionResult, ExitReason, Interpreter, Mxcsr, RFlags};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};

const ENTRY: u64 = 0x1000;
const DATA: u64 = 0x2000;
/// Address of `[rsi + 4*rdi + 16]` with RSI = DATA and RDI = 4.
const OPERAND: u64 = DATA + 32;
const FILLER: u128 = 0x1122_3344_5566_7788_99AA_BBCC_DDEE_FF00;

fn fixture() -> BTreeMap<&'static str, Vec<u8>> {
    include_str!("x86/f16c.txt")
        .lines()
        .map(|line| {
            let (hex, text) = line.split_once(" | ").unwrap();
            let code = hex
                .split(' ')
                .map(|byte| u8::from_str_radix(byte, 16).unwrap())
                .collect();
            (text, code)
        })
        .collect()
}

#[derive(Debug)]
struct Vector {
    name: &'static str,
    imm: u32,
    mxcsr: u32,
    src0: u128,
    src1: u128,
    result0: u128,
    result1: u128,
    flags: u32,
    width: usize,
}

fn vectors() -> Vec<Vector> {
    include_str!("x86/f16c_vectors.txt")
        .lines()
        .map(|line| {
            let mut fields = line.split(' ');
            let name = match fields.next().unwrap() {
                "vcvtph2ps" => "vcvtph2ps",
                "vcvtps2ph" => "vcvtps2ph",
                other => panic!("{other}"),
            };
            let number = |field: Option<&str>| u128::from_str_radix(field.unwrap(), 16).unwrap();
            let width = fields.next().unwrap().parse().unwrap();
            let imm = number(fields.next()) as u32;
            let mxcsr = number(fields.next()) as u32;
            let src0 = number(fields.next());
            let src1 = number(fields.next());
            let result0 = number(fields.next());
            let result1 = number(fields.next());
            let flags = number(fields.next()) as u32;
            assert!(fields.next().is_none());
            Vector {
                name,
                imm,
                mxcsr,
                src0,
                src1,
                result0,
                result1,
                flags,
                width,
            }
        })
        .collect()
}

/// The register and memory form of one vector. VCVTPS2PH writes an xmm
/// destination even for a 256-bit source.
fn forms(vector: &Vector) -> [String; 2] {
    let place = "ptr [rsi + 4*rdi + 16]";
    let imm = vector.imm;
    if vector.name == "vcvtph2ps" {
        if vector.width == 128 {
            [
                "vcvtph2ps xmm3, xmm9".into(),
                format!("vcvtph2ps xmm3, qword {place}"),
            ]
        } else {
            [
                "vcvtph2ps ymm3, xmm9".into(),
                format!("vcvtph2ps ymm3, xmmword {place}"),
            ]
        }
    } else if vector.width == 128 {
        [
            format!("vcvtps2ph xmm3, xmm9, {imm}"),
            format!("vcvtps2ph qword {place}, xmm9, {imm}"),
        ]
    } else {
        [
            format!("vcvtps2ph xmm3, ymm9, {imm}"),
            format!("vcvtps2ph xmmword {place}, ymm9, {imm}"),
        ]
    }
}

fn width(vector: &Vector, text: &str) -> usize {
    if vector.name == "vcvtph2ps" {
        if text.contains("ymm3") { 256 } else { 128 }
    } else if text.contains("ymm9") {
        256
    } else {
        128
    }
}

fn load(code: &[u8], vector: &Vector) -> (CpuState, GuestMemory) {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    let mut data = vec![0xEE; 0x100];
    if vector.name == "vcvtph2ps" {
        // A widening source occupies 8 or 16 bytes: 4 or 8 halves packed from
        // the two source words.
        data[32..40].copy_from_slice(&vector.src0.to_le_bytes()[..8]);
        data[40..48].copy_from_slice(&vector.src1.to_le_bytes()[..8]);
    } else {
        data[32..48].copy_from_slice(&vector.src0.to_le_bytes());
        data[48..64].copy_from_slice(&vector.src1.to_le_bytes());
    }
    memory
        .map_region_with_data(DATA, 0x100, MemoryPermissions::READ_WRITE, &data)
        .unwrap();
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr[6] = DATA;
    cpu.gpr[7] = 4;
    cpu.xmm[3] = FILLER;
    cpu.ymm_high[3] = FILLER;
    if vector.name == "vcvtph2ps" {
        // A widening source is 4 or 8 binary16 lanes in the low 64 or 128 bits
        // of one xmm register, never in the high half of a ymm.
        cpu.xmm[9] = vector.src0 | (vector.src1 << 64);
        cpu.ymm_high[9] = FILLER;
    } else {
        cpu.xmm[9] = vector.src0;
        cpu.ymm_high[9] = vector.src1;
    }
    cpu.mxcsr = Mxcsr(vector.mxcsr);
    cpu.flags = RFlags {
        cf: true,
        zf: false,
        sf: true,
        of: true,
        pf: false,
        af: true,
    };
    (cpu, memory)
}

fn run(native: bool, cpu: &CpuState, memory: &mut GuestMemory) -> ExecutionResult {
    if native {
        DbtEngine::new(64 * 1024)
            .unwrap()
            .run(&mut cpu.clone(), memory, 1)
    } else {
        Interpreter::new(cpu.clone(), memory, 1).run()
    }
}

fn data(memory: &GuestMemory, size: usize) -> Vec<u8> {
    let mut out = vec![0; size];
    memory.read_bytes(OPERAND, &mut out).unwrap();
    out
}

/// MXCSR flags recorded on an aborted instruction, the destination left alone
/// and the completion status. Pre-computation flags are the only ones recorded
/// when the invalid or denormal exception is unmasked.
fn outcome(vector: &Vector) -> (u32, bool) {
    let masks = vector.mxcsr >> 7 & 0x3F;
    let before = vector.flags & 7;
    if before & !masks != 0 {
        return (before, true);
    }
    (vector.flags, vector.flags & !masks != 0)
}

#[test]
fn every_f16c_vector_matches_in_both_engines_from_registers_and_memory() {
    let code = fixture();
    let all = vectors();
    assert!(all.len() > 800, "{}", all.len());
    let mut checked = 0;
    for vector in &all {
        for text in &forms(vector) {
            let length = width(vector, text);
            let bytes = code.get(text.as_str()).unwrap_or_else(|| panic!("{text}"));
            let (flags, aborted) = outcome(vector);
            for native in [false, true] {
                let (cpu, mut memory) = load(bytes, vector);
                let result = run(native, &cpu, &mut memory);
                let engine = if native { "DBT" } else { "interpreter" };
                let expected_exit = if aborted {
                    ExitReason::SimdException
                } else {
                    ExitReason::StepLimitExceeded
                };
                assert_eq!(
                    result.exit_reason, expected_exit,
                    "{text} {vector:x?} {engine}"
                );
                assert_eq!(
                    result.final_state.mxcsr,
                    Mxcsr(vector.mxcsr | flags),
                    "{text} {vector:x?} {engine}"
                );
                assert_eq!(result.final_state.flags, cpu.flags, "{text} {engine}");
                let mut expected = cpu.clone();
                expected.rip = ENTRY + bytes.len() as u64;
                if !aborted {
                    if vector.name == "vcvtph2ps" {
                        expected.xmm[3] = vector.result0;
                        expected.ymm_high[3] = if length == 256 { vector.result1 } else { 0 };
                    } else if !text.contains("ptr") {
                        expected.xmm[3] = if length == 256 {
                            vector.result0
                        } else {
                            vector.result0 & (u64::MAX as u128)
                        };
                        expected.ymm_high[3] = 0;
                    }
                }
                assert_eq!(
                    result.final_state.xmm, expected.xmm,
                    "{text} {vector:x?} {engine}"
                );
                assert_eq!(
                    result.final_state.ymm_high, expected.ymm_high,
                    "{text} {vector:x?} {engine}"
                );
                let size = if text.contains("qword") {
                    8
                } else if text.contains("xmmword") {
                    16
                } else {
                    0
                };
                if size != 0 {
                    let is_store = vector.name == "vcvtps2ph";
                    let mut wanted = if is_store {
                        vector.src0.to_le_bytes()[..size].to_vec()
                    } else {
                        let packed = vector.src0 | (vector.src1 << 64);
                        packed.to_le_bytes()[..size].to_vec()
                    };
                    if is_store && !aborted {
                        wanted.copy_from_slice(&vector.result0.to_le_bytes()[..size]);
                    }
                    assert_eq!(data(&memory, size), wanted, "{text} {engine}");
                }
            }
            checked += 1;
        }
    }
    assert!(checked > 1700, "{checked}");
}

/// VCvtPS2PH takes its rounding mode from imm8[1:0], or from MXCSR.RC when
/// imm8[2] is set. imm8[7:3] is ignored. 1 + 2^-11 is halfway between 1 and
/// the next binary16 value, so nearest picks the even one and up rounds away.
#[test]
fn the_rounding_mode_comes_from_the_immediate_or_mxcsr() {
    let code = fixture();
    let halfway = 0x3F80_1000_u128;
    for (imm, mxcsr, wanted) in [
        (0, 0x1F80, 0x3C00_u128),
        (1, 0x1F80, 0x3C00),
        (2, 0x1F80, 0x3C01),
        (3, 0x1F80, 0x3C00),
        (4, 0x1F80 | (2 << 13), 0x3C01),
        (7, 0x1F80 | (2 << 13), 0x3C01),
        (255, 0x1F80, 0x3C00),
    ] {
        let text = format!("vcvtps2ph xmm3, xmm9, {imm}");
        let bytes = &code[text.as_str()];
        let vector = Vector {
            name: "vcvtps2ph",
            imm,
            mxcsr,
            src0: halfway,
            src1: 0,
            result0: wanted,
            result1: 0,
            flags: 0,
            width: 128,
        };
        let (cpu, mut memory) = load(bytes, &vector);
        for native in [false, true] {
            let result = run(native, &cpu, &mut memory);
            assert_eq!(result.exit_reason, ExitReason::StepLimitExceeded, "{text}");
            assert_eq!(result.final_state.xmm[3] & 0xFFFF, wanted, "{text}");
        }
    }
}

/// FTZ and DAZ are ignored by these instructions: a tiny result becomes a
/// binary16 denormal, and a binary16 denormal source becomes a normal binary32.
#[test]
fn flush_and_denormals_are_zero_are_ignored() {
    let code = fixture();
    let mxcsr = 0x1F80 | (1 << 15) | (1 << 6);
    let vector = Vector {
        name: "vcvtps2ph",
        imm: 0,
        mxcsr,
        src0: 0x3380_0000, // 2^-24, exactly representable in binary16
        src1: 0,
        result0: 1,
        result1: 0,
        flags: 0,
        width: 128,
    };
    let text = "vcvtps2ph xmm3, xmm9, 0";
    let (cpu, mut memory) = load(&code[text], &vector);
    for native in [false, true] {
        let result = run(native, &cpu, &mut memory);
        assert_eq!(result.final_state.xmm[3] & 0xFFFF, 1, "FTZ was not ignored");
        assert_eq!(
            result.final_state.mxcsr.0, mxcsr,
            "a tiny exact result raised a flag"
        );
    }
    let vector = Vector { src0: 1, ..vector };
    let text = "vcvtph2ps xmm3, xmm9";
    let (cpu, mut memory) = load(&code[text], &vector);
    for native in [false, true] {
        let result = run(native, &cpu, &mut memory);
        assert_eq!(
            result.final_state.xmm[3], 0x3380_0000,
            "DAZ was not ignored"
        );
        assert_eq!(
            result.final_state.mxcsr.0, mxcsr,
            "a denormal source raised DE"
        );
    }
}

/// A denormal binary32 source reports DE with UE and PE when DM is masked and
/// UM or PM is unmasked, and DE alone when DM is unmasked.
#[test]
fn denormal_source_flags_follow_the_operation_section() {
    let code = fixture();
    let text = "vcvtps2ph xmm3, xmm9, 0";
    for (mask, flags) in [
        (0x1F80 & !(1 << 8), 2),
        (0x1F80 & !(1 << 12), 2 | 16 | 32),
        (0x1F80 & !(1 << 11), 2 | 16 | 32),
    ] {
        let vector = Vector {
            name: "vcvtps2ph",
            imm: 0,
            mxcsr: mask,
            src0: 0x007F_FFFF,
            src1: 0,
            result0: 0x0400,
            result1: 0,
            flags,
            width: 128,
        };
        let (cpu, mut memory) = load(&code[text], &vector);
        let (_, aborted) = outcome(&vector);
        for native in [false, true] {
            let result = run(native, &cpu, &mut memory);
            assert_eq!(
                result.exit_reason,
                if aborted {
                    ExitReason::SimdException
                } else {
                    ExitReason::StepLimitExceeded
                }
            );
            assert_eq!(result.final_state.mxcsr.0, mask | flags);
            if aborted {
                assert_eq!(result.final_state.xmm[3], FILLER);
            }
        }
    }
}

/// A store that cannot be performed faults before the conversion, so even an
/// unmasked invalid source leaves MXCSR alone.
#[test]
fn a_store_fault_precedes_the_conversion() {
    let code = fixture();
    let text = "vcvtps2ph qword ptr [rsi + 4*rdi + 16], xmm9, 0";
    let bytes = &code[text];
    let vector = Vector {
        name: "vcvtps2ph",
        imm: 0,
        mxcsr: 0x1F80 & !(1 << 7),
        src0: 0x7F80_0001, // signaling NaN, invalid unmasked
        src1: 0,
        result0: 0,
        result1: 0,
        flags: 1,
        width: 128,
    };
    for native in [false, true] {
        let (mut cpu, mut memory) = load(bytes, &vector);
        cpu.gpr[6] = 0x0000_8000_0000_0000; // noncanonical destination
        let result = run(native, &cpu, &mut memory);
        assert_eq!(
            result.exit_reason,
            ExitReason::GeneralProtection,
            "{native}"
        );
        assert_eq!(result.final_state.mxcsr, cpu.mxcsr, "{native}");

        let mut read_only = GuestMemory::new();
        read_only
            .map_region_with_data(ENTRY, bytes.len(), MemoryPermissions::READ_EXECUTE, bytes)
            .unwrap();
        read_only
            .map_region_with_data(
                DATA,
                0x100,
                MemoryPermissions::READ_EXECUTE,
                &vec![0; 0x100],
            )
            .unwrap();
        let mut cpu = cpu.clone();
        cpu.gpr[6] = DATA;
        let result = if native {
            DbtEngine::new(64 * 1024)
                .unwrap()
                .run(&mut cpu.clone(), &mut read_only, 1)
        } else {
            Interpreter::new(cpu.clone(), &mut read_only, 1).run()
        };
        assert!(
            matches!(result.exit_reason, ExitReason::MemoryFault(_)),
            "{native}: {:?}",
            result.exit_reason
        );
        assert_eq!(result.final_state.mxcsr, cpu.mxcsr, "{native}");
    }
}

/// VEX.W must be zero and VEX.vvvv must name no register. Both are checked by
/// mutating the third VEX byte of a valid form.
#[test]
fn vex_w_and_vvvv_are_refused() {
    let code = fixture();
    let valid = &code["vcvtph2ps xmm3, xmm9"];
    for byte in [valid[2] | 0x80, valid[2] & !0x08] {
        let mut bytes = valid.clone();
        bytes[2] = byte;
        let mut memory = GuestMemory::new();
        memory
            .map_region_with_data(ENTRY, bytes.len(), MemoryPermissions::READ_EXECUTE, &bytes)
            .unwrap();
        memory
            .map_region_with_data(DATA, 0x100, MemoryPermissions::READ_WRITE, &vec![0; 0x100])
            .unwrap();
        let mut cpu = CpuState::new(ENTRY, 0);
        cpu.gpr[6] = DATA;
        cpu.gpr[7] = 4;
        for native in [false, true] {
            let result = run(native, &cpu, &mut memory);
            assert!(
                matches!(result.exit_reason, ExitReason::IllegalInstruction(_)),
                "{byte:02x} on native={native}"
            );
        }
    }
}
