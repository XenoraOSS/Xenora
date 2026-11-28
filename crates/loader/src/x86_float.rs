// SPDX-License-Identifier: GPL-3.0-or-later

//! SSE, SSE2, SSE4.1 and AVX floating-point instructions over [`crate::softfloat`].
//!
//! Exceptions accumulate in MXCSR. If any is unmasked the instruction has no
//! other effect and exits with [`ExitReason::SimdException`]; pre-computation
//! exceptions in any lane are considered before post-computation ones.

use crate::cpu::{CpuState, ExitReason, RFlags};
use crate::memory::GuestMemory;
use crate::softfloat::{
    Arithmetic, Control, F32, F64, Format, Fused, Outcome, PRE_COMPUTATION, PRECISION, Rounding,
    arithmetic, compare, convert, from_integer, fused_multiply_add, round_to_integral, square_root,
    to_integer,
};
use crate::x86_decode::{Insn, RmOperand};
use crate::x86_exec::{address, read_mem, read_reg, rm_operand, write_reg};
use crate::x86_simd::{lane, read_rm};
use crate::x86_table::Op;
use std::cmp::Ordering;

/// Exception flags collected over the lanes of one instruction.
#[derive(Default)]
struct Raised(u32);

impl Raised {
    fn take(&mut self, outcome: Outcome) -> u64 {
        self.0 |= outcome.flags;
        outcome.bits
    }

    /// Record the flags in MXCSR, failing if an unmasked one was raised.
    fn commit(&self, cpu: &mut CpuState) -> Result<(), ExitReason> {
        let masks = cpu.mxcsr.0 >> 7 & 0x3F;
        let before = self.0 & PRE_COMPUTATION;
        if before & !masks != 0 {
            cpu.mxcsr.0 |= before;
            return Err(ExitReason::SimdException);
        }
        cpu.mxcsr.0 |= self.0;
        if self.0 & !masks != 0 {
            return Err(ExitReason::SimdException);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Arithmetic(Arithmetic),
    SquareRoot,
    Compare,
    Round,
}

/// Operation, format and whether only the low lane is involved.
fn classify(op: Op) -> Option<(Kind, Format, bool)> {
    use Arithmetic::{Add, Div, Max, Min, Mul, Sub};
    use Kind::{Arithmetic as A, Compare, Round, SquareRoot};
    Some(match op {
        Op::Addps => (A(Add), F32, false),
        Op::Addpd => (A(Add), F64, false),
        Op::Addss => (A(Add), F32, true),
        Op::Addsd => (A(Add), F64, true),
        Op::Subps => (A(Sub), F32, false),
        Op::Subpd => (A(Sub), F64, false),
        Op::Subss => (A(Sub), F32, true),
        Op::Subsd => (A(Sub), F64, true),
        Op::Mulps => (A(Mul), F32, false),
        Op::Mulpd => (A(Mul), F64, false),
        Op::Mulss => (A(Mul), F32, true),
        Op::Mulsd => (A(Mul), F64, true),
        Op::Divps => (A(Div), F32, false),
        Op::Divpd => (A(Div), F64, false),
        Op::Divss => (A(Div), F32, true),
        Op::Divsd => (A(Div), F64, true),
        Op::Minps => (A(Min), F32, false),
        Op::Minpd => (A(Min), F64, false),
        Op::Minss => (A(Min), F32, true),
        Op::Minsd => (A(Min), F64, true),
        Op::Maxps => (A(Max), F32, false),
        Op::Maxpd => (A(Max), F64, false),
        Op::Maxss => (A(Max), F32, true),
        Op::Maxsd => (A(Max), F64, true),
        Op::Sqrtps => (SquareRoot, F32, false),
        Op::Sqrtpd => (SquareRoot, F64, false),
        Op::Sqrtss => (SquareRoot, F32, true),
        Op::Sqrtsd => (SquareRoot, F64, true),
        Op::Cmpps => (Compare, F32, false),
        Op::Cmppd => (Compare, F64, false),
        Op::Cmpss => (Compare, F32, true),
        Op::Cmpsd => (Compare, F64, true),
        Op::Roundps => (Round, F32, false),
        Op::Roundpd => (Round, F64, false),
        Op::Roundss => (Round, F32, true),
        Op::Roundsd => (Round, F64, true),
        _ => return None,
    })
}

/// Source format, destination format and lane count of a vector conversion;
/// `None` for an integer side.
fn conversion(op: Op) -> Option<(Option<Format>, Option<Format>, u32)> {
    Some(match op {
        Op::Cvtps2pd => (Some(F32), Some(F64), 2),
        Op::Cvtpd2ps => (Some(F64), Some(F32), 2),
        Op::Cvtss2sd => (Some(F32), Some(F64), 1),
        Op::Cvtsd2ss => (Some(F64), Some(F32), 1),
        Op::Cvtdq2ps => (None, Some(F32), 4),
        Op::Cvtdq2pd => (None, Some(F64), 2),
        Op::Cvtps2dq | Op::Cvttps2dq => (Some(F32), None, 4),
        Op::Cvtpd2dq | Op::Cvttpd2dq => (Some(F64), None, 2),
        _ => return None,
    })
}

/// The form of a fused multiply-add and the operand ordering of one operation.
///
/// The Intel SDM names the three orderings `132`, `213` and `231`: each digit is
/// the position of the destination among the three source operands, so the
/// ordering selects which operand is added to the exact product.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FusedShape {
    /// The form of the addend in the odd lanes, or in every lane when
    /// `alternate` is false.
    pub form: Fused,
    /// Whether even lanes take the opposite form, as the alternating families
    /// do: VFMADDSUB subtracts in even lanes and adds in odd ones, VFMSUBADD the
    /// other way round.
    pub alternate: bool,
    /// 132, 213 or 231.
    pub ordering: u8,
    /// One lane, with the rest of the destination kept.
    pub scalar: bool,
}

/// The shape of a fused multiply-add operation, or `None` for any other
/// operation. The registry gives the packed and scalar forms separate
/// operations because they are separate opcodes; `VEX.W` selects single or
/// double precision within each.
pub(crate) fn fused_shape(op: Op) -> Option<FusedShape> {
    let (form, alternate, ordering, scalar) = match op {
        Op::Fmadd132p => (Fused::Add, false, 132, false),
        Op::Fmadd213p => (Fused::Add, false, 213, false),
        Op::Fmadd231p => (Fused::Add, false, 231, false),
        Op::Fmsub132p => (Fused::Sub, false, 132, false),
        Op::Fmsub213p => (Fused::Sub, false, 213, false),
        Op::Fmsub231p => (Fused::Sub, false, 231, false),
        Op::Fnmadd132p => (Fused::NegAdd, false, 132, false),
        Op::Fnmadd213p => (Fused::NegAdd, false, 213, false),
        Op::Fnmadd231p => (Fused::NegAdd, false, 231, false),
        Op::Fnmsub132p => (Fused::NegSub, false, 132, false),
        Op::Fnmsub213p => (Fused::NegSub, false, 213, false),
        Op::Fnmsub231p => (Fused::NegSub, false, 231, false),
        Op::Fmaddsub132p => (Fused::Add, true, 132, false),
        Op::Fmaddsub213p => (Fused::Add, true, 213, false),
        Op::Fmaddsub231p => (Fused::Add, true, 231, false),
        Op::Fmsubadd132p => (Fused::Sub, true, 132, false),
        Op::Fmsubadd213p => (Fused::Sub, true, 213, false),
        Op::Fmsubadd231p => (Fused::Sub, true, 231, false),
        Op::Fmadd132s => (Fused::Add, false, 132, true),
        Op::Fmadd213s => (Fused::Add, false, 213, true),
        Op::Fmadd231s => (Fused::Add, false, 231, true),
        Op::Fmsub132s => (Fused::Sub, false, 132, true),
        Op::Fmsub213s => (Fused::Sub, false, 213, true),
        Op::Fmsub231s => (Fused::Sub, false, 231, true),
        Op::Fnmadd132s => (Fused::NegAdd, false, 132, true),
        Op::Fnmadd213s => (Fused::NegAdd, false, 213, true),
        Op::Fnmadd231s => (Fused::NegAdd, false, 231, true),
        Op::Fnmsub132s => (Fused::NegSub, false, 132, true),
        Op::Fnmsub213s => (Fused::NegSub, false, 213, true),
        Op::Fnmsub231s => (Fused::NegSub, false, 231, true),
        _ => return None,
    };
    Some(FusedShape {
        form,
        alternate,
        ordering,
        scalar,
    })
}

/// Execute a fused multiply-add over the destination, the `VEX.vvvv` register
/// and the ModRM register or memory. The destination is also a source, and the
/// result is left in `destination` so that a scalar form keeps the rest of it.
pub(crate) fn fused(
    cpu: &mut CpuState,
    shape: FusedShape,
    destination: [u128; 2],
    second: [u128; 2],
    third: [u128; 2],
    length: u8,
    wide: bool,
) -> Result<[u128; 2], ExitReason> {
    let format = if wide { F64 } else { F32 };
    let control = Control::from_mxcsr(cpu.mxcsr.0);
    let halves = if shape.scalar {
        1
    } else {
        usize::from(length / 16)
    };
    let count = if shape.scalar { 1 } else { 128 / format.bits };
    let mut raised = Raised::default();
    let mut result = destination;
    for half in 0..halves {
        for index in 0..count {
            let (a, b, c) = match shape.ordering {
                132 => (
                    lane(destination[half], format.bits, index),
                    lane(third[half], format.bits, index),
                    lane(second[half], format.bits, index),
                ),
                213 => (
                    lane(second[half], format.bits, index),
                    lane(destination[half], format.bits, index),
                    lane(third[half], format.bits, index),
                ),
                _ => (
                    lane(second[half], format.bits, index),
                    lane(third[half], format.bits, index),
                    lane(destination[half], format.bits, index),
                ),
            };
            let mut form = shape.form;
            if shape.alternate && index % 2 == 0 {
                form = match form {
                    Fused::Add => Fused::Sub,
                    Fused::Sub => Fused::Add,
                    other => other,
                };
            }
            let outcome = fused_multiply_add(format, a, b, c, form, &control);
            insert(&mut result[half], raised.take(outcome), format.bits, index);
        }
    }
    raised.commit(cpu)?;
    Ok(result)
}

/// The relations for which each compare predicate holds: 1 less, 2 equal, 4 greater
/// and 8 unordered (Intel SDM Vol. 2A, CMPPD, table of comparison predicates).
const PREDICATES: [u8; 16] = [2, 1, 3, 8, 13, 14, 12, 7, 10, 9, 11, 0, 5, 6, 4, 15];

/// Compare predicate 0-31. Predicates 16-31 hold as the first sixteen do and exchange
/// signaling for not signaling on a quiet NaN.
fn predicate(format: Format, a: u64, b: u64, code: u64, control: &Control) -> Outcome {
    let signaling = matches!(code & 3, 1 | 2) != (code & 16 != 0);
    let (order, flags) = compare(format, a, b, signaling, control);
    let relation = match order {
        Some(Ordering::Less) => 1,
        Some(Ordering::Equal) => 2,
        Some(Ordering::Greater) => 4,
        None => 8,
    };
    let holds = PREDICATES[(code & 15) as usize] & relation != 0;
    Outcome {
        bits: if holds { u64::MAX } else { 0 },
        flags,
    }
}

/// ROUND: bit 2 of the immediate takes the mode from MXCSR instead of bits 1:0,
/// and bit 3 suppresses the precision exception. The other bits are ignored.
fn round_by(format: Format, value: u64, imm: u64, control: &Control) -> Outcome {
    let rounding = if imm & 4 != 0 {
        control.rounding
    } else {
        Rounding::from_bits(imm)
    };
    let mut outcome = round_to_integral(format, value, rounding, control);
    if imm & 8 != 0 {
        outcome.flags &= !PRECISION;
    }
    outcome
}

/// DPPS and DPPD: products of the lanes chosen by the high immediate bits, summed
/// in pairs, in the result lanes chosen by the low ones. A lane not chosen is +0.
///
/// The multiplications raise their exceptions together, then each addition before
/// the next, where the operation sections of the Intel SDM call the handler.
fn dot_product(
    cpu: &mut CpuState,
    format: Format,
    a: u128,
    b: u128,
    imm: u64,
    control: &Control,
) -> Result<u128, ExitReason> {
    let mut count = 128 / format.bits;
    let mut raised = Raised::default();
    let mut terms = [0; 4];
    for index in (0..count).filter(|index| imm >> (4 + index) & 1 != 0) {
        let (x, y) = (lane(a, format.bits, index), lane(b, format.bits, index));
        terms[index as usize] = raised.take(arithmetic(Arithmetic::Mul, format, x, y, control));
    }
    raised.commit(cpu)?;
    while count > 1 {
        count /= 2;
        for index in 0..count as usize {
            let (x, y) = (terms[2 * index], terms[2 * index + 1]);
            let mut raised = Raised::default();
            terms[index] = raised.take(arithmetic(Arithmetic::Add, format, x, y, control));
            raised.commit(cpu)?;
        }
    }
    let mut result = 0;
    for index in (0..128 / format.bits).filter(|index| imm >> index & 1 != 0) {
        insert(&mut result, terms[0], format.bits, index);
    }
    Ok(result)
}

fn low_bits(bits: u32) -> u128 {
    (1 << bits) - 1
}

/// Place `value` in lane `index` of `bits`.
fn insert(target: &mut u128, value: u64, bits: u32, index: u32) {
    let shift = bits * index;
    let mask = low_bits(bits) << shift;
    *target = (*target & !mask) | ((u128::from(value) << shift) & mask);
}

/// Lane-by-lane result in one half of a vector, or `None` if `op` is not such an
/// operation. Scalar operations keep the rest of `old`.
fn lanes(
    op: Op,
    old: u128,
    source: u128,
    imm: u64,
    control: &Control,
    raised: &mut Raised,
) -> Option<u128> {
    if let Some((kind, format, scalar)) = classify(op) {
        let mut result = old;
        let count = if scalar { 1 } else { 128 / format.bits };
        for index in 0..count {
            let a = lane(old, format.bits, index);
            let b = lane(source, format.bits, index);
            let outcome = match kind {
                Kind::Arithmetic(kind) => arithmetic(kind, format, a, b, control),
                Kind::SquareRoot => square_root(format, b, control),
                Kind::Compare => predicate(format, a, b, imm & 31, control),
                Kind::Round => round_by(format, b, imm, control),
            };
            insert(&mut result, raised.take(outcome), format.bits, index);
        }
        return Some(result);
    }
    let (from, to, count) = conversion(op)?;
    let truncate = matches!(op, Op::Cvttps2dq | Op::Cvttpd2dq);
    let mut result = if count == 1 { old } else { 0 };
    for index in 0..count {
        let value = lane(source, from.map_or(32, |format| format.bits), index);
        let outcome = match (from, to) {
            (Some(from), Some(to)) => convert(from, to, value, control),
            (None, Some(to)) => from_integer(to, i64::from(value as u32 as i32), control),
            (Some(from), None) => to_integer(from, value, 32, truncate, control),
            (None, None) => Outcome { bits: 0, flags: 0 },
        };
        let width = to.map_or(32, |format| format.bits);
        insert(&mut result, raised.take(outcome), width, index);
    }
    Some(result)
}

/// `old` with its low lane converted from a general register or memory.
pub(crate) fn from_general(
    cpu: &mut CpuState,
    memory: &GuestMemory,
    insn: &Insn,
    old: u128,
) -> Result<u128, ExitReason> {
    let value = match rm_operand(insn)? {
        RmOperand::Reg(source) => read_reg(cpu, source, insn.size),
        RmOperand::Mem(operand) => read_mem(memory, address(cpu, &operand), insn.size)?,
    };
    let integer = if insn.size == 8 {
        value as i64
    } else {
        i64::from(value as u32 as i32)
    };
    let format = if insn.op == Op::Cvtsi2ss { F32 } else { F64 };
    let mut raised = Raised::default();
    let control = Control::from_mxcsr(cpu.mxcsr.0);
    let bits = raised.take(from_integer(format, integer, &control));
    raised.commit(cpu)?;
    let mut result = old;
    insert(&mut result, bits, format.bits, 0);
    Ok(result)
}

/// The vector result of a VEX-encoded operation, or `None` if `op` is not a
/// floating-point operation with one. The halves of a 256-bit operation raise
/// their exceptions together, except those of the dot products.
pub(crate) fn vector(
    cpu: &mut CpuState,
    op: Op,
    first: [u128; 2],
    source: [u128; 2],
    length: u8,
    imm: u64,
) -> Result<Option<[u128; 2]>, ExitReason> {
    let control = Control::from_mxcsr(cpu.mxcsr.0);
    let halves = usize::from(length / 16);
    let mut result = [0; 2];
    if matches!(op, Op::Dpps | Op::Dppd) {
        let format = if op == Op::Dpps { F32 } else { F64 };
        for half in 0..halves {
            result[half] = dot_product(cpu, format, first[half], source[half], imm, &control)?;
        }
        return Ok(Some(result));
    }
    let scalar = classify(op).is_some_and(|(_, _, scalar)| scalar)
        || conversion(op).is_some_and(|(_, _, count)| count == 1);
    let halves = if scalar { 1 } else { halves };
    let mut raised = Raised::default();
    for half in 0..halves {
        // Widening takes both halves of the result from the low half of the source.
        let value = match op {
            Op::Cvtps2pd | Op::Cvtdq2pd => source[0] >> (64 * half),
            _ => source[half],
        };
        match lanes(op, first[half], value, imm, &control, &mut raised) {
            Some(value) => result[half] = value,
            None => return Ok(None),
        }
    }
    raised.commit(cpu)?;
    // Narrowing leaves two lanes of each half, which are joined.
    if matches!(op, Op::Cvtpd2ps | Op::Cvtpd2dq | Op::Cvttpd2dq) {
        result = [result[0] | result[1] << 64, 0];
    }
    Ok(Some(result))
}

/// Execute a floating-point instruction, or return `false` if it is not one.
pub(crate) fn execute(
    cpu: &mut CpuState,
    memory: &GuestMemory,
    insn: &Insn,
) -> Result<bool, ExitReason> {
    let control = Control::from_mxcsr(cpu.mxcsr.0);
    let reg = usize::from(insn.reg.index);
    let mut raised = Raised::default();

    if classify(insn.op).is_some() || conversion(insn.op).is_some() {
        let source = read_rm(cpu, memory, insn)?;
        // Legacy encodings define only compare predicates 0-7.
        let compares = matches!(classify(insn.op), Some((Kind::Compare, ..)));
        let imm = if compares { insn.imm & 7 } else { insn.imm };
        let old = cpu.xmm[reg];
        if let Some(result) = lanes(insn.op, old, source, imm, &control, &mut raised) {
            raised.commit(cpu)?;
            cpu.xmm[reg] = result;
        }
        return Ok(true);
    }

    match insn.op {
        Op::Dpps | Op::Dppd => {
            let format = if insn.op == Op::Dpps { F32 } else { F64 };
            let source = read_rm(cpu, memory, insn)?;
            let old = cpu.xmm[reg];
            cpu.xmm[reg] = dot_product(cpu, format, old, source, insn.imm, &control)?;
        }
        Op::Cvtsi2ss | Op::Cvtsi2sd => {
            cpu.xmm[reg] = from_general(cpu, memory, insn, cpu.xmm[reg])?;
        }
        Op::Cvtss2si | Op::Cvttss2si | Op::Cvtsd2si | Op::Cvttsd2si => {
            let format = if matches!(insn.op, Op::Cvtss2si | Op::Cvttss2si) {
                F32
            } else {
                F64
            };
            let truncate = matches!(insn.op, Op::Cvttss2si | Op::Cvttsd2si);
            let value = lane(read_rm(cpu, memory, insn)?, format.bits, 0);
            let bits = 8 * u32::from(insn.size);
            let integer = raised.take(to_integer(format, value, bits, truncate, &control));
            raised.commit(cpu)?;
            write_reg(cpu, insn.reg, insn.size, integer);
        }
        Op::Comiss | Op::Comisd | Op::Ucomiss | Op::Ucomisd => {
            let format = if matches!(insn.op, Op::Comiss | Op::Ucomiss) {
                F32
            } else {
                F64
            };
            let signaling = matches!(insn.op, Op::Comiss | Op::Comisd);
            let a = lane(cpu.xmm[reg], format.bits, 0);
            let b = lane(read_rm(cpu, memory, insn)?, format.bits, 0);
            let (order, flags) = compare(format, a, b, signaling, &control);
            raised.0 |= flags;
            raised.commit(cpu)?;
            cpu.flags = RFlags {
                zf: matches!(order, None | Some(Ordering::Equal)),
                pf: order.is_none(),
                cf: matches!(order, None | Some(Ordering::Less)),
                ..RFlags::default()
            };
        }
        _ => return Ok(false),
    }
    Ok(true)
}
