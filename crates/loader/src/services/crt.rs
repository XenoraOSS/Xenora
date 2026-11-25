// SPDX-License-Identifier: GPL-3.0-or-later

//! C runtime services that operate on checked guest memory.

use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus, Win64Args};
use crate::memory::{GuestMemory, MemoryError, PROCESS_HEAP};
use crate::runtime::ServiceContext;

use super::crt_files;
use super::crt_streams::{Failed, Outcome};

const SCAN_BUDGET: u64 = 1_048_576;
const UCRT: &str = "ucrtbase.dll";
const CRT_STRING_API: &str = "api-ms-win-crt-string-l1-1-0.dll";
/// The legacy CRT module that pre-UCRT mingw binaries import directly.
const MSVCRT: &str = "msvcrt.dll";

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

fn compare_strings(
    memory: &GuestMemory,
    left: u64,
    right: u64,
    limit: u64,
    unit: StringUnit,
) -> Result<u64, ExitReason> {
    let budget = SCAN_BUDGET / unit.width();
    for offset in 0..limit.min(budget) {
        let a = unit.read(memory, left, offset)?;
        let b = unit.read(memory, right, offset)?;
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
            compare_strings(memory, args.rcx, args.rdx, u64::MAX, unit)
        });
        registry.register(UCRT, bounded_compare, move |args, _, memory| {
            compare_strings(memory, args.rcx, args.rdx, args.r8, unit)
        });
        for symbol in [length, bounded_length, compare, bounded_compare] {
            registry.alias(CRT_STRING_API, UCRT, symbol);
            // The legacy CRT exports the same narrow and wide string forms.
            registry.alias(MSVCRT, UCRT, symbol);
            registry.set_status(UCRT, symbol, ServiceStatus::Partial);
        }
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
        // The legacy CRT exports the memory forms under the same names.
        registry.alias(MSVCRT, UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
    // The mingw-w64 C runtime imports `memcpy`, `memmove` and `memcmp`
    // through this API set.
    registry.alias("api-ms-win-crt-private-l1-1-0.dll", UCRT, "memcpy");
    registry.alias("api-ms-win-crt-private-l1-1-0.dll", UCRT, "memmove");
    registry.alias("api-ms-win-crt-private-l1-1-0.dll", UCRT, "memcmp");
}

const ENOMEM: u32 = 12;
pub(super) const EINVAL: u32 = 22;

pub(super) fn set_errno(memory: &mut GuestMemory, teb: u64, error: u32) -> Result<(), ExitReason> {
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
        // The legacy CRT carries the same errno accessors.
        registry.alias(MSVCRT, UCRT, symbol);
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
        // The legacy CRT exports the heap forms under the same names.
        registry.alias(MSVCRT, UCRT, symbol);
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

/// The Universal C runtime services that a C++ guest's runtime imports and
/// that this runtime models. They are called through the generated adapters
/// in `runtime_table.rs`, so each takes the service context and the
/// arguments the registry declares.
///
/// The result of a service that returns a value and may set `errno`: the
/// handler sets `errno` to the second field and returns the first.
fn or_errno<T>(context: &mut ServiceContext<'_>, outcome: Outcome<T>) -> Result<T, ExitReason> {
    match outcome? {
        Ok(value) => Ok(value),
        Err(Failed(value, errno)) => {
            set_errno(context.memory, context.teb, errno)?;
            Ok(value)
        }
    }
}

/// `fputc`. Writes the character converted to `unsigned char` to the stream
/// and returns it, or `EOF` with `errno` set (stdio.h).
pub fn fputc(
    context: &mut ServiceContext<'_>,
    character: i32,
    stream: u64,
) -> Result<i32, ExitReason> {
    let outcome = {
        let mut streams = context.state.crt_streams();
        let mut drive = context.state.objects.lock().expect("guest object state");
        streams.put_character(&mut drive, character, stream)
    };
    or_errno(context, outcome)
}

/// `_lock_file`. The lock itself does nothing: Xenora's guest scheduler runs
/// one guest thread at a time, so two guest threads can never hold the same
/// stream at once. That is a consequence of the execution model, not a
/// swallowed error.
pub fn _lock_file(
    context: &mut ServiceContext<'_>,
    stream: u64,
    _mode: i32,
) -> Result<i32, ExitReason> {
    let outcome = context.state.crt_streams().lock(stream);
    or_errno(context, outcome)
}

/// `_unlock_file`. The counterpart of `_lock_file`, and as empty.
pub fn _unlock_file(context: &mut ServiceContext<'_>, stream: u64) -> Result<i32, ExitReason> {
    let outcome = context.state.crt_streams().lock(stream);
    or_errno(context, outcome)
}

/// The message `strerror` gives for an error number. The runtime's own error
/// texts are used, so that `perror` and `strerror` name an error the same
/// way. Any other number gets the generic text.
fn error_message(code: i32) -> &'static str {
    match code {
        5 => "Input/output error",
        code if code < 0 => "Unknown error",
        code => crt_files::error_text(code as u32),
    }
}

/// `strerror`. Returns a pointer to a NUL-terminated message for the error
/// number. The message stays valid until the next call.
pub fn strerror(context: &mut ServiceContext<'_>, code: i32) -> Result<u64, ExitReason> {
    let text = error_message(code);
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    let address = {
        let mut streams = context.state.crt_streams();
        streams.message_block(context.memory, bytes.len() as u64)?
    };
    context
        .memory
        .write_bytes(address, &bytes)
        .map_err(ExitReason::MemoryFault)?;
    Ok(address)
}

/// The C locale's `struct lconv` (corecrt_locale.h): twelve pointers and
/// eight characters, in the order Microsoft documents.
const LCONV_BYTES: usize = 104;
/// The offset of the first character member of `struct lconv`.
const LCONV_CHARACTERS: usize = 80;
/// `CHAR_MAX`, the value of a member the current locale does not provide.
const CHAR_MAX: u8 = 127;

/// One pointer of `struct lconv`.
fn store_pointer(image: &mut [u8; LCONV_BYTES], offset: usize, value: u64) {
    image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// `localeconv`. Returns a pointer to the C locale's `struct lconv`. Only
/// the C locale is modelled: no other locale can be set.
pub fn localeconv(context: &mut ServiceContext<'_>) -> Result<u64, ExitReason> {
    // The decimal point of the C locale, and the empty string that every
    // other string member points at.
    let decimal = b".\0";
    let empty = b"\0";
    let size = (LCONV_BYTES + decimal.len() + empty.len()) as u64;
    let address = {
        let mut streams = context.state.crt_streams();
        streams.locale_block(context.memory, size)?
    };
    let decimal_at = address + LCONV_BYTES as u64;
    let empty_at = decimal_at + decimal.len() as u64;
    let mut image = [0u8; LCONV_BYTES];
    store_pointer(&mut image, 0, decimal_at);
    for offset in [8usize, 16, 24, 32, 40, 48, 56, 64, 72] {
        store_pointer(&mut image, offset, empty_at);
    }
    // The C locale has no monetary formatting, so none of its numeric
    // members is available and every one of them is `CHAR_MAX`.
    image[LCONV_CHARACTERS..LCONV_CHARACTERS + 8].fill(CHAR_MAX);
    context
        .memory
        .write_bytes(address, &image)
        .map_err(ExitReason::MemoryFault)?;
    context
        .memory
        .write_bytes(decimal_at, decimal)
        .map_err(ExitReason::MemoryFault)?;
    context
        .memory
        .write_bytes(empty_at, empty)
        .map_err(ExitReason::MemoryFault)?;
    Ok(address)
}

/// The characters of the C locale are the basic character set, so a byte
/// above 0x7F is not a valid single-byte character in it.
const C_LOCALE_LAST: u8 = 0x7F;
/// `(size_t)-1`, the result of a conversion that fails.
const CONVERSION_FAILED: u64 = u64::MAX;

/// `mbrtowc`. The C locale is single-byte, so one byte is one character.
/// The registry declares the multibyte character by value rather than
/// through a pointer, so the byte to convert is the `character` argument.
///
/// The conversion happens whether or not `output` points anywhere; nothing is
/// written then. The `mbstate_t` is not used in the C locale, so a null
/// `state` is accepted and a `state` that is not null is left alone.
pub fn mbrtowc(
    context: &mut ServiceContext<'_>,
    output: u64,
    count: u64,
    character: i32,
    _state: u64,
) -> Result<u64, ExitReason> {
    if count == 0 {
        return Ok(0);
    }
    let byte = character as u8;
    if byte > C_LOCALE_LAST {
        // Not a valid single-byte character in the C locale.
        return Ok(CONVERSION_FAILED);
    }
    if output != 0 {
        // The null character is the one wide character the C locale writes
        // as zero.
        context
            .memory
            .write_u32(output, u32::from(byte))
            .map_err(ExitReason::MemoryFault)?;
    }
    // The null character is a character of zero bytes.
    Ok(u64::from(byte != 0))
}

/// `wcrtomb`. The C locale is single-byte, so a wide character of the basic
/// character set is one byte. The conversion happens whether or not `output`
/// points anywhere; nothing is written then. The `mbstate_t` is not used in
/// the C locale, so a null `state` is accepted and a `state` that is not
/// null is left alone.
pub fn wcrtomb(
    context: &mut ServiceContext<'_>,
    output: u64,
    character: u32,
    _state: u64,
) -> Result<u64, ExitReason> {
    let byte = character;
    if byte > u32::from(C_LOCALE_LAST) {
        // The wide character cannot be represented in the C locale.
        return Ok(CONVERSION_FAILED);
    }
    if output != 0 {
        context
            .memory
            .write_u8(output, byte as u8)
            .map_err(ExitReason::MemoryFault)?;
    }
    Ok(1)
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

/// Copies a narrow string with its terminator; bounded, the copy is zero
/// padded like strncpy when the source is shorter than the count.
fn copy_string(
    memory: &mut GuestMemory,
    dest: u64,
    source: u64,
    count: u64,
) -> Result<(), ExitReason> {
    let length = string_length(memory, source, count, StringUnit::Byte)?;
    let copied = if count == u64::MAX {
        length + 1
    } else {
        length.min(count)
    };
    if copied != 0 {
        let mut bytes = transfer_buffer(copied, 0)?;
        memory
            .read_bytes(source, &mut bytes)
            .map_err(ExitReason::MemoryFault)?;
        memory
            .write_bytes(dest, &bytes)
            .map_err(ExitReason::MemoryFault)?;
    }
    if count != u64::MAX && copied < count {
        let zeroes = transfer_buffer(count - copied, 0)?;
        memory
            .write_bytes(
                dest.checked_add(copied)
                    .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?,
                &zeroes,
            )
            .map_err(ExitReason::MemoryFault)?;
    } else if count == u64::MAX || length >= count {
        // The unbounded form always terminates; the bounded one does not
        // when the source fills the count exactly.
        if count == u64::MAX {
            memory
                .write_u8(
                    dest.checked_add(length)
                        .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?,
                    0,
                )
                .map_err(ExitReason::MemoryFault)?;
        }
    }
    Ok(())
}

/// The remaining small CRT utilities of the survey: string reversal and
/// case conversion in place, duplication, the C-locale collation forms,
/// the Annex K copy forms, the missing character classes, the integer
/// formatting family and `getenv_s` over the empty environment.
pub(super) fn register_utilities(registry: &mut ImportRegistry) {
    // strrchr: the last occurrence, the terminator included in the scan.
    registry.register(UCRT, "strrchr", |args, _, memory| {
        let wanted = args.rdx as u8;
        let mut address = args.rcx;
        let mut last = 0u64;
        loop {
            let byte = memory.read_u8(address).map_err(ExitReason::MemoryFault)?;
            if byte == wanted {
                last = address;
            }
            if byte == 0 {
                return Ok(last);
            }
            address = address
                .checked_add(1)
                .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
        }
    });

    // _strdup: a heap copy the caller frees.
    registry.register(UCRT, "_strdup", |args, cpu, memory| {
        let length = string_length(memory, args.rcx, u64::MAX, StringUnit::Byte)?;
        let result =
            memory.map_heap_allocation(crate::memory::PROCESS_HEAP, (length + 1).max(1) as usize);
        let address = allocation_result(memory, cpu.gs_base, result)?;
        if address == 0 {
            return Ok(0);
        }
        let mut bytes = transfer_buffer(length + 1, 0)?;
        memory
            .read_bytes(args.rcx, &mut bytes)
            .map_err(ExitReason::MemoryFault)?;
        memory
            .write_bytes(address, &bytes)
            .map_err(ExitReason::MemoryFault)?;
        Ok(address)
    });

    // Case conversion and reversal in place, ASCII like the C locale.
    for (symbol, upper) in [("_strlwr", false), ("_strupr", true)] {
        registry.register(UCRT, symbol, move |args, _, memory| {
            let mut address = args.rcx;
            loop {
                let byte = memory.read_u8(address).map_err(ExitReason::MemoryFault)?;
                if byte == 0 {
                    return Ok(args.rcx);
                }
                let converted = if upper {
                    byte.to_ascii_uppercase()
                } else {
                    byte.to_ascii_lowercase()
                };
                if converted != byte {
                    memory
                        .write_u8(address, converted)
                        .map_err(ExitReason::MemoryFault)?;
                }
                address = address
                    .checked_add(1)
                    .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
            }
        });
    }

    registry.register(UCRT, "_strrev", |args, _, memory| {
        let length = string_length(memory, args.rcx, u64::MAX, StringUnit::Byte)?;
        if length > 1 {
            let mut bytes = transfer_buffer(length, 0)?;
            memory
                .read_bytes(args.rcx, &mut bytes)
                .map_err(ExitReason::MemoryFault)?;
            bytes.reverse();
            memory
                .write_bytes(args.rcx, &bytes)
                .map_err(ExitReason::MemoryFault)?;
        }
        Ok(args.rcx)
    });

    // The C locale collates byte-wise: strcoll is strcmp and strxfrm the
    // identity copy with the documented length semantics.
    registry.register(UCRT, "strcoll", |args, _, memory| {
        compare_strings(memory, args.rcx, args.rdx, u64::MAX, StringUnit::Byte)
    });

    registry.register(UCRT, "strxfrm", |args, _, memory| {
        let length = string_length(memory, args.rdx, u64::MAX, StringUnit::Byte)?;
        if args.r8 > length {
            // Only when the whole string fits is the output defined.
            let mut bytes = transfer_buffer(length + 1, 0)?;
            memory
                .read_bytes(args.rdx, &mut bytes)
                .map_err(ExitReason::MemoryFault)?;
            memory
                .write_bytes(args.rcx, &bytes)
                .map_err(ExitReason::MemoryFault)?;
        } else if args.r8 != 0 {
            // A small buffer copies what fits and the output is undefined
            // content-wise, but the copy itself is what programs observe.
            let mut bytes = transfer_buffer(args.r8, 0)?;
            memory
                .read_bytes(args.rdx, &mut bytes)
                .map_err(ExitReason::MemoryFault)?;
            memory
                .write_bytes(args.rcx, &bytes)
                .map_err(ExitReason::MemoryFault)?;
        }
        Ok(length)
    });

    // The Annex K copy forms: null pointers are invalid, a count over the
    // destination size zeroes the destination and answers ERANGE.
    for (symbol, overlapping) in [("memcpy_s", false), ("memmove_s", true)] {
        registry.register(UCRT, symbol, move |args, cpu, memory| {
            let (dest, dest_size, source, count) = (args.rcx, args.rdx, args.r8, args.r9);
            if dest == 0 || source == 0 {
                set_errno(memory, cpu.gs_base, EINVAL)?;
                return Ok(u64::from(EINVAL));
            }
            if count > dest_size {
                // The constraint violation zeroes the destination, as
                // documented, before answering the range error.
                if dest_size != 0 {
                    let zeroes = transfer_buffer(dest_size, 0)?;
                    memory
                        .write_bytes(dest, &zeroes)
                        .map_err(ExitReason::MemoryFault)?;
                }
                set_errno(memory, cpu.gs_base, ERANGE)?;
                return Ok(u64::from(ERANGE));
            }
            if count != 0 {
                if !overlapping {
                    // memcpy_s also rejects actual overlap; the ranges are
                    // half-open, the documented test.
                    let dest_end = dest.wrapping_add(count);
                    let source_end = source.wrapping_add(count);
                    if dest < source_end && source < dest_end {
                        let zeroes = transfer_buffer(dest_size.max(1), 0)?;
                        memory
                            .write_bytes(dest, &zeroes[..dest_size as usize])
                            .map_err(ExitReason::MemoryFault)?;
                        set_errno(memory, cpu.gs_base, EINVAL)?;
                        return Ok(u64::from(EINVAL));
                    }
                }
                let mut bytes = transfer_buffer(count, 0)?;
                memory
                    .read_bytes(source, &mut bytes)
                    .map_err(ExitReason::MemoryFault)?;
                memory
                    .write_bytes(dest, &bytes)
                    .map_err(ExitReason::MemoryFault)?;
            }
            Ok(0)
        });
    }

    // The missing character classes, ASCII like the rest of the C locale.
    registry.register(UCRT, "isblank", |args, _, _| {
        Ok(u64::from(
            matches!(args.rcx as u8, b' ' | b'\t') && args.rcx < 0x80,
        ))
    });
    registry.register(UCRT, "isgraph", |args, _, _| {
        Ok(u64::from((0x21..=0x7e).contains(&(args.rcx as u32))))
    });

    // The integer formatting family: radix 2 through 36, lowercase digits,
    // a minus sign for signed negative input in radix ten only, as
    // documented. The _s forms check the buffer size.
    for (symbol, bits, signed) in [
        ("_itoa", 32u32, true),
        ("_ltoa", 32, true),
        ("_i64toa", 64, true),
        ("_ultoa", 32, false),
        ("_ui64toa", 64, false),
    ] {
        registry.register(UCRT, symbol, move |args, cpu, memory| {
            format_integer(
                memory,
                cpu.gs_base,
                args.rcx,
                args.rdx,
                args.r8 as u32,
                0,
                bits,
                signed,
            )
        });
    }
    registry.register(UCRT, "_itoa_s", |args, cpu, memory| {
        format_integer(
            memory,
            cpu.gs_base,
            args.rcx,
            args.rdx,
            args.r9 as u32,
            args.r8,
            32,
            true,
        )
    });

    // getenv_s over the empty environment: every variable is undefined,
    // which is the documented empty answer, not an error.
    registry.register(UCRT, "getenv_s", |args, cpu, memory| {
        let (size_at, buffer, buffer_size, name) = (args.rcx, args.rdx, args.r8, args.r9);
        if size_at == 0 || name == 0 {
            set_errno(memory, cpu.gs_base, EINVAL)?;
            return Ok(u64::from(EINVAL));
        }
        memory
            .write_u64(size_at, 0)
            .map_err(ExitReason::MemoryFault)?;
        // Undefined variable: length zero, the buffer untouched when it
        // cannot hold even the terminator - documented as no error.
        if buffer != 0 && buffer_size != 0 {
            memory
                .write_u8(buffer, 0)
                .map_err(ExitReason::MemoryFault)?;
        }
        Ok(0)
    });

    // The copying and concatenation forms mingw inlines least often.
    registry.register(UCRT, "strcpy", |args, _, memory| {
        copy_string(memory, args.rcx, args.rdx, u64::MAX)?;
        Ok(args.rcx)
    });
    registry.register(UCRT, "strncpy", |args, _, memory| {
        copy_string(memory, args.rcx, args.rdx, args.r8)?;
        Ok(args.rcx)
    });
    registry.register(UCRT, "strcat", |args, _, memory| {
        let length = string_length(memory, args.rcx, u64::MAX, StringUnit::Byte)?;
        let end = args
            .rcx
            .checked_add(length)
            .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
        copy_string(memory, end, args.rdx, u64::MAX)?;
        Ok(args.rcx)
    });
    registry.register(UCRT, "strncat", |args, _, memory| {
        let length = string_length(memory, args.rcx, u64::MAX, StringUnit::Byte)?;
        let end = args
            .rcx
            .checked_add(length)
            .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
        copy_string(memory, end, args.rdx, args.r8)?;
        memory
            .write_u8(
                end.checked_add(string_length(memory, args.rdx, args.r8, StringUnit::Byte)?)
                    .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?,
                0,
            )
            .map_err(ExitReason::MemoryFault)?;
        Ok(args.rcx)
    });

    registry.alias("api-ms-win-crt-private-l1-1-0.dll", UCRT, "strrchr");
    registry.alias("api-ms-win-crt-private-l1-1-0.dll", UCRT, "strcpy");
    registry.alias("api-ms-win-crt-private-l1-1-0.dll", UCRT, "strncpy");
    registry.alias("api-ms-win-crt-private-l1-1-0.dll", UCRT, "strcat");
    registry.alias("api-ms-win-crt-private-l1-1-0.dll", UCRT, "strncat");

    for (symbol, api) in [
        ("strcpy", CRT_STRING_API),
        ("strncpy", CRT_STRING_API),
        ("strcat", CRT_STRING_API),
        ("strncat", CRT_STRING_API),
        ("strrchr", CRT_STRING_API),
        ("_strdup", CRT_STRING_API),
        ("_strlwr", CRT_STRING_API),
        ("_strupr", CRT_STRING_API),
        ("_strrev", CRT_STRING_API),
        ("strcoll", CRT_STRING_API),
        ("strxfrm", CRT_STRING_API),
        ("memcpy_s", CRT_STRING_API),
        ("memmove_s", CRT_STRING_API),
        ("isblank", "api-ms-win-crt-string-l1-1-0.dll"),
        ("isgraph", "api-ms-win-crt-string-l1-1-0.dll"),
        ("_itoa", "api-ms-win-crt-convert-l1-1-0.dll"),
        ("_ltoa", "api-ms-win-crt-convert-l1-1-0.dll"),
        ("_i64toa", "api-ms-win-crt-convert-l1-1-0.dll"),
        ("_ultoa", "api-ms-win-crt-convert-l1-1-0.dll"),
        ("_ui64toa", "api-ms-win-crt-convert-l1-1-0.dll"),
        ("_itoa_s", "api-ms-win-crt-convert-l1-1-0.dll"),
        ("getenv_s", "api-ms-win-crt-environment-l1-1-0.dll"),
    ] {
        registry.alias(api, UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
}

/// The shared integer formatting core: radix 2 through 36, lowercase
/// digits, the sign only for negative signed input in radix ten.
#[allow(clippy::too_many_arguments)]
fn format_integer(
    memory: &mut GuestMemory,
    teb: u64,
    value: u64,
    buffer: u64,
    radix: u32,
    size: u64,
    bits: u32,
    signed: bool,
) -> Result<u64, ExitReason> {
    if buffer == 0 || !(2..=36).contains(&radix) {
        if signed || bits == 32 {
            set_errno(memory, teb, EINVAL)?;
        }
        return Ok(if size == 0 { 0 } else { u64::from(EINVAL) });
    }
    // Normalize to the width the function formats.
    let normalized = if bits == 32 {
        u64::from(value as u32)
    } else {
        value
    };
    let negative = signed && radix == 10 && {
        let sign_bit = 1u64 << (bits - 1);
        normalized & sign_bit != 0
    };
    let magnitude = if negative {
        // The two's complement magnitude, correct for the minimum value.
        if bits == 32 {
            u64::from((normalized as u32).wrapping_neg())
        } else {
            normalized.wrapping_neg()
        }
    } else {
        normalized
    };
    let mut digits = Vec::new();
    let mut remaining = magnitude;
    loop {
        let digit = (remaining % u64::from(radix)) as u8;
        digits.push(if digit < 10 {
            b'0' + digit
        } else {
            b'a' + digit - 10
        });
        remaining /= u64::from(radix);
        if remaining == 0 {
            break;
        }
    }
    if negative {
        digits.push(b'-');
    }
    digits.reverse();
    digits.push(0);
    let needed = digits.len() as u64;
    if size != 0 {
        // The _s form: too small is an invalid parameter with zeroed start.
        if needed > size {
            memory
                .write_u8(buffer, 0)
                .map_err(ExitReason::MemoryFault)?;
            set_errno(memory, teb, EINVAL)?;
            return Ok(u64::from(EINVAL));
        }
        memory
            .write_bytes(buffer, &digits)
            .map_err(ExitReason::MemoryFault)?;
        return Ok(0);
    }
    memory
        .write_bytes(buffer, &digits)
        .map_err(ExitReason::MemoryFault)?;
    Ok(buffer)
}
