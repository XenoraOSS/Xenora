// SPDX-License-Identifier: GPL-3.0-or-later

//! The wide-character services of package 6 of #459, first part: the
//! conversions, the classification, and the wide string forms that are not
//! the printf family.
//!
//! The one multibyte encoding the emulator has is the C locale's: each byte
//! is the wide character of the same value, zero-extended, and a wide
//! character above 255 has no multibyte form. That is what the C locale
//! documents, and the documentation here says it is all Xenora answers:
//! there is no locale machinery to pick another encoding with.
//!
//! `wchar_t` is the 16-bit Windows one, little-endian in memory.

use super::crt::set_errno;
use super::crt_streams::{c_string, fault};
use crate::cpu::ExitReason;
use crate::memory::{GuestMemory, PROCESS_HEAP};

/// `EILSEQ`: a wide character that has no multibyte form in the encoding.
const EILSEQ: u32 = 42;

/// Reads one wide character.
fn read_wide(memory: &GuestMemory, address: u64) -> Result<u16, crate::memory::MemoryError> {
    memory.read_u16(address)
}

/// Writes the little-endian wide characters of `wide` at `address`,
/// terminated with a zero wide character.
fn write_wide(
    memory: &mut GuestMemory,
    address: u64,
    wide: &[u16],
) -> Result<(), crate::memory::MemoryError> {
    for (index, unit) in wide.iter().enumerate() {
        memory.write_u16(address + 2 * index as u64, *unit)?;
    }
    memory.write_u16(address + 2 * wide.len() as u64, 0)
}

/// Reads the zero-terminated wide string at `address`, at most `limit`
/// units.
fn wide_string(
    memory: &GuestMemory,
    address: u64,
    limit: u64,
) -> Result<Vec<u16>, crate::memory::MemoryError> {
    let mut wide = Vec::new();
    for index in 0..limit {
        let unit = read_wide(memory, address + 2 * index)?;
        if unit == 0 {
            break;
        }
        wide.push(unit);
    }
    Ok(wide)
}

/// The length of the zero-terminated wide string at `address`.
fn wide_len(memory: &GuestMemory, address: u64) -> Result<u64, crate::memory::MemoryError> {
    let mut length = 0;
    while read_wide(memory, address + 2 * length)? != 0 {
        length += 1;
    }
    Ok(length)
}

/// `mbtowc`: one wide character from the multibyte string at `text`, which
/// holds at most `count` bytes. Returns the bytes consumed, 0 for the null
/// character, -1 with `EILSEQ` for input the encoding cannot hold — which
/// the identity encoding never produces, as every byte converts.
pub(super) fn multibyte_to_wide(
    memory: &mut GuestMemory,
    teb: u64,
    wide: u64,
    text: u64,
    count: u64,
) -> Result<u64, ExitReason> {
    // A null string pointer asks whether the encoding has shift states.
    // The identity encoding has none.
    if text == 0 {
        return Ok(0);
    }
    if count == 0 {
        let _ = set_errno(memory, teb, EILSEQ);
        return Ok(u64::MAX);
    }
    let byte = memory.read_u8(text).map_err(fault)?;
    if byte == 0 {
        return Ok(0);
    }
    if wide != 0 {
        memory.write_u16(wide, u16::from(byte)).map_err(fault)?;
    }
    Ok(1)
}

/// `wctomb`: the multibyte form of `wide` at `text`, returning its length.
/// A wide character above 255 has none in this encoding.
pub(super) fn wide_to_multibyte(
    memory: &mut GuestMemory,
    teb: u64,
    text: u64,
    wide: u64,
) -> Result<u64, ExitReason> {
    if text == 0 {
        return Ok(0);
    }
    if wide > 0xFF {
        let _ = set_errno(memory, teb, EILSEQ);
        return Ok(u64::MAX);
    }
    memory.write_u8(text, wide as u8).map_err(fault)?;
    Ok(1)
}

/// `mblen`: the length of the multibyte character at `text` — always one
/// byte in this encoding, 0 for the null character.
pub(super) fn multibyte_length(
    memory: &mut GuestMemory,
    _teb: u64,
    text: u64,
    count: u64,
) -> Result<u64, ExitReason> {
    if text == 0 {
        return Ok(0);
    }
    if count == 0 {
        return Ok(u64::MAX);
    }
    let byte = memory.read_u8(text).map_err(fault)?;
    Ok(u64::from(byte != 0))
}

/// `mbstowcs`: converts the multibyte string at `text` into at most `count`
/// wide characters at `wide`. Returns the wide characters written, not
/// counting the terminator — which is written only when the source's null
/// character converts within the limit. A null destination is the length
/// query form.
pub(super) fn multibyte_string_to_wide(
    memory: &mut GuestMemory,
    _teb: u64,
    wide: u64,
    text: u64,
    count: u64,
) -> Result<u64, ExitReason> {
    let limit = if wide == 0 {
        65_536
    } else {
        count.saturating_add(1)
    };
    let bytes = c_string(memory, text, limit).map_err(fault)?;
    if wide == 0 {
        return Ok(bytes.len() as u64);
    }
    let mut written = 0u64;
    for byte in bytes.iter().copied() {
        if written >= count {
            break;
        }
        memory
            .write_u16(wide + 2 * written, u16::from(byte))
            .map_err(fault)?;
        written += 1;
    }
    if written < count && written == bytes.len() as u64 {
        memory.write_u16(wide + 2 * written, 0).map_err(fault)?;
    }
    Ok(written)
}

/// `wcstombs`: the inverse. A wide character above 255 has no multibyte
/// form and fails the conversion with `EILSEQ`.
pub(super) fn wide_string_to_multibyte(
    memory: &mut GuestMemory,
    teb: u64,
    text: u64,
    wide: u64,
    count: u64,
) -> Result<u64, ExitReason> {
    let limit = if text == 0 {
        65_536
    } else {
        count.saturating_add(1)
    };
    let units = wide_string(memory, wide, limit).map_err(fault)?;
    if units.iter().any(|unit| *unit > 0xFF) {
        let _ = set_errno(memory, teb, EILSEQ);
        return Ok(u64::MAX);
    }
    // A null destination is the length query form; it converts everything.
    if text == 0 {
        return Ok(units.len() as u64);
    }
    let mut written = 0u64;
    for unit in units.iter().copied() {
        if written >= count {
            break;
        }
        memory.write_u8(text + written, unit as u8).map_err(fault)?;
        written += 1;
    }
    if written < count && written == units.len() as u64 {
        memory.write_u8(text + written, 0).map_err(fault)?;
    }
    Ok(written)
}

/// `mbsrtowcs`/`wcsrtombs`: the restartable forms. The encoding has no
/// shift state, so they are the plain forms with a state argument the
/// service leaves alone.
pub(super) fn mbsrtowcs(
    memory: &mut GuestMemory,
    teb: u64,
    wide: u64,
    source: u64,
    count: u64,
) -> Result<u64, ExitReason> {
    // `source` holds the address of the string pointer, which the conversion
    // advances past what it consumed.
    let text = memory.read_u64(source).map_err(fault)?;
    if text == 0 {
        return Ok(0);
    }
    let converted = multibyte_string_to_wide(memory, teb, wide, text, count)?;
    if wide != 0 {
        let advanced = if converted == u64::MAX {
            0
        } else {
            converted.min(count)
        };
        let byte_at_end = memory.read_u8(text + advanced).map_err(fault)?;
        let next = if byte_at_end == 0 { 0 } else { text + advanced };
        memory.write_u64(source, next).map_err(fault)?;
    }
    Ok(converted)
}

pub(super) fn wcsrtombs(
    memory: &mut GuestMemory,
    teb: u64,
    text: u64,
    source: u64,
    count: u64,
) -> Result<u64, ExitReason> {
    let wide = memory.read_u64(source).map_err(fault)?;
    if wide == 0 {
        return Ok(0);
    }
    let converted = wide_string_to_multibyte(memory, teb, text, wide, count)?;
    if text != 0 && converted != u64::MAX {
        memory
            .write_u64(source, wide + 2 * converted.min(count))
            .map_err(fault)?;
    }
    Ok(converted)
}

/// ASCII classification of the C locale, exact. Characters at or above 128
/// are none of the classes the C locale knows.
fn ascii(class: Class, wide: u32) -> bool {
    let byte = u8::try_from(wide).unwrap_or(0x80);
    let ascii = byte < 0x80;
    match class {
        Class::Alpha => ascii && byte.is_ascii_alphabetic(),
        Class::Digit => ascii && byte.is_ascii_digit(),
        Class::Alnum => ascii && byte.is_ascii_alphanumeric(),
        Class::Lower => ascii && byte.is_ascii_lowercase(),
        Class::Upper => ascii && byte.is_ascii_uppercase(),
        Class::Space => ascii && matches!(byte, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r'),
    }
}

#[derive(Clone, Copy)]
enum Class {
    Alpha,
    Digit,
    Alnum,
    Lower,
    Upper,
    Space,
}

pub(super) fn iswalpha(_memory: &mut GuestMemory, _teb: u64, wide: u64) -> Result<u64, ExitReason> {
    Ok(u64::from(ascii(Class::Alpha, wide as u32)))
}

pub(super) fn iswdigit(_memory: &mut GuestMemory, _teb: u64, wide: u64) -> Result<u64, ExitReason> {
    Ok(u64::from(ascii(Class::Digit, wide as u32)))
}

pub(super) fn iswalnum(_memory: &mut GuestMemory, _teb: u64, wide: u64) -> Result<u64, ExitReason> {
    Ok(u64::from(ascii(Class::Alnum, wide as u32)))
}

pub(super) fn iswlower(_memory: &mut GuestMemory, _teb: u64, wide: u64) -> Result<u64, ExitReason> {
    Ok(u64::from(ascii(Class::Lower, wide as u32)))
}

pub(super) fn iswupper(_memory: &mut GuestMemory, _teb: u64, wide: u64) -> Result<u64, ExitReason> {
    Ok(u64::from(ascii(Class::Upper, wide as u32)))
}

pub(super) fn iswspace(_memory: &mut GuestMemory, _teb: u64, wide: u64) -> Result<u64, ExitReason> {
    Ok(u64::from(ascii(Class::Space, wide as u32)))
}

pub(super) fn towlower(_memory: &mut GuestMemory, _teb: u64, wide: u64) -> Result<u64, ExitReason> {
    let byte = wide as u32;
    Ok(u64::from(
        if byte < 0x80 && (byte as u8).is_ascii_uppercase() {
            byte + 32
        } else {
            byte
        },
    ))
}

pub(super) fn towupper(_memory: &mut GuestMemory, _teb: u64, wide: u64) -> Result<u64, ExitReason> {
    let byte = wide as u32;
    Ok(u64::from(
        if byte < 0x80 && (byte as u8).is_ascii_lowercase() {
            byte - 32
        } else {
            byte
        },
    ))
}

/// `iswctype`: the predicate behind every `isw*` macro in the Universal
/// CRT. The bits are mingw-w64's `<corecrt_wctype.h>`: `_UPPER` 1,
/// `_LOWER` 2, `_DIGIT` 4, `_SPACE` 8, `_PUNCT` 0x10, `_CONTROL` 0x20,
/// `_BLANK` 0x40, `_HEX` 0x80, `_ALPHA` 0x103. The answer is the ASCII
/// table of the C locale.
pub(super) fn iswctype(
    _memory: &mut GuestMemory,
    _teb: u64,
    wide: u64,
    mask: u64,
) -> Result<u64, ExitReason> {
    let byte = u8::try_from(wide as u32).unwrap_or(0x80);
    let ascii = byte < 0x80;
    let mut bits = 0u64;
    if ascii {
        if byte.is_ascii_uppercase() {
            bits |= 0x1;
        }
        if byte.is_ascii_lowercase() {
            bits |= 0x2;
        }
        if byte.is_ascii_digit() {
            bits |= 0x4;
        }
        if matches!(byte, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r') {
            bits |= 0x8;
        }
        if byte.is_ascii_punctuation() {
            bits |= 0x10;
        }
        if byte.is_ascii_control() {
            bits |= 0x20;
        }
        if byte == b' ' || byte == b'\t' {
            bits |= 0x40;
        }
        if byte.is_ascii_hexdigit() {
            bits |= 0x80;
        }
        if byte.is_ascii_alphabetic() {
            bits |= 0x100;
        }
    }
    Ok(u64::from(bits & mask != 0))
}

/// `wcsncat`: appends at most `count` wide characters of `end` to `start`.
pub(super) fn wcsncat(
    memory: &mut GuestMemory,
    _teb: u64,
    start: u64,
    end: u64,
    count: u64,
) -> Result<u64, ExitReason> {
    let length = wide_len(memory, start).map_err(fault)?;
    let suffix = wide_string(memory, end, count.saturating_add(1)).map_err(fault)?;
    let mut index = 0u64;
    while index < count && (index as usize) < suffix.len() {
        memory
            .write_u16(start + 2 * (length + index), suffix[index as usize])
            .map_err(fault)?;
        index += 1;
    }
    memory
        .write_u16(start + 2 * (length + index), 0)
        .map_err(fault)?;
    Ok(start)
}

/// `_wcsdup`: a copy of the wide string in memory the CRT allocates, which
/// the guest frees. The CRT allocates with `malloc`; here the process heap
/// stands in for it, as it does for the `FILE` objects.
pub(super) fn wcsdup(memory: &mut GuestMemory, _teb: u64, text: u64) -> Result<u64, ExitReason> {
    let wide = wide_string(memory, text, 65_536).map_err(fault)?;
    let copy = memory
        .map_heap_allocation(PROCESS_HEAP, 2 * (wide.len() + 1))
        .map_err(fault)?;
    write_wide(memory, copy, &wide).map_err(fault)?;
    Ok(copy)
}
