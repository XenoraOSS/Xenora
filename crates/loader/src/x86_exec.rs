// SPDX-License-Identifier: GPL-3.0-or-later

//! Reviewed semantic primitives for decoded x86-64 integer instructions.
//!
//! The interpreter executes every instruction here. The native DBT calls the
//! same code, counted explicitly, for forms it does not lower to ARM64.
//!
//! Every instruction is all-or-nothing: operands are read and results computed
//! first, then memory is written, then registers and flags, so a fault leaves
//! the state unchanged. Flags that the architecture leaves undefined keep their
//! previous value, which is permitted but not something guests may rely on.

use crate::cpu::{CpuState, ExitReason, RFlags, Register, X87State, cpuid};
use crate::memory::GuestMemory;
use crate::x86_decode::{Encoding, Insn, MemOperand, RegOperand, RmOperand, Segment};
use crate::x86_simd;
use crate::x86_table::Op;

const RAX: usize = Register::Rax as usize;
const RCX: usize = Register::Rcx as usize;
const RDX: usize = Register::Rdx as usize;
const RBX: usize = Register::Rbx as usize;
const RSP: usize = Register::Rsp as usize;
const RBP: usize = Register::Rbp as usize;
const RSI: usize = Register::Rsi as usize;
const RDI: usize = Register::Rdi as usize;

/// Value mask of an operand of `size` bytes.
fn mask(size: u8) -> u64 {
    u64::MAX >> (64 - 8 * u32::from(size))
}

fn sign_bit(size: u8) -> u64 {
    1 << (8 * u32::from(size) - 1)
}

fn sign_extend(value: u64, size: u8) -> u64 {
    let shift = 64 - 8 * u32::from(size);
    ((value << shift) as i64 >> shift) as u64
}

/// Effective address including any FS or GS base.
pub fn address(cpu: &CpuState, operand: &MemOperand) -> u64 {
    let base = match operand.segment {
        Some(Segment::Fs) => cpu.fs_base,
        Some(Segment::Gs) => cpu.gs_base,
        None => 0,
    };
    base.wrapping_add(offset(cpu, operand))
}

/// Whether a 64-bit guest address is canonical under the x86-64 48-bit
/// virtual-address rule (Intel SDM Vol. 1, section 3.3.7.1).
pub(crate) fn is_canonical(address: u64) -> bool {
    matches!(address >> 47, 0 | 0x1_ffff)
}

/// Whether every byte of a data access has a canonical address. An overflowing
/// range is left to [`GuestMemory`] so it keeps its existing overflow fault.
pub(crate) fn canonical_access(address: u64, size: u8) -> bool {
    if !is_canonical(address) {
        return false;
    }
    if size <= 1 {
        return true;
    }
    address
        .checked_add(u64::from(size) - 1)
        .is_none_or(is_canonical)
}

/// Effective address without a segment base, as LEA computes it.
fn offset(cpu: &CpuState, operand: &MemOperand) -> u64 {
    let base = operand.base.map_or(0, |index| cpu.gpr[usize::from(index)]);
    let index = operand
        .index
        .map_or(0, |(index, scale)| cpu.gpr[usize::from(index)] << scale);
    base.wrapping_add(index).wrapping_add_signed(operand.disp)
}

pub(crate) fn read_reg(cpu: &CpuState, reg: RegOperand, size: u8) -> u64 {
    let value = cpu.gpr[usize::from(reg.index)];
    if reg.high {
        (value >> 8) & 0xFF
    } else {
        value & mask(size)
    }
}

/// Write a register: 8- and 16-bit writes merge, 32-bit writes zero-extend.
pub(crate) fn write_reg(cpu: &mut CpuState, reg: RegOperand, size: u8, value: u64) {
    let slot = &mut cpu.gpr[usize::from(reg.index)];
    *slot = match (size, reg.high) {
        (1, true) => (*slot & !0xFF00) | ((value & 0xFF) << 8),
        (1 | 2, _) => (*slot & !mask(size)) | (value & mask(size)),
        _ => value & mask(size),
    };
}

pub(crate) fn read_mem(memory: &GuestMemory, address: u64, size: u8) -> Result<u64, ExitReason> {
    if !canonical_access(address, size) {
        return Err(ExitReason::GeneralProtection);
    }
    match size {
        1 => memory.read_u8(address).map(u64::from),
        2 => memory.read_u16(address).map(u64::from),
        4 => memory.read_u32(address).map(u64::from),
        _ => memory.read_u64(address),
    }
    .map_err(ExitReason::MemoryFault)
}

pub(crate) fn write_mem(
    memory: &mut GuestMemory,
    address: u64,
    size: u8,
    value: u64,
) -> Result<(), ExitReason> {
    if !canonical_access(address, size) {
        return Err(ExitReason::GeneralProtection);
    }
    match size {
        1 => memory.write_u8(address, value as u8),
        2 => memory.write_u16(address, value as u16),
        4 => memory.write_u32(address, value as u32),
        _ => memory.write_u64(address, value),
    }
    .map_err(ExitReason::MemoryFault)
}

pub(crate) fn rm_operand(insn: &Insn) -> Result<RmOperand, ExitReason> {
    // The decoder supplies a ModRM operand for every form that reaches here.
    insn.rm.ok_or(ExitReason::IllegalInstruction(0))
}

fn read_rm(cpu: &CpuState, memory: &GuestMemory, insn: &Insn) -> Result<u64, ExitReason> {
    match rm_operand(insn)? {
        RmOperand::Reg(reg) => Ok(read_reg(cpu, reg, insn.rm_size)),
        RmOperand::Mem(operand) => read_mem(memory, address(cpu, &operand), insn.rm_size),
    }
}

fn write_rm(
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
    insn: &Insn,
    value: u64,
) -> Result<(), ExitReason> {
    match rm_operand(insn)? {
        RmOperand::Reg(reg) => write_reg(cpu, reg, insn.rm_size, value),
        RmOperand::Mem(operand) => {
            write_mem(memory, address(cpu, &operand), insn.rm_size, value)?;
        }
    }
    Ok(())
}

/// First (destination) and second operand values of a two-operand form.
fn operands(cpu: &CpuState, memory: &GuestMemory, insn: &Insn) -> Result<(u64, u64), ExitReason> {
    let imm = insn.imm & mask(insn.size);
    Ok(match insn.encoding {
        Encoding::RmReg => (
            read_rm(cpu, memory, insn)?,
            read_reg(cpu, insn.reg, insn.size),
        ),
        Encoding::RegRm => (
            read_reg(cpu, insn.reg, insn.size),
            read_rm(cpu, memory, insn)?,
        ),
        Encoding::RmImm => (read_rm(cpu, memory, insn)?, imm),
        Encoding::RmOne => (read_rm(cpu, memory, insn)?, 1),
        Encoding::RmCl => (read_rm(cpu, memory, insn)?, cpu.gpr[RCX] & 0xFF),
        Encoding::RegRmImm => (read_rm(cpu, memory, insn)?, imm),
        Encoding::AccImm => (cpu.gpr[RAX] & mask(insn.size), imm),
        _ => (read_rm(cpu, memory, insn)?, 0),
    })
}

/// Store a result in the destination that [`operands`] read first.
fn write_dst(
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
    insn: &Insn,
    value: u64,
) -> Result<(), ExitReason> {
    match insn.encoding {
        Encoding::RegRm | Encoding::RegRmImm | Encoding::AccImm => {
            write_reg(cpu, insn.reg, insn.size, value);
            Ok(())
        }
        _ => write_rm(cpu, memory, insn, value),
    }
}

/// ZF, SF and PF of a result.
fn result_flags(flags: &mut RFlags, value: u64, size: u8) {
    flags.zf = value & mask(size) == 0;
    flags.sf = value & sign_bit(size) != 0;
    flags.pf = (value as u8).count_ones().is_multiple_of(2);
}

/// `a + b + carry` with every arithmetic flag.
fn add(a: u64, b: u64, carry: bool, size: u8) -> (u64, RFlags) {
    let wide = u128::from(a) + u128::from(b) + u128::from(carry);
    let value = wide as u64 & mask(size);
    let mut flags = RFlags {
        cf: wide > u128::from(mask(size)),
        of: (a ^ value) & (b ^ value) & sign_bit(size) != 0,
        af: (a ^ b ^ value) & 0x10 != 0,
        ..RFlags::default()
    };
    result_flags(&mut flags, value, size);
    (value, flags)
}

/// `a - b - borrow` with every arithmetic flag.
fn sub(a: u64, b: u64, borrow: bool, size: u8) -> (u64, RFlags) {
    let value = a.wrapping_sub(b).wrapping_sub(u64::from(borrow)) & mask(size);
    let mut flags = RFlags {
        cf: u128::from(a) < u128::from(b) + u128::from(borrow),
        of: (a ^ b) & (a ^ value) & sign_bit(size) != 0,
        af: (a ^ b ^ value) & 0x10 != 0,
        ..RFlags::default()
    };
    result_flags(&mut flags, value, size);
    (value, flags)
}

/// Evaluate an x86 condition code (Intel SDM Vol. 1, appendix B).
pub fn condition(flags: &RFlags, code: u8) -> bool {
    let holds = match code >> 1 {
        0 => flags.of,
        1 => flags.cf,
        2 => flags.zf,
        3 => flags.cf || flags.zf,
        4 => flags.sf,
        5 => flags.pf,
        6 => flags.sf != flags.of,
        _ => flags.zf || flags.sf != flags.of,
    };
    holds != (code & 1 != 0)
}

/// Shifts and rotates. `None` means a masked count of zero, which changes
/// neither the destination nor the flags.
fn shift(op: Op, value: u64, count: u64, size: u8, old: RFlags) -> Option<(u64, RFlags)> {
    let count = (count & if size == 8 { 0x3F } else { 0x1F }) as u32;
    if count == 0 {
        return None;
    }
    let bits = 8 * u32::from(size);
    let top = sign_bit(size);
    let mut flags = old;
    let wide = u128::from(value);
    let result = match op {
        Op::Shl => {
            let shifted = wide << count;
            flags.cf = shifted >> bits & 1 != 0;
            shifted as u64
        }
        Op::Shr => {
            flags.cf = wide >> (count - 1) & 1 != 0;
            (wide >> count) as u64
        }
        Op::Sar => {
            let signed = i128::from(sign_extend(value, size) as i64);
            flags.cf = signed >> (count - 1) & 1 != 0;
            (signed >> count) as u64
        }
        Op::Rcl | Op::Rcr => {
            // The carry flag is one more bit above the operand, so the rotation
            // is over `bits + 1` positions and the count is reduced modulo that.
            let width = bits + 1;
            let ring = wide | u128::from(old.cf) << bits;
            let by = count % width;
            let left = if op == Op::Rcl {
                by
            } else {
                (width - by) % width
            };
            let rotated = if left == 0 {
                ring
            } else {
                ring << left | ring >> (width - left)
            };
            flags.cf = rotated >> bits & 1 != 0;
            rotated as u64
        }
        _ => {
            let by = count % bits;
            let left = if op == Op::Rol {
                by
            } else {
                (bits - by) % bits
            };
            let rotated = if left == 0 {
                value
            } else {
                value << left | value >> (bits - left)
            };
            flags.cf = rotated & if op == Op::Rol { 1 } else { top } != 0;
            rotated
        }
    } & mask(size);
    if matches!(op, Op::Shl | Op::Shr | Op::Sar) {
        result_flags(&mut flags, result, size);
        // CF after a left or logical right shift by the width or more is
        // undefined for 8- and 16-bit operands; keep the previous value.
        if count >= bits && op != Op::Sar {
            flags.cf = old.cf;
        }
    }
    // OF is defined only for a count of one.
    if count == 1 {
        flags.of = match op {
            Op::Shl | Op::Rol | Op::Rcl => (result & top != 0) != flags.cf,
            Op::Shr => value & top != 0,
            Op::Sar => false,
            _ => (result & top != 0) != (result & (top >> 1) != 0),
        };
    }
    Some((result, flags))
}

/// SHLD and SHRD (Intel SDM Vol. 2B, "SHLD" and "SHRD"): `dst` is shifted by
/// `count` and the vacated bits are filled from `src`, which is unchanged.
/// `None` means a masked count of zero, which changes neither the destination
/// nor the flags.
///
/// A count above the operand size is possible only for 16-bit operands and
/// leaves the result and flags undefined. Xenora then returns the bits the
/// 32-bit concatenation gives and keeps every flag; no test relies on that.
fn double_shift(
    op: Op,
    dst: u64,
    src: u64,
    count: u64,
    size: u8,
    old: RFlags,
) -> Option<(u64, RFlags)> {
    let count = (count & if size == 8 { 0x3F } else { 0x1F }) as u32;
    if count == 0 {
        return None;
    }
    let bits = 8 * u32::from(size);
    let (dst, src) = (u128::from(dst), u128::from(src));
    // SHLD shifts `dst:src` left and keeps the high half; SHRD shifts
    // `src:dst` right and keeps the low half. CF is the last bit shifted out
    // of the destination.
    let (result, cf) = if op == Op::Shld {
        let pair = dst << bits | src;
        let shifted = pair << count;
        (shifted >> bits, pair >> (2 * bits - count) & 1 != 0)
    } else {
        let pair = src << bits | dst;
        (pair >> count, pair >> (count - 1) & 1 != 0)
    };
    let result = result as u64 & mask(size);
    let mut flags = old;
    if count > bits {
        return Some((result, flags));
    }
    flags.cf = cf;
    result_flags(&mut flags, result, size);
    // OF is defined only for a count of one: set when the sign changed.
    if count == 1 {
        let top = u128::from(sign_bit(size));
        flags.of = (u128::from(result) & top != 0) != (dst & top != 0);
    }
    Some((result, flags))
}

fn push(cpu: &mut CpuState, memory: &mut GuestMemory, value: u64) -> Result<(), ExitReason> {
    let sp = cpu.gpr[RSP]
        .checked_sub(8)
        .ok_or(ExitReason::StackUnderflow)?;
    write_mem(memory, sp, 8, value)?;
    cpu.gpr[RSP] = sp;
    Ok(())
}

/// Execute one decoded instruction, advancing RIP. On error nothing changes.
/// Returns an exit reason when the guest stops voluntarily.
pub fn execute(
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
    insn: &Insn,
) -> Result<Option<ExitReason>, ExitReason> {
    let size = insn.size;
    let mut next = insn.next_rip();
    match insn.op {
        Op::Add | Op::Adc | Op::Sub | Op::Sbb | Op::Cmp => {
            let (a, b) = operands(cpu, memory, insn)?;
            let carry = cpu.flags.cf;
            let (value, flags) = match insn.op {
                Op::Add => add(a, b, false, size),
                Op::Adc => add(a, b, carry, size),
                Op::Sbb => sub(a, b, carry, size),
                _ => sub(a, b, false, size),
            };
            if insn.op != Op::Cmp {
                write_dst(cpu, memory, insn, value)?;
            }
            cpu.flags = flags;
        }
        Op::And | Op::Or | Op::Xor | Op::Test => {
            let (a, b) = operands(cpu, memory, insn)?;
            let value = match insn.op {
                Op::Or => a | b,
                Op::Xor => a ^ b,
                _ => a & b,
            };
            if insn.op != Op::Test {
                write_dst(cpu, memory, insn, value)?;
            }
            cpu.flags.cf = false;
            cpu.flags.of = false;
            result_flags(&mut cpu.flags, value, size);
        }
        Op::Inc | Op::Dec => {
            let a = read_rm(cpu, memory, insn)?;
            let (value, mut flags) = if insn.op == Op::Inc {
                add(a, 1, false, size)
            } else {
                sub(a, 1, false, size)
            };
            write_rm(cpu, memory, insn, value)?;
            flags.cf = cpu.flags.cf;
            cpu.flags = flags;
        }
        Op::Neg => {
            let a = read_rm(cpu, memory, insn)?;
            let (value, flags) = sub(0, a, false, size);
            write_rm(cpu, memory, insn, value)?;
            cpu.flags = flags;
        }
        Op::Not => {
            let a = read_rm(cpu, memory, insn)?;
            write_rm(cpu, memory, insn, !a & mask(size))?;
        }
        Op::Mov => match insn.encoding {
            Encoding::RmReg => write_rm(cpu, memory, insn, read_reg(cpu, insn.reg, size))?,
            Encoding::RegRm => {
                let value = read_rm(cpu, memory, insn)?;
                write_reg(cpu, insn.reg, size, value);
            }
            Encoding::OpRegImm => write_reg(cpu, insn.reg, size, insn.imm),
            _ => write_rm(cpu, memory, insn, insn.imm & mask(size))?,
        },
        // A non-temporal store is an ordinary store: there is no cache to bypass.
        Op::Movnti => write_rm(cpu, memory, insn, read_reg(cpu, insn.reg, size))?,
        Op::Movzx => {
            let value = read_rm(cpu, memory, insn)?;
            write_reg(cpu, insn.reg, size, value);
        }
        Op::Movsx | Op::Movsxd => {
            let value = sign_extend(read_rm(cpu, memory, insn)?, insn.rm_size);
            write_reg(cpu, insn.reg, size, value);
        }
        Op::Lea => {
            if let Some(operand) = insn.memory() {
                write_reg(cpu, insn.reg, size, offset(cpu, &operand));
            }
        }
        Op::Xchg if insn.encoding == Encoding::OpReg => {
            // The accumulator with the register in the opcode, never itself:
            // 90 without REX.B is NOP.
            let rax = accumulator(RAX);
            let (a, b) = (read_reg(cpu, rax, size), read_reg(cpu, insn.reg, size));
            write_reg(cpu, rax, size, b);
            write_reg(cpu, insn.reg, size, a);
        }
        Op::Xchg => {
            let (a, b) = operands(cpu, memory, insn)?;
            write_rm(cpu, memory, insn, b)?;
            write_reg(cpu, insn.reg, size, a);
        }
        Op::Xadd => {
            let (a, b) = operands(cpu, memory, insn)?;
            let (value, flags) = add(a, b, false, size);
            write_rm(cpu, memory, insn, value)?;
            // The source is loaded before the destination is stored, so a
            // register added to itself keeps the sum.
            if insn.rm != Some(RmOperand::Reg(insn.reg)) {
                write_reg(cpu, insn.reg, size, a);
            }
            cpu.flags = flags;
        }
        Op::Cmpxchg => {
            let (destination, source) = operands(cpu, memory, insn)?;
            let expected = cpu.gpr[RAX] & mask(size);
            let (_, flags) = sub(expected, destination, false, size);
            // The destination is written whether or not the values match, so a
            // mismatch on memory that cannot be written still faults.
            let matched = expected == destination;
            write_rm(
                cpu,
                memory,
                insn,
                if matched { source } else { destination },
            )?;
            if !matched {
                write_reg(cpu, accumulator(RAX), size, destination);
            }
            cpu.flags = flags;
        }
        Op::Bswap => {
            let value = read_reg(cpu, insn.reg, size).swap_bytes() >> (64 - 8 * u32::from(size));
            write_reg(cpu, insn.reg, size, value);
        }
        Op::Cwde => {
            // CBW, CWDE and CDQE widen the lower half of the accumulator.
            let value = sign_extend(cpu.gpr[RAX] & mask(size / 2), size / 2);
            write_reg(cpu, accumulator(RAX), size, value);
        }
        Op::Cdq => {
            let negative = cpu.gpr[RAX] & sign_bit(size) != 0;
            write_reg(
                cpu,
                accumulator(RDX),
                size,
                if negative { mask(size) } else { 0 },
            );
        }
        Op::Shld | Op::Shrd => {
            let dst = read_rm(cpu, memory, insn)?;
            let src = read_reg(cpu, insn.reg, size);
            let count = match insn.encoding {
                Encoding::RmRegCl => cpu.gpr[RCX] & 0xFF,
                _ => insn.imm & 0xFF,
            };
            match double_shift(insn.op, dst, src, count, size, cpu.flags) {
                Some((value, flags)) => {
                    write_rm(cpu, memory, insn, value)?;
                    cpu.flags = flags;
                }
                // As for the other shifts, a zero count still performs the
                // write's permission check and the 32-bit zero-extension.
                None => write_rm(cpu, memory, insn, dst)?,
            }
        }
        Op::Rol | Op::Ror | Op::Rcl | Op::Rcr | Op::Shl | Op::Shr | Op::Sar => {
            let (a, count) = operands(cpu, memory, insn)?;
            match shift(insn.op, a, count, size, cpu.flags) {
                Some((value, flags)) => {
                    write_rm(cpu, memory, insn, value)?;
                    cpu.flags = flags;
                }
                // A zero count still performs the write's permission check and
                // the 32-bit register zero-extension.
                None => write_rm(cpu, memory, insn, a)?,
            }
        }
        Op::Mul | Op::Imul => {
            let factor = read_rm(cpu, memory, insn)?;
            let acc = cpu.gpr[RAX] & mask(size);
            let bits = 8 * u32::from(size);
            let product = if insn.op == Op::Mul {
                u128::from(acc) * u128::from(factor)
            } else {
                (i128::from(sign_extend(acc, size) as i64)
                    * i128::from(sign_extend(factor, size) as i64)) as u128
            };
            let low = product as u64 & mask(size);
            let high = (product >> bits) as u64 & mask(size);
            if size == 1 {
                write_reg(cpu, accumulator(RAX), 2, high << 8 | low);
            } else {
                write_reg(cpu, accumulator(RAX), size, low);
                write_reg(cpu, accumulator(RDX), size, high);
            }
            let overflow = if insn.op == Op::Mul {
                high != 0
            } else {
                high != if low & sign_bit(size) != 0 {
                    mask(size)
                } else {
                    0
                }
            };
            cpu.flags.cf = overflow;
            cpu.flags.of = overflow;
        }
        Op::Imul2 => {
            let (a, b) = operands(cpu, memory, insn)?;
            let product =
                i128::from(sign_extend(a, size) as i64) * i128::from(sign_extend(b, size) as i64);
            let value = product as u64 & mask(size);
            write_reg(cpu, insn.reg, size, value);
            let overflow = i128::from(sign_extend(value, size) as i64) != product;
            cpu.flags.cf = overflow;
            cpu.flags.of = overflow;
        }
        Op::Div | Op::Idiv => divide(cpu, memory, insn)?,
        Op::Cmov => {
            // The source is read, and may fault, whatever the condition.
            let source = read_rm(cpu, memory, insn)?;
            let value = if condition(&cpu.flags, insn.condition) {
                source
            } else {
                read_reg(cpu, insn.reg, size)
            };
            write_reg(cpu, insn.reg, size, value);
        }
        Op::Setcc => {
            let value = u64::from(condition(&cpu.flags, insn.condition));
            write_rm(cpu, memory, insn, value)?;
        }
        Op::Bt | Op::Bts | Op::Btr | Op::Btc => {
            let (a, index) = operands(cpu, memory, insn)?;
            let bit = 1u64 << (index & (8 * u64::from(size) - 1));
            let value = match insn.op {
                Op::Bts => a | bit,
                Op::Btr => a & !bit,
                Op::Btc => a ^ bit,
                _ => a,
            };
            if insn.op != Op::Bt {
                write_rm(cpu, memory, insn, value)?;
            }
            cpu.flags.cf = a & bit != 0;
        }
        Op::Bsf | Op::Bsr => {
            let value = read_rm(cpu, memory, insn)?;
            cpu.flags.zf = value == 0;
            // A zero source leaves the destination undefined (SDM) or unchanged
            // (APM); leave it unchanged.
            if value != 0 {
                let index = if insn.op == Op::Bsf {
                    value.trailing_zeros()
                } else {
                    63 - value.leading_zeros()
                };
                write_reg(cpu, insn.reg, size, u64::from(index));
            }
        }
        Op::Popcnt => {
            let value = read_rm(cpu, memory, insn)?;
            write_reg(cpu, insn.reg, size, u64::from(value.count_ones()));
            cpu.flags = RFlags {
                zf: value == 0,
                ..RFlags::default()
            };
        }
        Op::Tzcnt | Op::Lzcnt => {
            let value = read_rm(cpu, memory, insn)?;
            let bits = 8 * u32::from(size);
            let count = if insn.op == Op::Tzcnt {
                value.trailing_zeros().min(bits)
            } else {
                value.leading_zeros() - (64 - bits)
            };
            write_reg(cpu, insn.reg, size, u64::from(count));
            cpu.flags.cf = value == 0;
            cpu.flags.zf = count == 0;
        }
        Op::Push => {
            let value = match insn.encoding {
                Encoding::OpReg => read_reg(cpu, insn.reg, 8),
                Encoding::Imm => insn.imm,
                _ => read_rm(cpu, memory, insn)?,
            };
            push(cpu, memory, value)?;
        }
        Op::Pop => {
            let old = cpu.gpr[RSP];
            let value = read_mem(memory, old, 8)?;
            // A memory destination based on RSP uses the incremented value.
            cpu.gpr[RSP] = old.wrapping_add(8);
            if insn.encoding == Encoding::OpReg {
                write_reg(cpu, insn.reg, 8, value);
            } else if let Err(fault) = write_rm(cpu, memory, insn, value) {
                cpu.gpr[RSP] = old;
                return Err(fault);
            }
        }
        Op::Leave => {
            let frame = cpu.gpr[RBP];
            cpu.gpr[RBP] = read_mem(memory, frame, 8)?;
            cpu.gpr[RSP] = frame.wrapping_add(8);
        }
        Op::Call => {
            let target = branch_target(cpu, memory, insn)?;
            push(cpu, memory, next)?;
            next = target;
        }
        Op::Jmp => next = branch_target(cpu, memory, insn)?,
        Op::Jcc => {
            if condition(&cpu.flags, insn.condition) {
                next = insn.imm;
            }
        }
        // The count is RCX, decremented without changing any flag; LOOPE and
        // LOOPNE also test ZF (Intel SDM Vol. 2A, LOOP/LOOPcc).
        Op::Loop | Op::Loope | Op::Loopne => {
            let count = cpu.gpr[RCX].wrapping_sub(1);
            cpu.gpr[RCX] = count;
            let zf_holds = match insn.op {
                Op::Loope => cpu.flags.zf,
                Op::Loopne => !cpu.flags.zf,
                _ => true,
            };
            if count != 0 && zf_holds {
                next = insn.imm;
            }
        }
        // JRCXZ reads RCX and changes nothing (Intel SDM Vol. 2A, Jcc).
        Op::Jrcxz => {
            if cpu.gpr[RCX] == 0 {
                next = insn.imm;
            }
        }
        Op::Ret => {
            let sp = cpu.gpr[RSP];
            next = read_mem(memory, sp, 8)?;
            cpu.gpr[RSP] = sp.wrapping_add(8).wrapping_add(insn.imm);
        }
        Op::Nop => {}
        // Every load and store of an instruction completes before the next
        // instruction starts, on one host thread, so guest accesses are already
        // totally ordered and a fence has nothing further to enforce.
        Op::Lfence | Op::Mfence | Op::Sfence => {}
        // The control, status and tag words take their initial values. The
        // no-wait form checks no pending exception, and the data registers,
        // which Xenora does not model, are left alone and only tagged empty
        // (Intel SDM Vol. 2A, "FINIT/FNINIT"). FNINIT also clears the x87
        // instruction and data pointers and last opcode, which are not modelled.
        Op::Fninit => cpu.x87 = X87State::INIT,
        Op::Cpuid => {
            // The leaf is EAX and the subleaf ECX. Each result is a 32-bit
            // register write, so the upper halves are cleared.
            let values = cpuid(cpu.gpr[RAX] as u32, cpu.gpr[RCX] as u32);
            for (register, value) in [RAX, RBX, RCX, RDX].into_iter().zip(values) {
                cpu.gpr[register] = u64::from(value);
            }
        }
        Op::Movs | Op::Stos => {
            // One element per execution. A REP form with elements left keeps
            // RIP on itself, so every element is a step of its own: it counts
            // against the step budget, and a fault keeps the elements already
            // done, with RSI, RDI and RCX at the element that faulted.
            if !(insn.rep && cpu.gpr[RCX] == 0) {
                let value = if insn.op == Op::Movs {
                    read_rm(cpu, memory, insn)?
                } else {
                    cpu.gpr[RAX] & mask(size)
                };
                write_mem(memory, cpu.gpr[RDI], size, value)?;
                let step = if cpu.df {
                    u64::from(size).wrapping_neg()
                } else {
                    u64::from(size)
                };
                if insn.op == Op::Movs {
                    cpu.gpr[RSI] = cpu.gpr[RSI].wrapping_add(step);
                }
                cpu.gpr[RDI] = cpu.gpr[RDI].wrapping_add(step);
                if insn.rep {
                    cpu.gpr[RCX] -= 1;
                    if cpu.gpr[RCX] != 0 {
                        next = insn.rip;
                    }
                }
            }
        }
        Op::Cld => cpu.df = false,
        Op::Std => cpu.df = true,
        Op::Clc => cpu.flags.cf = false,
        Op::Stc => cpu.flags.cf = true,
        Op::Cmc => cpu.flags.cf = !cpu.flags.cf,
        // AH = SF:ZF:0:AF:0:PF:1:CF (Intel SDM Vol. 2A, LAHF).
        Op::Lahf => {
            let f = cpu.flags;
            let ah = u64::from(f.sf) << 7
                | u64::from(f.zf) << 6
                | u64::from(f.af) << 4
                | u64::from(f.pf) << 2
                | 1 << 1
                | u64::from(f.cf);
            cpu.gpr[RAX] = cpu.gpr[RAX] & !0xFF00 | ah << 8;
        }
        // SF, ZF, AF, PF and CF from AH bits 7, 6, 4, 2 and 0; OF is
        // unchanged (Intel SDM Vol. 2B, SAHF).
        Op::Sahf => {
            let ah = (cpu.gpr[RAX] >> 8) as u8;
            let flags = &mut cpu.flags;
            flags.sf = ah & 0x80 != 0;
            flags.zf = ah & 0x40 != 0;
            flags.af = ah & 0x10 != 0;
            flags.pf = ah & 0x04 != 0;
            flags.cf = ah & 0x01 != 0;
        }
        Op::Hlt => {
            cpu.rip = next;
            return Ok(Some(ExitReason::Halt));
        }
        Op::Syscall => {
            cpu.rip = next;
            return Ok(Some(ExitReason::Syscall(cpu.gpr[RAX])));
        }
        _ => x86_simd::execute(cpu, memory, insn)?,
    }
    cpu.rip = next;
    Ok(None)
}

fn accumulator(index: usize) -> RegOperand {
    RegOperand {
        index: index as u8,
        high: false,
    }
}

fn branch_target(cpu: &CpuState, memory: &GuestMemory, insn: &Insn) -> Result<u64, ExitReason> {
    if insn.encoding == Encoding::Rel {
        Ok(insn.imm)
    } else {
        read_rm(cpu, memory, insn)
    }
}

/// DIV and IDIV. A zero divisor or a quotient that does not fit raises #DE.
fn divide(cpu: &mut CpuState, memory: &GuestMemory, insn: &Insn) -> Result<(), ExitReason> {
    let size = insn.size;
    let divisor = read_rm(cpu, memory, insn)?;
    if divisor == 0 {
        return Err(ExitReason::DivideError);
    }
    let bits = 8 * u32::from(size);
    let dividend = if size == 1 {
        u128::from(cpu.gpr[RAX] & 0xFFFF)
    } else {
        u128::from(cpu.gpr[RDX] & mask(size)) << bits | u128::from(cpu.gpr[RAX] & mask(size))
    };
    let (quotient, remainder) = if insn.op == Op::Div {
        let divisor = u128::from(divisor);
        let quotient = dividend / divisor;
        if quotient > u128::from(mask(size)) {
            return Err(ExitReason::DivideError);
        }
        (quotient as u64, (dividend % divisor) as u64)
    } else {
        let unused = 128 - 2 * bits;
        let dividend = (dividend << unused) as i128 >> unused;
        let divisor = i128::from(sign_extend(divisor, size) as i64);
        // Only i128::MIN / -1 overflows i128, and it overflows #DE too.
        let quotient = dividend
            .checked_div(divisor)
            .ok_or(ExitReason::DivideError)?;
        let limit = i128::from(sign_bit(size));
        if quotient < -limit || quotient >= limit {
            return Err(ExitReason::DivideError);
        }
        (quotient as u64, (dividend % divisor) as u64)
    };
    if size == 1 {
        let value = (remainder & 0xFF) << 8 | quotient & 0xFF;
        write_reg(cpu, accumulator(RAX), 2, value);
    } else {
        write_reg(cpu, accumulator(RAX), size, quotient);
        write_reg(cpu, accumulator(RDX), size, remainder);
    }
    Ok(())
}
