// SPDX-License-Identifier: GPL-3.0-or-later

//! The narrow `ctype.h` classification and case conversion of the C locale,
//! and the narrow `atoi`/`atof` decimal parses.
//!
//! Legacy mingw binaries bind these straight out of `msvcrt.dll`; the same
//! spellings are the Universal CRT's own. The classification is the ASCII
//! table of the C locale, the only locale the machine models, so characters
//! at or above 128 are in no class - the same rule the wide forms in
//! `crt_wide.rs` implement.
//!
//! The classification predicates answer 1 or 0, which the truthiness
//! contract permits. `EOF` (-1) is in no class and converts to itself, as
//! the CRT documents. What none of them models: a locale other than the C
//! locale, since the machine has one locale.
//!
//! `atoi` is the decimal `strtol` contract without end pointer, base or
//! error channel: leading ASCII whitespace, an optional sign and decimal
//! digits, zero when no digit follows. Overflow is the C contract's
//! undefined behaviour; the machine clamps, which no correct program can
//! observe. `atof` is the decimal floating parse of the same shape - an
//! optional exponent after the fraction - and the result is the nearest
//! binary64 value, which is what the correctly rounded CRT computes.
//! Hexadecimal floating inputs and the `inf`/`nan` words are not part of
//! the batch and read as zero, honestly short of the CRT.

use super::crt_streams::{c_string, fault};
use crate::cpu::ExitReason;
use crate::memory::GuestMemory;

/// ASCII classification of the C locale, exact. Characters at or above 128
/// are none of the classes the C locale knows.
pub(super) fn ascii(class: Class, code: u32) -> bool {
    let byte = u8::try_from(code).unwrap_or(0x80);
    let ascii = byte < 0x80;
    match class {
        Class::Alpha => ascii && byte.is_ascii_alphabetic(),
        Class::Digit => ascii && byte.is_ascii_digit(),
        Class::Alnum => ascii && byte.is_ascii_alphanumeric(),
        Class::Lower => ascii && byte.is_ascii_lowercase(),
        Class::Upper => ascii && byte.is_ascii_uppercase(),
        Class::Space => ascii && matches!(byte, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r'),
        Class::Punct => ascii && byte.is_ascii_punctuation(),
        Class::Control => ascii && byte.is_ascii_control(),
        Class::Print => ascii && (byte.is_ascii_graphic() || byte == b' '),
        Class::Blank => ascii && matches!(byte, b' ' | b'\t'),
        Class::Graph => ascii && byte.is_ascii_graphic(),
        Class::HexDigit => ascii && byte.is_ascii_hexdigit(),
    }
}

#[derive(Clone, Copy)]
pub(super) enum Class {
    Alpha,
    Digit,
    Alnum,
    Lower,
    Upper,
    Space,
    Punct,
    Control,
    Print,
    Blank,
    Graph,
    HexDigit,
}

macro_rules! narrow_class {
    ($name:ident, $class:expr) => {
        pub(super) fn $name(
            _memory: &mut GuestMemory,
            _teb: u64,
            character: u64,
        ) -> Result<u64, ExitReason> {
            // The CRT spells the argument `int` and defines the classes
            // for 0 through 255 plus EOF (-1); anything outside is the
            // contract's undefined behaviour, read here as no class.
            let code = character as u32;
            if code > 0xff {
                return Ok(0);
            }
            Ok(u64::from(ascii($class, code)))
        }
    };
}

narrow_class!(isalpha, Class::Alpha);
narrow_class!(isdigit, Class::Digit);
narrow_class!(isalnum, Class::Alnum);
narrow_class!(islower, Class::Lower);
narrow_class!(isupper, Class::Upper);
narrow_class!(isspace, Class::Space);
narrow_class!(ispunct, Class::Punct);
narrow_class!(iscntrl, Class::Control);
narrow_class!(isprint, Class::Print);
narrow_class!(isgraph, Class::Graph);
narrow_class!(isblank, Class::Blank);
narrow_class!(isxdigit, Class::HexDigit);

pub(super) fn tolower(
    _memory: &mut GuestMemory,
    _teb: u64,
    character: u64,
) -> Result<u64, ExitReason> {
    let code = character as u32;
    Ok(u64::from(if code <= 0xff && ascii(Class::Upper, code) {
        code + 32
    } else {
        code
    }))
}

pub(super) fn toupper(
    _memory: &mut GuestMemory,
    _teb: u64,
    character: u64,
) -> Result<u64, ExitReason> {
    let code = character as u32;
    Ok(u64::from(if code <= 0xff && ascii(Class::Lower, code) {
        code - 32
    } else {
        code
    }))
}

fn skip_space(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && matches!(bytes[at], b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r') {
        at += 1;
    }
    at
}

pub(super) fn atoi(memory: &mut GuestMemory, _teb: u64, text: u64) -> Result<u64, ExitReason> {
    let bytes = c_string(memory, text, 256).map_err(fault)?;
    let mut at = skip_space(&bytes, 0);
    let negative = match bytes.get(at) {
        Some(b'-') => {
            at += 1;
            true
        }
        Some(b'+') => {
            at += 1;
            false
        }
        _ => false,
    };
    let mut value: i64 = 0;
    let mut digits = false;
    while let Some(byte) = bytes.get(at) {
        if !byte.is_ascii_digit() {
            break;
        }
        digits = true;
        value = value
            .saturating_mul(10)
            .saturating_add(i64::from(byte - b'0'))
            .min(i64::from(i32::MAX) + 1);
        at += 1;
    }
    if !digits {
        return Ok(0);
    }
    let signed = if negative { -value } else { value };
    let clamped = signed.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    Ok(u64::from(clamped as u32))
}

pub(super) fn atof(memory: &mut GuestMemory, _teb: u64, text: u64) -> Result<u64, ExitReason> {
    let bytes = c_string(memory, text, 512).map_err(fault)?;
    let mut at = skip_space(&bytes, 0);
    let start = at;
    if matches!(bytes.get(at), Some(b'-') | Some(b'+')) {
        at += 1;
    }
    let mut digits = false;
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        digits = true;
        at += 1;
    }
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            digits = true;
            at += 1;
        }
    }
    if !digits {
        return Ok(0.0f64.to_bits());
    }
    if matches!(bytes.get(at), Some(b'e') | Some(b'E')) {
        let mut exponent = at + 1;
        if matches!(bytes.get(exponent), Some(b'-') | Some(b'+')) {
            exponent += 1;
        }
        if bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
            at = exponent;
            while bytes.get(at).is_some_and(u8::is_ascii_digit) {
                at += 1;
            }
        }
    }
    let slice = &bytes[start..at];
    let text = core::str::from_utf8(slice).unwrap_or("0");
    Ok(text.parse::<f64>().unwrap_or(0.0).to_bits())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifies(class: Class, byte: u8) -> bool {
        ascii(class, u32::from(byte))
    }

    #[test]
    fn narrow_classification_is_the_c_locale() {
        assert!(classifies(Class::Alpha, b'a') && classifies(Class::Alpha, b'Z'));
        assert!(!classifies(Class::Alpha, b'0') && !classifies(Class::Alpha, 0xC0));
        assert!(classifies(Class::Digit, b'9') && !classifies(Class::Digit, b'x'));
        assert!(classifies(Class::Alnum, b'F') && classifies(Class::Alnum, b'7'));
        assert!(!classifies(Class::Alnum, b'_'));
        assert!(classifies(Class::Lower, b'z') && !classifies(Class::Lower, b'Z'));
        assert!(classifies(Class::Upper, b'Q') && !classifies(Class::Upper, b'q'));
        for space in [b' ', b'\t', b'\n', 0x0B, 0x0C, b'\r'] {
            assert!(classifies(Class::Space, space));
        }
        assert!(!classifies(Class::Space, b'0'));
        assert!(classifies(Class::Punct, b'!') && classifies(Class::Punct, b'_'));
        assert!(!classifies(Class::Punct, b'a') && !classifies(Class::Punct, b' '));
        assert!(classifies(Class::Control, 0x7F) && classifies(Class::Control, 0x01));
        assert!(!classifies(Class::Control, b' '));
        assert!(classifies(Class::Print, b' ') && classifies(Class::Print, b'~'));
        assert!(!classifies(Class::Print, 0x7F));
        assert!(classifies(Class::Graph, b'!') && !classifies(Class::Graph, b' '));
        assert!(classifies(Class::HexDigit, b'f') && classifies(Class::HexDigit, b'F'));
        assert!(classifies(Class::HexDigit, b'9') && !classifies(Class::HexDigit, b'g'));
        assert!(classifies(Class::Blank, b' ') && classifies(Class::Blank, b'\t'));
        assert!(!classifies(Class::Blank, b'\n') && !classifies(Class::Blank, b'a'));
    }

    #[test]
    fn out_of_range_characters_are_in_no_class() {
        let mut memory = GuestMemory::new();
        for value in [
            0x100_u64,
            0x141,
            u64::from(u32::MAX),
            u64::from(0xFFFF_FF41_u32),
        ] {
            assert_eq!(isalpha(&mut memory, 0, value).unwrap(), 0, "{value:#x}");
            assert_eq!(isdigit(&mut memory, 0, value).unwrap(), 0, "{value:#x}");
        }
        // EOF converts to itself; an upper-case ASCII converts down.
        assert_eq!(
            tolower(&mut memory, 0, u64::from(u32::MAX)).unwrap(),
            u64::from(u32::MAX)
        );
        assert_eq!(
            tolower(&mut memory, 0, u64::from(b'A')).unwrap(),
            u64::from(b'a')
        );
    }
}
