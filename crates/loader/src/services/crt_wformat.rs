// SPDX-License-Identifier: GPL-3.0-or-later

//! Formatting for the wide `wprintf` family.
//!
//! The syntax is the narrow family's, from Microsoft's "Format specification
//! syntax: printf and wprintf functions": the flags, widths, precisions and
//! size prefixes are the same, and so are the integer and floating-point
//! types, which the narrow engine formats. What the wide family turns around
//! is the character and string types, from the same page: in a wide format,
//! `%c` and `%s` are the wide character and the wide string, `%C` and `%S`
//! the narrow ones, and the `h` and `l` prefixes force narrow and wide. The
//! output is wide, so a wide character or string is copied as it is and a
//! narrow one is zero-extended — no encoding is involved, as none is in the
//! C locale. Everything the narrow engine refuses is refused here too.

use crate::memory::MemoryError;
use crate::services::crt_format::{
    Flags, Floats, FormatError, Size, Spec, bounded, int, integer, pad,
};

/// What the wide conversions need beyond the narrow ones: the argument
/// slots, and both kinds of string.
pub(super) trait WideSource {
    /// The next 8-byte argument slot.
    fn argument(&mut self) -> Result<u64, MemoryError>;
    /// The units of the wide string at `address` up to its terminator, or
    /// the first `limit` of them. Never called with a null address.
    fn wide_string(&mut self, address: u64, limit: usize) -> Result<Vec<u16>, MemoryError>;
    /// The bytes of the narrow string at `address`, as `wide_string` reads
    /// the wide ones.
    fn narrow_string(&mut self, address: u64, limit: usize) -> Result<Vec<u8>, MemoryError>;
}

struct Parser<'a> {
    format: &'a [u16],
    at: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u16> {
        self.format.get(self.at).copied()
    }

    fn ascii(&self) -> Option<u8> {
        match self.peek() {
            Some(unit) if unit < 0x80 => Some(unit as u8),
            _ => None,
        }
    }

    fn take(&mut self, byte: u8) -> bool {
        if self.ascii() == Some(byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn number(&mut self) -> Result<usize, FormatError> {
        let mut value = 0usize;
        while let Some(byte) = self.ascii() {
            if !byte.is_ascii_digit() {
                break;
            }
            value = bounded(
                value
                    .saturating_mul(10)
                    .saturating_add((byte - b'0') as usize),
            )?;
            self.at += 1;
        }
        Ok(value)
    }

    fn size(&mut self) -> Size {
        // The same prefixes as the narrow parser: `hh`, `h`, `l`, `ll`, `j`,
        // `z`, `t`, `w`, `I`, `I32`, `I64` and `L`.
        let start = self.at;
        let one = self.ascii();
        let two = if self.at + 1 < self.format.len() {
            let unit = self.format[self.at + 1];
            if unit < 0x80 { Some(unit as u8) } else { None }
        } else {
            None
        };
        let (size, len) = match (one, two) {
            (Some(b'h'), Some(b'h')) => (Size::Char, 2),
            (Some(b'h'), _) => (Size::Short, 1),
            (Some(b'l'), Some(b'l')) => (Size::Quad, 2),
            (Some(b'l'), _) => (Size::Long, 1),
            (Some(b'j'), _) | (Some(b'z'), _) | (Some(b't'), _) => (Size::Quad, 1),
            (Some(b'w'), _) => (Size::Wide, 1),
            (Some(b'I'), Some(b'3')) if self.format.get(self.at + 2) == Some(&(b'2' as u16)) => {
                (Size::Int32, 3)
            }
            (Some(b'I'), Some(b'6')) if self.format.get(self.at + 2) == Some(&(b'4' as u16)) => {
                (Size::Quad, 3)
            }
            (Some(b'I'), _) => (Size::Quad, 1),
            (Some(b'L'), _) => (Size::LongDouble, 1),
            _ => (Size::None, 0),
        };
        let _ = two;
        self.at = start + len;
        size
    }
}

/// Append every byte of `text` as its wide unit. The narrow engine's
/// characters are all ASCII — digits, signs, points, exponents, the fill of
/// the padding — so this is the text itself, not an encoding.
fn transcoded(out: &mut Vec<u16>, text: &[u8]) {
    out.extend(text.iter().map(|byte| u16::from(*byte)));
}

/// The wide format: the same conversions as the narrow [`super::crt_format`]
/// engine, with the character and string types turned around as the wide
/// family defines them.
pub(super) fn wformat(
    format: &[u16],
    source: &mut dyn WideSource,
    floats: Floats,
) -> Result<Vec<u16>, FormatError> {
    let mut out = Vec::new();
    let mut parser = Parser { format, at: 0 };
    while let Some(unit) = parser.peek() {
        parser.at += 1;
        if unit != u16::from(b'%') {
            out.push(unit);
            bounded(out.len())?;
            continue;
        }
        if parser.take(b'%') {
            out.push(u16::from(b'%'));
            continue;
        }

        let mut flags = Flags::default();
        loop {
            match parser.ascii() {
                Some(b'-') => flags.left = true,
                Some(b'+') => flags.plus = true,
                Some(b' ') => flags.space = true,
                Some(b'#') => flags.alternate = true,
                Some(b'0') => flags.zero = true,
                _ => break,
            }
            parser.at += 1;
        }
        let width = if parser.take(b'*') {
            let value = int(source.argument().map_err(FormatError::Fault)?);
            flags.left |= value < 0;
            bounded(value.unsigned_abs() as usize)?
        } else {
            parser.number()?
        };
        let precision = if !parser.take(b'.') {
            None
        } else if parser.take(b'*') {
            let value = int(source.argument().map_err(FormatError::Fault)?);
            match usize::try_from(value) {
                Ok(value) => Some(bounded(value)?),
                Err(_) => None,
            }
        } else {
            Some(parser.number()?)
        };
        let size = parser.size();
        let spec = Spec {
            flags,
            width,
            precision,
            size,
        };
        let kind = parser.ascii().ok_or(FormatError::Invalid)?;
        parser.at += 1;

        match kind {
            b'd' | b'i' | b'u' | b'o' | b'x' | b'X' => {
                let slot = source.argument().map_err(FormatError::Fault)?;
                // The narrow engine formats the number exactly as the wide
                // one should print it, digit for digit.
                // `integer` includes the padding, and the padding is ASCII.
                let mut text = Vec::new();
                integer(&mut text, &spec, kind, slot)?;
                transcoded(&mut out, &text);
            }
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'a' | b'A' => {
                let slot = source.argument().map_err(FormatError::Fault)?;
                let mut text = Vec::new();
                crate::services::crt_format::float(&mut text, &spec, kind, slot, floats)?;
                transcoded(&mut out, &text);
            }
            b'c' => {
                // Wide: the argument is a wide character. Narrow (`%C` or
                // `%hc`): zero-extended. Any other size is invalid.
                let wide = match spec.size {
                    Size::None | Size::Long | Size::Wide => true,
                    Size::Short => false,
                    _ => return Err(FormatError::Invalid),
                };
                let slot = source.argument().map_err(FormatError::Fault)?;
                let unit = if wide {
                    slot as u16
                } else {
                    u16::from(slot as u8)
                };
                pad_units(&mut out, &spec, &[unit]);
            }
            b'C' => {
                let slot = source.argument().map_err(FormatError::Fault)?;
                pad_units(&mut out, &spec, &[u16::from(slot as u8)]);
            }
            b's' => {
                let wide = match spec.size {
                    Size::None | Size::Long | Size::Wide => true,
                    Size::Short => false,
                    _ => return Err(FormatError::Invalid),
                };
                let slot = source.argument().map_err(FormatError::Fault)?;
                string(&mut out, &spec, source, slot, wide)?;
            }
            b'S' => {
                let slot = source.argument().map_err(FormatError::Fault)?;
                string(&mut out, &spec, source, slot, false)?;
            }
            b'p' => {
                if spec.size != Size::None {
                    return Err(FormatError::Invalid);
                }
                let slot = source.argument().map_err(FormatError::Fault)?;
                // As the narrow engine: sixteen upper-case digits.
                let text = format!("{slot:016X}");
                let mut padded = Vec::new();
                pad(&mut padded, &spec, 0, text.as_bytes(), false);
                transcoded(&mut out, &padded);
            }
            b'n' => return Err(FormatError::Invalid),
            _ => return Err(FormatError::Invalid),
        }
        bounded(out.len())?;
    }
    Ok(out)
}

fn pad_units(out: &mut Vec<u16>, spec: &Spec, body: &[u16]) {
    let fill = spec.width.saturating_sub(body.len());
    if spec.flags.left {
        out.extend_from_slice(body);
        out.resize(out.len() + fill, u16::from(b' '));
    } else {
        out.resize(out.len() + fill, u16::from(b' '));
        out.extend_from_slice(body);
    }
}

fn string(
    out: &mut Vec<u16>,
    spec: &Spec,
    source: &mut dyn WideSource,
    slot: u64,
    wide: bool,
) -> Result<(), FormatError> {
    let limit = spec.precision.unwrap_or(usize::MAX);
    let mut units = if slot == 0 {
        // "(null)", as the narrow engine prints it.
        "(null)".encode_utf16().collect::<Vec<u16>>()
    } else if wide {
        source
            .wide_string(slot, limit.min(MAX_STRING))
            .map_err(FormatError::Fault)?
    } else {
        source
            .narrow_string(slot, limit.min(MAX_STRING))
            .map_err(FormatError::Fault)?
            .iter()
            .map(|byte| u16::from(*byte))
            .collect()
    };
    units.truncate(limit.min(MAX_STRING));
    pad_units(out, spec, &units);
    Ok(())
}

/// The most of a string one conversion reads, as the narrow engine bounds
/// its own reads.
const MAX_STRING: usize = 1 << 20;
