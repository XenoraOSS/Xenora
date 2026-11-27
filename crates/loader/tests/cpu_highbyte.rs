// SPDX-License-Identifier: GPL-3.0-or-later

//! Boundary verification for legacy high-byte registers and REX low-byte registers.

use xenora_loader::cpu::{CpuState, ExitReason, Interpreter, Register};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};

const ENTRY: u64 = 0x1000;
const POISON: u64 = 0x5A5A_5A5A_5A5A_5A5A;

fn machine(code: &[u8]) -> GuestMemory {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    memory
}

fn cpu() -> CpuState {
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr = [POISON; 16];
    cpu
}

fn run_both(cpu: &CpuState, code: &[u8]) -> [xenora_loader::cpu::ExecutionResult; 2] {
    let memory = machine(code);
    let mut im = memory.clone();
    let interpreted = Interpreter::new(cpu.clone(), &mut im, 4).run();
    let mut tm = memory.clone();
    let mut tc = cpu.clone();
    let translated = DbtEngine::new(1024 * 1024)
        .unwrap()
        .run(&mut tc, &mut tm, 4);
    [interpreted, translated]
}

fn expect_halt(cpu: &CpuState, code_len: usize) -> CpuState {
    let mut expected = cpu.clone();
    expected.rip = ENTRY + code_len as u64;
    expected
}

#[test]
fn numeric_byte_registers_select_legacy_high_halves_only_without_rex() {
    // mov ah, bl / mov spl, bl. ModRM mod=3, reg=BL, rm field 4.
    for (prefix, target, high) in [
        (&[][..], Register::Rax, true),
        (&[0x40][..], Register::Rsp, false),
    ] {
        let mut code = prefix.to_vec();
        code.extend([0x88, 0xDC, 0xF4]);
        let mut cpu = cpu();
        cpu.set(Register::Rbx, 0x7f);
        for result in run_both(&cpu, &code) {
            assert_eq!(result.exit_reason, ExitReason::Halt);
            let mut expected = expect_halt(&cpu, code.len());
            if high {
                expected.set(Register::Rax, (POISON & !0xff00) | 0x7f00);
            } else {
                expected.set(Register::Rsp, (POISON & !0xff) | 0x7f);
            }
            assert_eq!(result.final_state, expected);
            assert_eq!(result.final_state.get(target), expected.get(target));
        }
    }
}

#[test]
fn every_legacy_high_byte_register_reads_and_writes_its_own_byte() {
    // mov ah/ch/dh/bh, bl one register-index pair at a time: rm fields 4..7.
    for (rm_field, register, shift) in [
        (4u8, Register::Rax, 8u64),
        (5, Register::Rcx, 8),
        (6, Register::Rdx, 8),
        (7, Register::Rbx, 8),
    ] {
        let modrm = 0xC0 | (3 << 3) | rm_field; // mov high8, bl
        let code = [0x88, modrm, 0xF4];
        let mut cpu = cpu();
        cpu.set(Register::Rcx, 0xA5);
        // BL is the source; for BH this first overwrites the high half only.
        cpu.set(Register::Rbx, (POISON & !0xff) | 0x5a);
        let source_register = Register::Rbx;
        let source = cpu.get(source_register) & 0xff;
        for result in run_both(&cpu, &code) {
            assert_eq!(result.exit_reason, ExitReason::Halt);
            let mut expected = expect_halt(&cpu, code.len());
            let old = expected.get(register);
            expected.set(register, (old & !(0xffu64 << shift)) | (source << shift));
            assert_eq!(result.final_state, expected, "rm field {rm_field}");
        }
    }
}

#[test]
fn rex_byte_registers_select_spl_bpl_sil_dil_and_extended_low_bytes() {
    // mov r8b, <rm byte register>; REX.R selects r8 as the destination register field.
    // ModRM mod=3, reg field 0 plus REX.R, rm fields 4..7.
    for (rm_field, register) in [
        (4u8, Register::Rsp),
        (5, Register::Rbp),
        (6, Register::Rsi),
        (7, Register::Rdi),
    ] {
        let modrm = 0xC0 | rm_field; // mov r8b, low-byte rm
        let code = [0x44, 0x8A, modrm, 0xF4];
        let mut cpu = cpu();
        cpu.set(register, (POISON & !0xff) | 0x33);
        for result in run_both(&cpu, &code) {
            assert_eq!(result.exit_reason, ExitReason::Halt);
            let mut expected = expect_halt(&cpu, code.len());
            expected.set(Register::R8, (POISON & !0xff) | 0x33);
            assert_eq!(result.final_state, expected, "rm field {rm_field}");
        }
    }
}

#[test]
fn the_same_numeric_source_field_reads_ah_without_rex_and_spl_with_rex() {
    // mov bl, r/m-source field 4: AH without REX, SPL with REX.
    for (rex, register, shift) in [(false, Register::Rax, 8u64), (true, Register::Rsp, 0)] {
        let mut code = if rex { vec![0x40] } else { Vec::new() };
        code.extend([0x88, 0xE3, 0xF4]);
        let mut cpu = cpu();
        cpu.set(
            register,
            (POISON & !(0xffu64 << shift)) | (0x91u64 << shift),
        );
        cpu.set(Register::Rbx, POISON);
        for result in run_both(&cpu, &code) {
            assert_eq!(result.exit_reason, ExitReason::Halt);
            let mut expected = expect_halt(&cpu, code.len());
            expected.set(Register::Rbx, (POISON & !0xff) | 0x91);
            assert_eq!(result.final_state, expected, "rex={rex}");
        }
    }
}
