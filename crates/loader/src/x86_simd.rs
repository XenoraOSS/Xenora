// SPDX-License-Identifier: GPL-3.0-or-later

//! Semantic primitives for the SSE to SSE4.1 moves and packed integer forms.
//!
//! Lane 0 is the least significant. Every instruction is all-or-nothing and only
//! PTEST changes RFLAGS. Floating-point arithmetic is in [`crate::x86_float`].

use crate::cpu::{CpuState, ExitReason, Mxcsr, RFlags};
use crate::memory::GuestMemory;
use crate::x86_decode::{Encoding, Insn, RegOperand, RmOperand};
use crate::x86_exec::{
    address, canonical_access, read_mem, read_reg, rm_operand, write_mem, write_reg,
};
use crate::x86_float;
use crate::x86_table::{FORMS, Op};

fn lane_mask(bits: u32) -> u128 {
    (1 << bits) - 1
}

pub(crate) fn lane(value: u128, bits: u32, index: u32) -> u64 {
    ((value >> (bits * index)) & lane_mask(bits)) as u64
}

/// Signed value of a lane of `bits`.
fn signed(value: u64, bits: u32) -> i64 {
    (value << (64 - bits)) as i64 >> (64 - bits)
}

/// Build a vector from a function of the lane index.
fn build(bits: u32, f: impl Fn(u32) -> u64) -> u128 {
    (0..128 / bits).fold(0, |out, index| {
        out | (u128::from(f(index)) & lane_mask(bits)) << (bits * index)
    })
}

fn map(a: u128, b: u128, bits: u32, f: impl Fn(u64, u64) -> u64) -> u128 {
    build(bits, |index| f(lane(a, bits, index), lane(b, bits, index)))
}

fn map_signed(a: u128, b: u128, bits: u32, f: impl Fn(i64, i64) -> i64) -> u128 {
    map(a, b, bits, |x, y| {
        f(signed(x, bits), signed(y, bits)) as u64
    })
}

fn all_ones(condition: bool) -> u64 {
    if condition { u64::MAX } else { 0 }
}

/// Clamp to the signed range of `bits`.
fn saturate(value: i64, bits: u32) -> i64 {
    let limit = 1i64 << (bits - 1);
    value.clamp(-limit, limit - 1)
}

/// Clamp to the unsigned range of `bits`.
fn saturate_unsigned(value: i64, bits: u32) -> i64 {
    value.clamp(0, (1 << bits) - 1)
}

/// Narrow the lanes of `a` then `b`, each `bits` wide, to half width.
fn pack(a: u128, b: u128, bits: u32, narrow: impl Fn(i64, u32) -> i64) -> u128 {
    let count = 128 / bits;
    build(bits / 2, |index| {
        let source = if index < count { a } else { b };
        narrow(signed(lane(source, bits, index % count), bits), bits / 2) as u64
    })
}

/// Interleave the low or high halves of `a` and `b`.
fn unpack(a: u128, b: u128, bits: u32, high: bool) -> u128 {
    let start = if high { 64 / bits } else { 0 };
    build(bits, |index| {
        let source = if index % 2 == 0 { a } else { b };
        lane(source, bits, start + index / 2)
    })
}

/// Select lanes of `bits` by two-bit fields of `imm`, reading `a` for the low
/// half of the result and `b` for the high half.
fn shuffle_32(a: u128, b: u128, imm: u64) -> u128 {
    build(32, |index| {
        let source = if index < 2 { a } else { b };
        lane(source, 32, (imm >> (2 * index) & 3) as u32)
    })
}

/// Shuffle the four words starting at word `start`, keeping the other four.
fn shuffle_16(value: u128, imm: u64, start: u32) -> u128 {
    build(16, |index| {
        if (start..start + 4).contains(&index) {
            lane(value, 16, start + (imm >> (2 * (index - start)) & 3) as u32)
        } else {
            lane(value, 16, index)
        }
    })
}

/// Logical or arithmetic shift of every lane; counts past the width give zero
/// or, for arithmetic shifts, copies of the sign.
fn shift(op: Op, value: u128, count: u64) -> u128 {
    let (bits, kind) = match op {
        Op::Psllw => (16, 0),
        Op::Pslld => (32, 0),
        Op::Psllq => (64, 0),
        Op::Psrlw => (16, 1),
        Op::Psrld => (32, 1),
        Op::Psrlq => (64, 1),
        Op::Psraw => (16, 2),
        _ => (32, 2),
    };
    let over = count >= u64::from(bits);
    map(value, 0, bits, |x, _| match kind {
        0 if !over => x << count,
        1 if !over => x >> count,
        2 => (signed(x, bits) >> count.min(u64::from(bits) - 1)) as u64,
        _ => 0,
    })
}

fn shift_bytes(value: u128, count: u64, left: bool) -> u128 {
    if count > 15 {
        0
    } else if left {
        value << (8 * count)
    } else {
        value >> (8 * count)
    }
}

/// Combine neighbouring lanes: those of `a` give the low half of the result,
/// those of `b` the high half. `f` gets the lower lane first.
fn horizontal(a: u128, b: u128, bits: u32, f: impl Fn(i64, i64) -> i64) -> u128 {
    let half = 64 / bits;
    build(bits, |index| {
        let source = if index < half { a } else { b };
        let at = 2 * (index % half);
        let value = |at| signed(lane(source, bits, at), bits);
        f(value(at), value(at + 1)) as u64
    })
}

/// `low` with `high` above it, shifted right by `count` bytes. Zeros follow.
fn align_right(high: u128, low: u128, count: u64) -> u128 {
    match count {
        0 => low,
        1..=15 => low >> (8 * count) | high << (128 - 8 * count),
        _ => shift_bytes(high, count - 16, false),
    }
}

/// Widen the low lanes of `value` from `from` to `to` bits.
fn widen(value: u128, from: u32, to: u32, sign: bool) -> u128 {
    build(to, |index| match lane(value, from, index) {
        low if sign => signed(low, from) as u64,
        low => low,
    })
}

/// Eight sums of four byte differences. A block of `b` chosen by the two low bits
/// of `imm` is compared with `a` from byte 0 or, by bit 2, byte 4 onwards.
fn sums_of_differences(a: u128, b: u128, imm: u64) -> u128 {
    let (from_a, from_b) = ((imm as u32 >> 2 & 1) * 4, (imm as u32 & 3) * 4);
    build(16, |index| {
        (0..4)
            .map(|at| lane(a, 8, from_a + index + at).abs_diff(lane(b, 8, from_b + at)))
            .sum()
    })
}

/// Lanes of `source` where the immediate has a bit set, else of `old`.
fn blend(old: u128, source: u128, bits: u32, imm: u64) -> u128 {
    build(bits, |index| {
        let chosen = imm >> index & 1 != 0;
        lane(if chosen { source } else { old }, bits, index)
    })
}

/// INSERTPS: `value` in the lane of bits 5:4, then zeros by bits 3:0.
fn insert_single(old: u128, value: u64, imm: u64) -> u128 {
    build(32, |index| {
        if imm >> index & 1 != 0 {
            0
        } else if u64::from(index) == imm >> 4 & 3 {
            value
        } else {
            lane(old, 32, index)
        }
    })
}

/// The smallest word and, above it, the index of its first occurrence.
fn smallest_word(value: u128) -> u128 {
    let least = (0..8).map(|index| (lane(value, 16, index), index)).min();
    least.map_or(0, |(word, index)| {
        u128::from(word) | u128::from(index) << 16
    })
}

/// Result of a two-source packed operation.
fn packed(op: Op, a: u128, b: u128) -> u128 {
    match op {
        Op::Pand => a & b,
        Op::Pandn => !a & b,
        Op::Por => a | b,
        Op::Pxor => a ^ b,
        Op::Paddb => map(a, b, 8, u64::wrapping_add),
        Op::Paddw => map(a, b, 16, u64::wrapping_add),
        Op::Paddd => map(a, b, 32, u64::wrapping_add),
        Op::Paddq => map(a, b, 64, u64::wrapping_add),
        Op::Psubb => map(a, b, 8, u64::wrapping_sub),
        Op::Psubw => map(a, b, 16, u64::wrapping_sub),
        Op::Psubd => map(a, b, 32, u64::wrapping_sub),
        Op::Psubq => map(a, b, 64, u64::wrapping_sub),
        Op::Paddsb => map_signed(a, b, 8, |x, y| saturate(x + y, 8)),
        Op::Paddsw => map_signed(a, b, 16, |x, y| saturate(x + y, 16)),
        Op::Psubsb => map_signed(a, b, 8, |x, y| saturate(x - y, 8)),
        Op::Psubsw => map_signed(a, b, 16, |x, y| saturate(x - y, 16)),
        Op::Paddusb => map(a, b, 8, |x, y| (x + y).min(0xFF)),
        Op::Paddusw => map(a, b, 16, |x, y| (x + y).min(0xFFFF)),
        Op::Psubusb => map(a, b, 8, u64::saturating_sub),
        Op::Psubusw => map(a, b, 16, u64::saturating_sub),
        Op::Pcmpeqb => map(a, b, 8, |x, y| all_ones(x == y)),
        Op::Pcmpeqw => map(a, b, 16, |x, y| all_ones(x == y)),
        Op::Pcmpeqd => map(a, b, 32, |x, y| all_ones(x == y)),
        Op::Pcmpgtb => map_signed(a, b, 8, |x, y| all_ones(x > y) as i64),
        Op::Pcmpgtw => map_signed(a, b, 16, |x, y| all_ones(x > y) as i64),
        Op::Pcmpgtd => map_signed(a, b, 32, |x, y| all_ones(x > y) as i64),
        Op::Pminub => map(a, b, 8, u64::min),
        Op::Pmaxub => map(a, b, 8, u64::max),
        Op::Pminsw => map_signed(a, b, 16, i64::min),
        Op::Pmaxsw => map_signed(a, b, 16, i64::max),
        Op::Pavgb => map(a, b, 8, |x, y| (x + y + 1) >> 1),
        Op::Pavgw => map(a, b, 16, |x, y| (x + y + 1) >> 1),
        Op::Pmullw => map(a, b, 16, u64::wrapping_mul),
        Op::Pmulhw => map_signed(a, b, 16, |x, y| (x * y) >> 16),
        Op::Pmulhuw => map(a, b, 16, |x, y| (x * y) >> 16),
        Op::Pmuludq => map(a, b, 64, |x, y| (x & 0xFFFF_FFFF) * (y & 0xFFFF_FFFF)),
        Op::Pmaddwd => build(32, |index| {
            let product = |at| signed(lane(a, 16, at), 16) * signed(lane(b, 16, at), 16);
            (product(2 * index) + product(2 * index + 1)) as u64
        }),
        Op::Pshufb => build(8, |index| match lane(b, 8, index) {
            control if control & 0x80 != 0 => 0,
            control => lane(a, 8, (control & 15) as u32),
        }),
        Op::Phaddw => horizontal(a, b, 16, |x, y| x + y),
        Op::Phaddd => horizontal(a, b, 32, |x, y| x + y),
        Op::Phaddsw => horizontal(a, b, 16, |x, y| saturate(x + y, 16)),
        Op::Phsubw => horizontal(a, b, 16, |x, y| x - y),
        Op::Phsubd => horizontal(a, b, 32, |x, y| x - y),
        Op::Phsubsw => horizontal(a, b, 16, |x, y| saturate(x - y, 16)),
        // Unsigned destination bytes by signed source bytes.
        Op::Pmaddubsw => build(16, |index| {
            let product = |at| lane(a, 8, at) as i64 * signed(lane(b, 8, at), 8);
            saturate(product(2 * index) + product(2 * index + 1), 16) as u64
        }),
        Op::Psignb => map_signed(a, b, 8, |x, y| x * y.signum()),
        Op::Psignw => map_signed(a, b, 16, |x, y| x * y.signum()),
        Op::Psignd => map_signed(a, b, 32, |x, y| x * y.signum()),
        Op::Pmulhrsw => map_signed(a, b, 16, |x, y| (((x * y) >> 14) + 1) >> 1),
        // The most negative value has no positive counterpart and stays.
        Op::Pabsb => map_signed(0, b, 8, |_, y| y.abs()),
        Op::Pabsw => map_signed(0, b, 16, |_, y| y.abs()),
        Op::Pabsd => map_signed(0, b, 32, |_, y| y.abs()),
        Op::Pmuldq => build(64, |index| {
            let low = |value| signed(lane(value, 32, 2 * index), 32);
            (low(a) * low(b)) as u64
        }),
        Op::Pmulld => map(a, b, 32, u64::wrapping_mul),
        Op::Pcmpeqq => map(a, b, 64, |x, y| all_ones(x == y)),
        Op::Packusdw => pack(a, b, 32, saturate_unsigned),
        Op::Pminsb => map_signed(a, b, 8, i64::min),
        Op::Pmaxsb => map_signed(a, b, 8, i64::max),
        Op::Pminsd => map_signed(a, b, 32, i64::min),
        Op::Pmaxsd => map_signed(a, b, 32, i64::max),
        Op::Pminuw => map(a, b, 16, u64::min),
        Op::Pmaxuw => map(a, b, 16, u64::max),
        Op::Pminud => map(a, b, 32, u64::min),
        Op::Pmaxud => map(a, b, 32, u64::max),
        Op::Phminposuw => smallest_word(b),
        Op::Pmovsxbw => widen(b, 8, 16, true),
        Op::Pmovsxbd => widen(b, 8, 32, true),
        Op::Pmovsxbq => widen(b, 8, 64, true),
        Op::Pmovsxwd => widen(b, 16, 32, true),
        Op::Pmovsxwq => widen(b, 16, 64, true),
        Op::Pmovsxdq => widen(b, 32, 64, true),
        Op::Pmovzxbw => widen(b, 8, 16, false),
        Op::Pmovzxbd => widen(b, 8, 32, false),
        Op::Pmovzxbq => widen(b, 8, 64, false),
        Op::Pmovzxwd => widen(b, 16, 32, false),
        Op::Pmovzxwq => widen(b, 16, 64, false),
        Op::Pmovzxdq => widen(b, 32, 64, false),
        Op::Psadbw => build(64, |half| {
            (0..8)
                .map(|at| lane(a, 8, 8 * half + at).abs_diff(lane(b, 8, 8 * half + at)))
                .sum()
        }),
        Op::Packsswb => pack(a, b, 16, saturate),
        Op::Packssdw => pack(a, b, 32, saturate),
        Op::Packuswb => pack(a, b, 16, saturate_unsigned),
        Op::Punpcklbw => unpack(a, b, 8, false),
        Op::Punpcklwd => unpack(a, b, 16, false),
        Op::Punpckldq => unpack(a, b, 32, false),
        Op::Punpcklqdq => unpack(a, b, 64, false),
        Op::Punpckhbw => unpack(a, b, 8, true),
        Op::Punpckhwd => unpack(a, b, 16, true),
        Op::Punpckhdq => unpack(a, b, 32, true),
        Op::Punpckhqdq => unpack(a, b, 64, true),
        // Vector counts are the low 64 bits of the source.
        _ => shift(op, a, b as u64),
    }
}

fn low_mask(size: u8) -> u128 {
    if size == 16 {
        u128::MAX
    } else {
        (1 << (8 * u32::from(size))) - 1
    }
}

/// Address of the memory operand, checked for 16-byte alignment where required.
fn aligned_address(cpu: &CpuState, insn: &Insn) -> Result<Option<u64>, ExitReason> {
    let Some(operand) = insn.memory() else {
        return Ok(None);
    };
    let address = address(cpu, &operand);
    let unaligned = FORMS[usize::from(insn.form)].unaligned;
    if insn.rm_size == 16 && !unaligned && !address.is_multiple_of(16) {
        return Err(ExitReason::GeneralProtection);
    }
    Ok(Some(address))
}

fn read_vector(memory: &GuestMemory, address: u64, size: u8) -> Result<u128, ExitReason> {
    if size < 16 {
        return read_mem(memory, address, size).map(u128::from);
    }
    if !canonical_access(address, size) {
        return Err(ExitReason::GeneralProtection);
    }
    let mut bytes = [0; 16];
    memory
        .read_bytes(address, &mut bytes)
        .map_err(ExitReason::MemoryFault)?;
    Ok(u128::from_le_bytes(bytes))
}

fn write_vector(
    memory: &mut GuestMemory,
    address: u64,
    size: u8,
    value: u128,
) -> Result<(), ExitReason> {
    if size < 16 {
        return write_mem(memory, address, size, value as u64);
    }
    if !canonical_access(address, size) {
        return Err(ExitReason::GeneralProtection);
    }
    memory
        .write_bytes(address, &value.to_le_bytes())
        .map_err(ExitReason::MemoryFault)
}

fn xmm(reg: RegOperand) -> usize {
    usize::from(reg.index)
}

/// Full XMM register, or zero-extended memory, named by ModRM `rm`.
pub(crate) fn read_rm(
    cpu: &CpuState,
    memory: &GuestMemory,
    insn: &Insn,
) -> Result<u128, ExitReason> {
    match (aligned_address(cpu, insn)?, rm_operand(insn)?) {
        (Some(address), _) => read_vector(memory, address, insn.rm_size),
        (None, RmOperand::Reg(reg)) => Ok(cpu.xmm[xmm(reg)]),
        (None, RmOperand::Mem(_)) => Err(ExitReason::IllegalInstruction(0)),
    }
}

/// Execute a vector instruction. RIP is advanced by the caller.
pub(crate) fn execute(
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
    insn: &Insn,
) -> Result<(), ExitReason> {
    if x86_float::execute(cpu, memory, insn)? {
        return Ok(());
    }
    let reg = xmm(insn.reg);
    let in_memory = insn.memory().is_some();
    match (insn.op, insn.encoding) {
        (Op::Ldmxcsr, _) => {
            let value = read_rm(cpu, memory, insn)? as u32;
            if value & !Mxcsr::WRITABLE != 0 {
                return Err(ExitReason::GeneralProtection);
            }
            cpu.mxcsr = Mxcsr(value);
        }
        (Op::Stmxcsr, _) => {
            if let Some(address) = aligned_address(cpu, insn)? {
                write_mem(memory, address, 4, u64::from(cpu.mxcsr.0))?;
            }
        }
        (Op::Movd, Encoding::XmmGpr) => {
            let value = match rm_operand(insn)? {
                RmOperand::Reg(source) => read_reg(cpu, source, insn.size),
                RmOperand::Mem(operand) => read_mem(memory, address(cpu, &operand), insn.size)?,
            };
            cpu.xmm[reg] = u128::from(value);
        }
        (Op::Movd, _) => {
            let value = cpu.xmm[reg] as u64;
            match rm_operand(insn)? {
                RmOperand::Reg(target) => write_reg(cpu, target, insn.size, value),
                RmOperand::Mem(operand) => {
                    write_mem(memory, address(cpu, &operand), insn.size, value)?;
                }
            }
        }
        // The immediate selects a lane of the operand's width, modulo their number.
        (_, Encoding::XmmGprImm) => {
            let width = insn.rm_size;
            let value = match rm_operand(insn)? {
                RmOperand::Reg(source) => read_reg(cpu, source, width.max(4)),
                RmOperand::Mem(operand) => read_mem(memory, address(cpu, &operand), width)?,
            };
            let bits = 8 * u64::from(width);
            let shift = bits * (insn.imm & (128 / bits - 1));
            let mask = low_mask(width);
            let value = (u128::from(value) & mask) << shift;
            cpu.xmm[reg] = cpu.xmm[reg] & !(mask << shift) | value;
        }
        (_, Encoding::GprXmmImm) => {
            let width = insn.rm_size;
            let bits = 8 * u32::from(width);
            let value = lane(cpu.xmm[reg], bits, insn.imm as u32 & (128 / bits - 1));
            match rm_operand(insn)? {
                // The rest of the register is cleared.
                RmOperand::Reg(target) => write_reg(cpu, target, width.max(4), value),
                RmOperand::Mem(operand) => {
                    write_mem(memory, address(cpu, &operand), width, value)?;
                }
            }
        }
        (Op::Ptest, _) => {
            let source = read_rm(cpu, memory, insn)?;
            let value = cpu.xmm[reg];
            cpu.flags = RFlags {
                zf: value & source == 0,
                cf: !value & source == 0,
                ..RFlags::default()
            };
        }
        // The sign bits of XMM0's lanes select the source.
        (Op::Pblendvb | Op::Blendvps | Op::Blendvpd, _) => {
            let bits = match insn.op {
                Op::Pblendvb => 8,
                Op::Blendvps => 32,
                _ => 64,
            };
            let source = read_rm(cpu, memory, insn)?;
            let (old, control) = (cpu.xmm[reg], cpu.xmm[0]);
            cpu.xmm[reg] = build(bits, |index| {
                let chosen = lane(control, bits, index) >> (bits - 1) != 0;
                lane(if chosen { source } else { old }, bits, index)
            });
        }
        (Op::Pextrw, _) => {
            let value = lane(read_rm(cpu, memory, insn)?, 16, (insn.imm & 7) as u32);
            write_reg(cpu, insn.reg, insn.size, value);
        }
        (Op::Movmskps | Op::Movmskpd | Op::Pmovmskb, _) => {
            let bits = match insn.op {
                Op::Movmskps => 32,
                Op::Movmskpd => 64,
                _ => 8,
            };
            let value = read_rm(cpu, memory, insn)?;
            let signs = (0..128 / bits).fold(0, |out, index| {
                out | (lane(value, bits, index) >> (bits - 1)) << index
            });
            write_reg(cpu, insn.reg, insn.size, signs);
        }
        // MOVHLPS and MOVLHPS are the register forms of these opcodes.
        (Op::Movlps, Encoding::XmmRm) => {
            let source = read_rm(cpu, memory, insn)?;
            let low = if in_memory { source } else { source >> 64 };
            cpu.xmm[reg] = cpu.xmm[reg] & !low_mask(8) | low;
        }
        (Op::Movhps, Encoding::XmmRm) => {
            let source = read_rm(cpu, memory, insn)?;
            cpu.xmm[reg] = cpu.xmm[reg] & low_mask(8) | source << 64;
        }
        (Op::Movhps, _) => {
            if let Some(address) = aligned_address(cpu, insn)? {
                write_vector(memory, address, 8, cpu.xmm[reg] >> 64)?;
            }
        }
        // Scalar loads from memory clear the rest of the register; between
        // registers MOVSS and MOVSD merge and MOVQ clears.
        (Op::Movss | Op::Movsd, Encoding::XmmRm) if !in_memory => {
            let mask = low_mask(insn.rm_size);
            let source = read_rm(cpu, memory, insn)?;
            cpu.xmm[reg] = cpu.xmm[reg] & !mask | source & mask;
        }
        (
            Op::Movaps
            | Op::Movups
            | Op::Movdqa
            | Op::Movdqu
            | Op::Movntdqa
            | Op::Movss
            | Op::Movsd
            | Op::Movq,
            Encoding::XmmRm,
        ) => {
            cpu.xmm[reg] = read_rm(cpu, memory, insn)? & low_mask(insn.rm_size);
        }
        (_, Encoding::RmXmm) => {
            let mask = low_mask(insn.rm_size);
            let value = cpu.xmm[reg] & mask;
            match (aligned_address(cpu, insn)?, rm_operand(insn)?) {
                (Some(address), _) => write_vector(memory, address, insn.rm_size, value)?,
                (None, RmOperand::Reg(target)) => {
                    let target = &mut cpu.xmm[xmm(target)];
                    *target = match insn.op {
                        Op::Movq => value,
                        _ => *target & !mask | value,
                    };
                }
                (None, RmOperand::Mem(_)) => return Err(ExitReason::IllegalInstruction(0)),
            }
        }
        (_, Encoding::RmImmXmm) => {
            if let RmOperand::Reg(target) = rm_operand(insn)? {
                let target = &mut cpu.xmm[xmm(target)];
                *target = match insn.op {
                    Op::Psrldq => shift_bytes(*target, insn.imm, false),
                    Op::Pslldq => shift_bytes(*target, insn.imm, true),
                    op => shift(op, *target, insn.imm),
                };
            }
        }
        (_, Encoding::XmmRmImm) => {
            let source = read_rm(cpu, memory, insn)?;
            let old = cpu.xmm[reg];
            let imm = insn.imm;
            cpu.xmm[reg] = match insn.op {
                Op::Pshufd => shuffle_32(source, source, imm),
                Op::Pshuflw => shuffle_16(source, imm, 0),
                Op::Pshufhw => shuffle_16(source, imm, 4),
                Op::Shufps => shuffle_32(old, source, imm),
                Op::Palignr => align_right(old, source, imm),
                Op::Pblendw => blend(old, source, 16, imm),
                Op::Blendps => blend(old, source, 32, imm),
                Op::Blendpd => blend(old, source, 64, imm),
                Op::Insertps => {
                    // A memory source has one lane.
                    let from = if in_memory { 0 } else { imm as u32 >> 6 & 3 };
                    insert_single(old, lane(source, 32, from), imm)
                }
                Op::Mpsadbw => sums_of_differences(old, source, imm),
                _ => build(64, |index| {
                    let from = if index == 0 { old } else { source };
                    lane(from, 64, (imm >> index & 1) as u32)
                }),
            };
        }
        (op, _) => {
            let source = read_rm(cpu, memory, insn)?;
            cpu.xmm[reg] = packed(op, cpu.xmm[reg], source);
        }
    }
    Ok(())
}
