// SPDX-License-Identifier: GPL-3.0-or-later

//! Formatting for the narrow `printf` family.
//!
//! The rules are from Microsoft's "Format specification syntax: printf and
//! wprintf functions" and, where that page is silent, from ISO C 7.21.6.1. A
//! conversion specification is `%[flags][width][.precision][size]type`.
//!
//! Supported: the flags `-`, `+`, space, `#` and `0`; a width and a precision
//! given as digits or as `*`; the size prefixes `hh`, `h`, `l`, `ll`, `j`,
//! `z`, `t`, `I`, `I32` and `I64`; and the types `d`, `i`, `u`, `o`, `x`, `X`,
//! `c`, `s`, `p`, `%%`, and the floating-point types `f`, `F`, `e`, `E`, `g`,
//! `G`, `a` and `A`.
//!
//! A floating-point argument is a `double`, with or without `l` or `L`: the
//! page gives both prefixes as `long double`, which in Visual C++ "has the
//! same internal representation as double". The value is printed from its
//! exact binary digits, rounded correctly with ties to the even digit, as
//! Microsoft's `printf` page describes since Windows 10 version 2004. That
//! page also says the rounding follows the mode `fesetround` sets; here it is
//! always to nearest. The decimal point is always `.`: no locale is modelled.
//! The caller's option bits choose two things, see [`Floats`]: a program that
//! asks for the rounding of older Windows gets no floating point at all,
//! because that rounding is not modelled.
//!
//! Refused as invalid, as Windows does by default: `%n`, and a character that
//! has no meaning as a type. Refused as not modelled, so that nothing is
//! printed wrongly: wide characters and strings.
//!
//! Where the documentation leaves a choice, the choice made here is marked
//! "Xenora's choice" at the code that makes it.

use crate::memory::MemoryError;

/// The most one call may produce, and the largest width or precision.
pub(super) const MAX_OUTPUT: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FormatError {
    /// The format is not valid: `%n`, an unknown type, a size prefix that
    /// does not apply to its type, or a string that ends inside a
    /// specification.
    Invalid,
    /// The format is valid and asks for something that is not modelled.
    Unsupported,
    /// The output, a width or a precision exceeds [`MAX_OUTPUT`].
    TooLong,
    /// Reading an argument or a string failed.
    Fault(MemoryError),
}

/// What the caller's option bits say about floating point
/// (`_CRT_INTERNAL_PRINTF_*` in corecrt_stdio_config.h).
#[derive(Clone, Copy)]
pub(super) struct Floats {
    /// `LEGACY_THREE_DIGIT_EXPONENTS`: the `e` style has at least three
    /// exponent digits, as before Visual Studio 2015, not two.
    pub(super) three_digit_exponents: bool,
    /// `STANDARD_ROUNDING`: ties go to the even digit. Without it the
    /// Universal CRT rounds as older versions of Windows did. That is not
    /// modelled, so a floating-point conversion is then refused.
    pub(super) standard_rounding: bool,
}

/// Where the arguments and strings of one call come from.
pub(super) trait Source {
    /// The next 8-byte argument slot.
    fn argument(&mut self) -> Result<u64, MemoryError>;
    /// The bytes of the string at `address` up to its terminator, or the first
    /// `limit` of them. Never called with a null address.
    fn string(&mut self, address: u64, limit: usize) -> Result<Vec<u8>, MemoryError>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Size {
    None,
    Char,
    Short,
    /// `l`: a `long`, which is 32 bits on Windows, or a wide character or
    /// string.
    Long,
    /// `ll`, `j`, `z`, `t`, `I` and `I64`: 64 bits on a 64-bit system.
    Quad,
    /// `I32`.
    Int32,
    /// `w`: a wide character or string.
    Wide,
    /// `L`: a `long double`.
    LongDouble,
}

#[derive(Default)]
pub(super) struct Flags {
    pub(super) left: bool,
    pub(super) plus: bool,
    pub(super) space: bool,
    pub(super) alternate: bool,
    pub(super) zero: bool,
}

pub(super) struct Spec {
    pub(super) flags: Flags,
    pub(super) width: usize,
    pub(super) precision: Option<usize>,
    pub(super) size: Size,
}

struct Parser<'a> {
    format: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.format.get(self.at).copied()
    }

    fn take(&mut self, byte: u8) -> bool {
        let found = self.peek() == Some(byte);
        self.at += usize::from(found);
        found
    }

    /// Decimal digits, possibly none.
    fn number(&mut self) -> Result<usize, FormatError> {
        let mut value = 0usize;
        while let Some(digit @ b'0'..=b'9') = self.peek() {
            value = value * 10 + usize::from(digit - b'0');
            if value > MAX_OUTPUT {
                return Err(FormatError::TooLong);
            }
            self.at += 1;
        }
        Ok(value)
    }

    fn size(&mut self) -> Size {
        if self.take(b'h') {
            return if self.take(b'h') {
                Size::Char
            } else {
                Size::Short
            };
        }
        if self.take(b'l') {
            return if self.take(b'l') {
                Size::Quad
            } else {
                Size::Long
            };
        }
        if self.take(b'I') {
            let rest = &self.format[self.at..];
            if rest.starts_with(b"64") {
                self.at += 2;
                return Size::Quad;
            }
            if rest.starts_with(b"32") {
                self.at += 2;
                return Size::Int32;
            }
            // `I` alone is ptrdiff_t or size_t.
            return Size::Quad;
        }
        match self.peek() {
            Some(b'j' | b'z' | b't') => {
                self.at += 1;
                Size::Quad
            }
            Some(b'w') => {
                self.at += 1;
                Size::Wide
            }
            Some(b'L') => {
                self.at += 1;
                Size::LongDouble
            }
            _ => Size::None,
        }
    }
}

/// An `int` argument is the low half of its slot.
pub(super) fn int(slot: u64) -> i32 {
    slot as u32 as i32
}

pub(super) fn bounded(value: usize) -> Result<usize, FormatError> {
    if value > MAX_OUTPUT {
        Err(FormatError::TooLong)
    } else {
        Ok(value)
    }
}

/// Pad `body` to the field width. `zeros` puts the padding after the first
/// `lead` bytes (a sign or prefix) as zeros instead of before them as blanks.
pub(super) fn pad(out: &mut Vec<u8>, spec: &Spec, lead: usize, body: &[u8], zeros: bool) {
    let fill = spec.width.saturating_sub(body.len());
    if spec.flags.left {
        out.extend_from_slice(body);
        out.resize(out.len() + fill, b' ');
    } else if zeros {
        out.extend_from_slice(&body[..lead]);
        out.resize(out.len() + fill, b'0');
        out.extend_from_slice(&body[lead..]);
    } else {
        out.resize(out.len() + fill, b' ');
        out.extend_from_slice(body);
    }
}

fn digits(mut value: u64, base: u64, upper: bool) -> Vec<u8> {
    let table: &[u8; 16] = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut reversed = Vec::new();
    loop {
        reversed.push(table[(value % base) as usize]);
        value /= base;
        if value == 0 {
            break;
        }
    }
    reversed.reverse();
    reversed
}

pub(super) fn integer(
    out: &mut Vec<u8>,
    spec: &Spec,
    kind: u8,
    slot: u64,
) -> Result<(), FormatError> {
    let signed = matches!(kind, b'd' | b'i');
    // Without a size prefix an integer argument is a 32-bit int, and so is a
    // long. `hh` and `h` arguments arrive as an int and are converted back.
    let (negative, magnitude) = if signed {
        let value = match spec.size {
            Size::None | Size::Long | Size::Int32 => i64::from(int(slot)),
            Size::Char => i64::from(slot as u8 as i8),
            Size::Short => i64::from(slot as u16 as i16),
            Size::Quad => slot as i64,
            Size::Wide | Size::LongDouble => return Err(FormatError::Invalid),
        };
        (value < 0, value.unsigned_abs())
    } else {
        let value = match spec.size {
            Size::None | Size::Long | Size::Int32 => u64::from(slot as u32),
            Size::Char => u64::from(slot as u8),
            Size::Short => u64::from(slot as u16),
            Size::Quad => slot,
            Size::Wide | Size::LongDouble => return Err(FormatError::Invalid),
        };
        (false, value)
    };

    let (base, upper) = match kind {
        b'o' => (8, false),
        b'x' => (16, false),
        b'X' => (16, true),
        _ => (10, false),
    };
    // The precision is the minimum number of digits, 1 by default. A zero
    // value with a precision of zero has no digits.
    let precision = spec.precision.unwrap_or(1);
    let mut number = if magnitude == 0 && precision == 0 {
        Vec::new()
    } else {
        digits(magnitude, base, upper)
    };
    if number.len() < precision {
        let mut padded = vec![b'0'; precision - number.len()];
        padded.append(&mut number);
        number = padded;
    }

    let mut body = Vec::new();
    if signed {
        // The blank is ignored when both it and + appear.
        if negative {
            body.push(b'-');
        } else if spec.flags.plus {
            body.push(b'+');
        } else if spec.flags.space {
            body.push(b' ');
        }
    }
    if spec.flags.alternate {
        match kind {
            // ISO C: the precision grows, only if it must, so that the first
            // digit is a zero. A zero value with a precision of zero is "0".
            b'o' if number.first() != Some(&b'0') => body.push(b'0'),
            b'x' if magnitude != 0 => body.extend_from_slice(b"0x"),
            b'X' if magnitude != 0 => body.extend_from_slice(b"0X"),
            _ => {}
        }
    }
    let lead = body.len();
    body.append(&mut number);
    bounded(body.len().max(spec.width))?;
    // 0 is ignored with - and when an integer has a precision.
    let zeros = spec.flags.zero && spec.precision.is_none();
    pad(out, spec, lead, &body, zeros);
    Ok(())
}

/// The exponent part of the `e` and `a` styles: the letter, a sign, and at
/// least `least` digits.
fn exponent(letter: u8, value: i32, least: usize) -> Vec<u8> {
    let sign = if value < 0 { '-' } else { '+' };
    format!(
        "{}{sign}{:0least$}",
        char::from(letter),
        value.unsigned_abs()
    )
    .into_bytes()
}

/// `magnitude` in the `e` style with `precision` digits after the point: the
/// digits, and the decimal exponent as a number.
fn scientific(magnitude: f64, precision: usize) -> (Vec<u8>, i32) {
    let text = format!("{magnitude:.precision$e}");
    let (digits, power) = text.split_once('e').expect("an exponent");
    (
        digits.as_bytes().to_vec(),
        power.parse().expect("a decimal exponent"),
    )
}

/// A point with nothing after it is written only for `#`.
fn point(digits: &mut Vec<u8>, alternate: bool) {
    if alternate && !digits.contains(&b'.') {
        digits.push(b'.');
    }
}

/// `%g` removes trailing zeros after the point, and the point with them.
fn trim(digits: &mut Vec<u8>) {
    if digits.contains(&b'.') {
        while digits.last() == Some(&b'0') {
            digits.pop();
        }
        if digits.last() == Some(&b'.') {
            digits.pop();
        }
    }
}

/// The `a` style: the value's own binary digits, four to a hexadecimal one.
fn hexadecimal(magnitude: f64, precision: Option<usize>, alternate: bool, upper: bool) -> Vec<u8> {
    const FRACTION_BITS: u32 = 52;
    let bits = magnitude.to_bits();
    let field = (bits >> FRACTION_BITS) & 0x7FF;
    let fraction = bits & ((1 << FRACTION_BITS) - 1);
    // A normal number is 1.f times a power of two. Zero is 0.0 times 2^0.
    // Xenora's choice: a subnormal number is written 0.f times 2^-1022, its
    // own form; the page does not say.
    let (lead, power) = match (field, fraction) {
        (0, 0) => (0, 0),
        (0, _) => (0, -1022),
        _ => (1, field as i32 - 1023),
    };
    // Thirteen hexadecimal digits hold the whole fraction, and the page gives
    // 13 as the default precision.
    let precision = precision.unwrap_or(13);
    let mut whole = (lead << FRACTION_BITS) | fraction;
    let kept = precision.min(13);
    let dropped = 4 * (13 - kept) as u32;
    if dropped > 0 {
        let half = 1u64 << (dropped - 1);
        let rest = whole & ((1 << dropped) - 1);
        whole >>= dropped;
        // Ties go to the even digit.
        if rest > half || (rest == half && whole & 1 == 1) {
            whole += 1;
        }
    }
    let mut text = format!("0x{:x}", whole >> (4 * kept));
    if precision > 0 || alternate {
        text.push('.');
    }
    if kept > 0 {
        text += &format!("{:0kept$x}", whole & ((1u64 << (4 * kept)) - 1));
    }
    let mut text = text.into_bytes();
    text.resize(text.len() + (precision - kept), b'0');
    text.extend(exponent(b'p', power, 1));
    if upper {
        text.make_ascii_uppercase();
    }
    text
}

/// One of `f`, `e`, `g` and `a` or its capital, for the `double` in `slot`.
pub(super) fn float(
    out: &mut Vec<u8>,
    spec: &Spec,
    kind: u8,
    slot: u64,
    floats: Floats,
) -> Result<(), FormatError> {
    if !floats.standard_rounding {
        return Err(FormatError::Unsupported);
    }
    let exponent_digits = if floats.three_digit_exponents { 3 } else { 2 };
    match spec.size {
        // `l` and `L` name a long double, which is a double here: the slot is
        // read as one, as the Universal CRT does. A program whose compiler
        // has a wider long double prints the wrong number on Windows too.
        Size::None | Size::Long | Size::LongDouble => {}
        _ => return Err(FormatError::Invalid),
    }
    let value = f64::from_bits(slot);
    let upper = kind.is_ascii_uppercase();
    let style = kind.to_ascii_lowercase();
    let alternate = spec.flags.alternate;
    let magnitude = value.abs();

    let mut body = Vec::new();
    // The blank is ignored when both it and + appear. A negative zero and a
    // negative NaN have their sign.
    if value.is_sign_negative() {
        body.push(b'-');
    } else if spec.flags.plus {
        body.push(b'+');
    } else if spec.flags.space {
        body.push(b' ');
    }
    let lead = body.len();

    if !value.is_finite() {
        // The format specification page: `inf`; `nan` for a quiet NaN,
        // `nan(snan)` for a signalling one and `nan(ind)` for the indefinite
        // NaN, which is the negative quiet NaN with no payload. Capitals for
        // the capital types. The precision does not apply.
        const QUIET: u64 = 1 << 51;
        let text: &[u8] = if value.is_infinite() {
            b"inf"
        } else if slot == 0xFFF8_0000_0000_0000 {
            b"nan(ind)"
        } else if slot & QUIET == 0 {
            b"nan(snan)"
        } else {
            b"nan"
        };
        body.extend_from_slice(text);
        if upper {
            body.make_ascii_uppercase();
        }
        bounded(body.len().max(spec.width))?;
        // The page, as ISO C: 0 does not pad an infinity or a NaN with zeros.
        pad(out, spec, lead, &body, false);
        return Ok(());
    }

    let mut digits = match style {
        b'f' => {
            let precision = spec.precision.unwrap_or(6);
            let mut digits = format!("{magnitude:.precision$}").into_bytes();
            point(&mut digits, alternate);
            digits
        }
        b'e' => {
            let (mut digits, power) = scientific(magnitude, spec.precision.unwrap_or(6));
            point(&mut digits, alternate);
            digits.extend(exponent(b'e', power, exponent_digits));
            digits
        }
        b'g' => {
            // ISO C: with P the precision (6 by default, 1 for 0) and X the
            // exponent the e style would have, the f style with P - 1 - X
            // digits if P > X >= -4, otherwise the e style with P - 1; then
            // trailing zeros are removed unless # is given.
            let significant = match spec.precision {
                None => 6,
                Some(0) => 1,
                Some(precision) => precision,
            };
            let (mut digits, power) = scientific(magnitude, significant - 1);
            let fixed =
                usize::try_from(i64::from(power) + 4).is_ok_and(|above| above < significant + 4);
            if fixed {
                let precision = (significant as i64 - 1 - i64::from(power)) as usize;
                digits = format!("{magnitude:.precision$}").into_bytes();
            }
            point(&mut digits, alternate);
            if !alternate {
                trim(&mut digits);
            }
            if !fixed {
                digits.extend(exponent(b'e', power, exponent_digits));
            }
            digits
        }
        _ => hexadecimal(magnitude, spec.precision, alternate, false),
    };
    if upper {
        digits.make_ascii_uppercase();
    }
    // Zeros from the 0 flag follow the sign and, in the a style, the 0x: the
    // page has them "after the 0x or 0X prefix".
    let lead = lead + if style == b'a' { 2 } else { 0 };
    body.append(&mut digits);
    bounded(body.len().max(spec.width))?;
    pad(out, spec, lead, &body, spec.flags.zero);
    Ok(())
}

/// Format `format` with the arguments and strings `source` supplies. Returns
/// the bytes to write; their number is the function result of the `printf`
/// family.
pub(super) fn format(
    format: &[u8],
    source: &mut dyn Source,
    floats: Floats,
) -> Result<Vec<u8>, FormatError> {
    let mut out = Vec::new();
    let mut parser = Parser { format, at: 0 };
    while let Some(byte) = parser.peek() {
        parser.at += 1;
        if byte != b'%' {
            out.push(byte);
            bounded(out.len())?;
            continue;
        }
        if parser.take(b'%') {
            out.push(b'%');
            continue;
        }

        let mut flags = Flags::default();
        loop {
            match parser.peek() {
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
            // ISO C: a negative width is the - flag and a positive width.
            let value = int(source.argument().map_err(FormatError::Fault)?);
            flags.left |= value < 0;
            bounded(value.unsigned_abs() as usize)?
        } else {
            parser.number()?
        };
        let precision = if !parser.take(b'.') {
            None
        } else if parser.take(b'*') {
            // ISO C: a negative precision is taken as if it were omitted.
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
        let kind = parser.peek().ok_or(FormatError::Invalid)?;
        parser.at += 1;

        match kind {
            b'd' | b'i' | b'u' | b'o' | b'x' | b'X' => {
                let slot = source.argument().map_err(FormatError::Fault)?;
                integer(&mut out, &spec, kind, slot)?;
            }
            b'c' => {
                match spec.size {
                    Size::None | Size::Short => {}
                    Size::Long | Size::Wide => return Err(FormatError::Unsupported),
                    _ => return Err(FormatError::Invalid),
                }
                // The precision has no effect on a character.
                let slot = source.argument().map_err(FormatError::Fault)?;
                // Xenora's choice: 0 pads any type, as the page's description
                // of the flag is not limited to numbers.
                pad(&mut out, &spec, 0, &[slot as u8], spec.flags.zero);
            }
            b's' => {
                match spec.size {
                    Size::None | Size::Short => {}
                    Size::Long | Size::Wide => return Err(FormatError::Unsupported),
                    _ => return Err(FormatError::Invalid),
                }
                let address = source.argument().map_err(FormatError::Fault)?;
                let limit = spec.precision.unwrap_or(MAX_OUTPUT + 1);
                let text = if address == 0 {
                    // Xenora's choice: the precision also limits "(null)".
                    let null = b"(null)";
                    null[..null.len().min(limit)].to_vec()
                } else {
                    source.string(address, limit).map_err(FormatError::Fault)?
                };
                bounded(text.len().max(spec.width))?;
                pad(&mut out, &spec, 0, &text, spec.flags.zero);
            }
            b'p' => {
                if spec.size != Size::None {
                    return Err(FormatError::Invalid);
                }
                let slot = source.argument().map_err(FormatError::Fault)?;
                // "An address in hexadecimal digits" is all the page says.
                // Xenora's choice: sixteen upper-case digits, and no other
                // flag or precision has an effect.
                let text = format!("{slot:016X}");
                pad(&mut out, &spec, 0, text.as_bytes(), false);
            }
            b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a' | b'A' => {
                let slot = source.argument().map_err(FormatError::Fault)?;
                float(&mut out, &spec, kind, slot, floats)?;
            }
            // Wide characters and strings and the counted string structures
            // are valid and not modelled.
            b'C' | b'S' | b'Z' => return Err(FormatError::Unsupported),
            // %n is disabled by default, and any other character has no
            // meaning as a type.
            _ => return Err(FormatError::Invalid),
        }
        bounded(out.len())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{Floats, FormatError, MAX_OUTPUT, Source, format};
    use crate::memory::MemoryError;

    /// What mingw-w64 asks for: two exponent digits and standard rounding.
    const STANDARD: Floats = Floats {
        three_digit_exponents: false,
        standard_rounding: true,
    };

    /// A string argument is the address `STRINGS + 0x100 * index`.
    const STRINGS: u64 = 0x1000_0000;

    struct Call {
        slots: Vec<u64>,
        strings: Vec<Vec<u8>>,
        next: usize,
    }

    impl Source for Call {
        fn argument(&mut self) -> Result<u64, MemoryError> {
            let slot = self.slots.get(self.next).copied();
            self.next += 1;
            slot.ok_or(MemoryError::UnmappedAddress(self.next as u64))
        }

        fn string(&mut self, address: u64, limit: usize) -> Result<Vec<u8>, MemoryError> {
            let index = ((address - STRINGS) / 0x100) as usize;
            let text = self
                .strings
                .get(index)
                .ok_or(MemoryError::UnmappedAddress(address))?;
            Ok(text[..text.len().min(limit)].to_vec())
        }
    }

    fn run(text: &str, slots: &[u64], strings: &[&str]) -> Result<String, FormatError> {
        let mut call = Call {
            slots: slots.to_vec(),
            strings: strings
                .iter()
                .map(|text| text.as_bytes().to_vec())
                .collect(),
            next: 0,
        };
        let bytes = format(text.as_bytes(), &mut call, STANDARD)?;
        assert_eq!(call.next, slots.len(), "{text}: arguments consumed");
        Ok(String::from_utf8(bytes).unwrap())
    }

    fn ok(text: &str, slots: &[u64], strings: &[&str]) -> String {
        run(text, slots, strings).unwrap_or_else(|error| panic!("{text}: {error:?}"))
    }

    /// The host C library's `snprintf`, on cases whose behaviour ISO C
    /// defines and Windows shares. `tests/printf/reference.py` wrote the file
    /// and says how each line is laid out.
    #[test]
    fn output_matches_the_host_c_library_on_defined_cases() {
        let vectors = include_str!("../../tests/printf/vectors.txt");
        let unhex = |text: &str| -> Vec<u8> {
            (0..text.len() / 2)
                .map(|at| u8::from_str_radix(&text[2 * at..2 * at + 2], 16).unwrap())
                .collect()
        };
        let mut cases = 0;
        for line in vectors.lines().filter(|line| !line.starts_with('#')) {
            let fields: Vec<&str> = line.split('\t').collect();
            let [template, arguments, expected] = fields[..] else {
                panic!("three fields: {line}");
            };
            let template = unhex(template);
            let (mut slots, mut strings) = (Vec::new(), Vec::new());
            for argument in arguments.split(',').filter(|text| !text.is_empty()) {
                let (kind, value) = argument.split_once(':').unwrap();
                match kind {
                    // An integer slot, or the bits of a double.
                    "i" | "f" => slots.push(u64::from_str_radix(value, 16).unwrap()),
                    "s" => {
                        slots.push(STRINGS + 0x100 * strings.len() as u64);
                        strings.push(unhex(value));
                    }
                    "n" => slots.push(0),
                    _ => panic!("argument kind {kind}"),
                }
            }
            let mut call = Call {
                slots,
                strings,
                next: 0,
            };
            let shown = String::from_utf8_lossy(&template).into_owned();
            let got = format(&template, &mut call, STANDARD)
                .unwrap_or_else(|error| panic!("{shown} {arguments}: {error:?}"));
            assert_eq!(
                String::from_utf8_lossy(&got),
                String::from_utf8_lossy(&unhex(expected)),
                "{shown} with {arguments}"
            );
            assert_eq!(call.next, call.slots.len(), "{shown}: arguments consumed");
            cases += 1;
        }
        assert!(cases > 4500, "{cases} cases");
    }

    /// Rules the host C library does not share or that the reference leaves
    /// out, each from the format specification page.
    #[test]
    fn windows_sizes_follow_the_format_specification_page() {
        // "The formatter consumes integer arguments ... as 32-bit int types";
        // a long is a 32-bit type on Windows. The upper half of the slot is
        // not part of either.
        let slot = 0xFFFF_FFFF_8000_0001;
        assert_eq!(ok("%d", &[slot], &[]), "-2147483647");
        assert_eq!(ok("%ld", &[slot], &[]), "-2147483647");
        assert_eq!(ok("%lu", &[slot], &[]), "2147483649");
        assert_eq!(ok("%lx", &[0x1_2345_6789], &[]), "23456789");
        assert_eq!(ok("%I32d", &[slot], &[]), "-2147483647");
        assert_eq!(ok("%I32u", &[slot], &[]), "2147483649");
        // ll, I64, j, t, z and I alone are 64-bit.
        for size in ["ll", "I64", "j", "t", "z", "I"] {
            assert_eq!(
                ok(&format!("%{size}d"), &[slot], &[]),
                "-2147483647",
                "{size}"
            );
            assert_eq!(
                ok(&format!("%{size}u"), &[u64::MAX], &[]),
                "18446744073709551615",
                "{size}"
            );
            assert_eq!(
                ok(&format!("%{size}x"), &[0x1_2345_6789], &[]),
                "123456789",
                "{size}"
            );
        }
        assert_eq!(ok("%I64d", &[1 << 63], &[]), "-9223372036854775808");
        // I followed by other digits is the size I and then literal text?
        // No: digits after the size are not a type, so the format is invalid.
        assert_eq!(run("%I16d", &[1], &[]), Err(FormatError::Invalid));
    }

    #[test]
    fn strings_characters_and_pointers() {
        // "If the argument that corresponds to %s ... is a null pointer,
        // "(null)" is displayed."
        assert_eq!(ok("[%s]", &[0], &[]), "[(null)]");
        assert_eq!(ok("[%10s]", &[0], &[]), "[    (null)]");
        assert_eq!(ok("[%-8s]", &[0], &[]), "[(null)  ]");
        // Xenora's choice: the precision limits it too.
        assert_eq!(ok("[%.3s]", &[0], &[]), "[(nu]");
        // h is a single-byte character or string, as no prefix is.
        assert_eq!(
            ok("%hc%hs", &[u64::from(b'x'), super::tests::STRINGS], &["yz"]),
            "xyz"
        );
        // The precision has no effect on a character.
        assert_eq!(ok("[%.0c]", &[0x141], &[]), "[A]");
        // Xenora's choice for %p: sixteen upper-case hexadecimal digits.
        assert_eq!(ok("%p", &[0x1_4000_1abc], &[]), "0000000140001ABC");
        assert_eq!(ok("%p", &[0], &[]), "0000000000000000");
        assert_eq!(ok("[%20p]", &[0xABC], &[]), "[    0000000000000ABC]");
        assert_eq!(ok("[%-20p]", &[0xABC], &[]), "[0000000000000ABC    ]");
        // Xenora's choice: 0 pads a character and a string as it pads a number.
        assert_eq!(ok("[%03c]", &[u64::from(b'a')], &[]), "[00a]");
        assert_eq!(ok("[%05s]", &[STRINGS], &["ab"]), "[000ab]");
        assert_eq!(ok("[%-05s]", &[STRINGS], &["ab"]), "[ab   ]");
        // "+: Use a sign (+ or -) to prefix the output value if it's of a
        // signed type", and the blank likewise: neither affects an unsigned
        // conversion.
        for (text, wanted) in [
            ("%+u", "7"),
            ("% u", "7"),
            ("%+x", "7"),
            ("% X", "7"),
            ("%+o", "7"),
            ("%+ 5u", "    7"),
            ("%+d", "+7"),
            ("% d", " 7"),
        ] {
            assert_eq!(ok(text, &[7], &[]), wanted, "{text}");
        }
        // ISO C for %#.0o with a zero value, which the page does not cover.
        assert_eq!(ok("[%#.0o]", &[0], &[]), "[0]");
        assert_eq!(ok("[%.0o]", &[0], &[]), "[]");
        // Text between and around specifications is copied, and %% is a
        // percent sign.
        assert_eq!(
            ok("100%% of %s%c", &[STRINGS, u64::from(b'!')], &["it"]),
            "100% of it!"
        );
        assert_eq!(ok("", &[], &[]), "");
        assert_eq!(ok("plain", &[], &[]), "plain");
    }

    #[test]
    fn invalid_formats_are_refused() {
        // "The n specifier is disabled by default", and "if a percent sign is
        // followed by a character that has no meaning as a format field, the
        // invalid parameter handler is invoked."
        for text in [
            "%n",
            "%hn",
            "count %d%n",
            "%y",
            "%",
            "%5",
            "%-",
            "%.",
            "%.3",
            "%l",
            "%ll",
            "%h",
            "%I64",
            "%5%",
            "% %",
            "%!",
            "%\u{1}",
        ] {
            let slots = [1, 2, 3];
            let mut call = Call {
                slots: slots.to_vec(),
                strings: Vec::new(),
                next: 0,
            };
            assert_eq!(
                format(text.as_bytes(), &mut call, STANDARD),
                Err(FormatError::Invalid),
                "{text:?}"
            );
        }
        // A size prefix that does not apply to its type.
        for text in [
            "%wd", "%Ld", "%wu", "%Lx", "%llc", "%hhc", "%jc", "%lls", "%zs", "%I64s", "%lp",
            "%hp", "%llp", "%I32c",
        ] {
            assert_eq!(run(text, &[1], &["s"]), Err(FormatError::Invalid), "{text}");
        }
    }

    /// The bits of a double, as its argument slot.
    fn double(value: f64) -> u64 {
        value.to_bits()
    }

    /// Floating-point rules the host C library does not share, each from the
    /// format specification page or from the value's own binary digits.
    #[test]
    fn floating_point_rules_the_host_does_not_share() {
        // Infinity and the three kinds of NaN, in small and capital letters.
        // The indefinite NaN is the negative quiet NaN with no payload.
        let quiet = 0x7FF8_0000_0000_0000;
        let indefinite = 0xFFF8_0000_0000_0000;
        let signalling = 0x7FF0_0000_0000_0001;
        for kind in ["f", "e", "g", "a"] {
            let capital = kind.to_uppercase();
            for (slot, small) in [
                (double(f64::INFINITY), "inf"),
                (double(f64::NEG_INFINITY), "-inf"),
                (quiet, "nan"),
                (quiet | 5, "nan"),
                // Negative and quiet, but with a payload: not the indefinite.
                (quiet | 1 << 63 | 5, "-nan"),
                (indefinite, "-nan(ind)"),
                (signalling, "nan(snan)"),
                (signalling | 1 << 63, "-nan(snan)"),
            ] {
                assert_eq!(ok(&format!("%{kind}"), &[slot], &[]), small, "{kind}");
                assert_eq!(
                    ok(&format!("%{capital}"), &[slot], &[]),
                    small.to_uppercase(),
                    "{capital}"
                );
                // The precision does not apply, and a sign is asked for as
                // for a number.
                assert_eq!(ok(&format!("%.2{kind}"), &[slot], &[]), small);
            }
        }
        assert_eq!(
            ok("%+f|% f", &[double(f64::INFINITY), quiet], &[]),
            "+inf| nan"
        );
        // ISO C: the 0 flag pads an infinity or a NaN with blanks.
        assert_eq!(ok("[%08f]", &[double(f64::INFINITY)], &[]), "[     inf]");
        assert_eq!(ok("[%-8E]", &[indefinite], &[]), "[-NAN(IND)]");
        assert_eq!(ok("[%-12E]", &[indefinite], &[]), "[-NAN(IND)   ]");

        // %a without a precision has thirteen digits after the point.
        assert_eq!(ok("%a", &[double(1.0)], &[]), "0x1.0000000000000p+0");
        assert_eq!(ok("%A", &[double(-1.5)], &[]), "-0X1.8000000000000P+0");
        assert_eq!(ok("%a", &[double(0.0)], &[]), "0x0.0000000000000p+0");
        // 0.1 is 0x1.999999999999a times 2^-4.
        assert_eq!(ok("%a", &[double(0.1)], &[]), "0x1.999999999999ap-4");
        assert_eq!(ok("%a", &[double(1024.0)], &[]), "0x1.0000000000000p+10");
        assert_eq!(
            ok("%a", &[double(f64::MAX)], &[]),
            "0x1.fffffffffffffp+1023"
        );
        // Xenora's choice for a subnormal number: its own form, 0.f times
        // 2^-1022. The smallest is one unit in the last of 13 digits.
        assert_eq!(
            ok("%a", &[double(f64::from_bits(1))], &[]),
            "0x0.0000000000001p-1022"
        );

        // A shorter precision rounds the binary value, ties to the even
        // digit. Each value is written here by its bits: 1.f in hexadecimal.
        let bits = |fraction: u64| 0x3FF0_0000_0000_0000 | fraction;
        for (fraction, text, expected) in [
            // 1.8 is half way between 1 and 2: to the even 2.
            (0x8_0000_0000_0000, "%.0a", "0x2p+0"),
            // 1.4 is below half way, 1.c above.
            (0x4_0000_0000_0000, "%.0a", "0x1p+0"),
            (0xC_0000_0000_0000, "%.0a", "0x2p+0"),
            // 1.08 is half way between 1.0 and 1.1: to the even 1.0; 1.18 is
            // half way between 1.1 and 1.2: to the even 1.2.
            (0x0_8000_0000_0000, "%.1a", "0x1.0p+0"),
            (0x1_8000_0000_0000, "%.1a", "0x1.2p+0"),
            // One unit past half way rounds up, however far down it is.
            (0x0_8000_0000_0001, "%.1a", "0x1.1p+0"),
            (0x1_7FFF_FFFF_FFFF, "%.1a", "0x1.1p+0"),
            // A carry runs through the digits into the leading one.
            (0xF_FFFF_FFFF_FFFF, "%.3a", "0x2.000p+0"),
            (0xF_FFFF_FFFF_FFFF, "%.12a", "0x2.000000000000p+0"),
            (0xB_FF2E_E48E_0530, "%.7a", "0x1.bff2ee5p+0"),
            (0x8_0000_0000_0000, "%#.0a", "0x2.p+0"),
            (0x8_0000_0000_0000, "%.2A", "0X1.80P+0"),
        ] {
            assert_eq!(ok(text, &[bits(fraction)], &[]), expected, "{fraction:x}");
        }
        // The 0 flag's zeros follow the sign and the 0x.
        assert_eq!(ok("[%+012.1a]", &[double(1.0)], &[]), "[+0x0001.0p+0]");

        // A double is one 8-byte slot. l and L both name a long double,
        // which is a double.
        assert_eq!(
            ok(
                "%f %lf %Lf %d",
                &[double(0.5), double(0.25), double(0.125), 7],
                &[]
            ),
            "0.500000 0.250000 0.125000 7"
        );
        for kind in ["e", "E", "f", "F", "g", "G", "a", "A"] {
            for value in [0.0, 1.0, -2.5, 1e100, f64::INFINITY] {
                let plain = ok(&format!("%12.3{kind}"), &[double(value)], &[]);
                assert_eq!(ok(&format!("%12.3L{kind}"), &[double(value)], &[]), plain);
                assert_eq!(ok(&format!("%12.3l{kind}"), &[double(value)], &[]), plain);
            }
        }
        // The page's own examples of the tie rule: 1.5 and 2.5 both print 2.
        assert_eq!(
            ok(
                "%1.0f %1.0f %.1f",
                &[double(1.5), double(2.5), double(2.35)],
                &[]
            ),
            "2 2 2.4"
        );
    }

    /// The two option bits that concern floating point.
    #[test]
    fn the_callers_options_choose_exponent_digits_and_rounding() {
        let with = |text: &str, slots: &[u64], floats: Floats| {
            let mut call = Call {
                slots: slots.to_vec(),
                strings: Vec::new(),
                next: 0,
            };
            format(text.as_bytes(), &mut call, floats)
                .map(|bytes| String::from_utf8(bytes).unwrap())
        };
        let three = Floats {
            three_digit_exponents: true,
            standard_rounding: true,
        };
        // At least three exponent digits in the e style, and in the g style
        // when it takes that form. The a style is not an e style, and the f
        // style has no exponent.
        for (text, value, two_digits, three_digits) in [
            ("%e", 1.0, "1.000000e+00", "1.000000e+000"),
            ("%.2E", -12345.0, "-1.23E+04", "-1.23E+004"),
            ("%e", 1e100, "1.000000e+100", "1.000000e+100"),
            ("%g", 1e-5, "1e-05", "1e-005"),
            ("%G", 1e20, "1E+20", "1E+020"),
            ("%g", 100.0, "100", "100"),
            ("%.1a", 1.0, "0x1.0p+0", "0x1.0p+0"),
            ("%.1f", 2.5, "2.5", "2.5"),
        ] {
            assert_eq!(with(text, &[double(value)], STANDARD).unwrap(), two_digits);
            assert_eq!(with(text, &[double(value)], three).unwrap(), three_digits);
        }
        // Without the standard rounding bit no floating-point conversion is
        // printed, whatever its value; everything else still is.
        let legacy = Floats {
            three_digit_exponents: false,
            standard_rounding: false,
        };
        for kind in ["e", "E", "f", "F", "g", "G", "a", "A"] {
            for value in [0.0, 2.5, f64::INFINITY] {
                assert_eq!(
                    with(&format!("%{kind}"), &[double(value)], legacy),
                    Err(FormatError::Unsupported),
                    "{kind}"
                );
            }
        }
        assert_eq!(with("%d|%x", &[7, 255], legacy), Ok("7|ff".to_owned()));
    }

    #[test]
    fn valid_conversions_that_are_not_modelled_are_refused() {
        // An integer size on a floating-point type has no meaning.
        for kind in ["e", "E", "f", "F", "g", "G", "a", "A"] {
            for prefix in ["h", "hh", "ll", "j", "z", "t", "I64", "I32", "w"] {
                let text = format!("%{prefix}{kind}");
                assert_eq!(
                    run(&text, &[0x3FF0_0000_0000_0000], &[]),
                    Err(FormatError::Invalid),
                    "{text}"
                );
            }
        }
        // Wide characters and strings: C and S, and c and s with l or w.
        for text in ["%C", "%S", "%lc", "%ls", "%wc", "%ws", "%Z", "%hC", "%hS"] {
            let mut call = Call {
                slots: vec![STRINGS],
                strings: vec![b"w".to_vec()],
                next: 0,
            };
            assert_eq!(
                format(text.as_bytes(), &mut call, STANDARD),
                Err(FormatError::Unsupported),
                "{text}"
            );
        }
        // Nothing is produced before a later refusal either: the caller gets
        // an error, not a prefix.
        assert_eq!(
            run("ok %d then %f and %ls", &[1, 2, 3], &[]),
            Err(FormatError::Unsupported)
        );
    }

    #[test]
    fn bounds_and_faults() {
        let most = MAX_OUTPUT;
        // A width or precision up to the bound is honoured; one more is not.
        assert_eq!(ok(&format!("%{most}d"), &[7], &[]).len(), most);
        assert_eq!(
            run(&format!("%{}d", most + 1), &[7], &[]),
            Err(FormatError::TooLong)
        );
        assert_eq!(ok(&format!("%.{most}d"), &[7], &[]).len(), most);
        assert_eq!(
            run(&format!("%.{}d", most + 1), &[7], &[]),
            Err(FormatError::TooLong)
        );
        assert_eq!(
            run("%99999999999999999999d", &[7], &[]),
            Err(FormatError::TooLong)
        );
        // The same through *, and the total over several conversions.
        assert_eq!(ok("%*d", &[most as u64, 7], &[]).len(), most);
        assert_eq!(
            run("%*d", &[most as u64 + 1, 7], &[]),
            Err(FormatError::TooLong)
        );
        assert_eq!(
            run("%.*d", &[most as u64 + 1, 7], &[]),
            Err(FormatError::TooLong)
        );
        let half = most / 2 + 1;
        assert_eq!(
            run(&format!("%{half}d%{half}d"), &[1, 2], &[]),
            Err(FormatError::TooLong)
        );
        // The most negative width is a left-justified width past the bound.
        assert_eq!(
            run("%*d", &[0x8000_0000, 7], &[]),
            Err(FormatError::TooLong)
        );
        // A string is read only up to its precision.
        assert_eq!(ok("%.2s", &[STRINGS], &["abcdef"]), "ab");

        // A missing argument or an unreadable string is a fault, wherever in
        // the specification it is needed.
        for text in [
            "%d",
            "%*d",
            "%.*d",
            "%5.*d",
            "%s",
            "%c",
            "%p",
            "%d %d %d %d",
        ] {
            assert!(
                matches!(run(text, &[], &[]), Err(FormatError::Fault(_))),
                "{text}"
            );
        }
        let mut call = Call {
            slots: vec![STRINGS + 0x900],
            strings: Vec::new(),
            next: 0,
        };
        assert_eq!(
            format(b"%s", &mut call, STANDARD),
            Err(FormatError::Fault(MemoryError::UnmappedAddress(
                STRINGS + 0x900
            )))
        );
    }
}
