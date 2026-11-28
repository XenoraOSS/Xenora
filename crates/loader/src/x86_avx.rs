// SPDX-License-Identifier: GPL-3.0-or-later

//! VEX-encoded vector instructions.
//!
//! A form shares its operation with a legacy SSE form where the lanes are the same,
//! and runs on both halves of a 256-bit vector. It differs in three ways (Intel SDM
//! Vol. 1, 14.1): the destination is cleared above the vector length, VEX.vvvv may
//! name a further source, and memory may have any alignment unless the form says
//! otherwise. Every instruction is all-or-nothing.

use crate::cpu::{CpuState, ExitReason, Mxcsr, RFlags};
use crate::memory::{GuestMemory, MemoryError};
use crate::x86_decode::{Encoding, Insn, RmOperand, Vex};
use crate::x86_exec::{
    address, canonical_access, read_mem, read_reg, rm_operand, write_mem, write_reg,
};
use crate::x86_float;
use crate::x86_simd::{
    aes, controlled, execute_legacy, insert_general, lane, lane_immediate, packed, select, shift_by,
};
use crate::x86_table::{FORMS, Op};

/// The low and high halves of a 256-bit vector.
type Vector = [u128; 2];

const LOW: u128 = u64::MAX as u128;

fn read_ymm(cpu: &CpuState, index: u8) -> Vector {
    let index = usize::from(index);
    [cpu.xmm[index], cpu.ymm_high[index]]
}

/// Write a register, clearing it above the vector length.
fn write_ymm(cpu: &mut CpuState, index: u8, value: Vector, length: u8) {
    let index = usize::from(index);
    cpu.xmm[index] = value[0];
    cpu.ymm_high[index] = if length == 32 { value[1] } else { 0 };
}

/// Address of a memory operand, checked for alignment where the form requires it.
fn vector_address(cpu: &CpuState, insn: &Insn) -> Result<Option<u64>, ExitReason> {
    let Some(operand) = insn.memory() else {
        return Ok(None);
    };
    let address = address(cpu, &operand);
    let unaligned = FORMS[usize::from(insn.form)].unaligned;
    if !unaligned && !address.is_multiple_of(u64::from(insn.rm_size)) {
        return Err(ExitReason::GeneralProtection);
    }
    Ok(Some(address))
}

/// `size` bytes of memory, zero-extended.
fn read_bytes(memory: &GuestMemory, address: u64, size: u8) -> Result<Vector, ExitReason> {
    if !canonical_access(address, size) {
        return Err(ExitReason::GeneralProtection);
    }
    let mut bytes = [0; 32];
    memory
        .read_bytes(address, &mut bytes[..usize::from(size)])
        .map_err(ExitReason::MemoryFault)?;
    let (halves, _) = bytes.as_chunks::<16>();
    Ok([halves[0], halves[1]].map(u128::from_le_bytes))
}

fn write_bytes(
    memory: &mut GuestMemory,
    address: u64,
    size: u8,
    value: Vector,
) -> Result<(), ExitReason> {
    if !canonical_access(address, size) {
        return Err(ExitReason::GeneralProtection);
    }
    let mut bytes = [0; 32];
    bytes[..16].copy_from_slice(&value[0].to_le_bytes());
    bytes[16..].copy_from_slice(&value[1].to_le_bytes());
    memory
        .write_bytes(address, &bytes[..usize::from(size)])
        .map_err(ExitReason::MemoryFault)
}

/// The register or memory named by ModRM `rm`.
fn read_source(cpu: &CpuState, memory: &GuestMemory, insn: &Insn) -> Result<Vector, ExitReason> {
    match (vector_address(cpu, insn)?, rm_operand(insn)?) {
        (Some(address), _) => read_bytes(memory, address, insn.rm_size),
        (None, RmOperand::Reg(reg)) => Ok(read_ymm(cpu, reg.index)),
        (None, RmOperand::Mem(_)) => Err(ExitReason::IllegalInstruction(0)),
    }
}

/// Both halves of a vector from 32-bit lanes chosen per lane of the result.
fn singles(source: u128, select: impl Fn(u32) -> u32) -> u128 {
    (0..4).fold(0, |out, index| {
        out | u128::from(lane(source, 32, select(index))) << (32 * index)
    })
}

fn doubles(source: u128, select: impl Fn(u32) -> u32) -> u128 {
    (0..2).fold(0, |out, index| {
        out | u128::from(lane(source, 64, select(index))) << (64 * index)
    })
}

/// A lane of a 256-bit vector, counting across both halves.
fn element(value: Vector, bits: u32, index: u32) -> u64 {
    let per_half = 128 / bits;
    lane(value[(index / per_half) as usize], bits, index % per_half)
}

/// A 256-bit vector of `length` bytes built lane by lane.
fn elements(length: u8, bits: u32, f: impl Fn(u32) -> u64) -> Vector {
    let per_half = 128 / bits;
    let count = u32::from(length) * 8 / bits;
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1 << bits) - 1
    };
    let mut out = [0; 2];
    for index in 0..count {
        let half = (index / per_half) as usize;
        out[half] |= u128::from(f(index) & mask) << (bits * (index % per_half));
    }
    out
}

/// A 128-bit vector of the low `bits` of `value` repeated.
fn splat(value: u64, bits: u32) -> u128 {
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1 << bits) - 1
    };
    let value = u128::from(value & mask);
    (0..128 / bits).fold(0, |out, index| out | value << (bits * index))
}

/// A half for VPERM2F128: two selector bits and a bit that zeroes.
fn half(a: Vector, b: Vector, control: u64) -> u128 {
    match control & 11 {
        0 => a[0],
        1 => a[1],
        2 => b[0],
        3 => b[1],
        _ => 0,
    }
}

/// Execute a VEX-encoded instruction. RIP is advanced by the caller.
pub(crate) fn execute(
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
    insn: &Insn,
    vex: Vex,
) -> Result<(), ExitReason> {
    let reg = insn.reg.index;
    let length = vex.length;
    let in_memory = insn.memory().is_some();
    // The first source: VEX.vvvv, or nothing for a form without one.
    let first = vex.source.map_or([0; 2], |index| read_ymm(cpu, index));
    let halves = usize::from(length / 16);
    match (insn.op, insn.encoding) {
        (Op::Vzeroupper, _) => cpu.ymm_high = [0; 16],
        (Op::Vzeroall, _) => {
            cpu.xmm = [0; 16];
            cpu.ymm_high = [0; 16];
        }
        (Op::Ldmxcsr, _) => {
            let value = read_source(cpu, memory, insn)?[0] as u32;
            if value & !Mxcsr::WRITABLE != 0 {
                return Err(ExitReason::GeneralProtection);
            }
            cpu.mxcsr = Mxcsr(value);
        }
        (Op::Stmxcsr, _) => {
            if let Some(address) = vector_address(cpu, insn)? {
                write_mem(memory, address, 4, u64::from(cpu.mxcsr.0))?;
            }
        }
        (Op::Movd, Encoding::XmmGpr) => {
            let value = match rm_operand(insn)? {
                RmOperand::Reg(source) => read_reg(cpu, source, insn.size),
                RmOperand::Mem(operand) => read_mem(memory, address(cpu, &operand), insn.size)?,
            };
            write_ymm(cpu, reg, [u128::from(value), 0], length);
        }
        (Op::Movd, _) => {
            let value = cpu.xmm[usize::from(reg)] as u64;
            match rm_operand(insn)? {
                RmOperand::Reg(target) => write_reg(cpu, target, insn.size, value),
                RmOperand::Mem(operand) => {
                    write_mem(memory, address(cpu, &operand), insn.size, value)?;
                }
            }
        }
        (Op::Movmskps | Op::Movmskpd | Op::Pmovmskb, _) => {
            let bits = match insn.op {
                Op::Movmskps => 32,
                Op::Movmskpd => 64,
                _ => 8,
            };
            let value = read_source(cpu, memory, insn)?;
            let lanes = 128 / bits;
            let signs = (0..u32::from(length) / 16 * lanes).fold(0, |out, index| {
                let sign = lane(value[(index / lanes) as usize], bits, index % lanes) >> (bits - 1);
                out | sign << index
            });
            write_reg(cpu, insn.reg, insn.size, signs);
        }
        // VPMASKMOVD and VPMASKMOVQ: a lane is loaded or stored only when the
        // sign bit of its mask lane is set. Masked lanes are never accessed,
        // which is exactly the fault suppression the manuals specify: an
        // enabled lane that faults still raises.
        (Op::Pmaskmov, Encoding::XmmRm) => {
            let bits = if vex.wide { 64 } else { 32 };
            let width = (bits / 8) as u8;
            let Some(base) = vector_address(cpu, insn)? else {
                return Err(ExitReason::IllegalInstruction(0));
            };
            let count = u32::from(length) * 8 / bits;
            let per_half = 128 / bits;
            let mut value = [0; 2];
            for index in 0..count {
                if element(first, bits, index) >> (bits - 1) & 1 == 0 {
                    continue;
                }
                let lane_value =
                    read_mem(memory, base + u64::from(index) * u64::from(width), width)?;
                let half = (index / per_half) as usize;
                value[half] |= u128::from(lane_value) << (bits * (index % per_half));
            }
            write_ymm(cpu, reg, value, length);
        }
        (Op::Pmaskmov, Encoding::RmXmm) => {
            let bits = if vex.wide { 64 } else { 32 };
            let width = (bits / 8) as u8;
            let Some(base) = vector_address(cpu, insn)? else {
                return Err(ExitReason::IllegalInstruction(0));
            };
            let count = u32::from(length) * 8 / bits;
            let value = read_ymm(cpu, reg);
            for index in 0..count {
                if element(first, bits, index) >> (bits - 1) & 1 == 0 {
                    continue;
                }
                write_mem(
                    memory,
                    base + u64::from(index) * u64::from(width),
                    width,
                    element(value, bits, index),
                )?;
            }
        }
        // The result is a general register, memory or flags, as in the legacy form.
        (_, Encoding::RegXmm | Encoding::GprXmmImm | Encoding::RegXmmImm)
        | (
            Op::Comiss | Op::Comisd | Op::Ucomiss | Op::Ucomisd | Op::Pcmpestri | Op::Pcmpistri,
            _,
        ) => execute_legacy(cpu, memory, insn)?,
        (Op::Pcmpestrm | Op::Pcmpistrm, _) => {
            execute_legacy(cpu, memory, insn)?;
            cpu.ymm_high[0] = 0;
        }
        (Op::Ptest, _) => {
            let source = read_source(cpu, memory, insn)?;
            let value = read_ymm(cpu, reg);
            cpu.flags = RFlags {
                zf: (0..halves).all(|half| value[half] & source[half] == 0),
                cf: (0..halves).all(|half| !value[half] & source[half] == 0),
                ..RFlags::default()
            };
        }
        (Op::Cvtsi2ss | Op::Cvtsi2sd, _) => {
            let result = x86_float::from_general(cpu, memory, insn, first[0])?;
            write_ymm(cpu, reg, [result, 0], 16);
        }
        (_, Encoding::XmmGprImm) => {
            let result = insert_general(cpu, memory, insn, first[0])?;
            write_ymm(cpu, reg, [result, 0], 16);
        }
        // VEX.vvvv names the destination.
        (op, Encoding::RmImmXmm) => {
            let target = vex.source.ok_or(ExitReason::IllegalInstruction(0))?;
            let source = read_source(cpu, memory, insn)?;
            let result = source.map(|half| shift_by(op, half, insn.imm));
            write_ymm(cpu, target, result, length);
        }
        // Loads and stores of part of a vector. VMOVHLPS and VMOVLHPS are the
        // register forms of VMOVLPS and VMOVHPS.
        (Op::Movlps, Encoding::XmmRm) => {
            let source = read_source(cpu, memory, insn)?[0];
            let low = if in_memory { source } else { source >> 64 };
            write_ymm(cpu, reg, [first[0] & !LOW | low, 0], length);
        }
        (Op::Movhps, Encoding::XmmRm) => {
            let source = read_source(cpu, memory, insn)?[0];
            write_ymm(cpu, reg, [first[0] & LOW | source << 64, 0], length);
        }
        (Op::Movhps, _) => {
            if let Some(address) = vector_address(cpu, insn)? {
                write_mem(memory, address, 8, (cpu.xmm[usize::from(reg)] >> 64) as u64)?;
            }
        }
        // Between registers VMOVSS and VMOVSD keep the rest of the first source.
        (Op::Movss | Op::Movsd, encoding) if !in_memory => {
            let mask = if insn.op == Op::Movss {
                u128::from(u32::MAX)
            } else {
                LOW
            };
            let RmOperand::Reg(rm) = rm_operand(insn)? else {
                return Err(ExitReason::IllegalInstruction(0));
            };
            let (target, source) = if encoding == Encoding::XmmRm {
                (reg, rm.index)
            } else {
                (rm.index, reg)
            };
            let low = cpu.xmm[usize::from(source)] & mask;
            write_ymm(cpu, target, [first[0] & !mask | low, 0], 16);
        }
        (Op::Movss | Op::Movsd | Op::Movq, Encoding::XmmRm) => {
            let mask = if insn.op == Op::Movss {
                u128::from(u32::MAX)
            } else {
                LOW
            };
            let source = read_source(cpu, memory, insn)?[0] & mask;
            write_ymm(cpu, reg, [source, 0], 16);
        }
        (_, Encoding::RmXmm) => {
            let value = read_ymm(cpu, reg);
            match (vector_address(cpu, insn)?, rm_operand(insn)?) {
                (Some(address), _) => write_bytes(memory, address, insn.rm_size, value)?,
                (None, RmOperand::Reg(target)) => {
                    let mask = if insn.op == Op::Movq { LOW } else { u128::MAX };
                    write_ymm(cpu, target.index, [value[0] & mask, value[1]], length);
                }
                (None, RmOperand::Mem(_)) => return Err(ExitReason::IllegalInstruction(0)),
            }
        }
        (Op::Vextractf128 | Op::Vextracti128, _) => {
            let value = read_ymm(cpu, reg)[(insn.imm & 1) as usize];
            match (vector_address(cpu, insn)?, rm_operand(insn)?) {
                (Some(address), _) => write_bytes(memory, address, 16, [value, 0])?,
                (None, RmOperand::Reg(target)) => write_ymm(cpu, target.index, [value, 0], 16),
                (None, RmOperand::Mem(_)) => return Err(ExitReason::IllegalInstruction(0)),
            }
        }
        (op, encoding) => {
            // F16C: VCVTPH2PS reads the ModRM operand; VCVTPS2PH writes it and
            // reads the register named by ModRM.reg. Both VEX.W and a nonzero
            // VEX.vvvv are rejected by the decoder. A store's address and
            // permissions are checked before the conversion, so a fault raises
            // no MXCSR flags.
            if matches!(op, Op::Vcvtph2ps | Op::Vcvtps2ph) {
                let destination = if op == Op::Vcvtps2ph {
                    match vector_address(cpu, insn)? {
                        Some(address) => {
                            if !canonical_access(address, insn.rm_size) {
                                return Err(ExitReason::GeneralProtection);
                            }
                            if !memory.can_write(address, usize::from(insn.rm_size)) {
                                return Err(ExitReason::MemoryFault(
                                    MemoryError::PermissionViolation {
                                        address,
                                        required: "write",
                                    },
                                ));
                            }
                            Some(address)
                        }
                        None => None,
                    }
                } else {
                    None
                };
                let source = if op == Op::Vcvtph2ps {
                    read_source(cpu, memory, insn)?
                } else {
                    read_ymm(cpu, reg)
                };
                let result =
                    x86_float::half_precision(cpu, op, source, length, lane_immediate(insn))?
                        .ok_or(ExitReason::IllegalInstruction(0))?;
                if op == Op::Vcvtph2ps {
                    write_ymm(cpu, reg, result, length);
                } else if let Some(address) = destination {
                    write_bytes(memory, address, insn.rm_size, result)?;
                } else {
                    let RmOperand::Reg(target) = rm_operand(insn)? else {
                        return Err(ExitReason::IllegalInstruction(0));
                    };
                    write_ymm(cpu, target.index, result, 16);
                }
                return Ok(());
            }
            let source = read_source(cpu, memory, insn)?;
            let imm = lane_immediate(insn);
            if let Some(shape) = x86_float::fused_shape(op) {
                let destination = read_ymm(cpu, reg);
                let result =
                    x86_float::fused(cpu, shape, destination, first, source, length, vex.wide)?;
                let write = if shape.scalar { 16 } else { length };
                write_ymm(cpu, reg, result, write);
                return Ok(());
            }
            if let Some(result) = x86_float::vector(cpu, op, first, source, length, imm)? {
                write_ymm(cpu, reg, result, length);
                return Ok(());
            }
            let result = match (op, encoding) {
                (
                    Op::Movaps | Op::Movups | Op::Movdqa | Op::Movdqu | Op::Movntdqa | Op::Lddqu,
                    _,
                ) => source,
                (Op::Movddup, _) => source.map(|half| doubles(half, |_| 0)),
                (Op::Movsldup, _) => source.map(|half| singles(half, |index| index & !1)),
                (Op::Movshdup, _) => source.map(|half| singles(half, |index| index | 1)),
                (Op::Vbroadcastss, _) => [singles(source[0], |_| 0); 2],
                (Op::Vbroadcastsd, _) => [doubles(source[0], |_| 0); 2],
                (Op::Vbroadcastf128, _) => [source[0]; 2],
                (Op::Vinsertf128, _) => {
                    let mut result = first;
                    result[(imm & 1) as usize] = source[0];
                    result
                }
                (Op::Vperm2f128 | Op::Vperm2i128, _) => {
                    [half(first, source, imm), half(first, source, imm >> 4)]
                }
                (Op::Vinserti128, _) => {
                    let mut result = first;
                    result[(imm & 1) as usize] = source[0];
                    result
                }
                // AVX2: the source selects a lane of the second source by the
                // index of the same lane in the first, for VPERMD and VPERMPS,
                // or by two immediate bits for VPERMQ and VPERMPD.
                (Op::Vpermd | Op::Vpermps, _) => elements(length, 32, |index| {
                    element(source, 32, (element(first, 32, index) & 7) as u32)
                }),
                (Op::Vpermq | Op::Vpermpd, _) => elements(length, 64, |index| {
                    element(source, 64, (imm >> (2 * index)) as u32 & 3)
                }),
                (Op::Vpbroadcastb, _) => [splat(source[0] as u64, 8); 2],
                (Op::Vpbroadcastw, _) => [splat(source[0] as u64, 16); 2],
                (Op::Vpbroadcastd, _) => [splat(source[0] as u64, 32); 2],
                (Op::Vpbroadcastq, _) => [splat(source[0] as u64, 64); 2],
                (Op::Vbroadcasti128, _) => [source[0]; 2],
                (Op::Vpblendd, _) => [0, 1].map(|half| {
                    (0..4).fold(0, |out, index| {
                        // One immediate bit for each 32-bit lane of the whole
                        // vector, so the high half reads bits 4 to 7.
                        let chosen = if imm >> (4 * half as u64 + index) & 1 == 1 {
                            source[half]
                        } else {
                            first[half]
                        };
                        out | chosen & (u128::from(u32::MAX) << (32 * index))
                    })
                }),
                // AVX2 per-lane shifts: VEX.vvvv holds the data and the ModRM
                // operand the counts, as the operation sections of the manual
                // name SRC1 and SRC2. A count wider than the element zeroes a
                // logical shift and sign-fills an arithmetic one.
                (Op::Vpsllv | Op::Vpsrlv | Op::Vpsravd, _) => {
                    let bits = if vex.wide { 64 } else { 32 };
                    elements(length, bits, |index| {
                        let value = element(first, bits, index);
                        let count = element(source, bits, index);
                        if count >= u64::from(bits) {
                            let negative = value >> (bits - 1) & 1 == 1;
                            if insn.op == Op::Vpsravd && negative {
                                if bits == 64 {
                                    u64::MAX
                                } else {
                                    u64::from(u32::MAX)
                                }
                            } else {
                                0
                            }
                        } else {
                            let shift = count as u32;
                            match insn.op {
                                Op::Vpsllv => value << shift,
                                Op::Vpsrlv => value >> shift,
                                _ => ((value << (64 - bits)) as i64 >> (64 - bits) >> shift) as u64,
                            }
                        }
                    })
                }
                // The same selectors serve both halves.
                (Op::Vpermilps, Encoding::XmmRmImm) => {
                    source.map(|half| singles(half, |index| (imm >> (2 * index) & 3) as u32))
                }
                // One selector bit for each of the four results.
                (Op::Vpermilpd, Encoding::XmmRmImm) => [0, 1].map(|half| {
                    doubles(source[half], |index| {
                        (imm >> (2 * half as u32 + index) & 1) as u32
                    })
                }),
                // The source selects from the first source, lane by lane.
                (Op::Vpermilps, _) => [0, 1].map(|half| {
                    singles(first[half], |index| {
                        (lane(source[half], 32, index) & 3) as u32
                    })
                }),
                (Op::Vpermilpd, _) => [0, 1].map(|half| {
                    doubles(first[half], |index| {
                        (lane(source[half], 64, index) >> 1 & 1) as u32
                    })
                }),
                // The sign bits of a fourth register select the source.
                (Op::Pblendvb | Op::Blendvps | Op::Blendvpd, _) => {
                    let control = read_ymm(cpu, (imm >> 4) as u8);
                    [0, 1].map(|half| select(op, first[half], source[half], control[half]))
                }
                // The VEX AES forms are the non-destructive encodings of the
                // legacy round operations: the state comes from VEX.vvvv, and
                // the round key or the source from the ModRM operand. AESIMC
                // and AESKEYGENASSIST have no VEX.vvvv source.
                (Op::Aesenc | Op::Aesenclast | Op::Aesdec | Op::Aesdeclast, _) => {
                    [0, 1].map(|half| aes(op, first[half], source[half], 0))
                }
                (Op::Aesimc, _) => [0, 1].map(|half| aes(op, 0, source[half], 0)),
                (Op::Aeskeygenassist, _) => [0, 1].map(|half| aes(op, 0, source[half], imm)),
                (_, Encoding::XmmRmImm) => {
                    // Immediate bits for the low half only; others serve both.
                    let used = match op {
                        Op::Blendps => 4,
                        Op::Blendpd | Op::Shufpd => 2,
                        _ => 0,
                    };
                    [0, 1].map(|half| {
                        let imm = imm >> (used * half);
                        controlled(op, first[half], source[half], imm)
                    })
                }
                // Uniform-count shifts: the count is the low 64 bits of the
                // second source, an xmm operand even in the 256-bit forms, so
                // both halves shift by the same count.
                (
                    Op::Psllw
                    | Op::Pslld
                    | Op::Psllq
                    | Op::Psrlw
                    | Op::Psrld
                    | Op::Psrlq
                    | Op::Psraw
                    | Op::Psrad,
                    Encoding::XmmRm,
                ) => {
                    let count = source[0] as u64;
                    [0, 1].map(|half| shift_by(op, first[half], count))
                }
                (_, Encoding::XmmRm) => [0, 1].map(|half| packed(op, first[half], source[half])),
                _ => return Err(ExitReason::IllegalInstruction(0)),
            };
            write_ymm(cpu, reg, result, length);
        }
    }
    Ok(())
}
