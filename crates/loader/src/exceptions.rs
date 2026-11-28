// SPDX-License-Identifier: GPL-3.0-or-later

//! Windows x64 structured exception dispatch for guest programs (#447).
//!
//! The guest's own language runtime finds the handlers and decides what to
//! do: a statically linked mingw-w64 C++ program carries GCC's unwinder and
//! personality as guest code. This module is the operating-system side that
//! runtime calls: function lookup over the image's exception directory,
//! virtual unwinding, context capture, `RaiseException` with a search phase
//! and `RtlUnwindEx` with an unwind phase. Guest CPU faults enter the same
//! search ([`faults`], #453), and `__C_specific_handler` runs MSVC-style C
//! scope tables ([`scope`]).
//!
//! The layouts and constants come from Microsoft's public x64
//! exception-handling pages and were checked against the mingw-w64 14.0.0
//! headers; docs/research/exception-handling.md lists the sources and the
//! behaviour those pages leave unknown. Nothing here was compared with Windows
//! or with an Xbox console.

use crate::cpu::{CpuState, ExitReason, Register};
use crate::imports::{CALLBACK_RETURN, ENTRY_RETURN, ImportRegistry, ServiceStatus, Win64Args};
use crate::memory::{GuestMemory, MemoryError};
use crate::pe::{MappedImageBacking, ParsedPe};

mod faults;
mod scope;

pub(crate) use faults::dispatch_fault;
pub(crate) use scope::{SCOPE_RETURN, ScopeWalk, scope_resume};

const KERNEL32: &str = "kernel32.dll";

/// Return address under a language handler the dispatcher calls. Reaching it
/// hands the handler's disposition back to [`resume`].
pub(crate) const EXCEPTION_RETURN: u64 = ENTRY_RETURN - crate::imports::IMPORT_TRAMPOLINE_STRIDE;
const _: () = assert!(EXCEPTION_RETURN & !0xFFF == crate::imports::RETURN_SENTINEL_PAGE);

/// Return address under the unhandled-exception filter the dispatcher calls
/// at the end of a search. Reaching it hands the filter's result to
/// [`filter_resume`].
pub(crate) const FILTER_RETURN: u64 = SCOPE_RETURN - crate::imports::IMPORT_TRAMPOLINE_STRIDE;
const _: () = assert!(FILTER_RETURN & !0xFFF == crate::imports::RETURN_SENTINEL_PAGE);

/// `UnhandledExceptionFilter` results (`SetUnhandledExceptionFilter`).
const FILTER_CONTINUE_EXECUTION: i32 = -1;
const FILTER_CONTINUE_SEARCH: i32 = 0;
const FILTER_EXECUTE_HANDLER: i32 = 1;

const UNW_FLAG_EHANDLER: u8 = 0x1;
const UNW_FLAG_UHANDLER: u8 = 0x2;
const UNW_FLAG_CHAININFO: u8 = 0x4;

const EXCEPTION_NONCONTINUABLE: u32 = 0x1;
const EXCEPTION_UNWINDING: u32 = 0x2;
const EXCEPTION_EXIT_UNWIND: u32 = 0x4;
const EXCEPTION_TARGET_UNWIND: u32 = 0x20;
const EXCEPTION_COLLIDED_UNWIND: u32 = 0x40;
const PHASE_FLAGS: u32 = EXCEPTION_UNWINDING
    | EXCEPTION_EXIT_UNWIND
    | EXCEPTION_TARGET_UNWIND
    | EXCEPTION_COLLIDED_UNWIND;
const STATUS_UNWIND: u32 = 0xC000_0027;
const EXCEPTION_MAXIMUM_PARAMETERS: u64 = 15;

const DISPOSITION_CONTINUE_EXECUTION: u32 = 0;
const DISPOSITION_CONTINUE_SEARCH: u32 = 1;

// Layouts, checked with offsetof against the mingw-w64 headers.
const CONTEXT_SIZE: u64 = 0x4D0;
const CONTEXT_FULL: u32 = 0x0010_000B;
const DISPATCHER_SIZE: u64 = 0x50;
const RECORD_SIZE: u64 = 0x98;
/// Guest stack the dispatcher uses below its caller: the CONTEXT of the frame
/// being looked at, a DISPATCHER_CONTEXT, the EXCEPTION_RECORD and, 16-byte
/// aligned, the CONTEXT of the raise or fault itself.
const SCRATCH: u64 = EXCEPTION_CONTEXT_AT + CONTEXT_SIZE;
const DISPATCHER_AT: u64 = CONTEXT_SIZE;
const RECORD_AT: u64 = CONTEXT_SIZE + DISPATCHER_SIZE;
const EXCEPTION_CONTEXT_AT: u64 = (RECORD_AT + RECORD_SIZE + 15) & !15;

/// Frames one dispatch may walk, and chained unwind entries one frame may have.
const MAX_FRAMES: u32 = 4096;
const MAX_CHAIN: u32 = 32;

/// One image's function table: the image span and its `RUNTIME_FUNCTION` array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FunctionTable {
    base: u64,
    size: u64,
    table: u64,
    count: u64,
}

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

/// Refuse a guest structure at `address` whose `size` bytes would wrap the
/// address space, so field offsets inside it cannot overflow.
fn span(address: u64, size: u64) -> Result<(), ExitReason> {
    address
        .checked_add(size)
        .map(|_| ())
        .ok_or(fault(MemoryError::Overflow))
}

/// The most an `UNWIND_INFO` can span: its header, 256 code slots and a
/// trailing handler RVA or chained 12-byte entry.
const UNWIND_INFO_SPAN: u64 = 4 + 2 * 256 + 12;

/// Fixed bytes of one unwind-info chain link: header, whole aligned unwind
/// codes, and the trailing handler RVA or chained RUNTIME_FUNCTION entry.
/// Language-specific handler data is variable-sized and intentionally excluded.
fn fixed_unwind_span(header: u32) -> u64 {
    let bytes = header.to_le_bytes();
    let count = u64::from(bytes[2]);
    let codes = (count + 1) & !1;
    let flags = bytes[0] >> 3;
    let trailing = if flags & UNW_FLAG_CHAININFO != 0 {
        12
    } else if flags & (UNW_FLAG_EHANDLER | UNW_FLAG_UHANDLER) != 0 {
        4
    } else {
        0
    };
    4 + 2 * codes + trailing
}

fn at(base: u64, offset: u64) -> Result<u64, ExitReason> {
    base.checked_add(offset).ok_or(fault(MemoryError::Overflow))
}

/// Read and check data directory 3 of a mapped image and record its function
/// table. A malformed table is refused here rather than missed at run time.
pub(crate) fn register_exception_directory(
    bytes: &[u8],
    base: u64,
    size_of_image: u32,
    memory: &mut GuestMemory,
) -> Result<u64, String> {
    register_exception_directory_impl(bytes, base, size_of_image, memory, None)
}

/// Read and check data directory 3 using the parsed image's committed header
/// and section backing, then record its function table.
pub(crate) fn register_exception_directory_for_image(
    pe: &ParsedPe,
    bytes: &[u8],
    base: u64,
    memory: &mut GuestMemory,
) -> Result<u64, String> {
    let backing = MappedImageBacking::new(pe);
    register_exception_directory_impl(bytes, base, pe.size_of_image, memory, Some(&backing))
}

fn register_exception_directory_impl(
    bytes: &[u8],
    base: u64,
    size_of_image: u32,
    memory: &mut GuestMemory,
    backing: Option<&MappedImageBacking>,
) -> Result<u64, String> {
    let u32_at = |offset: usize| -> Option<u32> {
        Some(u32::from_le_bytes(
            bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
        ))
    };
    let Some(lfanew) = u32_at(0x3C) else {
        return Ok(0);
    };
    let coff = lfanew as usize + 4;
    let optional = coff + 20;
    let optional_len = bytes
        .get(coff + 16..coff + 18)
        .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
    // PE32+: NumberOfRvaAndSizes at 108, the exception directory at 136.
    if optional_len < 144 || u32_at(optional + 108).unwrap_or(0) < 4 {
        return Ok(0);
    }
    let (Some(rva), Some(size)) = (u32_at(optional + 136), u32_at(optional + 140)) else {
        return Ok(0);
    };
    if rva == 0 && size == 0 {
        return Ok(0);
    }
    let bad = |why: &str| Err(format!("malformed exception directory: {why}"));
    if size == 0 || !size.is_multiple_of(12) || !rva.is_multiple_of(4) {
        return bad("its size is not a whole number of 12-byte entries, or it is misaligned");
    }
    if u64::from(rva) + u64::from(size) > u64::from(size_of_image) {
        return bad("it runs past the end of the image");
    }
    if backing.is_some_and(|backing| !backing.covers(rva, u64::from(size))) {
        return bad("its entries are not continuously backed by image memory");
    }
    let table = base + u64::from(rva);
    let count = u64::from(size / 12);
    let mut previous_end = 0;
    for index in 0..count {
        let read = |field: u64| {
            memory
                .read_u32(table + index * 12 + field)
                .map_err(|e| format!("malformed exception directory: entry {index}: {e}"))
        };
        let (begin, end, unwind) = (read(0)?, read(4)?, read(8)?);
        if begin >= end || end > size_of_image {
            return bad(&format!(
                "entry {index} has an empty range or one outside the image"
            ));
        }
        if begin < previous_end {
            return bad(&format!(
                "entry {index} is not sorted after the one before it"
            ));
        }
        if backing.is_some_and(|backing| !backing.covers(begin, u64::from(end - begin))) {
            return bad(&format!(
                "entry {index} has a function range outside committed image memory"
            ));
        }
        let unwind_address = base.checked_add(u64::from(unwind)).ok_or_else(|| {
            format!("malformed exception directory: entry {index}: unwind address overflows")
        })?;
        let unwind_header = memory.read_u32(unwind_address).map_err(|e| {
            format!("malformed exception directory: entry {index}: unwind header: {e}")
        })?;
        let fixed_span = fixed_unwind_span(unwind_header);
        if !unwind.is_multiple_of(4) || u64::from(unwind) + fixed_span > u64::from(size_of_image) {
            return bad(&format!("entry {index} has unwind data outside the image"));
        }
        if backing.is_some_and(|backing| !backing.covers(unwind, fixed_span)) {
            return bad(&format!(
                "entry {index} has unwind data outside committed image memory"
            ));
        }
        previous_end = end;
    }
    memory.function_tables.push(FunctionTable {
        base,
        size: u64::from(size_of_image),
        table,
        count,
    });
    Ok(count)
}

/// `RtlLookupFunctionEntry`: the image base and the guest address of the
/// entry covering `pc`, or `None` for leaf code and addresses in no image.
pub(crate) fn lookup(memory: &GuestMemory, pc: u64) -> Result<Option<(u64, u64)>, ExitReason> {
    let Some(table) = memory
        .function_tables
        .iter()
        .find(|t| pc >= t.base && pc - t.base < t.size)
    else {
        return Ok(None);
    };
    let rva = pc - table.base;
    let (mut low, mut high) = (0, table.count);
    while low < high {
        let middle = (low + high) / 2;
        let entry = table.table + middle * 12;
        let begin = u64::from(memory.read_u32(entry).map_err(fault)?);
        let end = u64::from(memory.read_u32(entry + 4).map_err(fault)?);
        if rva < begin {
            high = middle;
        } else if rva >= end {
            low = middle + 1;
        } else {
            return Ok(Some((table.base, entry)));
        }
    }
    Ok(None)
}

fn in_image(memory: &GuestMemory, pc: u64) -> bool {
    memory
        .function_tables
        .iter()
        .any(|t| pc >= t.base && pc - t.base < t.size)
}

/// What unwinding one frame found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    /// The language handler and its data, when the frame has one of the
    /// requested type and the address is in the body.
    pub handler: Option<(u64, u64)>,
    pub establisher: u64,
}

fn read_u64(memory: &GuestMemory, address: u64) -> Result<u64, ExitReason> {
    memory.read_u64(address).map_err(fault)
}

fn pop(memory: &GuestMemory, ctx: &mut CpuState) -> Result<u64, ExitReason> {
    let rsp = ctx.get(Register::Rsp);
    let value = read_u64(memory, rsp)?;
    ctx.set(Register::Rsp, at(rsp, 8)?);
    Ok(value)
}

/// `RtlVirtualUnwind` for one frame: undo its prolog in `ctx` and pop the
/// return address. Unknown unwind versions and operations are refused.
pub(crate) fn virtual_unwind(
    memory: &GuestMemory,
    base: u64,
    entry: u64,
    ctx: &mut CpuState,
    handler_type: u8,
) -> Result<Frame, ExitReason> {
    let pc = ctx.rip;
    span(entry, 12)?;
    let begin = at(base, u64::from(memory.read_u32(entry).map_err(fault)?))?;
    let end = at(base, u64::from(memory.read_u32(entry + 4).map_err(fault)?))?;
    let info = at(base, u64::from(memory.read_u32(entry + 8).map_err(fault)?))?;
    span(info, UNWIND_INFO_SPAN)?;
    let offset = pc.checked_sub(begin).ok_or(ExitReason::InvalidUnwind)?;
    let header = memory.read_u32(info).map_err(fault)?.to_le_bytes();
    let (flags, prolog) = (header[0] >> 3, u64::from(header[1]));
    let (frame_register, frame_offset) = (header[3] & 0xF, u64::from(header[3] >> 4) * 16);
    let in_prolog = offset < prolog;

    let establisher = if frame_register != 0 && fp_in_effect(memory, info, offset, in_prolog)? {
        ctx.gpr[frame_register as usize]
            .checked_sub(frame_offset)
            .ok_or(ExitReason::InvalidUnwind)?
    } else {
        ctx.get(Register::Rsp)
    };

    if !in_prolog && unwind_epilog(memory, begin, end, frame_register, ctx)? {
        return Ok(Frame {
            handler: None,
            establisher,
        });
    }

    let primary = info;
    let mut info = info;
    let mut skip_after = in_prolog.then_some(offset);
    let mut machine_frame = false;
    for depth in 0.. {
        if depth > MAX_CHAIN {
            return Err(ExitReason::InvalidUnwind);
        }
        let header = memory.read_u32(info).map_err(fault)?.to_le_bytes();
        if header[0] & 0x7 != 1 {
            return Err(ExitReason::InvalidUnwind);
        }
        let count = u64::from(header[2]);
        machine_frame |= apply_codes(memory, info, count, skip_after, establisher, ctx)?;
        if (header[0] >> 3) & UNW_FLAG_CHAININFO == 0 {
            break;
        }
        // The chained entry is the function's primary entry; its prolog ran in full.
        let chained = info + 4 + 2 * ((count + 1) & !1);
        info = at(
            base,
            u64::from(memory.read_u32(chained + 8).map_err(fault)?),
        )?;
        span(info, UNWIND_INFO_SPAN)?;
        skip_after = None;
    }

    let handler =
        if !in_prolog && flags & handler_type & (UNW_FLAG_EHANDLER | UNW_FLAG_UHANDLER) != 0 {
            let slot = primary + 4 + 2 * ((u64::from(header[2]) + 1) & !1);
            let rva = u64::from(memory.read_u32(slot).map_err(fault)?);
            Some((at(base, rva)?, slot + 4))
        } else {
            None
        };
    if !machine_frame {
        ctx.rip = pop(memory, ctx)?;
    }
    Ok(Frame {
        handler,
        establisher,
    })
}

/// Slots each unwind operation uses, or `None` for an operation the public
/// pages do not describe.
fn slots(op: u8, op_info: u8) -> Option<u64> {
    Some(match op {
        0 | 2 | 3 | 10 => 1,
        1 if op_info == 0 => 2,
        1 if op_info == 1 => 3,
        4 | 8 => 2,
        5 | 9 => 3,
        _ => return None,
    })
}

fn code(memory: &GuestMemory, info: u64, index: u64) -> Result<(u8, u8, u8), ExitReason> {
    let raw = memory.read_u16(info + 4 + 2 * index).map_err(fault)?;
    Ok((raw as u8, (raw >> 8) as u8 & 0xF, (raw >> 12) as u8))
}

fn slot(memory: &GuestMemory, info: u64, index: u64) -> Result<u64, ExitReason> {
    Ok(u64::from(
        memory.read_u16(info + 4 + 2 * index).map_err(fault)?,
    ))
}

/// Whether the frame register has been set at `offset`.
fn fp_in_effect(
    memory: &GuestMemory,
    info: u64,
    offset: u64,
    in_prolog: bool,
) -> Result<bool, ExitReason> {
    if !in_prolog {
        return Ok(true);
    }
    let count = u64::from(memory.read_u8(info + 2).map_err(fault)?);
    let mut index = 0;
    while index < count {
        let (code_offset, op, op_info) = code(memory, info, index)?;
        if op == 3 {
            return Ok(u64::from(code_offset) <= offset);
        }
        index += slots(op, op_info).ok_or(ExitReason::InvalidUnwind)?;
    }
    Ok(false)
}

/// Undo the unwind codes of one `UNWIND_INFO`. Codes whose instruction has not
/// run yet (offset beyond `skip_after`) are skipped. Returns whether a machine
/// frame was popped.
fn apply_codes(
    memory: &GuestMemory,
    info: u64,
    count: u64,
    skip_after: Option<u64>,
    establisher: u64,
    ctx: &mut CpuState,
) -> Result<bool, ExitReason> {
    let mut index = 0;
    let mut machine_frame = false;
    while index < count {
        let (code_offset, op, op_info) = code(memory, info, index)?;
        let used = slots(op, op_info).ok_or(ExitReason::InvalidUnwind)?;
        if index + used > count {
            return Err(ExitReason::InvalidUnwind);
        }
        if skip_after.is_some_and(|offset| u64::from(code_offset) > offset) {
            index += used;
            continue;
        }
        let rsp = ctx.get(Register::Rsp);
        let register = op_info as usize;
        let far = || -> Result<u64, ExitReason> {
            Ok(slot(memory, info, index + 1)? | slot(memory, info, index + 2)? << 16)
        };
        match op {
            0 => ctx.gpr[register] = pop(memory, ctx)?,
            1 => {
                let size = if op_info == 0 {
                    slot(memory, info, index + 1)? * 8
                } else {
                    far()?
                };
                ctx.set(Register::Rsp, at(rsp, size)?);
            }
            2 => ctx.set(Register::Rsp, at(rsp, u64::from(op_info) * 8 + 8)?),
            3 => ctx.set(Register::Rsp, establisher),
            4 => {
                ctx.gpr[register] =
                    read_u64(memory, at(establisher, slot(memory, info, index + 1)? * 8)?)?
            }
            5 => ctx.gpr[register] = read_u64(memory, at(establisher, far()?)?)?,
            8 | 9 => {
                let offset = if op == 8 {
                    slot(memory, info, index + 1)? * 16
                } else {
                    far()?
                };
                let address = at(establisher, offset)?;
                let low = read_u64(memory, address)?;
                let high = read_u64(memory, at(address, 8)?)?;
                ctx.xmm[register] = u128::from(low) | u128::from(high) << 64;
            }
            10 => {
                let frame = at(rsp, if op_info == 1 { 8 } else { 0 })?;
                ctx.rip = read_u64(memory, frame)?;
                ctx.set(Register::Rsp, read_u64(memory, at(frame, 24)?)?);
                machine_frame = true;
            }
            _ => return Err(ExitReason::InvalidUnwind),
        }
        index += used;
    }
    Ok(machine_frame)
}

/// When `ctx.rip` starts or is inside an epilog of one of the forms the
/// `RtlVirtualUnwind` page lists, finish it in `ctx` and return true. The
/// epilog is: at most one `add rsp, imm` or `lea rsp, [frame + disp]`, then
/// pops, then `ret`, `rep ret`, `ret imm16`, a `jmp` leaving the function,
/// `jmp qword [rip + disp32]` or `rex.w jmp reg`. Anything else is body code.
fn unwind_epilog(
    memory: &GuestMemory,
    begin: u64,
    end: u64,
    frame_register: u8,
    ctx: &mut CpuState,
) -> Result<bool, ExitReason> {
    let byte = |address: u64| memory.fetch_instruction_byte(address).map_err(fault);
    // An epilog is at most 64 bytes: one adjustment, 16 pops and a jump.
    if ctx.rip.checked_add(64).is_none() {
        return Ok(false);
    }
    let mut pc = ctx.rip;
    let mut trial = ctx.clone();
    let rsp = trial.get(Register::Rsp);
    // add rsp, imm8 / imm32
    if byte(pc)? == 0x48 && matches!(byte(pc + 1)?, 0x83 | 0x81) && byte(pc + 2)? == 0xC4 {
        let (size, len) = if byte(pc + 1)? == 0x83 {
            (byte(pc + 3)? as i8 as i64 as u64, 4)
        } else {
            (u64::from(memory_u32(memory, pc + 3)?), 7)
        };
        trial.set(Register::Rsp, rsp.wrapping_add(size));
        pc += len;
    } else if matches!(byte(pc)?, 0x48 | 0x49) && byte(pc + 1)? == 0x8D {
        // lea rsp, [frame register + disp8/disp32]. An r/m field of 100 is
        // followed by a SIB byte. Under REX.B, SIB 0x24 (no index, base 100)
        // is R12, which can be a frame register. Any other SIB byte adds an
        // index or names another base. Without REX.B the base is RSP, and
        // Microsoft's "x64 prolog and epilog" page rules out `lea rsp,
        // constant[rsp]` in an epilog.
        let modrm = byte(pc + 2)?;
        let rex_b = byte(pc)? == 0x49;
        let rm = (modrm & 7) | if rex_b { 8 } else { 0 };
        let sib = u64::from(modrm & 7 == 4);
        if sib == 1 && !(rex_b && byte(pc + 3)? == 0x24) {
            return Ok(false);
        }
        let disp = match modrm >> 6 {
            1 => Some((byte(pc + 3 + sib)? as i8 as i64 as u64, 4 + sib)),
            2 => Some((
                memory_u32(memory, pc + 3 + sib)? as i32 as i64 as u64,
                7 + sib,
            )),
            _ => None,
        };
        let Some((disp, len)) = disp.filter(|_| (modrm >> 3) & 7 == 4 && rm == frame_register)
        else {
            return Ok(false);
        };
        trial.set(Register::Rsp, trial.gpr[rm as usize].wrapping_add(disp));
        pc += len;
    }
    let mut pops = Vec::new();
    loop {
        let (rex, op) = match byte(pc)? {
            0x41 => (8, byte(pc + 1)?),
            op => (0, op),
        };
        if !(0x58..=0x5F).contains(&op) || pops.len() == 16 {
            break;
        }
        pops.push(rex + (op - 0x58) as usize);
        pc += if rex == 8 { 2 } else { 1 };
    }
    let terminator = match byte(pc)? {
        0xC3 => true,
        0xF3 => byte(pc + 1)? == 0xC3,
        0xC2 => true,
        0xEB | 0xE9 => {
            let (rel, len) = if byte(pc)? == 0xEB {
                (byte(pc + 1)? as i8 as i64, 2)
            } else {
                (memory_u32(memory, pc + 1)? as i32 as i64, 5)
            };
            let target = (pc + len).wrapping_add(rel as u64);
            target < begin || target >= end
        }
        0xFF => byte(pc + 1)? == 0x25,
        0x48 | 0x49 => byte(pc + 1)? == 0xFF && (byte(pc + 2)? & 0xF8) == 0xE0,
        _ => false,
    };
    if !terminator {
        return Ok(false);
    }
    for register in pops {
        trial.gpr[register] = pop(memory, &mut trial)?;
    }
    trial.rip = pop(memory, &mut trial)?;
    if byte(pc)? == 0xC2 {
        let extra = u64::from(memory.fetch_instruction_byte(pc + 1).map_err(fault)?)
            | u64::from(memory.fetch_instruction_byte(pc + 2).map_err(fault)?) << 8;
        let rsp = trial.get(Register::Rsp);
        trial.set(Register::Rsp, at(rsp, extra)?);
    }
    *ctx = trial;
    Ok(true)
}

fn memory_u32(memory: &GuestMemory, address: u64) -> Result<u32, ExitReason> {
    let mut value = 0;
    for index in 0..4 {
        value |= u32::from(
            memory
                .fetch_instruction_byte(address + index)
                .map_err(fault)?,
        ) << (8 * index);
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// CONTEXT records
// ---------------------------------------------------------------------------

fn eflags(cpu: &CpuState) -> u32 {
    let f = cpu.flags;
    2 | u32::from(f.cf)
        | u32::from(f.pf) << 2
        | u32::from(f.af) << 4
        | u32::from(f.zf) << 6
        | u32::from(f.sf) << 7
        | u32::from(cpu.df) << 10
        | u32::from(f.of) << 11
}

/// Write `cpu` as a full x64 `CONTEXT` at `address`.
fn write_context(memory: &mut GuestMemory, address: u64, cpu: &CpuState) -> Result<(), ExitReason> {
    span(address, CONTEXT_SIZE)?;
    memory
        .write_bytes(address, &[0; CONTEXT_SIZE as usize])
        .map_err(fault)?;
    memory
        .write_u32(address + 0x30, CONTEXT_FULL)
        .map_err(fault)?;
    memory
        .write_u32(address + 0x34, cpu.mxcsr.0)
        .map_err(fault)?;
    memory
        .write_u32(address + 0x118, cpu.mxcsr.0)
        .map_err(fault)?;
    store_registers(memory, address, cpu)
}

/// The integer, flag, instruction pointer and XMM fields of a `CONTEXT`.
fn store_registers(
    memory: &mut GuestMemory,
    address: u64,
    cpu: &CpuState,
) -> Result<(), ExitReason> {
    span(address, CONTEXT_SIZE)?;
    memory
        .write_u32(address + 0x44, eflags(cpu))
        .map_err(fault)?;
    for (index, value) in cpu.gpr.iter().enumerate() {
        memory
            .write_u64(address + 0x78 + 8 * index as u64, *value)
            .map_err(fault)?;
    }
    memory.write_u64(address + 0xF8, cpu.rip).map_err(fault)?;
    for (index, value) in cpu.xmm.iter().enumerate() {
        memory
            .write_bytes(address + 0x1A0 + 16 * index as u64, &value.to_le_bytes())
            .map_err(fault)?;
    }
    Ok(())
}

/// Load the integer, flag, instruction pointer and XMM fields of a `CONTEXT`.
fn load_registers(
    memory: &GuestMemory,
    address: u64,
    cpu: &mut CpuState,
) -> Result<(), ExitReason> {
    span(address, CONTEXT_SIZE)?;
    let flags = memory.read_u32(address + 0x44).map_err(fault)?;
    let bit = |n: u32| flags >> n & 1 != 0;
    cpu.flags.cf = bit(0);
    cpu.flags.pf = bit(2);
    cpu.flags.af = bit(4);
    cpu.flags.zf = bit(6);
    cpu.flags.sf = bit(7);
    cpu.df = bit(10);
    cpu.flags.of = bit(11);
    for (index, value) in cpu.gpr.iter_mut().enumerate() {
        *value = read_u64(memory, address + 0x78 + 8 * index as u64)?;
    }
    cpu.rip = read_u64(memory, address + 0xF8)?;
    for (index, value) in cpu.xmm.iter_mut().enumerate() {
        let mut bytes = [0; 16];
        memory
            .read_bytes(address + 0x1A0 + 16 * index as u64, &mut bytes)
            .map_err(fault)?;
        *value = u128::from_le_bytes(bytes);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Target {
    frame: u64,
    ip: u64,
    value: u64,
}

/// A dispatch waiting for a guest language handler to return.
#[derive(Clone)]
pub(crate) struct Dispatch {
    /// `None` in the search phase.
    unwind: Option<Target>,
    /// The context of the code that called the service. A walk that reaches
    /// this dispatch's own frame continues from here.
    origin: CpuState,
    /// The next frame to look at.
    next: CpuState,
    establisher: u64,
    /// Guest address of the CONTEXT, DISPATCHER_CONTEXT and EXCEPTION_RECORD.
    scratch: u64,
    frames: u32,
    /// The search started from an instruction-fetch fault, whose address can
    /// be outside every image: that first frame is unwound as a leaf.
    fetch_fault: bool,
    /// The next frame may be outside every image (see `fetch_fault`).
    leaf: bool,
    /// The search is waiting for the unhandled-exception filter.
    filter: bool,
    /// The search started from `RaiseException`, not from a CPU fault.
    raised: bool,
}

impl Dispatch {
    fn search(origin: CpuState, scratch: u64, fetch_fault: bool) -> Self {
        Self {
            unwind: None,
            next: origin.clone(),
            origin,
            establisher: 0,
            scratch,
            frames: 0,
            fetch_fault,
            leaf: fetch_fault,
            filter: false,
            raised: false,
        }
    }
}

/// The caller's context at a service entry, and the scratch area below it.
fn caller(cpu: &CpuState, memory: &GuestMemory) -> Result<(CpuState, u64), ExitReason> {
    let rsp = cpu.get(Register::Rsp);
    let mut origin = cpu.clone();
    origin.rip = read_u64(memory, rsp)?;
    origin.set(Register::Rsp, at(rsp, 8)?);
    Ok((origin, scratch_below(rsp, memory)?))
}

/// A writable dispatcher scratch area, and room for a handler call, below `rsp`.
fn scratch_below(rsp: u64, memory: &GuestMemory) -> Result<u64, ExitReason> {
    let scratch = rsp
        .checked_sub(SCRATCH)
        .ok_or(fault(MemoryError::Overflow))?
        & !15;
    let call = scratch
        .checked_sub(40)
        .ok_or(fault(MemoryError::Overflow))?;
    memory
        .check_write(call, (SCRATCH + 40) as usize)
        .map_err(fault)?;
    Ok(scratch)
}

/// Whether `rip` is where a guest stack ends: the return address of the
/// entry point, of a thread's start routine, of a TLS callback, or of a
/// guest callback a host service made. No guest frame lies beyond it.
fn end_of_stack(rip: u64) -> bool {
    matches!(
        rip,
        0 | ENTRY_RETURN
            | CALLBACK_RETURN
            | crate::services::threads::THREAD_RETURN
            | crate::tls::TLS_CALLBACK_RETURN
    )
}

/// Walk frames from `dispatch.next` until a handler must run or the dispatch ends.
fn walk(
    mut dispatch: Dispatch,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    let teb = cpu.gs_base;
    loop {
        let frame = dispatch.next.clone();
        if frame.rip == EXCEPTION_RETURN || frame.rip == FILTER_RETURN {
            // An outer dispatch's handler or unhandled-exception filter frame: the stack continues at the
            // code that called that dispatch.
            let outer = memory
                .exception_frames
                .get(&(teb, frame.get(Register::Rsp)))
                .ok_or(ExitReason::InvalidUnwind)?;
            dispatch.next = outer.origin.clone();
            dispatch.leaf = outer.fetch_fault;
            continue;
        }
        if frame.rip == SCOPE_RETURN {
            // A filter or termination handler `__C_specific_handler` called:
            // the stack continues at that handler's own caller.
            let walk = memory
                .scope_frames
                .get(&(teb, frame.get(Register::Rsp)))
                .ok_or(ExitReason::InvalidUnwind)?;
            dispatch.next = walk.caller(memory)?;
            continue;
        }
        if end_of_stack(frame.rip) {
            if dispatch.unwind.is_some() {
                return Err(ExitReason::InvalidUnwind);
            }
            return unhandled(dispatch, cpu, memory);
        }
        dispatch.frames += 1;
        if dispatch.frames > MAX_FRAMES {
            return Err(ExitReason::InvalidUnwind);
        }
        let mut next = frame.clone();
        let handler_type = if dispatch.unwind.is_some() {
            UNW_FLAG_UHANDLER
        } else {
            UNW_FLAG_EHANDLER
        };
        let (found, image) = match lookup(memory, frame.rip)? {
            Some((base, entry)) => (
                virtual_unwind(memory, base, entry, &mut next, handler_type)?,
                Some((base, entry)),
            ),
            None if !dispatch.leaf && !in_image(memory, frame.rip) => {
                return Err(ExitReason::InvalidUnwind);
            }
            None => {
                let establisher = next.get(Register::Rsp);
                next.rip = pop(memory, &mut next)?;
                (
                    Frame {
                        handler: None,
                        establisher,
                    },
                    None,
                )
            }
        };
        let mut phase = 0;
        if let Some(target) = dispatch.unwind {
            if found.establisher > target.frame {
                return Err(ExitReason::InvalidUnwind);
            }
            phase = EXCEPTION_UNWINDING;
            if found.establisher == target.frame {
                phase |= EXCEPTION_TARGET_UNWIND;
            }
        }
        dispatch.establisher = found.establisher;
        dispatch.next = next;
        dispatch.leaf = false;
        match (found.handler, image) {
            (Some((handler, data)), Some((base, entry))) => {
                return enter(
                    dispatch, &frame, base, entry, handler, data, phase, cpu, memory,
                );
            }
            _ if phase & EXCEPTION_TARGET_UNWIND != 0 => {
                let target = dispatch.unwind.expect("unwind phase");
                return install(frame, target, cpu, memory);
            }
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn enter(
    dispatch: Dispatch,
    frame: &CpuState,
    base: u64,
    entry: u64,
    handler: u64,
    data: u64,
    phase: u32,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    let s = dispatch.scratch;
    let record = s + RECORD_AT;
    let flags = memory.read_u32(record + 4).map_err(fault)?;
    memory
        .write_u32(record + 4, flags & !PHASE_FLAGS | phase)
        .map_err(fault)?;
    write_context(memory, s, frame)?;
    // Exception handlers get the context of the raise or fault, which they
    // may change before continuing execution; termination handlers get the
    // context of the frame being unwound (Microsoft Learn, x64 exception
    // handling, "Language-specific handler").
    let context = if dispatch.unwind.is_some() {
        s
    } else {
        s + EXCEPTION_CONTEXT_AT
    };
    let d = s + DISPATCHER_AT;
    let target_ip = dispatch.unwind.map_or(0, |t| t.ip);
    for (offset, value) in [
        (0x00, frame.rip),
        (0x08, base),
        (0x10, entry),
        (0x18, dispatch.establisher),
        (0x20, target_ip),
        (0x28, s),
        (0x30, handler),
        (0x38, data),
        (0x40, 0),
        (0x48, 0),
    ] {
        memory.write_u64(d + offset, value).map_err(fault)?;
    }
    let call = s - 40;
    memory.write_u64(call, EXCEPTION_RETURN).map_err(fault)?;
    memory.fetch_instruction_byte(handler).map_err(fault)?;
    let key = (cpu.gs_base, call + 8);
    if memory.exception_frames.contains_key(&key) {
        return Err(ExitReason::InvalidUnwind);
    }
    let establisher = dispatch.establisher;
    memory.exception_frames.insert(key, dispatch);
    cpu.set(Register::Rcx, record);
    cpu.set(Register::Rdx, establisher);
    cpu.set(Register::R8, context);
    cpu.set(Register::R9, d);
    cpu.set(Register::Rsp, call);
    cpu.rip = handler;
    Ok(())
}

/// Continue at the unwind target with `frame`'s registers, and forget the
/// dispatch and callback frames on the stack that is being discarded.
fn install(
    mut frame: CpuState,
    target: Target,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    memory.fetch_instruction_byte(target.ip).map_err(fault)?;
    frame.rip = target.ip;
    frame.set(Register::Rax, target.value);
    let teb = cpu.gs_base;
    let rsp = frame.get(Register::Rsp);
    memory
        .exception_frames
        .retain(|&(t, key), _| t != teb || key >= rsp);
    memory
        .callback_frames
        .retain(|&(t, key), _| t != teb || key >= rsp);
    memory
        .scope_frames
        .retain(|&(t, key), _| t != teb || key >= rsp);
    *cpu = frame;
    Ok(())
}

/// A search reached the end of the stack with no frame accepting the
/// exception. Call the unhandled-exception filter, once per exception, or
/// take the default action.
///
/// `SetUnhandledExceptionFilter` documents that the filter takes an
/// `EXCEPTION_POINTERS` and runs on the thread that raised the exception, and
/// what each of its three results means. The filter is not called again for
/// an exception raised while a filter of the same thread runs: that one gets
/// the default action at once (a Xenora policy; the pages read are silent).
fn unhandled(
    mut dispatch: Dispatch,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    let filter = memory.unhandled_filter;
    let teb = cpu.gs_base;
    let filtering = memory
        .exception_frames
        .iter()
        .any(|(&(t, _), pending)| t == teb && pending.filter);
    if filter == 0 || dispatch.filter || filtering {
        return default_action(&dispatch, cpu, memory);
    }
    // EXCEPTION_POINTERS { ExceptionRecord, ContextRecord }, in the
    // DISPATCHER_CONTEXT slot, which no handler is using now.
    let s = dispatch.scratch;
    let pointers = s + DISPATCHER_AT;
    memory.write_u64(pointers, s + RECORD_AT).map_err(fault)?;
    memory
        .write_u64(pointers + 8, s + EXCEPTION_CONTEXT_AT)
        .map_err(fault)?;
    let call = s - 40;
    memory.write_u64(call, FILTER_RETURN).map_err(fault)?;
    memory.fetch_instruction_byte(filter).map_err(fault)?;
    let key = (teb, call + 8);
    if memory.exception_frames.contains_key(&key) {
        return Err(ExitReason::InvalidUnwind);
    }
    dispatch.filter = true;
    memory.exception_frames.insert(key, dispatch);
    cpu.set(Register::Rcx, pointers);
    cpu.set(Register::Rsp, call);
    cpu.rip = filter;
    Ok(())
}

/// Xenora's default action for an exception that nothing handled, a policy:
/// the guest process ends with the exception code as its exit status.
/// `RaiseException` names `ExitProcess` as the default action for most
/// exceptions but the pages read give no exit status. The guest's registers
/// are those of the raise or fault, so a fault's RIP is the faulting
/// instruction. The services' recorded exit status is not set: the
/// dispatcher has no access to them, only the guest's exit reason carries it.
fn default_action(
    dispatch: &Dispatch,
    cpu: &mut CpuState,
    memory: &GuestMemory,
) -> Result<(), ExitReason> {
    let code = memory
        .read_u32(dispatch.scratch + RECORD_AT)
        .map_err(fault)?;
    let mut stop = dispatch.origin.clone();
    if dispatch.raised {
        // As at the call into RaiseException: RSP on its return address, so
        // the nonvolatile registers are those the caller had.
        stop.set(Register::Rsp, stop.get(Register::Rsp) - 8);
    }
    *cpu = stop;
    Err(ExitReason::ProcessExit(code))
}

/// The unhandled-exception filter returned to [`FILTER_RETURN`] with its
/// result in EAX.
pub(crate) fn filter_resume(
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    let key = (cpu.gs_base, cpu.get(Register::Rsp));
    let dispatch = match memory.exception_frames.get(&key) {
        Some(pending) if pending.filter => memory
            .exception_frames
            .remove(&key)
            .expect("the pending filter"),
        _ => return Err(ExitReason::InvalidUnwind),
    };
    match cpu.get(Register::Rax) as u32 as i32 {
        // "Continue execution from the point of the exception", with the
        // filter's changes to the exception information.
        FILTER_CONTINUE_EXECUTION => continue_execution(dispatch, cpu, memory),
        // Continue-search goes on to the default handling, and execute-handler
        // "usually results in process termination": both end here in the
        // default action.
        FILTER_CONTINUE_SEARCH | FILTER_EXECUTE_HANDLER => default_action(&dispatch, cpu, memory),
        _ => Err(ExitReason::InvalidUnwind),
    }
}

/// Continue with the exception context the handler or filter was given,
/// including any change it made: after a raise this returns from the raise,
/// and after a fault it runs the faulting instruction again unless the
/// handler moved RIP. A noncontinuable exception is refused.
fn continue_execution(
    dispatch: Dispatch,
    cpu: &mut CpuState,
    memory: &GuestMemory,
) -> Result<(), ExitReason> {
    let flags = memory
        .read_u32(dispatch.scratch + RECORD_AT + 4)
        .map_err(fault)?;
    if flags & EXCEPTION_NONCONTINUABLE != 0 {
        return Err(ExitReason::InvalidUnwind);
    }
    let mut resumed = dispatch.origin;
    load_registers(
        memory,
        dispatch.scratch + EXCEPTION_CONTEXT_AT,
        &mut resumed,
    )?;
    *cpu = resumed;
    Ok(())
}

/// A language handler returned to [`EXCEPTION_RETURN`] with its disposition in EAX.
pub(crate) fn resume(cpu: &mut CpuState, memory: &mut GuestMemory) -> Result<(), ExitReason> {
    let key = (cpu.gs_base, cpu.get(Register::Rsp));
    let dispatch = match memory.exception_frames.get(&key) {
        Some(pending) if !pending.filter => memory
            .exception_frames
            .remove(&key)
            .expect("the pending dispatch"),
        _ => return Err(ExitReason::InvalidUnwind),
    };
    let disposition = cpu.get(Register::Rax) as u32;
    match (dispatch.unwind, disposition) {
        (Some(target), DISPOSITION_CONTINUE_SEARCH) => {
            if dispatch.establisher == target.frame {
                // The target's handler may have changed the context it was given.
                let mut frame = dispatch.origin.clone();
                load_registers(memory, dispatch.scratch, &mut frame)?;
                return install(frame, target, cpu, memory);
            }
            walk(dispatch, cpu, memory)
        }
        (None, DISPOSITION_CONTINUE_SEARCH) => walk(dispatch, cpu, memory),
        (None, DISPOSITION_CONTINUE_EXECUTION) => continue_execution(dispatch, cpu, memory),
        // ExceptionNestedException, ExceptionCollidedUnwind and any other
        // value: the public pages read do not describe how a dispatcher
        // handles the first two, so they are refused rather than guessed.
        _ => Err(ExitReason::InvalidUnwind),
    }
}

/// Raise a breakpoint from a DebugBreak service call.
pub(crate) fn debug_break(
    args: &Win64Args,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    const STATUS_BREAKPOINT: u64 = 0x8000_0003;
    let breakpoint = Win64Args {
        rcx: STATUS_BREAKPOINT,
        rdx: 0,
        r8: 0,
        r9: 0,
        rsp: args.rsp,
    };
    raise_exception(&breakpoint, cpu, memory)
}

/// `RaiseException(code, flags, count, arguments)`.
fn raise_exception(
    args: &Win64Args,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    let count = args.r8 & 0xFFFF_FFFF;
    if count > EXCEPTION_MAXIMUM_PARAMETERS {
        return Err(ExitReason::InvalidUnwind);
    }
    let (origin, scratch) = caller(cpu, memory)?;
    let record = scratch + RECORD_AT;
    memory
        .write_bytes(record, &[0; RECORD_SIZE as usize])
        .map_err(fault)?;
    // Bit 28 of the code is reserved for the system.
    memory
        .write_u32(record, args.rcx as u32 & !0x1000_0000)
        .map_err(fault)?;
    memory
        .write_u32(record + 4, args.rdx as u32 & EXCEPTION_NONCONTINUABLE)
        .map_err(fault)?;
    memory.write_u64(record + 0x10, origin.rip).map_err(fault)?;
    memory
        .write_u32(record + 0x18, count as u32)
        .map_err(fault)?;
    for index in 0..count {
        let value = read_u64(memory, at(args.r9, index * 8)?)?;
        memory
            .write_u64(record + 0x20 + index * 8, value)
            .map_err(fault)?;
    }
    write_context(memory, scratch + EXCEPTION_CONTEXT_AT, &origin)?;
    let mut dispatch = Dispatch::search(origin, scratch, false);
    dispatch.raised = true;
    walk(dispatch, cpu, memory)
}

/// `RtlUnwindEx(target_frame, target_ip, record, return_value, context, history)`.
fn unwind_ex(
    args: &Win64Args,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    if args.rcx == 0 {
        // An exit unwind, which has no target frame. `RtlUnwindEx` says only
        // that a NULL target frame performs one and that the target IP is
        // then ignored; where it stops and what follows are not documented,
        // so it is refused rather than guessed.
        return Err(ExitReason::InvalidUnwind);
    }
    let (origin, scratch) = caller(cpu, memory)?;
    let target = Target {
        frame: args.rcx,
        ip: args.rdx,
        value: args.r9,
    };
    unwind(origin, scratch, target, args.r8, cpu, memory)
}

/// Start an unwind to `target` from `origin`, using the scratch area at
/// `scratch`, with a copy of the record at `source` (or a `STATUS_UNWIND`
/// record when it is 0).
fn unwind(
    origin: CpuState,
    scratch: u64,
    target: Target,
    source: u64,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    let record = scratch + RECORD_AT;
    if source == 0 {
        memory
            .write_bytes(record, &[0; RECORD_SIZE as usize])
            .map_err(fault)?;
        memory.write_u32(record, STATUS_UNWIND).map_err(fault)?;
    } else {
        let mut copy = [0; RECORD_SIZE as usize];
        memory.read_bytes(source, &mut copy).map_err(fault)?;
        memory.write_bytes(record, &copy).map_err(fault)?;
    }
    let dispatch = Dispatch {
        unwind: Some(target),
        ..Dispatch::search(origin, scratch, false)
    };
    walk(dispatch, cpu, memory)
}

pub(crate) fn register(registry: &mut ImportRegistry) {
    registry.register_context_service(
        KERNEL32,
        "RaiseException",
        ServiceStatus::Partial,
        raise_exception,
    );
    registry.register_context_service(KERNEL32, "RtlUnwindEx", ServiceStatus::Partial, unwind_ex);
    scope::register(registry);
    registry.register_with_status(
        KERNEL32,
        "RtlCaptureContext",
        ServiceStatus::Implemented,
        |args, cpu, memory| {
            let (origin, _) = caller(cpu, memory)?;
            write_context(memory, args.rcx, &origin)?;
            Ok(0)
        },
    );
    registry.register_with_status(
        KERNEL32,
        "RtlLookupFunctionEntry",
        ServiceStatus::Implemented,
        |args, _, memory| {
            let Some((base, entry)) = lookup(memory, args.rcx)? else {
                return Ok(0);
            };
            if args.rdx != 0 {
                memory.write_u64(args.rdx, base).map_err(fault)?;
            }
            Ok(entry)
        },
    );
    registry.register_with_status(
        KERNEL32,
        "RtlVirtualUnwind",
        ServiceStatus::Partial,
        |args, cpu, memory| {
            let arg = |index| args.get(index, memory).map_err(fault);
            let (context, handler_data, establisher, pointers) =
                (arg(4)?, arg(5)?, arg(6)?, arg(7)?);
            if pointers != 0 {
                // KNONVOLATILE_CONTEXT_POINTERS is not filled in.
                return Err(ExitReason::InvalidUnwind);
            }
            let mut frame = cpu.clone();
            load_registers(memory, context, &mut frame)?;
            frame.rip = args.r8;
            let found = virtual_unwind(memory, args.rdx, args.r9, &mut frame, args.rcx as u8)?;
            store_registers(memory, context, &frame)?;
            memory
                .write_u64(establisher, found.establisher)
                .map_err(fault)?;
            let Some((handler, data)) = found.handler else {
                return Ok(0);
            };
            memory.write_u64(handler_data, data).map_err(fault)?;
            Ok(handler)
        },
    );
}

#[cfg(test)]
mod tests {
    //! Expected values are worked by hand from the unwind rules on the public
    //! x64 exception-handling page, not taken from a run.
    use super::*;
    use crate::memory::MemoryPermissions;

    const BASE: u64 = 0x1_4000_0000;
    const IMAGE: u32 = 0x4000;
    const STACK: u64 = 0x10_0000;
    const BEGIN: u64 = 0x1000;
    const INFO: u64 = 0x2000;

    /// Headers naming an exception directory at `rva`, `size`.
    fn headers(rva: u32, size: u32) -> Vec<u8> {
        let mut bytes = vec![0; 0x40 + 24 + 240];
        bytes[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        bytes[0x40 + 4 + 16..0x40 + 4 + 18].copy_from_slice(&240u16.to_le_bytes());
        let optional = 0x40 + 24;
        bytes[optional + 108..optional + 112].copy_from_slice(&16u32.to_le_bytes());
        bytes[optional + 136..optional + 140].copy_from_slice(&rva.to_le_bytes());
        bytes[optional + 140..optional + 144].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    /// An image with one function at BEGIN..BEGIN+0x100 whose prolog is
    /// `push rbp; push rbx; sub rsp, 0x28` (ends at offsets 1, 2 and 6), and
    /// an epilog `add rsp, 0x28; pop rbx; pop rbp; ret` at BEGIN+0x80.
    fn image(version_flags: u8, extra_code: Option<u16>) -> GuestMemory {
        let mut memory = GuestMemory::new();
        memory
            .map_region(BASE, IMAGE as usize, MemoryPermissions::READ_WRITE_EXECUTE)
            .unwrap();
        memory
            .map_region(STACK, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();
        let entry = [BEGIN as u32, BEGIN as u32 + 0x100, INFO as u32];
        for (index, value) in entry.iter().enumerate() {
            memory
                .write_u32(BASE + 0x3000 + 4 * index as u64, *value)
                .unwrap();
        }
        let mut codes: Vec<u16> = vec![0x4206, 0x3002, 0x5001];
        if let Some(code) = extra_code {
            codes.insert(0, code);
        }
        let header = [version_flags, 6, codes.len() as u8, 0];
        memory.write_bytes(BASE + INFO, &header).unwrap();
        for (index, code) in codes.iter().enumerate() {
            memory
                .write_u16(BASE + INFO + 4 + 2 * index as u64, *code)
                .unwrap();
        }
        memory
            .write_bytes(
                BASE + BEGIN + 0x80,
                &[0x48, 0x83, 0xC4, 0x28, 0x5B, 0x5D, 0xC3],
            )
            .unwrap();
        register_exception_directory(&headers(0x3000, 12), BASE, IMAGE, &mut memory).unwrap();
        memory
    }

    /// The stack a body address sees: the 0x28 allocation at `rsp`, then the
    /// saved RBX, the saved RBP and the return address.
    fn stack(memory: &mut GuestMemory, rsp: u64) {
        memory.write_u64(rsp + 0x28, 0xB0B0).unwrap();
        memory.write_u64(rsp + 0x30, 0xBEBE).unwrap();
        memory.write_u64(rsp + 0x38, 0xCA11E7).unwrap();
    }

    fn unwind_at(memory: &GuestMemory, pc: u64, rsp: u64) -> Result<(CpuState, Frame), ExitReason> {
        let mut ctx = CpuState::new(pc, rsp);
        let (base, entry) = lookup(memory, pc)?.expect("the function is in the table");
        let frame = virtual_unwind(memory, base, entry, &mut ctx, UNW_FLAG_EHANDLER)?;
        Ok((ctx, frame))
    }

    #[test]
    fn a_body_address_undoes_the_whole_prolog() {
        let mut memory = image(1, None);
        let rsp = STACK + 0x100;
        stack(&mut memory, rsp);
        let (ctx, frame) = unwind_at(&memory, BASE + BEGIN + 0x40, rsp).unwrap();
        assert_eq!(ctx.get(Register::Rbx), 0xB0B0);
        assert_eq!(ctx.get(Register::Rbp), 0xBEBE);
        assert_eq!(ctx.rip, 0xCA11E7);
        assert_eq!(ctx.get(Register::Rsp), rsp + 0x40);
        assert_eq!(
            frame,
            Frame {
                handler: None,
                establisher: rsp
            }
        );
    }

    #[test]
    fn a_prolog_address_undoes_only_what_has_run() {
        let mut memory = image(1, None);
        // After `push rbp; push rbx` (offset 2) the allocation has not happened.
        let rsp = STACK + 0x100;
        memory.write_u64(rsp, 0xB0B0).unwrap();
        memory.write_u64(rsp + 8, 0xBEBE).unwrap();
        memory.write_u64(rsp + 16, 0xCA11E7).unwrap();
        let (ctx, _) = unwind_at(&memory, BASE + BEGIN + 2, rsp).unwrap();
        assert_eq!(ctx.get(Register::Rbx), 0xB0B0);
        assert_eq!(ctx.get(Register::Rbp), 0xBEBE);
        assert_eq!(ctx.rip, 0xCA11E7);
        assert_eq!(ctx.get(Register::Rsp), rsp + 24);
    }

    #[test]
    fn an_epilog_address_finishes_the_epilog_instead() {
        let mut memory = image(1, None);
        let rsp = STACK + 0x100;
        stack(&mut memory, rsp);
        // At `pop rbx`: the allocation is already released.
        let (ctx, _) = unwind_at(&memory, BASE + BEGIN + 0x84, rsp + 0x28).unwrap();
        assert_eq!(ctx.get(Register::Rbx), 0xB0B0);
        assert_eq!(ctx.get(Register::Rbp), 0xBEBE);
        assert_eq!(ctx.rip, 0xCA11E7);
        assert_eq!(ctx.get(Register::Rsp), rsp + 0x40);
    }

    /// The handler a framed function names, and its data after the slot.
    const HANDLER: u32 = 0x1F00;
    /// Where the framed function's fixed allocation starts, and the values
    /// saved above it: RBX, R12 and the return address.
    const ALLOCATION: u64 = STACK + 0x200;
    const SAVED_RBX: u64 = 0xB0B0_B0B0;
    const SAVED_R12: u64 = 0x1212_1212_1212;
    const RETURN: u64 = 0xCA11E7;

    /// An image with one function at BEGIN..BEGIN+0x100 that uses R12 as its
    /// frame register, laid out from Microsoft's "x64 prolog and epilog" and
    /// "x64 exception handling" pages. The encodings, epilogs included, were
    /// checked with LLVM `llvm-mc`:
    ///
    /// ```text
    /// 0:  push r12              41 54
    /// 2:  push rbx              53
    /// 3:  sub rsp, 0x40         48 83 EC 40
    /// 7:  lea r12, [rsp + 0x20] 4C 8D 64 24 20
    /// ```
    ///
    /// UNWIND_INFO names R12 (12) with a scaled frame offset of 2, which is
    /// 0x20 bytes, and an exception handler. `epilog` is written at
    /// BEGIN+0x80. It also fills the stack: RBX, R12 and the return address
    /// above the 0x40 allocation at ALLOCATION.
    fn r12_image(epilog: &[u8]) -> GuestMemory {
        let mut memory = GuestMemory::new();
        memory
            .map_region(BASE, IMAGE as usize, MemoryPermissions::READ_WRITE_EXECUTE)
            .unwrap();
        memory
            .map_region(STACK, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();
        let entry = [BEGIN as u32, BEGIN as u32 + 0x100, INFO as u32];
        for (index, value) in entry.iter().enumerate() {
            memory
                .write_u32(BASE + 0x3000 + 4 * index as u64, *value)
                .unwrap();
        }
        // Version 1 with UNW_FLAG_EHANDLER, a 12-byte prolog, four codes, and
        // frame register 12 with scaled offset 2.
        memory
            .write_bytes(BASE + INFO, &[1 | UNW_FLAG_EHANDLER << 3, 12, 4, 0x2C])
            .unwrap();
        // UWOP_SET_FPREG at 12, UWOP_ALLOC_SMALL of 0x40 at 7, then
        // UWOP_PUSH_NONVOL of RBX at 3 and of R12 at 2: offset, then the
        // operation in the low and its information in the high nibble.
        for (index, code) in [0x030C_u16, 0x7207, 0x3003, 0xC002].iter().enumerate() {
            memory
                .write_u16(BASE + INFO + 4 + 2 * index as u64, *code)
                .unwrap();
        }
        memory.write_u32(BASE + INFO + 12, HANDLER).unwrap();
        memory
            .write_bytes(
                BASE + BEGIN,
                &[
                    0x41, 0x54, 0x53, 0x48, 0x83, 0xEC, 0x40, 0x4C, 0x8D, 0x64, 0x24, 0x20,
                ],
            )
            .unwrap();
        memory.write_bytes(BASE + BEGIN + 0x80, epilog).unwrap();
        memory.write_u64(ALLOCATION + 0x40, SAVED_RBX).unwrap();
        memory.write_u64(ALLOCATION + 0x48, SAVED_R12).unwrap();
        memory.write_u64(ALLOCATION + 0x50, RETURN).unwrap();
        register_exception_directory(&headers(0x3000, 12), BASE, IMAGE, &mut memory).unwrap();
        memory
    }

    /// `lea rsp, [r12 + 0x20]; pop rbx; pop r12; ret`, with an 8-bit
    /// displacement as assemblers emit it.
    const R12_EPILOG: [u8; 9] = [0x49, 0x8D, 0x64, 0x24, 0x20, 0x5B, 0x41, 0x5C, 0xC3];
    /// The same epilog with a 32-bit displacement.
    const R12_EPILOG_DISP32: [u8; 12] = [
        0x49, 0x8D, 0xA4, 0x24, 0x20, 0x00, 0x00, 0x00, 0x5B, 0x41, 0x5C, 0xC3,
    ];

    /// Unwind from `pc` with RSP, RBX and R12 as given. The frame register
    /// R12 holds ALLOCATION + 0x20 until the epilog pops it.
    fn unwind_r12(
        memory: &GuestMemory,
        pc: u64,
        rsp: u64,
        rbx: u64,
        r12: u64,
    ) -> (CpuState, Frame) {
        let mut ctx = CpuState::new(pc, rsp);
        ctx.set(Register::Rbx, rbx);
        ctx.set(Register::R12, r12);
        let (base, entry) = lookup(memory, pc).unwrap().expect("in the table");
        let frame = virtual_unwind(memory, base, entry, &mut ctx, UNW_FLAG_EHANDLER).unwrap();
        (ctx, frame)
    }

    /// The caller's registers after any unwind of the framed function.
    fn assert_caller(ctx: &CpuState, what: &str) {
        assert_eq!(ctx.get(Register::Rbx), SAVED_RBX, "{what}");
        assert_eq!(ctx.get(Register::R12), SAVED_R12, "{what}");
        assert_eq!(ctx.rip, RETURN, "{what}");
        assert_eq!(ctx.get(Register::Rsp), ALLOCATION + 0x58, "{what}");
    }

    const R12_FRAME: u64 = ALLOCATION + 0x20;
    /// RSP in the body, below a further 0x100 bytes the body allocated, so only
    /// the frame register locates the fixed allocation.
    const BODY_RSP: u64 = ALLOCATION - 0x100;

    #[test]
    fn an_r12_frame_body_address_unwinds_from_the_frame_register() {
        let memory = r12_image(&R12_EPILOG);
        let (ctx, frame) = unwind_r12(&memory, BASE + BEGIN + 0x40, BODY_RSP, 0x5555, R12_FRAME);
        assert_caller(&ctx, "body");
        // R12 - 0x20 is the fixed allocation, and the body selects the handler.
        assert_eq!(
            frame,
            Frame {
                handler: Some((BASE + u64::from(HANDLER), BASE + INFO + 16)),
                establisher: ALLOCATION,
            }
        );
    }

    #[test]
    fn an_r12_frame_epilog_is_finished_from_each_of_its_instructions() {
        for (epilog, lea) in [(&R12_EPILOG[..], 5), (&R12_EPILOG_DISP32[..], 8)] {
            let memory = r12_image(epilog);
            let start = BASE + BEGIN + 0x80;
            // (PC, RSP, RBX, R12) as each instruction of the epilog finds them.
            let points = [
                (start, BODY_RSP, 0x5555, R12_FRAME),
                (start + lea, ALLOCATION + 0x40, 0x5555, R12_FRAME),
                (start + lea + 1, ALLOCATION + 0x48, SAVED_RBX, R12_FRAME),
                (start + lea + 3, ALLOCATION + 0x50, SAVED_RBX, SAVED_R12),
            ];
            for (pc, rsp, rbx, r12) in points {
                let what = format!("{epilog:02X?} at {:#x}", pc - start);
                let (ctx, frame) = unwind_r12(&memory, pc, rsp, rbx, r12);
                assert_caller(&ctx, &what);
                // An epilog address calls no handler (Microsoft, "x64
                // exception handling", RtlVirtualUnwind).
                assert_eq!(frame.handler, None, "{what}");
                if r12 == R12_FRAME {
                    assert_eq!(frame.establisher, ALLOCATION, "{what}");
                }
            }
        }
    }

    #[test]
    fn an_r12_frame_lea_of_another_form_is_body_code() {
        let pops = [0x5B, 0x41, 0x5C, 0xC3];
        for lea in [
            &[0x49, 0x8D, 0x64, 0x04, 0x20][..], // lea rsp, [r12 + rax + 0x20]: an index
            &[0x48, 0x8D, 0x64, 0x24, 0x20],     // lea rsp, [rsp + 0x20]: RSP is no frame
            &[0x49, 0x8D, 0x65, 0x20],           // lea rsp, [r13 + 0x20]: not the frame register
        ] {
            let memory = r12_image(&[lea, &pops[..]].concat());
            let (ctx, frame) =
                unwind_r12(&memory, BASE + BEGIN + 0x80, BODY_RSP, 0x5555, R12_FRAME);
            // The body rules apply: the same caller state, and the handler.
            assert_caller(&ctx, &format!("{lea:02X?}"));
            assert!(frame.handler.is_some(), "{lea:02X?}");
        }
    }

    #[test]
    fn unknown_versions_and_operations_are_refused() {
        let rsp = STACK + 0x100;
        // Version 2, and operation 6 which the public page does not describe.
        for memory in [image(2, None), image(1, Some(0x0606))] {
            let mut memory = memory;
            stack(&mut memory, rsp);
            assert_eq!(
                unwind_at(&memory, BASE + BEGIN + 0x40, rsp).unwrap_err(),
                ExitReason::InvalidUnwind
            );
        }
    }

    #[test]
    fn addresses_outside_every_function_are_not_found() {
        let memory = image(1, None);
        assert_eq!(lookup(&memory, BASE + BEGIN + 0x100), Ok(None));
        assert_eq!(lookup(&memory, BASE + BEGIN - 1), Ok(None));
        assert_eq!(lookup(&memory, 0x1000), Ok(None));
    }

    #[test]
    fn malformed_exception_directories_are_refused() {
        let mut memory = GuestMemory::new();
        memory
            .map_region(BASE, IMAGE as usize, MemoryPermissions::READ_WRITE)
            .unwrap();
        let entries = |memory: &mut GuestMemory, list: &[[u32; 3]]| {
            for (index, entry) in list.iter().enumerate() {
                for (field, value) in entry.iter().enumerate() {
                    memory
                        .write_u32(BASE + 0x3000 + 12 * index as u64 + 4 * field as u64, *value)
                        .unwrap();
                }
            }
        };
        let mut check = |list: &[[u32; 3]], rva: u32, size: u32| {
            entries(&mut memory, list);
            register_exception_directory(&headers(rva, size), BASE, IMAGE, &mut memory)
        };
        let good = [0x1000, 0x1100, 0x2000];
        assert!(check(&[good], 0x3000, 12).is_ok());
        assert!(check(&[good], 0x3000, 13).is_err(), "not whole entries");
        assert!(check(&[good], 0x3002, 12).is_err(), "misaligned");
        assert!(check(&[good], 0x3FF8, 12).is_err(), "past the image");
        assert!(
            check(&[[0x1100, 0x1000, 0x2000]], 0x3000, 12).is_err(),
            "empty range"
        );
        assert!(
            check(&[[0x1000, 0x5000, 0x2000]], 0x3000, 12).is_err(),
            "range outside the image"
        );
        assert!(
            check(&[good, [0x1080, 0x1200, 0x2000]], 0x3000, 24).is_err(),
            "overlap"
        );
        assert!(
            check(&[[0x1000, 0x1100, 0x2002]], 0x3000, 12).is_err(),
            "misaligned unwind data"
        );
    }
}
