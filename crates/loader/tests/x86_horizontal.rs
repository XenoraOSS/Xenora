// SPDX-License-Identifier: GPL-3.0-or-later

//! Results of the horizontal and add-subtract float forms, legacy and VEX.
//!
//! Expected values come from `tests/x86/horizontal_vectors.txt`, computed with
//! exact fractions by `tests/x86/float_reference.py` from the Intel SDM rules;
//! instruction bytes come from LLVM (`tests/x86/float.txt` and
//! `tests/x86/avx_forms.txt`). Neither shares code with `softfloat.rs`. The
//! operations combine lanes across positions, which the lane-positional
//! composition of `x86_avx_forms.rs` cannot express, so they are validated here
//! against full 128-bit vectors: the 256-bit forms repeat one vector in both
//! halves, which the lane-local half decomposition makes exact.

use xenora_loader::cpu::{CpuState, ExecutionResult, ExitReason, Interpreter, Mxcsr, RFlags};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};

const ENTRY: u64 = 0x1000;
const DATA: u64 = 0x2000;
/// Address of `[rsi + 4*rdi + 16]` with RSI = DATA and RDI = 4.
const OPERAND: u64 = DATA + 32;

fn fixture(text: &'static str) -> std::collections::BTreeMap<&'static str, Vec<u8>> {
    text.lines()
        .map(|line| {
            let (hex, text) = line.split_once(" | ").unwrap();
            let code = hex
                .split(' ')
                .map(|b| u8::from_str_radix(b, 16).unwrap())
                .collect();
            (text, code)
        })
        .collect()
}

struct Vector {
    name: &'static str,
    mxcsr: u32,
    a: u128,
    b: u128,
    result: u128,
    flags: u32,
}

fn vectors() -> Vec<Vector> {
    include_str!("x86/horizontal_vectors.txt")
        .lines()
        .map(|line| {
            let parts: Vec<&str> = line.split(' ').collect();
            let hex = |index: usize| u128::from_str_radix(parts[index], 16).unwrap();
            Vector {
                name: parts[0],
                mxcsr: hex(1) as u32,
                a: hex(2),
                b: hex(3),
                result: hex(4),
                flags: hex(5) as u32,
            }
        })
        .collect()
}

fn load(
    code: &[u8],
    operands: &[(usize, u128, u128)],
    memory_operand: Option<(u128, u128)>,
    mxcsr: u32,
) -> (CpuState, GuestMemory) {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    memory
        .map_region(DATA, 0x100, MemoryPermissions::READ_WRITE)
        .unwrap();
    if let Some((low, high)) = memory_operand {
        memory.write_bytes(OPERAND, &low.to_le_bytes()).unwrap();
        memory
            .write_bytes(OPERAND + 16, &high.to_le_bytes())
            .unwrap();
    }
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr[6] = DATA;
    cpu.gpr[7] = 4;
    for &(index, low, high) in operands {
        cpu.xmm[index] = low;
        cpu.ymm_high[index] = high;
    }
    cpu.mxcsr = Mxcsr(mxcsr);
    cpu.flags = RFlags::default();
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

/// Run one instruction in both engines and compare the whole machine state.
fn check(
    code: &[u8],
    operands: &[(usize, u128, u128)],
    memory_operand: Option<(u128, u128)>,
    target: usize,
    result: (u128, u128),
    vector: &Vector,
    what: &str,
) {
    for native in [false, true] {
        let (cpu, mut memory) = load(code, operands, memory_operand, vector.mxcsr);
        let result_state = run(native, &cpu, &mut memory);
        assert_eq!(
            result_state.exit_reason,
            ExitReason::StepLimitExceeded,
            "{what}"
        );
        let after = result_state.final_state;
        assert_eq!(after.rip, ENTRY + code.len() as u64, "{what}");
        assert_eq!(after.xmm[target], result.0, "{what} low");
        assert_eq!(after.ymm_high[target], result.1, "{what} high");
        assert_eq!(after.mxcsr.0, vector.mxcsr | vector.flags, "{what} mxcsr");
        // No other vector register moves.
        for index in 0..16 {
            if index == target {
                continue;
            }
            let initial = operands
                .iter()
                .find(|&&(slot, _, _)| slot == index)
                .map(|&(_, low, high)| (low, high))
                .unwrap_or((0, 0));
            assert_eq!(
                (after.xmm[index], after.ymm_high[index]),
                initial,
                "{what} xmm{index}"
            );
        }
    }
}

fn half(value: u128) -> u128 {
    value
}

/// The bits of one lane width, as they repeat in a 256-bit form.
fn pair(value: u128) -> (u128, u128) {
    (value, value)
}

#[test]
fn horizontal_forms_match_the_exact_reference() {
    let legacy = fixture(include_str!("x86/float.txt"));
    let vex = fixture(include_str!("x86/avx_forms.txt"));
    let mut checked = 0;
    for vector in vectors() {
        let bits = if vector.name.ends_with("ps") { 32 } else { 64 };
        let _ = bits;
        let a = half(vector.a);
        let b = half(vector.b);
        let result = half(vector.result);
        let what = format!("{} {:x} {:x} {:x}", vector.name, vector.mxcsr, a, b);
        // Legacy form: the destination is the first source.
        for (text, second) in [
            (format!("{} xmm3, xmm9", vector.name), None),
            (
                format!("{} xmm3, xmmword ptr [rsi + 4*rdi + 16]", vector.name),
                Some((b, 0)),
            ),
        ] {
            let code = legacy[text.as_str()].clone();
            let operands = if second.is_some() {
                vec![(3, a, 0)]
            } else {
                vec![(3, a, 0), (9, b, 0)]
            };
            // The legacy form leaves the high 128 bits of YMM untouched; they
            // are zero here because no VEX instruction ran.
            check(&code, &operands, second, 3, (result, 0), &vector, &what);
            checked += 1;
        }
        // VEX forms: three operands, the register cleared above the result.
        for v in ["xmm", "ymm"] {
            let wide = v == "ymm";
            let (av, bv, rv) = if wide {
                (pair(a), pair(b), pair(result))
            } else {
                ((a, 0), (b, 0), (result, 0))
            };
            let register = format!("v{} {}3, {}9, {}10", vector.name, v, v, v);
            let memory = format!(
                "v{} {}12, {}1, {}word ptr [rsi + 4*rdi + 16]",
                vector.name, v, v, v
            );
            for (text, target, first, mem) in [
                (register, 3usize, 9usize, None),
                (memory, 12usize, 1usize, Some((bv.0, bv.1))),
            ] {
                let code = vex[text.as_str()].clone();
                let mut operands = vec![(first, av.0, av.1)];
                if mem.is_none() {
                    operands.push((10, bv.0, bv.1));
                }
                check(&code, &operands, mem, target, rv, &vector, &what);
                checked += 1;
            }
        }
    }
    assert!(checked > 4000, "{checked}");
}
