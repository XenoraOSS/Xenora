// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared x86-64 decoder.
//!
//! The static PE probe, the software interpreter and the native DBT all decode
//! through [`decode`], driven by static tables generated from
//! `x86/instructions.toml`, so instruction boundaries, operands and rejected
//! forms agree across them.

use crate::x86_table::{
    FORMS, GROUPS, ONE_BYTE, Op, THREE_BYTE_3A_66, THREE_BYTE_38_66, THREE_BYTE_38_F2, TWO_BYTE,
    TWO_BYTE_66, TWO_BYTE_F2, TWO_BYTE_F3, VEX_FORMS,
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

/// The rule of a VEX-encoded form, beside its [`Form`]. `None` accepts either.
#[derive(Clone, Copy, Debug)]
pub struct VexForm {
    /// Opcode map 1-3, then mandatory prefix 0-3, then opcode.
    pub key: u16,
    /// Index into [`FORMS`].
    pub form: u16,
    pub digit: Option<u8>,
    /// VEX.L: 256-bit vectors.
    pub length: Option<bool>,
    /// VEX.W.
    pub wide: Option<bool>,
    /// Memory width in bytes: 0 is the vector length, 255 the general operand.
    pub memory: u8,
    /// VEX.vvvv names a register. Otherwise it must be all ones.
    pub nds: bool,
}

/// The operands only a VEX-encoded instruction has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vex {
    /// Vector length in bytes: 16 or 32.
    pub length: u8,
    /// Register named by VEX.vvvv: a further source, or the destination of a shift
    /// by an immediate.
    pub source: Option<u8>,
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
    /// XMM or memory from XMM `reg`, with an immediate.
    RmXmmImm,
    /// XMM `reg` with XMM or memory, and lengths in RAX and RDX of the operand
    /// size.
    XmmRmLength,
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
    /// A string instruction carrying a repeat prefix: REP or REPE (F3), or
    /// REPNE (F2).
    pub rep: bool,
    /// The repeat prefix is REPNE (F2), which only CMPS and SCAS accept.
    /// On those two, a repeat without it is REPE.
    pub repne: bool,
    /// Set for a VEX-encoded instruction, which shares `op` with a legacy form.
    pub vex: Option<Vex>,
}

impl Insn {
    /// Address of the following instruction.
    pub fn next_rip(&self) -> u64 {
        self.rip.wrapping_add(u64::from(self.len))
    }

    /// Mnemonic stem. A VEX-encoded form of a legacy operation has a `v` prefix.
    pub fn name(&self) -> std::borrow::Cow<'static, str> {
        let name = self.op.name();
        if self.vex.is_some() && !name.starts_with('v') {
            format!("v{name}").into()
        } else {
            name.into()
        }
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

/// The memory operand of a ModRM byte whose mode is not 3, and whether it is
/// RIP-relative. The extension bits are 0 or 8.
fn memory_operand<E, F: FnMut(u64) -> Result<u8, E>>(
    cursor: &mut Cursor<F>,
    modrm: u8,
    rex_x: u8,
    rex_b: u8,
    segment: Option<Segment>,
) -> Result<(MemOperand, bool), DecodeError<E>> {
    let mode = modrm >> 6;
    let mut rip_relative = false;
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
    Ok((operand, rip_relative))
}

/// Decode a VEX-encoded instruction after its first prefix byte (Intel SDM Vol. 2A,
/// 2.3). A form the registry does not declare, a set bit that must be clear and
/// VEX.vvvv on a form without that operand are rejected.
fn decode_vex<E, F: FnMut(u64) -> Result<u8, E>>(
    mut cursor: Cursor<F>,
    first: u8,
    segment: Option<Segment>,
) -> Result<Insn, DecodeError<E>> {
    let rip = cursor.rip;
    // The register extension bits and VEX.vvvv are stored inverted.
    let second = cursor.u8()?;
    let extension = |bit: u8| (!second >> bit & 1) << 3;
    let rex_r = extension(7);
    let (map, rex_x, rex_b, wide, last) = if first == 0xC5 {
        (1, 0, 0, false, second)
    } else {
        let last = cursor.u8()?;
        (
            second & 0x1F,
            extension(6),
            extension(5),
            last & 0x80 != 0,
            last,
        )
    };
    if !(1..=3).contains(&map) {
        return Err(DecodeError::Unsupported(first));
    }
    let source = !last >> 3 & 15;
    let long = last & 4 != 0;
    let opcode = cursor.u8()?;
    let unsupported = DecodeError::Unsupported(opcode);
    let key = u16::from(map) << 10 | u16::from(last & 3) << 8 | u16::from(opcode);
    let start = VEX_FORMS.partition_point(|form| form.key < key);
    let candidates = || {
        VEX_FORMS[start..]
            .iter()
            .take_while(|form| form.key == key)
            .filter(|form| {
                form.length.is_none_or(|length| length == long)
                    && form.wide.is_none_or(|accepted| accepted == wide)
            })
    };
    // Every form of one opcode has a ModRM byte, or none has.
    let Some(any) = candidates().next() else {
        return Err(unsupported);
    };
    let modrm = if FORMS[usize::from(any.form)].has_modrm {
        Some(cursor.u8()?)
    } else {
        None
    };
    let found = candidates().find(|candidate| {
        let form = &FORMS[usize::from(candidate.form)];
        modrm.is_none_or(|modrm| {
            let register = modrm >> 6 == 3;
            candidate.digit.is_none_or(|digit| digit == modrm >> 3 & 7)
                && !(form.memory_only && register)
                && !(form.register_only && !register)
        })
    });
    let Some(rule) = found else {
        return Err(unsupported);
    };
    if !rule.nds && source != 0 {
        return Err(unsupported);
    }
    let form = &FORMS[usize::from(rule.form)];
    let length = if long { 32 } else { 16 };
    let size = match form.size {
        OperandSize::Xmm => length,
        OperandSize::None => 0,
        _ if wide => 8,
        _ => 4,
    };
    let rm_size = match rule.memory {
        0 => length,
        u8::MAX => size,
        bytes => bytes,
    };
    let mut reg = RegOperand {
        index: 0,
        high: false,
    };
    let mut rm = None;
    let mut rip_relative = false;
    if let Some(modrm) = modrm {
        reg.index = (modrm >> 3 & 7) | rex_r;
        if modrm >> 6 == 3 {
            rm = Some(RmOperand::Reg(RegOperand {
                index: (modrm & 7) | rex_b,
                high: false,
            }));
        } else {
            let operand;
            (operand, rip_relative) = memory_operand(&mut cursor, modrm, rex_x, rex_b, segment)?;
            rm = Some(RmOperand::Mem(operand));
        }
    }
    let imm = match form.immediate {
        Immediate::Byte => cursor.bytes(1)?,
        _ => 0,
    };
    if rip_relative && let Some(RmOperand::Mem(operand)) = &mut rm {
        let end = rip.wrapping_add(cursor.len as u64);
        operand.disp = end.wrapping_add_signed(operand.disp) as i64;
    }
    Ok(Insn {
        op: form.op,
        encoding: form.encoding,
        form: rule.form,
        rip,
        len: cursor.len as u8,
        size,
        rm_size,
        rm,
        reg,
        imm,
        condition: opcode & 0xF,
        lock: false,
        rep: false,
        repne: false,
        vex: Some(Vex {
            length,
            source: rule.nds.then_some(source),
        }),
    })
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
    let mut any_rex = false;
    let mut byte = cursor.u8()?;
    loop {
        let selected = match byte {
            // REX applies only immediately before the opcode. One that is
            // followed by a legacy prefix is ignored (Intel SDM Vol. 2A, 2.2.1).
            0x40..=0x4F => {
                any_rex = true;
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

    // C4 and C5 are always VEX prefixes in 64-bit mode. After 66, F2, F3, LOCK or
    // REX they are an invalid opcode (Intel SDM Vol. 2A, 2.3.2 to 2.3.4).
    if matches!(byte, 0xC4 | 0xC5) {
        if operand_size || rep || repne || lock || any_rex {
            return Err(DecodeError::Unsupported(byte));
        }
        return decode_vex(cursor, byte, segment);
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
    // declared form there has 0x66 or, in the first map, F2 as its mandatory
    // prefix. F2 and F3 take precedence over 0x66, which then is an operand size.
    let three_byte = if two_byte && matches!(opcode, 0x38 | 0x3A) {
        let table = match (opcode, repne) {
            (0x38, true) => &THREE_BYTE_38_F2,
            (0x38, false) if operand_size && !rep => &THREE_BYTE_38_66,
            (0x3A, false) if operand_size && !rep => &THREE_BYTE_3A_66,
            _ => return Err(DecodeError::Unsupported(opcode)),
        };
        operand_size &= repne;
        opcode = cursor.u8()?;
        Some(table)
    } else {
        None
    };
    let slot = usize::from(opcode);
    let table = if let Some(table) = three_byte {
        table
    } else if !two_byte {
        // On a one-byte opcode F3 is PAUSE (F3 90) or REP on a string form.
        // F2 is REPNE, which only the string compares CMPS and SCAS take
        // (Intel SDM Vol. 2B, "REP/REPE/REPZ/REPNE/REPNZ").
        let (string, compares) = match ONE_BYTE[slot] {
            Entry::Form(index) => {
                let form = &FORMS[usize::from(index)];
                (
                    form.encoding == Encoding::String,
                    matches!(form.op, Op::Cmps | Op::Scas),
                )
            }
            _ => (false, false),
        };
        if (repne && !compares) || (rep && opcode != 0x90 && !string) {
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
            let operand;
            (operand, rip_relative) = memory_operand(&mut cursor, modrm, rex_x, rex_b, segment)?;
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
        rep: (rep || repne) && form.encoding == Encoding::String,
        repne: repne && form.encoding == Encoding::String,
        vex: None,
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
