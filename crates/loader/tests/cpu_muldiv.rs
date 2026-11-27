// SPDX-License-Identifier: GPL-3.0-or-later

//! Boundary-focused MUL/IMUL/DIV/IDIV memory-operand verification.

use xenora_loader::cpu::{CpuState, ExitReason, Interpreter, Register};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};

const ENTRY: u64 = 0x1000;
const DATA: u64 = 0x4000;
const REGION_LEN: usize = 0x100;
const POISON: u64 = 0x5A5A_5A5A_5A5A_5A5A;

fn mask(bits: u32) -> u64 {
    if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

fn sign_extend(value: u64, bits: u32) -> i128 {
    let shift = 64 - bits;
    (((value & mask(bits)) << shift) as i64 >> shift) as i128
}

fn prefix(bits: u32) -> Vec<u8> {
    match bits {
        16 => vec![0x66],
        64 => vec![0x48],
        _ => Vec::new(),
    }
}

fn group_memory(bits: u32, digit: u8) -> Vec<u8> {
    let mut code = prefix(bits);
    code.push(if bits == 8 { 0xF6 } else { 0xF7 });
    code.push((digit << 3) | 3); // mod=00, rm=[rbx]
    code.push(0xF4);
    code
}

fn machine(code: &[u8], data: &[u8]) -> GuestMemory {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    memory
        .map_region_with_data(DATA, REGION_LEN, MemoryPermissions::READ_ONLY, data)
        .unwrap();
    memory
}

fn cpu() -> CpuState {
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr = [POISON; 16];
    cpu.set(Register::Rbx, DATA);
    cpu
}

fn data_image(value: u64, bits: u32) -> Vec<u8> {
    let mut data = vec![0xC3; REGION_LEN];
    data[..(bits / 8) as usize].copy_from_slice(&value.to_le_bytes()[..(bits / 8) as usize]);
    data
}

fn run_both(cpu: &CpuState, memory: &GuestMemory) -> [xenora_loader::cpu::ExecutionResult; 2] {
    let mut im = memory.clone();
    let interpreted = Interpreter::new(cpu.clone(), &mut im, 4).run();
    let mut tm = memory.clone();
    let mut tc = cpu.clone();
    let translated = DbtEngine::new(1024 * 1024)
        .unwrap()
        .run(&mut tc, &mut tm, 4);
    [interpreted, translated]
}

fn write_partial(cpu: &mut CpuState, register: Register, bits: u32, value: u64) {
    let width_mask = mask(bits);
    let old = cpu.get(register);
    cpu.set(
        register,
        if bits >= 32 {
            value & width_mask
        } else {
            (old & !width_mask) | (value & width_mask)
        },
    );
}

fn set_ax_dx(cpu: &mut CpuState, bits: u32, low: u64, high: u64) {
    if bits == 8 {
        // Byte forms use AX: the low byte is AL and the high byte is AH.
        write_partial(cpu, Register::Rax, 16, ((high & 0xff) << 8) | (low & 0xff));
    } else {
        write_partial(cpu, Register::Rax, bits, low);
        write_partial(cpu, Register::Rdx, bits, high);
    }
}

fn expect_ax_dx(cpu: &CpuState, bits: u32, low: u64, high: u64) -> CpuState {
    let mut expected = cpu.clone();
    set_ax_dx(&mut expected, bits, low, high);
    expected.rip = ENTRY + 3 + u64::from(bits == 16 || bits == 64);
    expected
}

#[test]
fn memory_multiplication_writes_the_independently_derived_wide_product() {
    for bits in [8u32, 16, 32, 64] {
        for signed in [false, true] {
            let left = match bits {
                8 => 0xfd,
                16 => 0xfffd,
                32 => 0xffff_fffd,
                _ => 0xffff_ffff_ffff_fffd,
            };
            let right = match bits {
                8 => 0xfb,
                16 => 0xfffb,
                32 => 0xffff_fffb,
                _ => 0xffff_ffff_ffff_fffb,
            };
            let mut cpu = cpu();
            write_partial(&mut cpu, Register::Rax, bits, left);
            let product = if signed {
                let value = sign_extend(left, bits) * sign_extend(right, bits);
                value as u128
            } else {
                u128::from(left & mask(bits)) * u128::from(right & mask(bits))
            };
            let low = (product & u128::from(mask(bits))) as u64;
            let high = ((product >> bits) & u128::from(mask(bits))) as u64;
            let code = group_memory(bits, if signed { 5 } else { 4 });
            let memory = machine(&code, &data_image(right, bits));
            let mut expected = expect_ax_dx(&cpu, bits, low, high);
            for mut result in run_both(&cpu, &memory) {
                assert_eq!(
                    result.exit_reason,
                    ExitReason::Halt,
                    "{bits} signed={signed}"
                );
                // Multiplication leaves most flags undefined; only CF/OF are architectural.
                let sign_extension = if signed && (low & (1u64 << (bits - 1))) != 0 {
                    mask(bits)
                } else {
                    0
                };
                let overflow = high != sign_extension;
                assert_eq!(result.final_state.flags.cf, overflow);
                assert_eq!(result.final_state.flags.of, overflow);
                expected.flags.cf = overflow;
                expected.flags.of = overflow;
                result.final_state.flags.zf = false;
                result.final_state.flags.sf = false;
                result.final_state.flags.pf = false;
                result.final_state.flags.af = false;
                expected.flags.zf = false;
                expected.flags.sf = false;
                expected.flags.pf = false;
                expected.flags.af = false;
                assert_eq!(result.final_state, expected, "{bits} signed={signed}");
            }
        }
    }
}

#[test]
fn memory_division_writes_quotient_and_remainder_or_faults_before_mutation() {
    for bits in [8u32, 16, 32, 64] {
        for signed in [false, true] {
            let divisor = 3u64;
            let low_dividend = 0x10u64;
            let mut cpu = cpu();
            set_ax_dx(&mut cpu, bits, low_dividend, 0);
            let width_mask = mask(bits);
            let dividend = if bits == 8 {
                u128::from(cpu.get(Register::Rax) & 0xffff)
            } else {
                (u128::from(cpu.get(Register::Rdx) & width_mask) << bits)
                    + u128::from(low_dividend & width_mask)
            };
            let quotient = if signed {
                sign_extend(low_dividend, bits) / sign_extend(divisor, bits)
            } else {
                (dividend / u128::from(divisor)) as i128
            };
            let remainder = if signed {
                sign_extend(low_dividend, bits) % sign_extend(divisor, bits)
            } else {
                (dividend % u128::from(divisor)) as i128
            };
            let (q, r) = (quotient as u64 & width_mask, remainder as u64 & width_mask);
            let code = group_memory(bits, if signed { 7 } else { 6 });
            let memory = machine(&code, &data_image(divisor, bits));
            let mut expected = expect_ax_dx(&cpu, bits, q, r);
            for mut result in run_both(&cpu, &memory) {
                assert_eq!(
                    result.exit_reason,
                    ExitReason::Halt,
                    "{bits} signed={signed}"
                );
                result.final_state.flags = Default::default();
                expected.flags = Default::default();
                assert_eq!(result.final_state, expected, "{bits} signed={signed}");
            }
        }
    }
}

#[test]
fn a_zero_memory_divisor_faults_without_mutating_any_state() {
    for bits in [8u32, 16, 32, 64] {
        for digit in [6u8, 7] {
            let cpu = cpu();
            let code = group_memory(bits, digit);
            let memory = machine(&code, &data_image(0, bits));
            for result in run_both(&cpu, &memory) {
                assert_eq!(
                    result.exit_reason,
                    ExitReason::DivideError,
                    "{bits} digit={digit}"
                );
                assert_eq!(result.final_state, cpu, "{bits} digit={digit}");
            }
        }
    }
}

#[test]
fn quotient_overflow_faults_without_mutating_any_state() {
    for bits in [8u32, 16, 32, 64] {
        for digit in [6u8, 7] {
            let mut cpu = cpu();
            set_ax_dx(&mut cpu, bits, 0, 1);
            let code = group_memory(bits, digit);
            let memory = machine(&code, &data_image(1, bits));
            for result in run_both(&cpu, &memory) {
                assert_eq!(
                    result.exit_reason,
                    ExitReason::DivideError,
                    "{bits} digit={digit}"
                );
                assert_eq!(result.final_state, cpu, "{bits} digit={digit}");
            }
        }
    }
}
