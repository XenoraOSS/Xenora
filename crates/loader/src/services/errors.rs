// SPDX-License-Identifier: GPL-3.0-or-later

//! Error reporting services: `FormatMessage` in both widths and the error
//! mode. Message texts come from the public winerror.h documentation; the
//! table covers the error codes the synthetic services produce, and an
//! unknown identifier fails the way Windows does for a missing message.

use super::{ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, KERNEL32, ServicesState};
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, PROCESS_HEAP};
use std::sync::Arc;

const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
/// "The system cannot find message text for message number 0x%1 in the
/// message file for %2." - the failure an unknown message identifier gets.
const ERROR_MR_MID_NOT_FOUND: u32 = 317;

const ALLOCATE_BUFFER: u32 = 0x100;
const IGNORE_INSERTS: u32 = 0x200;
const FROM_STRING: u32 = 0x400;
const FROM_HMODULE: u32 = 0x800;
const FROM_SYSTEM: u32 = 0x1000;
const ARGUMENT_ARRAY: u32 = 0x2000;
const MAX_WIDTH_MASK: u32 = 0xff;
const ACCEPTED_FLAGS: u32 = ALLOCATE_BUFFER
    | IGNORE_INSERTS
    | FROM_STRING
    | FROM_HMODULE
    | FROM_SYSTEM
    | ARGUMENT_ARRAY
    | MAX_WIDTH_MASK;

const ERRHANDLING_API: &str = "api-ms-win-core-errorhandling-l1-1-0.dll";

/// The system message table, in en-US, from the public winerror.h
/// documentation. Entries end with the trailing newline real system
/// messages carry; programs strip it.
fn system_message(identifier: u32) -> Option<&'static str> {
    Some(match identifier {
        0 => "The operation completed successfully.\r\n",
        2 => "The system cannot find the file specified.\r\n",
        3 => "The system cannot find the path specified.\r\n",
        4 => "The system cannot open the file.\r\n",
        5 => "Access is denied.\r\n",
        6 => "The handle is invalid.\r\n",
        8 => "Not enough memory resources are available to process this command.\r\n",
        13 => "The data is invalid.\r\n",
        14 => "Not enough storage is available to complete this operation.\r\n",
        18 => "There are no more files.\r\n",
        32 => "The process cannot access the file because it is being used by another process.\r\n",
        50 => "The request is not supported.\r\n",
        80 => "The file exists.\r\n",
        87 => "The parameter is incorrect.\r\n",
        112 => "There is not enough space on the disk.\r\n",
        122 => "The data area passed to a system call is too small.\r\n",
        123 => "The filename, directory name, or volume label syntax is incorrect.\r\n",
        131 => "An attempt was made to move the file pointer before the beginning of the file.\r\n",
        145 => "The directory is not empty.\r\n",
        183 => "Cannot create a file when that file already exists.\r\n",
        317 => {
            "The system cannot find message text for message number 0x%1 in the message file for %2.\r\n"
        }
        998 => "Invalid access to memory location.\r\n",
        _ => return None,
    })
}

/// One substitution: an insert number in the message text is replaced by
/// the matching argument - a string pointer for `!s!` and the bare form,
/// a number otherwise. Arguments are read as a flat array of machine
/// words; the difference from a real va_list is documented in
/// docs/interfaces.md and matters only for floating point inserts, which
/// the system messages never use.
fn substitution(
    memory: &GuestMemory,
    arguments: u64,
    index: u64,
    spec: &str,
    wide: bool,
) -> Result<String, crate::memory::MemoryError> {
    let value = memory.read_u64(arguments + 8 * (index - 1))?;
    let string = spec.is_empty() || spec.eq_ignore_ascii_case("s");
    if string {
        let mut text = String::new();
        if wide {
            let mut at = value;
            loop {
                let unit = memory.read_u16(at)?;
                if unit == 0 {
                    break;
                }
                text.push(char::from_u32(u32::from(unit)).unwrap_or('\u{fffd}'));
                at += 2;
            }
        } else {
            let mut at = value;
            loop {
                let byte = memory.read_u8(at)?;
                if byte == 0 {
                    break;
                }
                text.push(byte as char);
                at += 1;
            }
        }
        Ok(text)
    } else {
        Ok(number(
            value,
            spec.contains('x') || spec.contains('X'),
            spec.contains('X'),
        ))
    }
}

/// Format one numeric argument. The decimal specifiers format decimal;
/// `x` and `X` format lowercase and uppercase hexadecimal, as documented.
fn number(value: u64, hex: bool, upper: bool) -> String {
    if hex && upper {
        format!("{value:X}")
    } else if hex {
        format!("{value:x}")
    } else {
        format!("{value}")
    }
}

/// Expand the insert sequences of a format text. `%%` is a literal percent
/// sign; an insert is `%` followed by one or two digits and an optional
/// `!specifier!`, of which `s` and `S` read a string pointer and anything
/// else formats the word as a decimal number. Any other sequence is copied
/// literally - none appear in the system messages.
fn expand(
    memory: &GuestMemory,
    format: &str,
    arguments: u64,
    wide_arguments: bool,
    ignore_inserts: bool,
) -> Result<String, crate::memory::MemoryError> {
    let mut out = String::new();
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'%') {
            chars.next();
            out.push('%');
            continue;
        }
        let mut digits = String::new();
        while let Some(d) = chars.peek() {
            if d.is_ascii_digit() {
                digits.push(*d);
                chars.next();
            } else {
                break;
            }
        }
        if digits.is_empty() {
            out.push('%');
            continue;
        }
        let index: u64 = digits.parse().unwrap_or(0);
        let mut spec = String::new();
        let mut had_spec = false;
        if chars.peek() == Some(&'!') {
            had_spec = true;
            chars.next();
            for c in chars.by_ref() {
                if c == '!' {
                    break;
                }
                spec.push(c);
            }
        }
        if ignore_inserts {
            // IGNORE_INSERTS passes the insert sequence through unchanged,
            // as documented: rebuild it verbatim, specifier included.
            out.push('%');
            out.push_str(&digits);
            if had_spec {
                out.push('!');
                out.push_str(&spec);
                out.push('!');
            }
            continue;
        }
        if arguments == 0 || index == 0 {
            continue;
        }
        out.push_str(&substitution(
            memory,
            arguments,
            index,
            &spec,
            wide_arguments,
        )?);
    }
    Ok(out)
}

fn write_output(
    memory: &mut GuestMemory,
    text: &str,
    wide: bool,
    buffer: u64,
    size: u64,
    allocate: bool,
) -> Result<u64, u32> {
    let units: Vec<u8> = if wide {
        text.encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .chain([0, 0])
            .collect()
    } else {
        text.as_bytes().iter().copied().chain([0]).collect()
    };
    let written = if wide {
        text.encode_utf16().count() as u64
    } else {
        text.len() as u64
    };
    if allocate {
        let address = memory
            .map_heap_allocation(PROCESS_HEAP, units.len().max(1))
            .map_err(|_| ERROR_INSUFFICIENT_BUFFER)?;
        memory
            .write_bytes(address, &units)
            .map_err(|_| ERROR_INSUFFICIENT_BUFFER)?;
        memory
            .write_u64(buffer, address)
            .map_err(|_| ERROR_INSUFFICIENT_BUFFER)?;
        Ok(written)
    } else {
        let capacity = size * if wide { 2 } else { 1 };
        if (units.len() as u64) > capacity {
            return Err(ERROR_INSUFFICIENT_BUFFER);
        }
        memory
            .write_bytes(buffer, &units)
            .map_err(|_| ERROR_INSUFFICIENT_BUFFER)?;
        Ok(written)
    }
}

#[allow(clippy::too_many_arguments)]
fn format_message(
    state: &ServicesState,
    teb: u64,
    memory: &mut GuestMemory,
    flags: u32,
    source: u64,
    identifier: u32,
    buffer: u64,
    size: u64,
    arguments: u64,
    wide: bool,
) -> Result<u64, ExitReason> {
    let fault = ExitReason::MemoryFault;
    if flags & !ACCEPTED_FLAGS != 0 {
        return state.fail(teb, ERROR_INVALID_PARAMETER, 0);
    }
    if flags & FROM_HMODULE != 0 {
        // No module message tables exist yet: the honest answer.
        return state.fail(teb, ERROR_NOT_SUPPORTED, 0);
    }
    if flags & FROM_STRING == 0 && flags & FROM_SYSTEM == 0 {
        return state.fail(teb, ERROR_INVALID_PARAMETER, 0);
    }
    let format = if flags & FROM_STRING != 0 {
        // The source is the format text itself, in the service's width.
        let mut text = String::new();
        let mut at = source;
        loop {
            let unit = if wide {
                match memory.read_u16(at).map_err(fault)? {
                    0 => break,
                    unit => unit,
                }
            } else {
                match memory.read_u8(at).map_err(fault)? {
                    0 => break,
                    byte => u16::from(byte),
                }
            };
            text.push(char::from_u32(u32::from(unit)).unwrap_or('\u{fffd}'));
            at += if wide { 2 } else { 1 };
            if text.len() > 8192 {
                return state.fail(teb, ERROR_INVALID_PARAMETER, 0);
            }
        }
        text
    } else {
        let Some(message) = system_message(identifier) else {
            return state.fail(teb, ERROR_MR_MID_NOT_FOUND, 0);
        };
        message.to_string()
    };
    let expanded = match expand(
        memory,
        &format,
        arguments,
        wide,
        flags & IGNORE_INSERTS != 0,
    ) {
        Ok(text) => text,
        Err(_) => return state.fail(teb, ERROR_INVALID_PARAMETER, 0),
    };
    match write_output(
        memory,
        &expanded,
        wide,
        buffer,
        size,
        flags & ALLOCATE_BUFFER != 0,
    ) {
        Ok(written) => Ok(written),
        Err(error) => state.fail(teb, error, 0),
    }
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    for (symbol, wide) in [("FormatMessageW", true), ("FormatMessageA", false)] {
        let service_state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let teb = cpu.gs_base;
            let flags = args.get(0, memory).map_err(ExitReason::MemoryFault)? as u32;
            let source = args.get(1, memory).map_err(ExitReason::MemoryFault)?;
            let identifier = args.get(2, memory).map_err(ExitReason::MemoryFault)? as u32;
            // The language identifier is accepted and ignored: the table is
            // the en-US one, documented in docs/interfaces.md.
            let _language = args.get(3, memory).map_err(ExitReason::MemoryFault)?;
            let buffer = args.get(4, memory).map_err(ExitReason::MemoryFault)?;
            let size = args.get(5, memory).map_err(ExitReason::MemoryFault)?;
            let arguments = args.get(6, memory).map_err(ExitReason::MemoryFault)?;
            format_message(
                &service_state,
                teb,
                memory,
                flags,
                source,
                identifier,
                buffer,
                size,
                arguments,
                wide,
            )
        });
    }

    // The error mode controls error dialogs that do not exist here; it is
    // stored and returned faithfully so programs that propagate it - a
    // parent passing it to a child, say - behave coherently.
    let service_state = Arc::clone(state);
    registry.register(KERNEL32, "SetErrorMode", move |args, _, memory| {
        let mode = args.get(0, memory).map_err(ExitReason::MemoryFault)? as u32;
        Ok(u64::from(service_state.exchange_error_mode(mode)))
    });

    for symbol in ["FormatMessageA", "FormatMessageW", "SetErrorMode"] {
        registry.alias(ERRHANDLING_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }

    // No debugger is attached to a Xenora guest, so the documented answer is
    // the honest one: false. A program that changes its behavior under a
    // debugger takes its non-debugged path, which is what a real machine
    // without a debugger does.
    registry.register(KERNEL32, "IsDebuggerPresent", move |_, _, _| Ok(0));

    // OutputDebugString writes its string to the attached debugger, and the
    // documentation is explicit that with no debugger attached the output
    // goes nowhere. There is no debugger here, so the honest behavior is
    // that the string has no observable effect. The text is still read so a
    // bad pointer fails the way it would with a debugger attached.
    for symbol in ["OutputDebugStringA", "OutputDebugStringW"] {
        registry.register(KERNEL32, symbol, move |args, _, memory| {
            let text = args.rcx;
            if text != 0 {
                // Touch the first unit so a faulting pointer raises the same
                // memory fault a debugger-attached read would.
                let mut unit = [0u8; 2];
                memory
                    .read_bytes(text, &mut unit)
                    .map_err(ExitReason::MemoryFault)?;
            }
            Ok(0)
        });
    }

    for symbol in [
        "IsDebuggerPresent",
        "OutputDebugStringA",
        "OutputDebugStringW",
    ] {
        registry.alias(ERRHANDLING_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Implemented);
    }

    registry.register_context_service(
        KERNEL32,
        "DebugBreak",
        ServiceStatus::Partial,
        crate::exceptions::debug_break,
    );
    registry.alias("api-ms-win-core-debug-l1-1-2.dll", KERNEL32, "DebugBreak");
}
