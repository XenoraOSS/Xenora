// SPDX-License-Identifier: GPL-3.0-or-later

//! Focused CMOVcc/SETcc boundary verification (#123).

use xenora_loader::cpu::{CpuState, ExitReason, Interpreter, RFlags, Register};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryError, MemoryPermissions};

const ENTRY: u64 = 0x1000;
const OLD: u64 = 0x1122_3344_5566_7788;
const NEW: u64 = 0x89AB_CDEF_0123_4567;

fn machine(code: &[u8]) -> GuestMemory {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    memory
}

fn cpu() -> CpuState {
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr = [0x5A5A_5A5A_5A5A_5A5A; 16];
    cpu.set(Register::Rax, OLD);
    cpu.set(Register::Rcx, NEW);
    cpu
}

fn run_both(cpu: &CpuState, memory: &GuestMemory) -> [xenora_loader::cpu::ExecutionResult; 2] {
    let mut im = memory.clone();
    let interpreted = Interpreter::new(cpu.clone(), &mut im, 8).run();
    let mut tm = memory.clone();
    let mut tc = cpu.clone();
    let translated = DbtEngine::new(1024 * 1024)
        .unwrap()
        .run(&mut tc, &mut tm, 8);
    [interpreted, translated]
}

fn condition(cc: u8, flags: &RFlags) -> bool {
    match cc {
        0x0 => flags.of,
        0x1 => !flags.of,
        0x2 => flags.cf,
        0x3 => !flags.cf,
        0x4 => flags.zf,
        0x5 => !flags.zf,
        0x6 => flags.cf || flags.zf,
        0x7 => !flags.cf && !flags.zf,
        0x8 => flags.sf,
        0x9 => !flags.sf,
        0xA => flags.pf,
        0xB => !flags.pf,
        0xC => flags.sf != flags.of,
        0xD => flags.sf == flags.of,
        0xE => flags.zf || flags.sf != flags.of,
        0xF => !flags.zf && flags.sf == flags.of,
        _ => unreachable!(),
    }
}

fn set_flags(cpu: &mut CpuState, pattern: u8) {
    cpu.flags = RFlags::default();
    cpu.flags.cf = pattern & 1 != 0;
    cpu.flags.pf = pattern & 2 != 0;
    cpu.flags.zf = pattern & 4 != 0;
    cpu.flags.sf = pattern & 8 != 0;
    cpu.flags.of = pattern & 16 != 0;
}

#[test]
fn every_cmov_condition_matches_an_independent_flag_evaluation() {
    for pattern in 0..32u8 {
        for cc in 0..16u8 {
            let mut cpu = cpu();
            set_flags(&mut cpu, pattern);
            let code = [0x48, 0x0F, 0x40 + cc, 0xC1, 0xF4];
            let memory = machine(&code);
            let mut expected = cpu.clone();
            if condition(cc, &cpu.flags) {
                expected.set(Register::Rax, NEW);
            }
            expected.rip = ENTRY + code.len() as u64;

            for result in run_both(&cpu, &memory) {
                assert_eq!(
                    result.exit_reason,
                    ExitReason::Halt,
                    "cc={cc:#x} pattern={pattern:#x}"
                );
                assert_eq!(
                    result.final_state, expected,
                    "cc={cc:#x} pattern={pattern:#x}"
                );
            }
        }
    }
}

#[test]
fn cmov_width_rules_apply_even_when_the_condition_is_false() {
    for bits in [16u32, 32, 64] {
        for zf in [false, true] {
            let mut cpu = cpu();
            cpu.flags.zf = zf;
            let mut code = match bits {
                16 => vec![0x66],
                64 => vec![0x48],
                _ => Vec::new(),
            };
            code.extend_from_slice(&[0x0F, 0x44, 0xC1, 0xF4]); // cmovz rax/eax/ax, rcx/ecx/cx
            let memory = machine(&code);
            let mut expected = cpu.clone();
            let source = NEW & u64::MAX.wrapping_shr(64 - bits);
            expected.set(
                Register::Rax,
                match (bits, zf) {
                    (16, true) => (OLD & !0xffff) | source,
                    (16, false) => OLD,
                    (32, true) => source,
                    // A false 32-bit CMOV still zero-extends the old destination.
                    (32, false) => OLD & 0xffff_ffff,
                    (64, true) => source,
                    (64, false) => OLD,
                    _ => unreachable!(),
                },
            );
            expected.rip = ENTRY + code.len() as u64;

            for result in run_both(&cpu, &memory) {
                assert_eq!(result.exit_reason, ExitReason::Halt, "{bits} zf={zf}");
                assert_eq!(result.final_state, expected, "{bits} zf={zf}");
            }
        }
    }
}

#[test]
fn setcc_byte_writes_preserve_only_the_aliased_bytes_the_architecture_requires() {
    for zf in [false, true] {
        for (name, code, register, update) in [
            (
                "al",
                vec![0x0F, 0x94, 0xC0, 0xF4],
                Register::Rax,
                (|old: u64, value: u64| (old & !0xff) | value) as fn(u64, u64) -> u64,
            ),
            (
                "ah",
                vec![0x0F, 0x94, 0xC4, 0xF4],
                Register::Rax,
                (|old: u64, value: u64| (old & !0xff00) | (value << 8)) as fn(u64, u64) -> u64,
            ),
            (
                "r8b",
                vec![0x41, 0x0F, 0x94, 0xC0, 0xF4],
                Register::R8,
                (|old: u64, value: u64| (old & !0xff) | value) as fn(u64, u64) -> u64,
            ),
        ] {
            let mut cpu = cpu();
            cpu.flags.zf = zf;
            cpu.set(register, OLD);
            let memory = machine(&code);
            let mut expected = cpu.clone();
            let written = u64::from(zf);
            expected.set(register, update(OLD, written));
            expected.rip = ENTRY + code.len() as u64;

            for result in run_both(&cpu, &memory) {
                assert_eq!(result.exit_reason, ExitReason::Halt, "{name} zf={zf}");
                assert_eq!(result.final_state, expected, "{name} zf={zf}");
            }
        }
    }
}

#[test]
fn false_cmov_still_reads_its_memory_source_before_faulting() {
    let mut cpu = cpu();
    cpu.flags.zf = false;
    cpu.set(Register::Rsi, 0xDEAD_0000);
    let code = [0x48, 0x0F, 0x44, 0x06, 0xF4]; // cmovz rax, [rsi]
    let memory = machine(&code);

    for result in run_both(&cpu, &memory) {
        assert!(
            matches!(
                result.exit_reason,
                ExitReason::MemoryFault(MemoryError::UnmappedAddress(0xDEAD_0000))
            ),
            "{:?}",
            result.exit_reason
        );
        assert_eq!(result.final_state, cpu);
    }
}

#[test]
fn setcc_memory_destination_faults_before_mutating_state() {
    let mut cpu = cpu();
    cpu.flags.zf = true;
    cpu.set(Register::Rdi, 0xDEAD_0000);
    let code = [0x0F, 0x94, 0x07, 0xF4]; // setz [rdi]
    let memory = machine(&code);

    for result in run_both(&cpu, &memory) {
        assert!(
            matches!(
                result.exit_reason,
                ExitReason::MemoryFault(MemoryError::UnmappedAddress(0xDEAD_0000))
            ),
            "{:?}",
            result.exit_reason
        );
        assert_eq!(result.final_state, cpu);
    }
}
