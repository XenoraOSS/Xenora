// SPDX-License-Identifier: GPL-3.0-or-later

//! SSE, SSE2 and SSE4.1 floating-point instructions over [`crate::softfloat`].
//!
//! Exceptions accumulate in MXCSR. If any is unmasked the instruction has no
//! other effect and exits with [`ExitReason::SimdException`]; pre-computation
//! exceptions in any lane are considered before post-computation ones.

use crate::cpu::{CpuState, ExitReason, RFlags};
use crate::memory::GuestMemory;
use crate::softfloat::{
    Arithmetic, Control, F32, F64, Format, Outcome, PRE_COMPUTATION, PRECISION, Rounding,
    arithmetic, compare, convert, from_integer, round_to_integral, square_root, to_integer,
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

/// CMPccPS predicate: a relation, optionally negated. Bit 2 negates; LT and LE
/// and their negations signal on quiet NaNs.
fn predicate(format: Format, a: u64, b: u64, code: u64, control: &Control) -> Outcome {
    let relation = code & 3;
    let signaling = matches!(relation, 1 | 2);
    let (order, flags) = compare(format, a, b, signaling, control);
    let holds = match relation {
        0 => order == Some(Ordering::Equal),
        1 => order == Some(Ordering::Less),
        2 => matches!(order, Some(Ordering::Less | Ordering::Equal)),
        _ => order.is_none(),
    };
    Outcome {
        bits: if holds != (code & 4 != 0) {
            u64::MAX
        } else {
            0
        },
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

/// Execute a floating-point instruction, or return `false` if it is not one.
pub(crate) fn execute(
    cpu: &mut CpuState,
    memory: &GuestMemory,
    insn: &Insn,
) -> Result<bool, ExitReason> {
    let control = Control::from_mxcsr(cpu.mxcsr.0);
    let reg = usize::from(insn.reg.index);
    let mut raised = Raised::default();

    if let Some((kind, format, scalar)) = classify(insn.op) {
        let source = read_rm(cpu, memory, insn)?;
        let old = cpu.xmm[reg];
        let mut result = old;
        let count = if scalar { 1 } else { 128 / format.bits };
        for index in 0..count {
            let a = lane(old, format.bits, index);
            let b = lane(source, format.bits, index);
            let outcome = match kind {
                Kind::Arithmetic(kind) => arithmetic(kind, format, a, b, &control),
                Kind::SquareRoot => square_root(format, b, &control),
                // Legacy encodings define only predicates 0-7.
                Kind::Compare => predicate(format, a, b, insn.imm & 7, &control),
                Kind::Round => round_by(format, b, insn.imm, &control),
            };
            insert(&mut result, raised.take(outcome), format.bits, index);
        }
        raised.commit(cpu)?;
        cpu.xmm[reg] = result;
        return Ok(true);
    }

    if let Some((from, to, count)) = conversion(insn.op) {
        let source = read_rm(cpu, memory, insn)?;
        let truncate = matches!(insn.op, Op::Cvttps2dq | Op::Cvttpd2dq);
        let merges = matches!(insn.op, Op::Cvtss2sd | Op::Cvtsd2ss);
        let mut result = if merges { cpu.xmm[reg] } else { 0 };
        for index in 0..count {
            let value = lane(source, from.map_or(32, |format| format.bits), index);
            let outcome = match (from, to) {
                (Some(from), Some(to)) => convert(from, to, value, &control),
                (None, Some(to)) => from_integer(to, i64::from(value as u32 as i32), &control),
                (Some(from), None) => to_integer(from, value, 32, truncate, &control),
                (None, None) => Outcome { bits: 0, flags: 0 },
            };
            let width = to.map_or(32, |format| format.bits);
            insert(&mut result, raised.take(outcome), width, index);
        }
        raised.commit(cpu)?;
        cpu.xmm[reg] = result;
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
            let bits = raised.take(from_integer(format, integer, &control));
            raised.commit(cpu)?;
            insert(&mut cpu.xmm[reg], bits, format.bits, 0);
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
