// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared x86-64 decoder.
//!
//! The static PE probe, the software interpreter and the native DBT all decode
//! through [`decode`], driven by static tables generated from
//! `x86/instructions.toml`, so instruction boundaries, operands and rejected
//! forms agree across them.

use crate::x86_table::{
    FORMS, GROUPS, ONE_BYTE, Op, THREE_BYTE_3A_66, THREE_BYTE_38_66, TWO_BYTE, TWO_BYTE_66,
    TWO_BYTE_F2, TWO_BYTE_F3,
};

/// Architectural x86-64 instruction length limit, in bytes.
///
/// An instruction may not exceed 15 bytes (Intel SDM Vol. 2A, "Instruction
/// Length"). Decoding stops before a 16th byte would be fetched and reports
/// [`DecodeError::TooLong`], a general-protection fault (Intel SDM Vol. 3A,
/// 7.15).
pub const MAX_INSTRUCTION_LEN: usize = 15;

/// Number of leading legacy segment prefix bytes (0x64 FS, 0x65 GS) at the
/// start of a code slice, capped at [`MAX_INSTRUCTION_LEN`].
///
/// The count includes the first byte when it is a segment prefix, so a run of
/// more than [`MAX_INSTRUCTION_LEN`] prefixes is reported as
/// [`MAX_INSTRUCTION_LEN`].
pub fn segment_prefix_len(code: &[u8]) -> usize {
    let mut len = match code.first() {
        Some(&0x64 | &0x65) => 1,
        _ => 0,
    };
    while len < MAX_INSTRUCTION_LEN && matches!(code.get(len), Some(&0x64 | &0x65)) {
        len += 1;
    }
    len
}

/// One x86-64 instruction form from the generated registry tables.
#[derive(Clone, Copy, Debug)]
pub struct Form {
    pub op: Op,
    pub encoding: Encoding,
    pub size: OperandSize,
    pub immediate: Immediate,
    /// Source width in bytes when it differs from the operand size, else 0.
    pub source: u8,
    pub has_modrm: bool,
    pub memory_only: bool,
    pub register_only: bool,
    /// The only ModRM r/m field value the form accepts. Set only on
    /// register-only forms, so such a form matches exactly one ModRM byte.
    pub fixed_rm: Option<u8>,
    pub no_rex_b: bool,
    /// A 16-byte memory operand may have any alignment.
    pub unaligned: bool,
    /// The LOCK prefix is accepted when the ModRM operand is memory.
    pub lockable: bool,
}

/// Opcode table entry.
#[derive(Clone, Copy, Debug)]
pub enum Entry {
    Unsupported,
    Form(u16),
    Group(u16),
}

/// How a form names its operands, destination first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    None,
    /// A string instruction: source `[RSI]`, destination `[RDI]`, REP allowed.
    String,
    RmReg,
    RegRm,
    RegRmImm,
    Rm,
    RmUnread,
    RmImm,
    RmOne,
    RmCl,
    /// `rm` from `rm` and `reg`, with an 8-bit count (SHLD, SHRD).
    RmRegImm,
    /// `rm` from `rm` and `reg`, with the count in CL (SHLD, SHRD).
    RmRegCl,
    AccImm,
    OpReg,
    OpRegImm,
    Imm,
    Rel,
    /// XMM `reg` from XMM or memory.
    XmmRm,
    /// XMM or memory from XMM `reg`.
    RmXmm,
    XmmRmImm,
    /// XMM `rm` shifted in place by an immediate.
    RmImmXmm,
    /// XMM `reg` from a general register or memory.
    XmmGpr,
    /// General register or memory from XMM `reg`.
    GprXmm,
    XmmGprImm,
    /// General register or memory from a lane of XMM `reg`.
    GprXmmImm,
    /// General `reg` from XMM `rm`.
    RegXmm,
    RegXmmImm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperandSize {
    Byte,
    Variable,
    Wide,
    Quad,
    Xmm,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Immediate {
    None,
    SignedByte,
    SignedFull,
    Signed32,
    Full,
    Word,
    /// Zero-extended byte.
    Byte,
    /// A 64-bit absolute address, decoded into the memory operand (MOV
    /// `A0`-`A3`, Intel SDM Vol. 2A, 2.2.1.4).
    Offset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment {
    Fs,
    Gs,
}

/// A general or XMM register operand, as the encoding says. `high` selects AH,
/// CH, DH or BH of `index`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegOperand {
    pub index: u8,
    pub high: bool,
}

/// A memory operand. A RIP-relative operand has its absolute address folded
/// into `disp` and no base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemOperand {
    pub base: Option<u8>,
    pub index: Option<(u8, u8)>,
    pub disp: i64,
    pub segment: Option<Segment>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RmOperand {
    Reg(RegOperand),
    Mem(MemOperand),
}

/// A fully decoded instruction at a known address.
#[derive(Clone, Copy, Debug)]
pub struct Insn {
    pub op: Op,
    pub encoding: Encoding,
    /// Index into [`FORMS`].
    pub form: u16,
    /// Guest address of the first byte.
    pub rip: u64,
    pub len: u8,
    /// Operand size in bytes: 1, 2, 4, 8 or 16 (XMM), or 0 when there is none.
    pub size: u8,
    /// Width of the ModRM operand in bytes.
    pub rm_size: u8,
    pub rm: Option<RmOperand>,
    /// ModRM `reg` operand, or the register encoded in the opcode.
    pub reg: RegOperand,
    /// Sign-extended immediate, or the absolute branch target.
    pub imm: u64,
    /// Condition code for Jcc, CMOVcc and SETcc.
    pub condition: u8,
    /// Carries a LOCK prefix. Set only for a lockable form with a memory
    /// destination.
    pub lock: bool,
    /// A string instruction carrying a REP prefix.
    pub rep: bool,
}

impl Insn {
    /// Address of the following instruction.
    pub fn next_rip(&self) -> u64 {
        self.rip.wrapping_add(u64::from(self.len))
    }

    /// The memory operand, if any.
    pub fn memory(&self) -> Option<MemOperand> {
        match self.rm {
            Some(RmOperand::Mem(operand)) => Some(operand),
            _ => None,
        }
    }
}

/// Why bytes did not decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError<E> {
    /// The byte source refused a fetch.
    Fetch(E),
    /// No declared form matches; carries the byte that decided it.
    Unsupported(u8),
    /// The instruction would be longer than [`MAX_INSTRUCTION_LEN`] bytes.
    TooLong,
}

struct Cursor<F> {
    fetch: F,
    rip: u64,
    len: usize,
}

impl<E, F: FnMut(u64) -> Result<u8, E>> Cursor<F> {
    fn u8(&mut self) -> Result<u8, DecodeError<E>> {
        if self.len == MAX_INSTRUCTION_LEN {
            return Err(DecodeError::TooLong);
        }
        let byte =
            (self.fetch)(self.rip.wrapping_add(self.len as u64)).map_err(DecodeError::Fetch)?;
        self.len += 1;
        Ok(byte)
    }

    /// Little-endian value of `count` bytes.
    fn bytes(&mut self, count: usize) -> Result<u64, DecodeError<E>> {
        let mut value = 0;
        for index in 0..count {
            value |= u64::from(self.u8()?) << (8 * index);
        }
        Ok(value)
    }

    fn signed(&mut self, count: usize) -> Result<u64, DecodeError<E>> {
        let value = self.bytes(count)?;
        let shift = 64 - 8 * count as u32;
        Ok(((value << shift) as i64 >> shift) as u64)
    }
}

/// Whether `byte` is a legacy prefix: an operand-size, address-size, segment,
/// LOCK or repeat prefix (Intel SDM Vol. 2A, 2.1.1).
fn is_legacy_prefix(byte: u8) -> bool {
    matches!(
        byte,
        0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 | 0x66 | 0x67 | 0xF0 | 0xF2 | 0xF3
    )
}

/// Decode one instruction at `rip`. `fetch` supplies the byte at an address and
/// is called only for bytes that belong to the instruction, in order.
///
/// Forms the registry does not declare, LOCK anywhere but on a lockable form
/// with a memory destination, address-size overrides and an ignored segment
/// override after FS or GS are rejected rather than approximated.
pub fn decode<E>(
    rip: u64,
    fetch: impl FnMut(u64) -> Result<u8, E>,
) -> Result<Insn, DecodeError<E>> {
    let mut cursor = Cursor { fetch, rip, len: 0 };
    let mut operand_size = false;
    let mut rep = false;
    let mut repne = false;
    let mut lock = false;
    let mut segment = None;
    let mut rex = 0;
    let mut byte = cursor.u8()?;
    loop {
        let selected = match byte {
            // REX applies only immediately before the opcode. One that is
            // followed by a legacy prefix is ignored (Intel SDM Vol. 2A, 2.2.1).
            0x40..=0x4F => {
                let next = cursor.u8()?;
                if !is_legacy_prefix(next) {
                    rex = byte;
                    byte = next;
                    break;
                }
                byte = next;
                continue;
            }
            0x64 => Segment::Fs,
            0x65 => Segment::Gs,
            0x66 => {
                operand_size = true;
                byte = cursor.u8()?;
                continue;
            }
            0xF3 => {
                rep = true;
                byte = cursor.u8()?;
                continue;
            }
            0xF2 => {
                repne = true;
                byte = cursor.u8()?;
                continue;
            }
            0xF0 => {
                lock = true;
                byte = cursor.u8()?;
                continue;
            }
            // CS, DS, ES and SS overrides have no effect in 64-bit mode. After
            // FS or GS their meaning is not relied on, so it is rejected.
            0x26 | 0x2E | 0x36 | 0x3E if segment.is_none() => {
                byte = cursor.u8()?;
                continue;
            }
            _ => break,
        };
        // FS and GS together are architecturally undefined (Intel SDM Vol. 2,
        // "Instruction Prefixes"); the local rule is that GS applies.
        if segment != Some(Segment::Gs) {
            segment = Some(selected);
        }
        byte = cursor.u8()?;
    }

    let (rex_w, rex_r, rex_x, rex_b) =
        (rex & 8 != 0, (rex & 4) << 1, (rex & 2) << 2, (rex & 1) << 3);

    // F2 and F3 together have no defined meaning for any declared form.
    if rep && repne {
        return Err(DecodeError::Unsupported(0xF2));
    }
    let two_byte = byte == 0x0F;
    let mut opcode = if two_byte { cursor.u8()? } else { byte };
    // 0F 38 and 0F 3A are escapes to maps with a third opcode byte. Every
    // declared form there has 0x66 as its mandatory prefix, over which F2 and
    // F3 take precedence.
    let three_byte = match opcode {
        0x38 if two_byte => Some(&THREE_BYTE_38_66),
        0x3A if two_byte => Some(&THREE_BYTE_3A_66),
        _ => None,
    };
    if three_byte.is_some() {
        if !operand_size || rep || repne {
            return Err(DecodeError::Unsupported(opcode));
        }
        opcode = cursor.u8()?;
    }
    let slot = usize::from(opcode);
    let table = if let Some(table) = three_byte {
        operand_size = false;
        table
    } else if !two_byte {
        // On a one-byte opcode F3 is PAUSE (F3 90) or REP on a string form.
        let string = matches!(
            ONE_BYTE[slot],
            Entry::Form(index) if FORMS[usize::from(index)].encoding == Encoding::String
        );
        if repne || (rep && opcode != 0x90 && !string) {
            return Err(DecodeError::Unsupported(if rep { 0xF3 } else { 0xF2 }));
        }
        &ONE_BYTE
    } else if rep {
        &TWO_BYTE_F3
    } else if repne {
        &TWO_BYTE_F2
    } else if operand_size && !matches!(TWO_BYTE_66[slot], Entry::Unsupported) {
        // 0x66 selects the form instead of an operand size.
        operand_size = false;
        &TWO_BYTE_66
    } else {
        &TWO_BYTE
    };

    let entry = table[slot];
    let mut modrm = 0;
    let form_index = match entry {
        Entry::Unsupported => return Err(DecodeError::Unsupported(opcode)),
        Entry::Form(index) => index,
        Entry::Group(index) => {
            modrm = cursor.u8()?;
            GROUPS[usize::from(index)][usize::from((modrm >> 3) & 7)]
        }
    };
    let mut form = FORMS
        .get(usize::from(form_index))
        .ok_or(DecodeError::Unsupported(opcode))?;
    let mut form_index = form_index;
    if form.no_rex_b && rex_b != 0 {
        // 90 is NOP only without REX.B. With it the byte exchanges r8 with the
        // accumulator, as the first register of the 91..97 row would (Intel
        // SDM Vol. 2B, "NOP" and "XCHG").
        match ONE_BYTE[slot + 1] {
            Entry::Form(index)
                if !two_byte && !rep && FORMS[usize::from(index)].encoding == Encoding::OpReg =>
            {
                form_index = index;
                form = &FORMS[usize::from(index)];
            }
            _ => return Err(DecodeError::Unsupported(opcode)),
        }
    }
    if form.has_modrm && matches!(entry, Entry::Form(_)) {
        modrm = cursor.u8()?;
    }

    let size = match form.size {
        OperandSize::None => 0,
        OperandSize::Byte => 1,
        OperandSize::Quad => 8,
        OperandSize::Xmm => 16,
        OperandSize::Wide | OperandSize::Variable if rex_w => 8,
        OperandSize::Variable if operand_size => 2,
        OperandSize::Wide | OperandSize::Variable => 4,
    };
    let takes_operand_size =
        matches!(form.size, OperandSize::Byte | OperandSize::Variable) || form.op == Op::Nop;
    // REX.W takes precedence over 0x66 wherever the size is selectable.
    let overridden = rex_w && form.size == OperandSize::Wide;
    if operand_size && !takes_operand_size && !overridden {
        return Err(DecodeError::Unsupported(0x66));
    }
    let rm_size = if form.source != 0 { form.source } else { size };
    let register = |field: u8, extension: u8, width: u8| {
        if width == 1 && rex == 0 && field >= 4 {
            RegOperand {
                index: field - 4,
                high: true,
            }
        } else {
            RegOperand {
                index: field | extension,
                high: false,
            }
        }
    };

    let mut rm = None;
    let mut rip_relative = false;
    let mut reg = RegOperand {
        index: 0,
        high: false,
    };
    if form.has_modrm {
        reg = register((modrm >> 3) & 7, rex_r, size);
        let mode = modrm >> 6;
        if mode == 3 {
            if form.memory_only || form.fixed_rm.is_some_and(|field| field != modrm & 7) {
                return Err(DecodeError::Unsupported(opcode));
            }
            // A lane moves through a 32-bit or wider register, never AH to BH.
            let lane = matches!(form.encoding, Encoding::XmmGprImm | Encoding::GprXmmImm);
            let width = if lane { 4 } else { rm_size };
            rm = Some(RmOperand::Reg(register(modrm & 7, rex_b, width)));
        } else {
            if form.register_only {
                return Err(DecodeError::Unsupported(opcode));
            }
            let mut operand = MemOperand {
                base: Some((modrm & 7) | rex_b),
                index: None,
                disp: 0,
                segment,
            };
            let mut low_base = modrm & 7;
            if low_base == 4 {
                let sib = cursor.u8()?;
                low_base = sib & 7;
                operand.base = Some(low_base | rex_b);
                // Index 100b means no index unless REX.X selects R12.
                let index = ((sib >> 3) & 7) | rex_x;
                operand.index = (index != 4).then_some((index, sib >> 6));
            } else {
                rip_relative = mode == 0 && low_base == 5;
            }
            if mode == 0 && low_base == 5 {
                operand.base = None;
                operand.disp = cursor.signed(4)? as i64;
            } else if mode != 0 {
                operand.disp = cursor.signed(if mode == 1 { 1 } else { 4 })? as i64;
            }
            rm = Some(RmOperand::Mem(operand));
        }
    } else if form.immediate == Immediate::Offset {
        // The accumulator and the memory at an absolute address. With the
        // default 64-bit address size the offset is eight bytes; FS or GS may
        // override the segment as for any memory operand.
        rm = Some(RmOperand::Mem(MemOperand {
            base: None,
            index: None,
            disp: cursor.bytes(8)? as i64,
            segment,
        }));
    } else if matches!(form.encoding, Encoding::OpReg | Encoding::OpRegImm) {
        reg = register(opcode & 7, rex_b, size);
    } else if form.encoding == Encoding::String {
        // The source is DS:[RSI], which FS or GS may override. The destination
        // ES:[RDI] takes no override.
        rm = Some(RmOperand::Mem(MemOperand {
            base: Some(6),
            index: None,
            disp: 0,
            segment,
        }));
    }
    // LOCK is an invalid opcode unless the instruction is lockable and its
    // destination is memory (Intel SDM Vol. 2, "LOCK").
    if lock && !(form.lockable && matches!(rm, Some(RmOperand::Mem(_)))) {
        return Err(DecodeError::Unsupported(0xF0));
    }

    let mut imm = match form.immediate {
        Immediate::None | Immediate::Offset => 0,
        Immediate::SignedByte => cursor.signed(1)?,
        Immediate::SignedFull => cursor.signed(usize::from(size.min(4)))?,
        Immediate::Signed32 => cursor.signed(4)?,
        Immediate::Full => cursor.bytes(usize::from(size))?,
        Immediate::Word => cursor.bytes(2)?,
        Immediate::Byte => cursor.bytes(1)?,
    };

    let end = rip.wrapping_add(cursor.len as u64);
    if rip_relative && let Some(RmOperand::Mem(operand)) = &mut rm {
        operand.disp = end.wrapping_add_signed(operand.disp) as i64;
    }
    if form.encoding == Encoding::Rel {
        imm = end.wrapping_add(imm);
    }
    if form.encoding == Encoding::AccImm {
        reg = RegOperand {
            index: 0,
            high: false,
        };
    }
    Ok(Insn {
        op: form.op,
        encoding: form.encoding,
        form: form_index,
        rip,
        len: cursor.len as u8,
        size,
        rm_size,
        rm,
        reg,
        imm,
        condition: opcode & 0xF,
        lock,
        rep: rep && form.encoding == Encoding::String,
    })
}

#[cfg(test)]
mod tests {
    use super::{MAX_INSTRUCTION_LEN, segment_prefix_len};

    #[test]
    fn segment_prefix_len_counts_legacy_segment_prefixes() {
        assert_eq!(segment_prefix_len(&[]), 0);
        assert_eq!(segment_prefix_len(&[0x90]), 0);
        assert_eq!(segment_prefix_len(&[0x65, 0x90]), 1);
        assert_eq!(segment_prefix_len(&[0x64, 0x65, 0x31, 0xc0]), 2);
        assert_eq!(segment_prefix_len(&[0x65; 20]), MAX_INSTRUCTION_LEN);
        assert_eq!(segment_prefix_len(&[0x65; 15]), MAX_INSTRUCTION_LEN);
    }
}
