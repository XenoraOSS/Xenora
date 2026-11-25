// SPDX-License-Identifier: GPL-3.0-or-later

//! C runtime services that operate on checked guest memory.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus, Win64Args};
use crate::memory::{GuestMemory, MemoryError, PROCESS_HEAP};
use std::sync::Arc;
use std::sync::atomic::Ordering;

const SCAN_BUDGET: u64 = 1_048_576;
const UCRT: &str = "ucrtbase.dll";
const CRT_STRING_API: &str = "api-ms-win-crt-string-l1-1-0.dll";

#[derive(Clone, Copy)]
enum StringUnit {
    Byte,
    Wide,
}

impl StringUnit {
    fn width(self) -> u64 {
        match self {
            Self::Byte => 1,
            Self::Wide => 2,
        }
    }

    fn read(self, memory: &GuestMemory, base: u64, offset: u64) -> Result<u16, ExitReason> {
        if base == 0 {
            return Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(0)));
        }
        let address = base
            .checked_add(offset * self.width())
            .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
        match self {
            Self::Byte => memory.read_u8(address).map(u16::from),
            Self::Wide => memory.read_u16(address),
        }
        .map_err(ExitReason::MemoryFault)
    }
}

fn string_length(
    memory: &GuestMemory,
    address: u64,
    limit: u64,
    unit: StringUnit,
) -> Result<u64, ExitReason> {
    const BLOCK: u64 = 256;
    let budget = SCAN_BUDGET / unit.width();
    let scan = limit.min(budget);
    let mut buffer = [0u8; 2 * BLOCK as usize];
    let mut offset = 0;
    while offset < scan {
        let count = BLOCK.min(scan - offset);
        let bytes = &mut buffer[..(count * unit.width()) as usize];
        // A block that crosses into unmapped or unreadable memory is scanned
        // one unit at a time, so a terminator before the boundary still ends
        // the scan and the fault reports the first inaccessible unit.
        let block = address
            .checked_add(offset * unit.width())
            .filter(|_| address != 0)
            .is_some_and(|start| memory.read_bytes(start, bytes).is_ok());
        for index in 0..count {
            let value = if block {
                match unit {
                    StringUnit::Byte => u16::from(bytes[index as usize]),
                    StringUnit::Wide => {
                        let at = 2 * index as usize;
                        u16::from_le_bytes([bytes[at], bytes[at + 1]])
                    }
                }
            } else {
                unit.read(memory, address, offset + index)?
            };
            if value == 0 {
                return Ok(offset + index);
            }
        }
        offset += count;
    }
    if limit <= budget {
        return Ok(limit);
    }
    Err(ExitReason::IllegalInstruction(0xFE))
}

fn fold_ascii(unit: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&unit) {
        unit + 32
    } else {
        unit
    }
}

fn compare_strings(
    memory: &GuestMemory,
    left: u64,
    right: u64,
    limit: u64,
    unit: StringUnit,
    case_insensitive: bool,
) -> Result<u64, ExitReason> {
    let budget = SCAN_BUDGET / unit.width();
    for offset in 0..limit.min(budget) {
        let mut a = unit.read(memory, left, offset)?;
        let mut b = unit.read(memory, right, offset)?;
        if case_insensitive {
            a = fold_ascii(a);
            b = fold_ascii(b);
        }
        if a != b || a == 0 {
            return Ok(u64::from((i32::from(a) - i32::from(b)) as u32));
        }
    }
    if limit <= budget {
        return Ok(0);
    }
    Err(ExitReason::IllegalInstruction(0xFE))
}

fn transfer_buffer(count: u64, fill: u8) -> Result<Vec<u8>, ExitReason> {
    if count > SCAN_BUDGET {
        return Err(ExitReason::IllegalInstruction(0xFE));
    }
    let count = count as usize;
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(count)
        .map_err(|_| ExitReason::MemoryFault(MemoryError::AllocationFailed))?;
    buffer.resize(count, fill);
    Ok(buffer)
}

pub(super) fn register(registry: &mut ImportRegistry) {
    for (length, bounded_length, compare, bounded_compare, unit) in [
        ("strlen", "strnlen", "strcmp", "strncmp", StringUnit::Byte),
        ("wcslen", "wcsnlen", "wcscmp", "wcsncmp", StringUnit::Wide),
    ] {
        registry.register(UCRT, length, move |args, _, memory| {
            string_length(memory, args.rcx, u64::MAX, unit)
        });
        registry.register(UCRT, bounded_length, move |args, _, memory| {
            string_length(memory, args.rcx, args.rdx, unit)
        });
        registry.register(UCRT, compare, move |args, _, memory| {
            compare_strings(memory, args.rcx, args.rdx, u64::MAX, unit, false)
        });
        registry.register(UCRT, bounded_compare, move |args, _, memory| {
            compare_strings(memory, args.rcx, args.rdx, args.r8, unit, false)
        });
        for symbol in [length, bounded_length, compare, bounded_compare] {
            registry.alias(CRT_STRING_API, UCRT, symbol);
            registry.set_status(UCRT, symbol, ServiceStatus::Partial);
        }
    }

    // C-locale ASCII case folding only (not Unicode case mapping).
    for (symbol, bounded, unit) in [
        ("_stricmp", false, StringUnit::Byte),
        ("_strnicmp", true, StringUnit::Byte),
        ("_wcsicmp", false, StringUnit::Wide),
        ("_wcsnicmp", true, StringUnit::Wide),
    ] {
        registry.register(UCRT, symbol, move |args, _, memory| {
            let limit = if bounded { args.r8 } else { u64::MAX };
            compare_strings(memory, args.rcx, args.rdx, limit, unit, true)
        });
        registry.alias(CRT_STRING_API, UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }

    registry.register(UCRT, "memset", |args, _, memory| {
        if args.r8 != 0 {
            let buffer = transfer_buffer(args.r8, args.rdx as u8)?;
            memory
                .write_bytes(args.rcx, &buffer)
                .map_err(ExitReason::MemoryFault)?;
        }
        Ok(args.rcx)
    });

    for symbol in ["memcpy", "memmove"] {
        registry.register(UCRT, symbol, |args, _, memory| {
            if args.r8 != 0 {
                let mut buffer = transfer_buffer(args.r8, 0)?;
                memory
                    .read_bytes(args.rdx, &mut buffer)
                    .map_err(ExitReason::MemoryFault)?;
                memory
                    .write_bytes(args.rcx, &buffer)
                    .map_err(ExitReason::MemoryFault)?;
            }
            Ok(args.rcx)
        });
    }

    registry.register(UCRT, "memcmp", |args, _, memory| {
        if args.r8 == 0 {
            return Ok(0);
        }
        let mut left = transfer_buffer(args.r8, 0)?;
        let mut right = transfer_buffer(args.r8, 0)?;
        memory
            .read_bytes(args.rcx, &mut left)
            .map_err(ExitReason::MemoryFault)?;
        memory
            .read_bytes(args.rdx, &mut right)
            .map_err(ExitReason::MemoryFault)?;
        let result = left
            .iter()
            .zip(right)
            .find_map(|(&a, b)| (a != b).then_some(i32::from(a) - i32::from(b)))
            .unwrap_or(0);
        Ok(u64::from(result as u32))
    });

    for symbol in ["memset", "memcpy", "memmove", "memcmp"] {
        registry.alias(CRT_STRING_API, UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }

    registry.register(UCRT, "memchr", |args, _, memory| {
        if args.r8 == 0 {
            return Ok(0);
        }
        let mut buffer = transfer_buffer(args.r8, 0)?;
        memory
            .read_bytes(args.rcx, &mut buffer)
            .map_err(ExitReason::MemoryFault)?;
        Ok(buffer
            .iter()
            .position(|&byte| byte == args.rdx as u8)
            .map_or(0, |index| args.rcx + index as u64))
    });

    registry.register(UCRT, "strcpy", |args, _, memory| {
        copy_string(memory, args.rcx, args.rdx, u64::MAX, true)
    });
    registry.register(UCRT, "strncpy", |args, _, memory| {
        copy_string(memory, args.rcx, args.rdx, args.r8, false)
    });
    registry.register(UCRT, "strcat", |args, _, memory| {
        append_string(memory, args.rcx, args.rdx, u64::MAX)
    });
    registry.register(UCRT, "strncat", |args, _, memory| {
        append_string(memory, args.rcx, args.rdx, args.r8)
    });
    for (symbol, unit) in [("strstr", StringUnit::Byte), ("wcsstr", StringUnit::Wide)] {
        registry.register(UCRT, symbol, move |args, _, memory| {
            find_substring(memory, args.rcx, args.rdx, unit)
        });
    }
    registry.register(UCRT, "strspn", |args, _, memory| {
        span_length(memory, args.rcx, args.rdx, true)
    });
    registry.register(UCRT, "strcspn", |args, _, memory| {
        span_length(memory, args.rcx, args.rdx, false)
    });
    registry.register(UCRT, "strpbrk", |args, _, memory| {
        let offset = span_length(memory, args.rcx, args.rdx, false)?;
        match StringUnit::Byte.read(memory, args.rcx, offset)? {
            0 => Ok(0),
            _ => Ok(args.rcx + offset),
        }
    });
    for (symbol, accept) in [("wcsspn", true), ("wcscspn", false)] {
        registry.register(UCRT, symbol, move |args, _, memory| {
            wide_span(memory, args.rcx, args.rdx, accept)
        });
    }
    registry.register(UCRT, "wcspbrk", |args, _, memory| {
        let offset = wide_span(memory, args.rcx, args.rdx, false)?;
        match StringUnit::Wide.read(memory, args.rcx, offset)? {
            0 => Ok(0),
            _ => Ok(args.rcx + offset * 2),
        }
    });
    registry.register(UCRT, "strtok", |args, cpu, memory| {
        tokenize(memory, cpu.gs_base, args.rcx, args.rdx)
    });
    for (symbol, unit) in [
        ("strtok_s", StringUnit::Byte),
        ("wcstok_s", StringUnit::Wide),
    ] {
        registry.register(UCRT, symbol, move |args, cpu, memory| {
            tokenize_s(memory, cpu.gs_base, args.rcx, args.rdx, args.r8, unit)
        });
    }
    registry.register(UCRT, "wcstok", |args, _, memory| {
        tokenize_wide(memory, args.rcx, args.rdx, args.r8)
    });
    registry.register(UCRT, "wcscpy", |args, _, memory| {
        copy_wide(memory, args.rcx, args.rdx, u64::MAX, true)
    });
    registry.register(UCRT, "wcsncpy", |args, _, memory| {
        copy_wide(memory, args.rcx, args.rdx, args.r8, false)
    });
    registry.register(UCRT, "wcscat", |args, _, memory| {
        append_wide(memory, args.rcx, args.rdx)
    });
    registry.register(UCRT, "wcschr", |args, _, memory| {
        find_wide(memory, args.rcx, args.rdx as u16, true)
    });
    registry.register(UCRT, "wcsrchr", |args, _, memory| {
        find_wide(memory, args.rcx, args.rdx as u16, false)
    });
    for symbol in [
        "memchr", "strcpy", "strncpy", "strcat", "strncat", "strstr", "wcsstr", "strspn",
        "strcspn", "wcscpy", "wcsncpy", "wcscat", "wcschr", "wcsrchr", "strtok", "strpbrk",
        "wcsspn", "wcscspn", "wcspbrk", "wcstok", "strtok_s", "wcstok_s",
    ] {
        registry.alias(CRT_STRING_API, UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
}

fn find_substring(
    memory: &GuestMemory,
    haystack: u64,
    needle: u64,
    unit: StringUnit,
) -> Result<u64, ExitReason> {
    if haystack == 0 || needle == 0 {
        return Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(0)));
    }
    let length = string_length(memory, needle, u64::MAX, unit)?;
    if length == 0 {
        return Ok(haystack);
    }
    let pattern = (0..length)
        .map(|offset| unit.read(memory, needle, offset))
        .collect::<Result<Vec<_>, _>>()?;
    // Prefix lengths avoid rescanning repeated substrings in guest memory.
    let mut prefix = vec![0usize; pattern.len()];
    for index in 1..pattern.len() {
        let mut matched = prefix[index - 1];
        while matched != 0 && pattern[index] != pattern[matched] {
            matched = prefix[matched - 1];
        }
        prefix[index] = matched + usize::from(pattern[index] == pattern[matched]);
    }
    let mut matched = 0;
    for offset in 0..(SCAN_BUDGET / unit.width()) {
        let current = unit.read(memory, haystack, offset)?;
        if current == 0 {
            return Ok(0);
        }
        while matched != 0 && current != pattern[matched] {
            matched = prefix[matched - 1];
        }
        if current == pattern[matched] {
            matched += 1;
            if matched == pattern.len() {
                return Ok(haystack + (offset + 1 - length) * unit.width());
            }
        }
    }
    Err(ExitReason::IllegalInstruction(0xFE))
}

fn copy_wide(
    memory: &mut GuestMemory,
    dest: u64,
    source: u64,
    limit: u64,
    always_terminate: bool,
) -> Result<u64, ExitReason> {
    if dest == 0 || source == 0 {
        return Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(0)));
    }
    if limit == 0 {
        return Ok(dest);
    }
    let mut words = Vec::new();
    let scan = limit.min(SCAN_BUDGET / 2);
    for index in 0..scan {
        let word = memory
            .read_u16(source + index * 2)
            .map_err(ExitReason::MemoryFault)?;
        words.push(word);
        if word == 0 {
            break;
        }
        if !always_terminate && words.len() as u64 == limit {
            break;
        }
    }
    if always_terminate && words.last().copied() != Some(0) {
        return Err(ExitReason::IllegalInstruction(0xFE));
    }
    let mut bytes = Vec::with_capacity(words.len() * 2);
    for word in &words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    memory
        .write_bytes(dest, &bytes)
        .map_err(ExitReason::MemoryFault)?;
    if !always_terminate {
        let total = limit
            .checked_mul(2)
            .ok_or(ExitReason::MemoryFault(MemoryError::UnmappedAddress(dest)))?;
        pad_zeros(memory, dest, bytes.len() as u64, total)?;
    }
    Ok(dest)
}

/// Zero `dest[from..to]` for the strncpy family, which pads to the count.
/// The count is guest controlled, so the padding is written in bounded
/// chunks and the first unmapped or read-only byte faults; the whole count
/// is never allocated on the host.
fn pad_zeros(memory: &mut GuestMemory, dest: u64, from: u64, to: u64) -> Result<(), ExitReason> {
    const CHUNK: u64 = 4096;
    let zeros = [0u8; CHUNK as usize];
    let mut written = from;
    while written < to {
        let length = (to - written).min(CHUNK);
        let at = dest
            .checked_add(written)
            .ok_or(ExitReason::MemoryFault(MemoryError::UnmappedAddress(dest)))?;
        memory
            .write_bytes(at, &zeros[..length as usize])
            .map_err(ExitReason::MemoryFault)?;
        written += length;
    }
    Ok(())
}

fn append_wide(memory: &mut GuestMemory, dest: u64, source: u64) -> Result<u64, ExitReason> {
    let end = string_length(memory, dest, SCAN_BUDGET / 2, StringUnit::Wide)?;
    let mut words = Vec::new();
    for index in 0..(SCAN_BUDGET / 2) {
        let word = memory
            .read_u16(source + index * 2)
            .map_err(ExitReason::MemoryFault)?;
        words.push(word);
        if word == 0 {
            break;
        }
    }
    if words.last().copied() != Some(0) {
        return Err(ExitReason::IllegalInstruction(0xFE));
    }
    let mut bytes = Vec::with_capacity(words.len() * 2);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    memory
        .write_bytes(dest + end * 2, &bytes)
        .map_err(ExitReason::MemoryFault)?;
    Ok(dest)
}

fn find_wide(
    memory: &GuestMemory,
    address: u64,
    needle: u16,
    first: bool,
) -> Result<u64, ExitReason> {
    if address == 0 {
        return Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(0)));
    }
    let mut last = None;
    for index in 0..(SCAN_BUDGET / 2) {
        let word = memory
            .read_u16(address + index * 2)
            .map_err(ExitReason::MemoryFault)?;
        if word == needle {
            let found = address + index * 2;
            if first {
                return Ok(found);
            }
            last = Some(found);
        }
        if word == 0 {
            return Ok(if needle == 0 {
                address + index * 2
            } else {
                last.unwrap_or(0)
            });
        }
    }
    Err(ExitReason::IllegalInstruction(0xFE))
}

fn span_length(
    memory: &GuestMemory,
    address: u64,
    set: u64,
    accept: bool,
) -> Result<u64, ExitReason> {
    if address == 0 || set == 0 {
        return Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(0)));
    }
    let present = byte_set(memory, set)?;
    for index in 0..SCAN_BUDGET {
        let byte = memory
            .read_u8(address + index)
            .map_err(ExitReason::MemoryFault)?;
        if byte == 0 || present[byte as usize] != accept {
            return Ok(index);
        }
    }
    Err(ExitReason::IllegalInstruction(0xFE))
}

fn byte_set(memory: &GuestMemory, address: u64) -> Result<[bool; 256], ExitReason> {
    let mut present = [false; 256];
    for offset in 0..SCAN_BUDGET {
        let byte = StringUnit::Byte.read(memory, address, offset)? as usize;
        if byte == 0 {
            return Ok(present);
        }
        present[byte] = true;
    }
    Err(ExitReason::IllegalInstruction(0xFE))
}

fn wide_span(
    memory: &GuestMemory,
    address: u64,
    set: u64,
    accept: bool,
) -> Result<u64, ExitReason> {
    if address == 0 || set == 0 {
        return Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(0)));
    }
    let present = wide_set(memory, set)?;
    let limit = SCAN_BUDGET / 2;
    for offset in 0..limit {
        let unit = StringUnit::Wide.read(memory, address, offset)? as usize;
        let found = present[unit / 64] & (1 << (unit % 64)) != 0;
        if unit == 0 || found != accept {
            return Ok(offset);
        }
    }
    Err(ExitReason::IllegalInstruction(0xFE))
}

fn wide_set(memory: &GuestMemory, address: u64) -> Result<[u64; 1024], ExitReason> {
    let mut present = [0; 1024];
    for offset in 0..(SCAN_BUDGET / 2) {
        let unit = StringUnit::Wide.read(memory, address, offset)? as usize;
        if unit == 0 {
            return Ok(present);
        }
        present[unit / 64] |= 1 << (unit % 64);
    }
    Err(ExitReason::IllegalInstruction(0xFE))
}

fn locate_token(
    memory: &GuestMemory,
    cursor: u64,
    unit: StringUnit,
    delimiter: impl Fn(usize) -> bool,
) -> Result<(u64, u64, bool), ExitReason> {
    let mut token = None;
    for offset in 0..(SCAN_BUDGET / unit.width()) {
        let value = unit.read(memory, cursor, offset)? as usize;
        let position = cursor + offset * unit.width();
        if value == 0 {
            return Ok((token.unwrap_or(0), position, false));
        }
        if !delimiter(value) {
            token.get_or_insert(position);
        } else if let Some(token) = token {
            return Ok((token, position, true));
        }
    }
    Err(ExitReason::IllegalInstruction(0xFE))
}

fn tokenize_wide(
    memory: &mut GuestMemory,
    input: u64,
    delimiters: u64,
    context: u64,
) -> Result<u64, ExitReason> {
    if delimiters == 0 || context == 0 {
        return Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(0)));
    }
    let cursor = if input != 0 {
        input
    } else {
        memory.read_u64(context).map_err(ExitReason::MemoryFault)?
    };
    if cursor == 0 {
        return Ok(0);
    }
    let present = wide_set(memory, delimiters)?;
    let (token, position, split) = locate_token(memory, cursor, StringUnit::Wide, |unit| {
        present[unit / 64] & (1 << (unit % 64)) != 0
    })?;
    memory
        .check_write(context, 8)
        .map_err(ExitReason::MemoryFault)?;
    let next = if split {
        let next = position
            .checked_add(2)
            .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
        memory
            .write_u16(position, 0)
            .map_err(ExitReason::MemoryFault)?;
        next
    } else {
        0
    };
    memory
        .write_u64(context, next)
        .map_err(ExitReason::MemoryFault)?;
    Ok(token)
}

fn tokenize_s(
    memory: &mut GuestMemory,
    teb: u64,
    input: u64,
    delimiters: u64,
    context: u64,
    unit: StringUnit,
) -> Result<u64, ExitReason> {
    if delimiters == 0 || context == 0 {
        set_errno(memory, teb, EINVAL)?;
        return Ok(0);
    }
    let cursor = if input == 0 {
        memory.read_u64(context).map_err(ExitReason::MemoryFault)?
    } else {
        input
    };
    if cursor == 0 {
        set_errno(memory, teb, EINVAL)?;
        return Ok(0);
    }
    let (token, position, split) = match unit {
        StringUnit::Byte => {
            let present = byte_set(memory, delimiters)?;
            locate_token(memory, cursor, unit, |byte| present[byte])?
        }
        StringUnit::Wide => {
            let present = wide_set(memory, delimiters)?;
            locate_token(memory, cursor, unit, |value| {
                present[value / 64] & (1 << (value % 64)) != 0
            })?
        }
    };
    memory
        .check_write(context, 8)
        .map_err(ExitReason::MemoryFault)?;
    let next = if split {
        let next = position
            .checked_add(unit.width())
            .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
        match unit {
            StringUnit::Byte => memory.write_u8(position, 0),
            StringUnit::Wide => memory.write_u16(position, 0),
        }
        .map_err(ExitReason::MemoryFault)?;
        next
    } else {
        position
    };
    memory
        .write_u64(context, next)
        .map_err(ExitReason::MemoryFault)?;
    Ok(token)
}

fn tokenize(
    memory: &mut GuestMemory,
    teb: u64,
    input: u64,
    delimiters: u64,
) -> Result<u64, ExitReason> {
    let cursor = if input == 0 {
        let Some(&saved) = memory.crt_token_cursors.get(&teb) else {
            return Ok(0);
        };
        saved
    } else {
        input
    };
    if delimiters == 0 {
        return Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(0)));
    }
    let present = byte_set(memory, delimiters)?;
    let (token, position, split) =
        locate_token(memory, cursor, StringUnit::Byte, |byte| present[byte])?;
    if split {
        if memory.crt_token_cursors.len() >= 1024 && !memory.crt_token_cursors.contains_key(&teb) {
            return Err(ExitReason::MemoryFault(MemoryError::AllocationFailed));
        }
        let next = position
            .checked_add(1)
            .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
        memory
            .write_u8(position, 0)
            .map_err(ExitReason::MemoryFault)?;
        memory.crt_token_cursors.insert(teb, next);
    } else {
        memory.crt_token_cursors.remove(&teb);
    }
    Ok(token)
}

fn copy_string(
    memory: &mut GuestMemory,
    dest: u64,
    source: u64,
    limit: u64,
    always_terminate: bool,
) -> Result<u64, ExitReason> {
    if dest == 0 || source == 0 {
        return Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(0)));
    }
    if limit == 0 {
        return Ok(dest);
    }
    let mut copied = Vec::new();
    let scan = limit.min(SCAN_BUDGET);
    for index in 0..scan {
        let byte = memory
            .read_u8(source + index)
            .map_err(ExitReason::MemoryFault)?;
        copied.push(byte);
        if byte == 0 {
            break;
        }
        if !always_terminate && copied.len() as u64 == limit {
            break;
        }
    }
    if always_terminate && copied.last().copied() != Some(0) {
        return Err(ExitReason::IllegalInstruction(0xFE));
    }
    memory
        .write_bytes(dest, &copied)
        .map_err(ExitReason::MemoryFault)?;
    if !always_terminate {
        pad_zeros(memory, dest, copied.len() as u64, limit)?;
    }
    Ok(dest)
}

fn append_string(
    memory: &mut GuestMemory,
    dest: u64,
    source: u64,
    limit: u64,
) -> Result<u64, ExitReason> {
    let end = string_length(memory, dest, SCAN_BUDGET, StringUnit::Byte)?;
    if limit == 0 {
        memory
            .write_u8(dest + end, 0)
            .map_err(ExitReason::MemoryFault)?;
        return Ok(dest);
    }
    let mut extra = Vec::new();
    let scan = limit.min(SCAN_BUDGET);
    for index in 0..scan {
        let byte = memory
            .read_u8(source + index)
            .map_err(ExitReason::MemoryFault)?;
        if byte == 0 {
            break;
        }
        extra.push(byte);
    }
    extra.push(0);
    memory
        .write_bytes(dest + end, &extra)
        .map_err(ExitReason::MemoryFault)?;
    Ok(dest)
}

const ENOMEM: u32 = 12;
const EINVAL: u32 = 22;

fn set_errno(memory: &mut GuestMemory, teb: u64, error: u32) -> Result<(), ExitReason> {
    let address = memory
        .crt_errno_address(teb)
        .map_err(ExitReason::MemoryFault)?;
    memory
        .write_u32(address, error)
        .map_err(ExitReason::MemoryFault)
}

fn allocation_result(
    memory: &mut GuestMemory,
    teb: u64,
    result: Result<u64, MemoryError>,
) -> Result<u64, ExitReason> {
    match result {
        Ok(address) => Ok(address),
        Err(_) => {
            set_errno(memory, teb, ENOMEM)?;
            Ok(0)
        }
    }
}

const ERANGE: u32 = 34;

fn parse_integer(
    memory: &mut GuestMemory,
    args: &Win64Args,
    teb: u64,
    signed: bool,
    wide_result: bool,
    unit: StringUnit,
) -> Result<u64, ExitReason> {
    let (address, end, mut radix) = (args.rcx, args.rdx, args.r8 as u32);
    if address == 0 || (radix != 0 && !(2..=36).contains(&radix)) {
        if end != 0 {
            memory
                .write_u64(end, address)
                .map_err(ExitReason::MemoryFault)?;
        }
        set_errno(memory, teb, EINVAL)?;
        return Ok(0);
    }

    let read = |index| {
        if index >= SCAN_BUDGET / unit.width() {
            return Err(ExitReason::IllegalInstruction(0xFE));
        }
        unit.read(memory, address, index)
            .map(|value| char::from_u32(u32::from(value)).unwrap_or(char::REPLACEMENT_CHARACTER))
    };
    let mut index = 0;
    let mut next = read(index)?;
    while matches!(next, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c') {
        index += 1;
        next = read(index)?;
    }
    let negative = next == '-';
    if matches!(next, '+' | '-') {
        index += 1;
        next = read(index)?;
    }
    if next == '0'
        && matches!(radix, 0 | 16)
        && matches!(read(index + 1)?, 'x' | 'X')
        && read(index + 2)?.is_ascii_hexdigit()
    {
        radix = 16;
        index += 2;
        next = read(index)?;
    }
    if radix == 0 {
        radix = if next == '0' { 8 } else { 10 };
    }

    let limit = match (wide_result, signed) {
        (false, false) => u64::from(u32::MAX),
        (false, true) => i32::MAX as u64 + u64::from(negative),
        (true, false) => u64::MAX,
        (true, true) => i64::MAX as u64 + u64::from(negative),
    };
    let start = index;
    let mut value = 0u64;
    let mut overflow = false;
    while let Some(digit) = next.to_digit(radix) {
        match value
            .checked_mul(u64::from(radix))
            .and_then(|v| v.checked_add(u64::from(digit)))
        {
            Some(v) if v <= limit => value = v,
            _ => overflow = true,
        }
        index += 1;
        next = read(index)?;
    }
    let consumed = if start == index { 0 } else { index };
    if end != 0 {
        let pointer = address
            .checked_add(consumed * unit.width())
            .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
        memory
            .write_u64(end, pointer)
            .map_err(ExitReason::MemoryFault)?;
    }
    if overflow {
        set_errno(memory, teb, ERANGE)?;
        value = limit;
    }
    if negative && (!overflow || signed) {
        value = value.wrapping_neg();
    }
    Ok(if wide_result {
        value
    } else {
        u64::from(value as u32)
    })
}

pub(super) fn register_conversions(registry: &mut ImportRegistry) {
    for (symbol, signed, wide_result, unit) in [
        ("strtol", true, false, StringUnit::Byte),
        ("strtoul", false, false, StringUnit::Byte),
        ("wcstol", true, false, StringUnit::Wide),
        ("wcstoul", false, false, StringUnit::Wide),
        ("strtoll", true, true, StringUnit::Byte),
        ("strtoull", false, true, StringUnit::Byte),
        ("wcstoll", true, true, StringUnit::Wide),
        ("wcstoull", false, true, StringUnit::Wide),
        ("_strtoi64", true, true, StringUnit::Byte),
        ("_strtoui64", false, true, StringUnit::Byte),
        ("_wcstoi64", true, true, StringUnit::Wide),
        ("_wcstoui64", false, true, StringUnit::Wide),
    ] {
        registry.register(UCRT, symbol, move |args, cpu, memory| {
            parse_integer(memory, args, cpu.gs_base, signed, wide_result, unit)
        });
        registry.alias("api-ms-win-crt-convert-l1-1-0.dll", UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
    // `atoi` / `atol` and wide / 64-bit siblings: base 10, no end pointer.
    // On Windows LLP64, `long` is 32-bit, so `atol` matches `atoi`.
    for (symbol, wide_result, unit) in [
        ("atoi", false, StringUnit::Byte),
        ("atol", false, StringUnit::Byte),
        ("atoll", true, StringUnit::Byte),
        ("_atoi64", true, StringUnit::Byte),
        ("_wtoi", false, StringUnit::Wide),
        ("_wtol", false, StringUnit::Wide),
        ("_wtoi64", true, StringUnit::Wide),
    ] {
        registry.register(UCRT, symbol, move |args, cpu, memory| {
            let fixed = Win64Args {
                rcx: args.rcx,
                rdx: 0,
                r8: 10,
                r9: 0,
                rsp: args.rsp,
            };
            parse_integer(memory, &fixed, cpu.gs_base, true, wide_result, unit)
        });
        registry.alias("api-ms-win-crt-convert-l1-1-0.dll", UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
}

pub(super) fn register_errno(registry: &mut ImportRegistry) {
    registry.register(UCRT, "_errno", |_, cpu, memory| {
        memory
            .crt_errno_address(cpu.gs_base)
            .map_err(ExitReason::MemoryFault)
    });
    registry.register(UCRT, "_set_errno", |args, cpu, memory| {
        set_errno(memory, cpu.gs_base, args.rcx as u32)?;
        Ok(0)
    });
    registry.register(UCRT, "_get_errno", |args, cpu, memory| {
        if args.rcx == 0 {
            set_errno(memory, cpu.gs_base, EINVAL)?;
            return Ok(u64::from(EINVAL));
        }
        let address = memory
            .crt_errno_address(cpu.gs_base)
            .map_err(ExitReason::MemoryFault)?;
        let value = memory.read_u32(address).map_err(ExitReason::MemoryFault)?;
        memory
            .write_u32(args.rcx, value)
            .map_err(ExitReason::MemoryFault)?;
        Ok(0)
    });
    for symbol in ["_errno", "_set_errno", "_get_errno"] {
        registry.alias("api-ms-win-crt-runtime-l1-1-0.dll", UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
}

pub(super) fn register_heap(registry: &mut ImportRegistry) {
    registry.register(UCRT, "malloc", |args, cpu, memory| {
        let result = memory.map_heap_allocation(PROCESS_HEAP, args.rcx.max(1) as usize);
        allocation_result(memory, cpu.gs_base, result)
    });
    registry.register(UCRT, "calloc", |args, cpu, memory| {
        let result = args
            .rcx
            .checked_mul(args.rdx)
            .ok_or(MemoryError::AllocationFailed)
            .and_then(|size| memory.map_heap_allocation(PROCESS_HEAP, size.max(1) as usize));
        allocation_result(memory, cpu.gs_base, result)
    });
    registry.register(UCRT, "realloc", |args, cpu, memory| {
        let address = args.rcx;
        let size = args.rdx;
        if address == 0 {
            let result = memory.map_heap_allocation(PROCESS_HEAP, size.max(1) as usize);
            return allocation_result(memory, cpu.gs_base, result);
        }
        if size == 0 {
            free(memory, address)?;
            return Ok(0);
        }
        let result = memory.resize_heap_allocation(PROCESS_HEAP, address, size as usize, false);
        allocation_result(memory, cpu.gs_base, result)
    });
    registry.register(UCRT, "free", |args, _, memory| {
        free(memory, args.rcx)?;
        Ok(0)
    });

    for symbol in ["malloc", "calloc", "realloc", "free"] {
        registry.alias("api-ms-win-crt-heap-l1-1-0.dll", UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
}

fn free(memory: &mut GuestMemory, address: u64) -> Result<(), ExitReason> {
    if address == 0 || memory.release_heap_allocation(PROCESS_HEAP, address) {
        Ok(())
    } else {
        Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(
            address,
        )))
    }
}

/// MSVC-compatible linear congruential generator used by historical UCRT
/// `rand` / `srand` (publicly documented recurrence). Not cryptographic.
fn next_rand(seed: &std::sync::atomic::AtomicU32) -> u32 {
    let mut produced = 0u32;
    seed.try_update(Ordering::SeqCst, Ordering::SeqCst, |state| {
        let next = state.wrapping_mul(214013).wrapping_add(2531011);
        produced = (next >> 16) & 0x7fff;
        Some(next)
    })
    .expect("rand update always returns a value");
    produced
}

pub(super) fn register_rand(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let srand = Arc::clone(state);
    registry.register(UCRT, "srand", move |args, _, _| {
        srand.crt_rand.store(args.rcx as u32, Ordering::SeqCst);
        Ok(0)
    });
    let rand = Arc::clone(state);
    registry.register(UCRT, "rand", move |_, _, _| {
        Ok(u64::from(next_rand(&rand.crt_rand)))
    });
    let rand_s = Arc::clone(state);
    registry.register(UCRT, "rand_s", move |args, cpu, memory| {
        if args.rcx == 0 {
            set_errno(memory, cpu.gs_base, EINVAL)?;
            return Ok(u64::from(EINVAL));
        }
        // Fill a guest `unsigned int` with 15 bits of LCG output in the low
        // half, matching `rand`, rather than claiming CSPRNG strength.
        let value = next_rand(&rand_s.crt_rand);
        memory
            .write_u32(args.rcx, value)
            .map_err(ExitReason::MemoryFault)?;
        Ok(0)
    });
    for symbol in ["srand", "rand", "rand_s"] {
        registry.alias("api-ms-win-crt-utility-l1-1-0.dll", UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
}

#[cfg(test)]
#[test]
fn stricmp_folds_ascii_case_only() {
    use crate::cpu::{CpuState, Register};
    use crate::import_table::ImportName;

    let mut registry = ImportRegistry::new();
    register(&mut registry);
    let mut memory = GuestMemory::new();
    let mut cpu = CpuState::default();
    let left = memory.map_heap_allocation(PROCESS_HEAP, 8).unwrap();
    let right = memory.map_heap_allocation(PROCESS_HEAP, 8).unwrap();
    memory.write_bytes(left, b"AbC\0").unwrap();
    memory.write_bytes(right, b"aBc\0").unwrap();
    cpu.set(Register::Rcx, left);
    cpu.set(Register::Rdx, right);
    let service = registry
        .resolve(UCRT, &ImportName::Name("_stricmp".into()))
        .unwrap();
    assert_eq!(
        (service.handler)(&Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap(),
        0
    );
    memory.write_bytes(right, b"aBd\0").unwrap();
    let ordered = (service.handler)(&Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap() as i32;
    assert!(ordered < 0);
    cpu.set(Register::R8, 2);
    let nicmp = registry
        .resolve(UCRT, &ImportName::Name("_strnicmp".into()))
        .unwrap();
    assert_eq!(
        (nicmp.handler)(&Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap(),
        0
    );
}

#[cfg(test)]
#[test]
fn srand_rand_match_msvc_lcg_sequence() {
    use crate::cpu::{CpuState, Register};
    use crate::import_table::ImportName;
    use crate::services::MockClock;

    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_rand(&mut registry, &state);
    let mut memory = GuestMemory::new();
    let mut cpu = CpuState::default();
    let call = |symbol: &str, arg: u64, cpu: &mut CpuState, memory: &mut GuestMemory| {
        cpu.set(Register::Rcx, arg);
        let service = registry
            .resolve(UCRT, &ImportName::Name(symbol.to_owned()))
            .unwrap();
        (service.handler)(&Win64Args::new(cpu), cpu, memory).unwrap()
    };
    assert_eq!(call("srand", 1, &mut cpu, &mut memory), 0);
    // First three outputs after srand(1) for the documented MSVC LCG.
    assert_eq!(call("rand", 0, &mut cpu, &mut memory), 41);
    assert_eq!(call("rand", 0, &mut cpu, &mut memory), 18_467);
    assert_eq!(call("rand", 0, &mut cpu, &mut memory), 6_334);
}

#[cfg(test)]
#[test]
fn errno_isolated_by_guest_teb_on_one_host_thread() {
    use crate::cpu::{CpuState, Register};
    use crate::import_table::ImportName;

    let mut registry = ImportRegistry::new();
    register_errno(&mut registry);
    register_heap(&mut registry);
    let mut memory = GuestMemory::new();
    let mut first = CpuState {
        gs_base: 0x10000,
        ..CpuState::default()
    };
    let mut second = CpuState {
        gs_base: 0x20000,
        ..CpuState::default()
    };
    let call = |symbol: &str, cpu: &mut CpuState, memory: &mut GuestMemory| {
        let service = registry
            .resolve(UCRT, &ImportName::Name(symbol.to_owned()))
            .unwrap();
        (service.handler)(&Win64Args::new(cpu), cpu, memory).unwrap()
    };

    let first_slot = call("_errno", &mut first, &mut memory);
    first.set(Register::Rcx, 23);
    assert_eq!(call("_set_errno", &mut first, &mut memory), 0);
    let second_slot = call("_errno", &mut second, &mut memory);
    assert_ne!(first_slot, second_slot);
    assert_eq!(memory.read_u32(second_slot).unwrap(), 0);
    second.set(Register::Rcx, u64::MAX);
    assert_eq!(call("malloc", &mut second, &mut memory), 0);
    assert_eq!(memory.read_u32(second_slot).unwrap(), ENOMEM);
    assert_eq!(call("_errno", &mut first, &mut memory), first_slot);
    assert_eq!(memory.read_u32(first_slot).unwrap(), 23);
}

#[cfg(test)]
#[test]
fn strcpy_strncpy_strcat_and_memchr_operate_on_guest_memory() {
    use crate::cpu::{CpuState, Register};
    use crate::import_table::ImportName;

    let mut registry = ImportRegistry::new();
    register(&mut registry);
    let mut memory = GuestMemory::new();
    let mut cpu = CpuState::default();
    let call = |symbol: &str, args: [u64; 3], cpu: &mut CpuState, memory: &mut GuestMemory| {
        cpu.set(Register::Rcx, args[0]);
        cpu.set(Register::Rdx, args[1]);
        cpu.set(Register::R8, args[2]);
        let service = registry
            .resolve(UCRT, &ImportName::Name(symbol.to_owned()))
            .unwrap();
        (service.handler)(&Win64Args::new(cpu), cpu, memory).unwrap()
    };
    let dest = memory.map_heap_allocation(PROCESS_HEAP, 16).unwrap();
    let source = memory.map_heap_allocation(PROCESS_HEAP, 8).unwrap();
    memory.write_bytes(source, b"ab\0").unwrap();
    assert_eq!(
        call("strcpy", [dest, source, 0], &mut cpu, &mut memory),
        dest
    );
    let mut out = [0u8; 3];
    memory.read_bytes(dest, &mut out).unwrap();
    assert_eq!(&out, b"ab\0");
    assert_eq!(
        call("memchr", [dest, u64::from(b'b'), 3], &mut cpu, &mut memory),
        dest + 1
    );
    assert_eq!(
        call("strncpy", [dest, source, 4], &mut cpu, &mut memory),
        dest
    );
    let mut padded = [0u8; 4];
    memory.read_bytes(dest, &mut padded).unwrap();
    assert_eq!(&padded, b"ab\0\0");
    memory.write_bytes(dest, b"x\0").unwrap();
    assert_eq!(
        call("strcat", [dest, source, 0], &mut cpu, &mut memory),
        dest
    );
    let mut joined = [0u8; 4];
    memory.read_bytes(dest, &mut joined).unwrap();
    assert_eq!(&joined, b"xab\0");
}

#[cfg(test)]
#[test]
fn strtok_keeps_independent_cursors_and_accepts_new_delimiters() {
    use crate::memory::MemoryPermissions;

    let mut memory = GuestMemory::new();
    memory
        .map_region(0x1000, 0x100, MemoryPermissions::READ_WRITE)
        .unwrap();
    memory.write_bytes(0x1000, b",a,b;c\0").unwrap();
    memory.write_bytes(0x1020, b"x,y\0").unwrap();
    memory.write_bytes(0x1040, b",\0").unwrap();
    memory.write_bytes(0x1048, b";\0").unwrap();
    assert_eq!(tokenize(&mut memory, 1, 0x1000, 0x1040).unwrap(), 0x1001);
    assert_eq!(tokenize(&mut memory, 2, 0x1020, 0x1040).unwrap(), 0x1020);
    assert_eq!(memory.read_u8(0x1002).unwrap(), 0);
    assert!(tokenize(&mut memory, 1, 0, 0x2000).is_err());
    assert_eq!(tokenize(&mut memory, 1, 0, 0x1048).unwrap(), 0x1003);
    assert_eq!(tokenize(&mut memory, 2, 0, 0x1040).unwrap(), 0x1022);
    assert_eq!(tokenize(&mut memory, 1, 0, 0x1048).unwrap(), 0x1005);
    assert_eq!(tokenize(&mut memory, 1, 0, 0x1048).unwrap(), 0);
    assert_eq!(tokenize(&mut memory, 2, 0, 0x1040).unwrap(), 0);
}

#[cfg(test)]
#[test]
fn substring_search_handles_overlapping_prefixes_and_stops_at_first_match() {
    use crate::memory::MemoryPermissions;

    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(0x1000, 7, MemoryPermissions::READ_ONLY, b"aaaaaab")
        .unwrap();
    memory
        .map_region_with_data(0x2000, 5, MemoryPermissions::READ_ONLY, b"aaab\0")
        .unwrap();
    assert_eq!(
        find_substring(&memory, 0x1000, 0x2000, StringUnit::Byte).unwrap(),
        0x1003
    );
}

#[cfg(test)]
#[test]
fn wcstok_uses_explicit_context_and_preserves_input_on_invalid_context() {
    use crate::memory::MemoryPermissions;

    let mut memory = GuestMemory::new();
    memory
        .map_region(0x1000, 0x100, MemoryPermissions::READ_WRITE)
        .unwrap();
    memory
        .map_region(0x2000, 8, MemoryPermissions::READ_ONLY)
        .unwrap();
    for (index, value) in [0x03bb, 0x2603, 0x03bc, 0].into_iter().enumerate() {
        memory.write_u16(0x1000 + index as u64 * 2, value).unwrap();
    }
    memory.write_u16(0x1020, 0x2603).unwrap();
    assert!(tokenize_wide(&mut memory, 0x1000, 0x1020, 0x2000).is_err());
    assert_eq!(memory.read_u16(0x1002).unwrap(), 0x2603);
    assert_eq!(
        tokenize_wide(&mut memory, 0x1000, 0x1020, 0x1040).unwrap(),
        0x1000
    );
    assert_eq!(memory.read_u16(0x1002).unwrap(), 0);
    assert_eq!(memory.read_u64(0x1040).unwrap(), 0x1004);
    assert_eq!(tokenize_wide(&mut memory, 0, 0x1020, 0x1048).unwrap(), 0);
    assert_eq!(
        tokenize_wide(&mut memory, 0, 0x1020, 0x1040).unwrap(),
        0x1004
    );
    assert_eq!(memory.read_u64(0x1040).unwrap(), 0);
}

#[cfg(test)]
#[test]
fn strtok_s_keeps_context_and_errno_separate() {
    use crate::memory::MemoryPermissions;

    let mut memory = GuestMemory::new();
    memory
        .map_region(0x1000, 0x100, MemoryPermissions::READ_WRITE)
        .unwrap();
    memory.write_bytes(0x1000, b",a,b;c\0").unwrap();
    memory.write_bytes(0x1010, b"x,y\0").unwrap();
    memory.write_bytes(0x1020, b",\0;\0").unwrap();
    let call = |memory: &mut GuestMemory, input, delimiters, context| {
        tokenize_s(memory, 0, input, delimiters, context, StringUnit::Byte).unwrap()
    };
    let errno = memory.crt_errno_address(0).unwrap();
    assert_eq!(call(&mut memory, 0, 0x1020, 0x1040), 0);
    assert_eq!(memory.read_u32(errno).unwrap(), EINVAL);
    memory.write_u32(errno, 0).unwrap();
    assert_eq!(call(&mut memory, 0x1000, 0x1020, 0x1040), 0x1001);
    assert_eq!(call(&mut memory, 0x1010, 0x1020, 0x1048), 0x1010);
    assert_eq!(call(&mut memory, 0, 0x1022, 0x1040), 0x1003);
    assert_eq!(call(&mut memory, 0, 0x1022, 0x1040), 0x1005);
    assert_eq!(memory.read_u64(0x1040).unwrap(), 0x1006);
    assert_eq!(call(&mut memory, 0, 0x1022, 0x1040), 0);
    assert_eq!(call(&mut memory, 0, 0x1020, 0x1048), 0x1012);
    assert_eq!(memory.read_u32(errno).unwrap(), 0);
}
