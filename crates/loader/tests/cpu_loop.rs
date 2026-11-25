// SPDX-License-Identifier: GPL-3.0-or-later

//! LOOP, LOOPE, LOOPNE and JRCXZ (issue #396).
//!
//! Expected results come from a step-by-step model written from the operation
//! sections of the Intel SDM Vol. 2A for LOOP/LOOPcc and Jcc. The model never
//! decodes or calls the production semantics. Encodings were assembled with
//! `clang -target x86_64-apple-macos` and read with `objdump`. Nothing was
//! compared with x86 hardware.
//!
//! In 64-bit mode with the default address size the count is RCX. LOOP
//! decrements it without changing a flag and branches while the result is not
//! zero; LOOPE also needs ZF set and LOOPNE ZF clear. JRCXZ branches when RCX is
//! zero and changes nothing.

use xenora_loader::cpu::{CpuState, ExecutionResult, ExitReason, Interpreter, RFlags};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};

const ENTRY: u64 = 0x1000;

const RAX: usize = 0;
const RCX: usize = 1;
const RBX: usize = 3;
const RDI: usize = 7;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Loop,
    Loope,
    Loopne,
}

impl Kind {
    fn opcode(self) -> u8 {
        match self {
            Kind::Loopne => 0xE0,
            Kind::Loope => 0xE1,
            Kind::Loop => 0xE2,
        }
    }
}

/// ```text
/// top: inc rbx            48 ff c3
///      cmp rbx, rdi       48 39 fb
///      jmp next           eb 00      ; ends a translated block
/// next: loop/loope/loopne top
///      hlt                f4
/// ```
fn loop_program(kind: Kind) -> Vec<u8> {
    vec![
        0x48,
        0xFF,
        0xC3,
        0x48,
        0x39,
        0xFB,
        0xEB,
        0x00,
        kind.opcode(),
        0xF6,
        0xF4,
    ]
}

/// Start of each instruction of [`loop_program`], and the end of HLT.
const OFFSETS: [u64; 6] = [0, 3, 6, 8, 10, 11];

fn image(code: &[u8]) -> GuestMemory {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    memory
}

/// The interpreter and the DBT, each from its own copy of the state. The DBT
/// result carries the state it left in its own copy.
fn both(cpu: &CpuState, code: &[u8], budget: usize) -> [(&'static str, ExecutionResult); 2] {
    let mut interpreted = image(code);
    let result = Interpreter::new(cpu.clone(), &mut interpreted, budget).run();
    let mut translated = image(code);
    let mut state = cpu.clone();
    let mut native = DbtEngine::new(64 * 1024)
        .unwrap()
        .run(&mut state, &mut translated, budget);
    native.final_state = state;
    [("interpreter", result), ("DBT", native)]
}

/// What the model predicts after at most `budget` instructions.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    exit: ExitReason,
    executed: usize,
    rip: u64,
    rcx: u64,
    rbx: u64,
    zf: bool,
}

fn model(kind: Kind, rcx: u64, rbx: u64, rdi: u64, zf: bool, budget: usize) -> Outcome {
    let (mut rcx, mut rbx, mut zf) = (rcx, rbx, zf);
    let mut at = 0;
    let mut executed = 0;
    while executed < budget {
        executed += 1;
        at = match at {
            // INC writes ZF too (Intel SDM Vol. 2A, INC).
            0 => {
                rbx = rbx.wrapping_add(1);
                zf = rbx == 0;
                1
            }
            1 => {
                zf = rbx == rdi;
                2
            }
            2 => 3,
            3 => {
                rcx = rcx.wrapping_sub(1);
                let condition = match kind {
                    Kind::Loop => true,
                    Kind::Loope => zf,
                    Kind::Loopne => !zf,
                };
                if rcx != 0 && condition { 0 } else { 4 }
            }
            _ => {
                return Outcome {
                    exit: ExitReason::Halt,
                    executed,
                    rip: ENTRY + OFFSETS[5],
                    rcx,
                    rbx,
                    zf,
                };
            }
        };
    }
    Outcome {
        exit: ExitReason::StepLimitExceeded,
        executed,
        rip: ENTRY + OFFSETS[at],
        rcx,
        rbx,
        zf,
    }
}

#[test]
fn loops_match_the_model_for_every_count_match_point_and_budget() {
    let mut checked = 0;
    for kind in [Kind::Loop, Kind::Loope, Kind::Loopne] {
        let code = loop_program(kind);
        for rcx in [0, 1, 2, 3, 5, 0x1_0000_0000] {
            // RDI is the value of RBX after the iteration whose CMP sets ZF;
            // 0 never matches within these counts.
            for rdi in [0, 1, 2, 3, 4] {
                for zf in [false, true] {
                    // Budgets that stop before, inside and after the loop.
                    for budget in [1, 3, 4, 5, 8, 9, 11, 13, 22, 200] {
                        let wanted = model(kind, rcx, 0, rdi, zf, budget);
                        let mut cpu = CpuState::new(ENTRY, 0);
                        cpu.gpr[RCX] = rcx;
                        cpu.gpr[RDI] = rdi;
                        cpu.flags.zf = zf;
                        for (engine, result) in both(&cpu, &code, budget) {
                            let state = &result.final_state;
                            let found = Outcome {
                                exit: result.exit_reason,
                                executed: result.instructions_executed,
                                rip: state.rip,
                                rcx: state.gpr[RCX],
                                rbx: state.gpr[RBX],
                                zf: state.flags.zf,
                            };
                            assert_eq!(
                                found, wanted,
                                "{kind:?} rcx={rcx:#x} rdi={rdi} zf={zf} budget={budget} on the {engine}"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(checked, 3 * 6 * 5 * 2 * 10 * 2);
}

/// ```text
///      mov eax, 7     b8 07 00 00 00
///      jrcxz done     e3 05
///      mov eax, 1     b8 01 00 00 00
/// done: hlt           f4
/// ```
#[test]
fn jrcxz_branches_only_on_a_zero_rcx_and_changes_nothing() {
    let code = [
        0xB8, 0x07, 0x00, 0x00, 0x00, 0xE3, 0x05, 0xB8, 0x01, 0x00, 0x00, 0x00, 0xF4,
    ];
    let mut values = vec![0, 1, u64::MAX];
    values.extend((0..64).map(|bit| 1u64 << bit));
    for rcx in values {
        for flags in [RFlags::default(), all_flags()] {
            let mut cpu = CpuState::new(ENTRY, 0);
            cpu.gpr[RCX] = rcx;
            cpu.flags = flags;
            let mut wanted = cpu.clone();
            wanted.gpr[RAX] = if rcx == 0 { 7 } else { 1 };
            wanted.rip = ENTRY + code.len() as u64;
            for (engine, result) in both(&cpu, &code, 8) {
                assert_eq!(
                    result.exit_reason,
                    ExitReason::Halt,
                    "rcx={rcx:#x} {engine}"
                );
                let executed = if rcx == 0 { 3 } else { 4 };
                assert_eq!(
                    result.instructions_executed, executed,
                    "rcx={rcx:#x} {engine}"
                );
                assert_eq!(result.final_state, wanted, "rcx={rcx:#x} {engine}");
            }
        }
    }
}

/// LOOP from RCX = 0 wraps to 2^64 - 1 and branches. With a budget the loop
/// stops early, and both engines agree on the whole state.
#[test]
fn loop_from_zero_wraps_and_keeps_every_flag() {
    let code = loop_program(Kind::Loop);
    for flags in [RFlags::default(), all_flags()] {
        let mut cpu = CpuState::new(ENTRY, 0);
        cpu.gpr[RDI] = 1;
        cpu.flags = flags;
        let [(_, interpreted), (_, translated)] = both(&cpu, &code, 4);
        assert_eq!(interpreted.exit_reason, ExitReason::StepLimitExceeded);
        assert_eq!(interpreted.final_state.gpr[RCX], u64::MAX);
        assert_eq!(interpreted.final_state.rip, ENTRY);
        // INC and CMP set the flags; LOOP leaves them as CMP left them.
        let after_cmp = model(Kind::Loop, 0, 0, 1, flags.zf, 2).zf;
        assert_eq!(interpreted.final_state.flags.zf, after_cmp);
        assert_eq!(interpreted.final_state, translated.final_state);
        assert_eq!(interpreted.exit_reason, translated.exit_reason);
    }
}

fn all_flags() -> RFlags {
    RFlags {
        cf: true,
        zf: true,
        sf: true,
        of: true,
        pf: true,
        af: true,
    }
}
