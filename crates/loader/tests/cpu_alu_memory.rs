// SPDX-License-Identifier: GPL-3.0-or-later

//! Boundary-focused verification for integer ALU memory operands.
//! The broad independent form matrix already lives in x86_integer.rs; these
//! original byte-level tests pin writeback, crossing and denied-write behavior.

use xenora_loader::cpu::{CpuState, ExitReason, Interpreter, RFlags, Register};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryError, MemoryPermissions};

const ENTRY: u64 = 0x1000;
const DATA: u64 = 0x4000;
const REGION_LEN: usize = 0x100;
const POISON: u64 = 0x5A5A_5A5A_5A5A_5A5A;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AluOp {
    Add,
    Or,
    And,
    Sub,
    Xor,
}

impl AluOp {
    fn rm_reg_opcode(self, byte: bool) -> u8 {
        let base = match self {
            Self::Add => 0x00,
            Self::Or => 0x08,
            Self::And => 0x20,
            Self::Sub => 0x28,
            Self::Xor => 0x30,
        };
        base + u8::from(!byte)
    }

    fn reg_rm_opcode(self, byte: bool) -> u8 {
        self.rm_reg_opcode(byte) + 2
    }

    fn group_digit(self) -> u8 {
        match self {
            Self::Add => 0,
            Self::Or => 1,
            Self::And => 4,
            Self::Sub => 5,
            Self::Xor => 6,
        }
    }

    fn logical(self) -> bool {
        matches!(self, Self::Or | Self::And | Self::Xor)
    }
}

fn mask(bits: u32) -> u64 {
    if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

fn sign_bit(bits: u32) -> u64 {
    1u64 << (bits - 1)
}

fn result_flags(flags: &mut RFlags, result: u64, bits: u32) {
    flags.zf = result & mask(bits) == 0;
    flags.sf = result & sign_bit(bits) != 0;
    flags.pf = (result as u8).count_ones().is_multiple_of(2);
}

fn add_flags(left: u64, right: u64, bits: u32) -> (u64, RFlags) {
    let m = mask(bits);
    let (a, b) = (left & m, right & m);
    let result = a.wrapping_add(b) & m;
    let sum = u128::from(a) + u128::from(b);
    let mut flags = RFlags {
        cf: sum >> bits != 0,
        af: ((a ^ b ^ result) & 0x10) != 0,
        of: ((!(a ^ b) & (a ^ result) & sign_bit(bits)) != 0),
        ..Default::default()
    };
    result_flags(&mut flags, result, bits);
    (result, flags)
}

fn sub_flags(left: u64, right: u64, bits: u32) -> (u64, RFlags) {
    let m = mask(bits);
    let (a, b) = (left & m, right & m);
    let result = a.wrapping_sub(b) & m;
    let mut flags = RFlags {
        cf: a < b,
        af: ((a ^ b ^ result) & 0x10) != 0,
        of: (((a ^ b) & (a ^ result) & sign_bit(bits)) != 0),
        ..Default::default()
    };
    result_flags(&mut flags, result, bits);
    (result, flags)
}

fn logic_flags(op: AluOp, left: u64, right: u64, bits: u32) -> (u64, RFlags) {
    let result = match op {
        AluOp::Or => left | right,
        AluOp::Xor => left ^ right,
        _ => left & right,
    } & mask(bits);
    let mut flags = RFlags::default();
    result_flags(&mut flags, result, bits);
    (result, flags)
}

fn alu(op: AluOp, left: u64, right: u64, bits: u32) -> (u64, RFlags) {
    match op {
        AluOp::Add => add_flags(left, right, bits),
        AluOp::Sub => sub_flags(left, right, bits),
        _ => logic_flags(op, left, right, bits),
    }
}

fn prefix(bits: u32) -> Vec<u8> {
    match bits {
        16 => vec![0x66],
        64 => vec![0x48],
        _ => Vec::new(),
    }
}

/// `op [rbx], rdx` at the requested width. ModRM mod=00, reg=RDX, rm=RBX.
fn rm_reg(op: AluOp, bits: u32) -> Vec<u8> {
    let mut code = prefix(bits);
    code.push(op.rm_reg_opcode(bits == 8));
    code.push(0x13);
    code
}

/// `op rdx, [rbx]` at the requested width.
fn reg_rm(op: AluOp, bits: u32) -> Vec<u8> {
    let mut code = prefix(bits);
    code.push(op.reg_rm_opcode(bits == 8));
    code.push(0x13);
    code
}

fn machine(code: &[u8], data: &[u8], permissions: MemoryPermissions) -> GuestMemory {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .expect("code");
    memory
        .map_region_with_data(DATA, REGION_LEN, permissions, data)
        .expect("data");
    memory
}

fn cpu(bits: u32, register_value: u64) -> CpuState {
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr = [POISON; 16];
    cpu.set(Register::Rbx, DATA);
    let value = if bits == 64 {
        register_value
    } else {
        (POISON & !mask(bits)) | (register_value & mask(bits))
    };
    cpu.set(Register::Rdx, value);
    cpu.flags = RFlags {
        cf: true,
        zf: true,
        sf: true,
        of: true,
        pf: true,
        af: true,
    };
    cpu
}

fn read_memory(memory: &GuestMemory, len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    memory.read_bytes(DATA, &mut bytes).expect("read data");
    bytes
}

fn run_both(cpu: &CpuState, memory: &GuestMemory) -> [xenora_loader::cpu::ExecutionResult; 2] {
    let mut im = memory.clone();
    let interpreted = Interpreter::new(cpu.clone(), &mut im, 4).run();
    let mut tm = memory.clone();
    let mut tc = cpu.clone();
    let translated = DbtEngine::new(1024 * 1024)
        .expect("JIT")
        .run(&mut tc, &mut tm, 4);
    assert_eq!(
        read_memory(&im, REGION_LEN),
        read_memory(&tm, REGION_LEN),
        "engines wrote different memory"
    );
    [interpreted, translated]
}

fn data_image(value: u64, bits: u32) -> Vec<u8> {
    let mut data = vec![0xC3; REGION_LEN];
    let width = (bits / 8) as usize;
    data[..width].copy_from_slice(&value.to_le_bytes()[..width]);
    data
}

#[test]
fn memory_destinations_write_the_independently_derived_result_at_every_width() {
    for op in [AluOp::Add, AluOp::Or, AluOp::And, AluOp::Sub, AluOp::Xor] {
        for bits in [8u32, 16, 32, 64] {
            let memory_value = match bits {
                8 => 0x55,
                16 => 0x5555,
                32 => 0x5555_5555,
                _ => 0x5555_5555_5555_5555,
            };
            let register_value = match bits {
                8 => 0x0f,
                16 => 0x0f0f,
                32 => 0x0f0f_0f0f,
                _ => 0x0f0f_0f0f_0f0f_0f0f,
            };
            let mut code = rm_reg(op, bits);
            code.push(0xF4);
            let data = data_image(memory_value, bits);
            let cpu = cpu(bits, register_value);
            let memory = machine(&code, &data, MemoryPermissions::READ_WRITE);
            let (result_value, flags) = alu(op, memory_value, register_value, bits);
            let mut expected_memory = data.clone();
            expected_memory[..(bits / 8) as usize]
                .copy_from_slice(&result_value.to_le_bytes()[..(bits / 8) as usize]);

            for result in run_both(&cpu, &memory) {
                assert_eq!(result.exit_reason, ExitReason::Halt, "{op:?} {bits}");
                let mut actual = result.final_state.clone();
                let mut expected = cpu.clone();
                expected.flags = flags;
                expected.rip = ENTRY + code.len() as u64;
                if op.logical() {
                    actual.flags.af = false;
                    expected.flags.af = false;
                }
                assert_eq!(actual, expected, "{op:?} {bits} final state");
            }
            let mut im = memory.clone();
            Interpreter::new(cpu.clone(), &mut im, 4).run();
            assert_eq!(
                read_memory(&im, REGION_LEN),
                expected_memory,
                "{op:?} {bits}"
            );
        }
    }
}

#[test]
fn memory_sources_update_only_the_destination_register() {
    for op in [AluOp::Add, AluOp::Or, AluOp::And, AluOp::Sub, AluOp::Xor] {
        for bits in [8u32, 16, 32, 64] {
            let memory_value = 3;
            let register_value = mask(bits);
            let mut code = reg_rm(op, bits);
            code.push(0xF4);
            let data = data_image(memory_value, bits);
            let cpu = cpu(bits, register_value);
            let memory = machine(&code, &data, MemoryPermissions::READ_ONLY);
            let (result_value, flags) = alu(op, register_value, memory_value, bits);
            for result in run_both(&cpu, &memory) {
                assert_eq!(result.exit_reason, ExitReason::Halt, "{op:?} {bits}");
                let mut actual = result.final_state.clone();
                let mut expected = cpu.clone();
                expected.flags = flags;
                expected.rip = ENTRY + code.len() as u64;
                let width_mask = mask(bits);
                let old = cpu.get(Register::Rdx);
                let written = match bits {
                    8 | 16 => (old & !width_mask) | (result_value & width_mask),
                    _ => result_value & width_mask,
                };
                expected.set(Register::Rdx, written);
                if op.logical() {
                    actual.flags.af = false;
                    expected.flags.af = false;
                }
                assert_eq!(actual, expected, "{op:?} {bits} final state");
            }
        }
    }
}

#[test]
fn group_one_memory_immediates_write_and_sign_extend() {
    for op in [AluOp::Add, AluOp::Or, AluOp::And, AluOp::Sub, AluOp::Xor] {
        for bits in [8u32, 16, 32, 64] {
            let memory_value = 0x20;
            let immediate = 0x8fu64; // imm8 -113, sign-extended to the width.
            let mut code = prefix(bits);
            code.push(if bits == 8 { 0x80 } else { 0x83 });
            code.push(0x03 | (op.group_digit() << 3));
            code.push(immediate as u8);
            code.push(0xF4);
            let data = data_image(memory_value, bits);
            let cpu = cpu(bits, 0);
            let memory = machine(&code, &data, MemoryPermissions::READ_WRITE);
            let rhs = (immediate as i8 as i64) as u64 & mask(bits);
            let (result_value, flags) = alu(op, memory_value, rhs, bits);
            for result in run_both(&cpu, &memory) {
                assert_eq!(result.exit_reason, ExitReason::Halt, "{op:?} {bits}");
                let mut actual = result.final_state.clone();
                let mut expected = cpu.clone();
                expected.flags = flags;
                expected.rip = ENTRY + code.len() as u64;
                if op.logical() {
                    actual.flags.af = false;
                    expected.flags.af = false;
                }
                assert_eq!(actual, expected, "{op:?} {bits} final state");
            }
            let mut check_memory = memory.clone();
            Interpreter::new(cpu.clone(), &mut check_memory, 4).run();
            let mut expected_memory = data.clone();
            expected_memory[..(bits / 8) as usize]
                .copy_from_slice(&result_value.to_le_bytes()[..(bits / 8) as usize]);
            assert_eq!(
                read_memory(&check_memory, REGION_LEN),
                expected_memory,
                "{op:?} {bits} memory write"
            );
        }
    }
}

fn boundary_machine(first_write: bool, second_write: bool) -> (GuestMemory, u64) {
    let boundary = DATA + REGION_LEN as u64;
    let mut low = vec![0x11; REGION_LEN];
    low[REGION_LEN - 4..].copy_from_slice(&0x20u32.to_le_bytes());
    let high = vec![0x22; REGION_LEN];
    let mut memory = GuestMemory::new();
    let code = [0x01, 0x13, 0xF4]; // add [rbx], edx; hlt
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, &code)
        .expect("code");
    memory
        .map_region_with_data(
            DATA,
            REGION_LEN,
            MemoryPermissions {
                read: true,
                write: first_write,
                execute: false,
            },
            &low,
        )
        .expect("low");
    memory
        .map_region_with_data(
            boundary,
            REGION_LEN,
            MemoryPermissions {
                read: true,
                write: second_write,
                execute: false,
            },
            &high,
        )
        .expect("high");
    (memory, boundary)
}

fn boundary_cpu(target: u64) -> CpuState {
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr = [POISON; 16];
    cpu.set(Register::Rbx, target);
    cpu.set(Register::Rdx, 1);
    cpu.flags = RFlags {
        cf: true,
        zf: true,
        sf: true,
        of: true,
        pf: true,
        af: true,
    };
    cpu
}

fn region_bytes(memory: &GuestMemory, base: u64) -> Vec<u8> {
    let mut bytes = vec![0; REGION_LEN];
    memory.read_bytes(base, &mut bytes).expect("region bytes");
    bytes
}

fn expected_low_after_crossing_write() -> Vec<u8> {
    let mut low = vec![0x11; REGION_LEN];
    low[REGION_LEN - 4..].copy_from_slice(&[0x20, 0, 0x01, 0]);
    low
}

#[test]
fn a_read_modify_write_spanning_two_writable_regions_completes_in_both_engines() {
    for native in [false, true] {
        let (mut memory, boundary) = boundary_machine(true, true);
        let target = boundary - 2;
        let cpu = boundary_cpu(target);
        let initial = 0x2222_0000u64;
        let (result_value, flags) = add_flags(initial, 1, 32);
        assert_eq!(result_value, 0x2222_0001);
        let result = if native {
            let mut engine = DbtEngine::new(1024 * 1024).expect("JIT");
            let mut translated = cpu.clone();
            let result = engine.run(&mut translated, &mut memory, 2);
            assert_eq!(
                engine.metrics.interpreter_fallbacks, 0,
                "32-bit crossing ADD should remain native"
            );
            result
        } else {
            Interpreter::new(cpu.clone(), &mut memory, 2).run()
        };
        assert_eq!(result.exit_reason, ExitReason::Halt, "native={native}");
        assert_eq!(result.instructions_executed, 2, "native={native}");
        let mut expected = cpu.clone();
        expected.flags = flags;
        expected.rip = ENTRY + 3;
        assert_eq!(result.final_state, expected, "native={native}");
        assert_eq!(
            region_bytes(&memory, DATA),
            expected_low_after_crossing_write(),
            "native={native} low bytes"
        );
        assert_eq!(
            region_bytes(&memory, boundary),
            vec![0x22; REGION_LEN],
            "native={native} high bytes"
        );
    }
}

#[test]
fn a_denied_write_crossing_a_region_boundary_has_no_partial_side_effects() {
    for native in [false, true] {
        let (mut memory, boundary) = boundary_machine(true, false);
        let target = boundary - 2;
        let mut cpu = boundary_cpu(target);
        let before = cpu.clone();
        let before_low = region_bytes(&memory, DATA);
        let before_high = region_bytes(&memory, boundary);
        let result = if native {
            DbtEngine::new(1024 * 1024)
                .expect("JIT")
                .run(&mut cpu, &mut memory, 2)
        } else {
            Interpreter::new(cpu.clone(), &mut memory, 2).run()
        };
        assert_eq!(
            result.exit_reason,
            ExitReason::MemoryFault(MemoryError::PermissionViolation {
                address: boundary,
                required: "write",
            }),
            "native={native}"
        );
        assert_eq!(result.instructions_executed, 0, "native={native}");
        assert_eq!(result.final_state, before, "native={native}");
        assert_eq!(region_bytes(&memory, DATA), before_low, "native={native}");
        assert_eq!(
            region_bytes(&memory, boundary),
            before_high,
            "native={native}"
        );
    }
}
