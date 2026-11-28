// SPDX-License-Identifier: GPL-3.0-or-later

//! FMA3 tests against exact-fraction vectors.
//!
//! Encodings come from LLVM through `tests/x86/fma.txt`, paired with
//! Intel-syntax text. Expected values come from `tests/x86/fma_vectors.txt`,
//! computed with exact `Fraction` arithmetic by `tests/x86/float_reference.py`
//! from the operation sections of Intel SDM Vol. 2. Nothing here shares code
//! with `softfloat.rs`, the registry, the decoder or the executor.
//!
//! A separately rounded multiply followed by an add disagrees with these
//! vectors whenever the exact product does not fit the format, so the suite
//! detects a multiply-add that is not fused.

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
    include_str!("x86/fma.txt")
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

/// One line of `fma_vectors.txt`: the three sources, the result and the flags.
#[derive(Clone, Copy, Debug)]
struct Vector {
    name: &'static str,
    mxcsr: u32,
    a: u128,
    b: u128,
    c: u128,
    result: u128,
    flags: u32,
}

fn vectors() -> Vec<Vector> {
    include_str!("x86/fma_vectors.txt")
        .lines()
        .map(|line| {
            let mut fields = line.split(' ');
            let name = fields.next().unwrap();
            let mut number = || u128::from_str_radix(fields.next().unwrap(), 16).unwrap();
            let mxcsr = number() as u32;
            let a = number();
            let b = number();
            let c = number();
            let result = number();
            let flags = number() as u32;
            Vector {
                name,
                mxcsr,
                a,
                b,
                c,
                result,
                flags,
            }
        })
        .collect()
}

/// The register and memory forms of one vector's instruction: text, whether the
/// ModRM operand is memory, and the vector length in bytes.
fn forms(name: &str) -> Vec<(String, bool, u8)> {
    let scalar = name.ends_with("ss") || name.ends_with("sd");
    if scalar {
        let width = if name.ends_with("ss") {
            "dword"
        } else {
            "qword"
        };
        return vec![
            (format!("v{name} xmm3, xmm9, xmm10"), false, 16),
            (
                format!("v{name} xmm3, xmm9, {width} ptr [rsi + 4*rdi + 16]"),
                true,
                16,
            ),
        ];
    }
    vec![
        (format!("v{name} xmm3, xmm9, xmm10"), false, 16),
        (
            format!("v{name} xmm3, xmm9, xmmword ptr [rsi + 4*rdi + 16]"),
            true,
            16,
        ),
        (format!("v{name} ymm3, ymm9, ymm10"), false, 32),
        (
            format!("v{name} ymm3, ymm9, ymmword ptr [rsi + 4*rdi + 16]"),
            true,
            32,
        ),
    ]
}

fn load(code: &[u8], mxcsr: u32, a: u128, b: u128, c: u128) -> (CpuState, GuestMemory) {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    memory
        .map_region(DATA, 0x100, MemoryPermissions::READ_WRITE)
        .unwrap();
    // A 256-bit form reads 32 bytes of the third source.
    let mut bytes = [0; 32];
    bytes[..16].copy_from_slice(&c.to_le_bytes());
    bytes[16..].copy_from_slice(&c.to_le_bytes());
    memory.write_bytes(OPERAND, &bytes).unwrap();
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr[6] = DATA;
    cpu.gpr[7] = 4;
    cpu.xmm = [FILLER; 16];
    cpu.ymm_high = [FILLER; 16];
    cpu.xmm[3] = a;
    cpu.xmm[9] = b;
    cpu.xmm[10] = c;
    cpu.ymm_high[3] = a;
    cpu.ymm_high[9] = b;
    cpu.ymm_high[10] = c;
    cpu.mxcsr = Mxcsr(mxcsr);
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

fn run(native: bool, cpu: &CpuState, memory: &mut GuestMemory, budget: usize) -> ExecutionResult {
    if native {
        DbtEngine::new(64 * 1024)
            .unwrap()
            .run(&mut cpu.clone(), memory, budget)
    } else {
        Interpreter::new(cpu.clone(), memory, budget).run()
    }
}

/// The state after the instruction: the destination replaced, cleared above the
/// vector length, and the flags it raised accumulated into MXCSR.
fn expect(before: &CpuState, vector: &Vector, length: usize, size: u8) -> CpuState {
    let mut after = before.clone();
    after.rip = ENTRY + length as u64;
    after.mxcsr = Mxcsr(before.mxcsr.0 | vector.flags);
    after.xmm[3] = vector.result;
    after.ymm_high[3] = if size == 32 { vector.result } else { 0 };
    after
}

#[test]
fn every_fma_vector_matches_in_both_engines_from_registers_and_memory() {
    let code = fixture();
    let all = vectors();
    assert!(all.len() > 4000, "{}", all.len());
    for vector in &all {
        for (text, _, size) in forms(vector.name) {
            let bytes = code.get(text.as_str()).unwrap_or_else(|| panic!("{text}"));
            for native in [false, true] {
                let (cpu, mut memory) = load(bytes, vector.mxcsr, vector.a, vector.b, vector.c);
                let result = run(native, &cpu, &mut memory, 1);
                assert_eq!(
                    result.exit_reason,
                    ExitReason::StepLimitExceeded,
                    "{text} {vector:x?}"
                );
                assert_eq!(
                    result.final_state,
                    expect(&cpu, vector, bytes.len(), size),
                    "{text} {vector:x?}"
                );
            }
        }
    }
}

/// A fused product keeps the part of it that a separate multiply would round
/// away. Each case has an exact result that is nonzero, and a multiply that
/// rounds to one first turns it into a zero.
#[test]
fn the_product_is_not_rounded_before_the_addition() {
    let code = fixture();
    // (1 + 2^-23)(1 - 2^-23) = 1 - 2^-46, and (1 + 2^-52)(1 - 2^-52) = 1 - 2^-104.
    // The 231 ordering multiplies the two register sources and adds the
    // destination, so the destination holds the cancelling addend.
    let above_s = 0x3F80_0001_u128;
    let below_s = 0x3F7F_FFFE_u128;
    let above_d = 0x3FF0_0000_0000_0001_u128;
    let below_d = 0x3FEF_FFFF_FFFF_FFFE_u128;
    for (name, a, b, c, wanted) in [
        (
            "fmadd231ps",
            0xBF80_0000,
            above_s,
            below_s,
            0xA880_0000_u128,
        ),
        ("fmsub231ps", 0x3F80_0000, above_s, below_s, 0xA880_0000),
        ("fnmadd231ps", 0x3F80_0000, above_s, below_s, 0x2880_0000),
        (
            "fmadd231pd",
            0xBFF0_0000_0000_0000,
            above_d,
            below_d,
            0xB970_0000_0000_0000,
        ),
    ] {
        let text = format!("v{name} xmm3, xmm9, xmm10");
        let bytes = &code[text.as_str()];
        let (cpu, mut memory) = load(bytes, 0x1F80, a, b, c);
        for native in [false, true] {
            let result = run(native, &cpu, &mut memory, 1);
            assert_eq!(
                result.final_state.xmm[3], wanted,
                "{text} lost the fused product"
            );
        }
    }
}

/// An unmasked exception delivers no result: the destination and the other
/// registers are unchanged, the flag is recorded, and the instruction exits
/// with a SIMD exception.
#[test]
fn an_unmasked_invalid_leaves_no_result() {
    let code = fixture();
    let text = "vfmadd132ps xmm3, xmm9, xmm10";
    let bytes = &code[text];
    // A signaling NaN in the first source, with the invalid mask cleared.
    let mxcsr = 0x1F80 & !0x80;
    let (cpu, mut memory) = load(bytes, mxcsr, 0x7F80_0001, 0x3F80_0000, 0x3F80_0000);
    for native in [false, true] {
        let result = run(native, &cpu, &mut memory, 1);
        assert_eq!(result.exit_reason, ExitReason::SimdException, "{text}");
        assert_eq!(result.instructions_executed, 0, "{text}");
        assert_eq!(result.final_state.mxcsr, Mxcsr(mxcsr | 1), "{text}");
        assert_eq!(result.final_state.xmm, cpu.xmm, "{text}");
    }
}
