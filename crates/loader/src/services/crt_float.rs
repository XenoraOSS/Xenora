// SPDX-License-Identifier: GPL-3.0-or-later

//! Floating-point string conversion: `strtod`, `strtof`, `wcstod`, `wcstof`,
//! `atof` and `_wtof`.
//!
//! Parsing follows the ISO C grammar in the "C" locale: optional white space,
//! an optional sign, then a decimal number (digits with an optional `.` and an
//! optional exponent), `inf` or `infinity`, or `nan` with an optional
//! `(n-char-sequence)`, all case-insensitive. The decimal digits are converted
//! with Rust's correctly rounded parser straight to the result type, so
//! `strtof` never rounds through `double`. The result is returned in XMM0.
//!
//! A result that overflows becomes infinity and sets `ERANGE`. Nonzero digits
//! that round to zero also set `ERANGE`. Whether the UCRT sets `ERANGE` for a
//! subnormal but nonzero result has not been established from public
//! documentation, so Xenora leaves `errno` unchanged there. A `nan` payload is
//! accepted and ignored; the result is the default quiet NaN with the parsed
//! sign. Hexadecimal floating constants (`0x1.8p3`) are not converted: they
//! stop the guest instead of being parsed as `0`.

use crate::cpu::{CpuState, ExitReason};
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError};
use crate::runtime::{Float32, Float64, Return};

const UCRT: &str = "ucrtbase.dll";
const CONVERT_API: &str = "api-ms-win-crt-convert-l1-1-0.dll";
const ERANGE: u32 = 34;
/// Longest number accepted, in characters.
const MAX_LENGTH: usize = 1 << 20;

#[derive(Clone, Copy, PartialEq)]
enum Width {
    Byte,
    Wide,
}

#[derive(Clone, Copy)]
enum Target {
    Double,
    Single,
}

fn unsupported() -> ExitReason {
    ExitReason::IllegalInstruction(0xFE)
}

/// Reads one character at a time from a guest string.
struct Cursor<'a> {
    memory: &'a GuestMemory,
    base: u64,
    width: Width,
    index: usize,
}

impl Cursor<'_> {
    fn peek_at(&self, ahead: usize) -> Result<char, ExitReason> {
        let index = self.index + ahead;
        if index > MAX_LENGTH {
            return Err(unsupported());
        }
        let step = if self.width == Width::Wide { 2 } else { 1 };
        let address = (index as u64)
            .checked_mul(step)
            .and_then(|offset| self.base.checked_add(offset))
            .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
        let unit = match self.width {
            Width::Byte => self.memory.read_u8(address).map(u16::from),
            Width::Wide => self.memory.read_u16(address),
        }
        .map_err(ExitReason::MemoryFault)?;
        // Only ASCII takes part in the grammar; anything else ends the number.
        Ok(if unit < 0x80 {
            unit as u8 as char
        } else {
            '\u{FFFD}'
        })
    }

    fn peek(&self) -> Result<char, ExitReason> {
        self.peek_at(0)
    }

    /// Consume `word` (lowercase ASCII) if the input matches it ignoring case.
    fn eat_word(&mut self, word: &str) -> Result<bool, ExitReason> {
        for (ahead, expected) in word.chars().enumerate() {
            if self.peek_at(ahead)?.to_ascii_lowercase() != expected {
                return Ok(false);
            }
        }
        self.index += word.len();
        Ok(true)
    }

    fn take_digits(&mut self, into: &mut String) -> Result<usize, ExitReason> {
        let mut count = 0;
        while self.peek()?.is_ascii_digit() {
            into.push(self.peek()?);
            self.index += 1;
            count += 1;
        }
        Ok(count)
    }
}

enum Parsed {
    /// Nothing was converted; the end pointer is the start of the string.
    None,
    Value {
        bits: u64,
        range_error: bool,
        end: usize,
    },
}

fn parse(
    memory: &GuestMemory,
    base: u64,
    width: Width,
    target: Target,
) -> Result<Parsed, ExitReason> {
    let mut cursor = Cursor {
        memory,
        base,
        width,
        index: 0,
    };
    while matches!(
        cursor.peek()?,
        ' ' | '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r'
    ) {
        cursor.index += 1;
    }
    let mut text = String::new();
    let negative = cursor.peek()? == '-';
    if matches!(cursor.peek()?, '-' | '+') {
        cursor.index += 1;
    }
    if negative {
        text.push('-');
    }

    if cursor.eat_word("infinity")? || cursor.eat_word("inf")? {
        let bits = match target {
            Target::Double => f64::INFINITY.to_bits(),
            Target::Single => u64::from(f32::INFINITY.to_bits()),
        };
        return Ok(Parsed::Value {
            bits: with_sign(bits, negative, target),
            range_error: false,
            end: cursor.index,
        });
    }
    if cursor.eat_word("nan")? {
        // An optional (n-char-sequence) is consumed only when it closes.
        if cursor.peek()? == '(' {
            let mut ahead = 1;
            loop {
                let c = cursor.peek_at(ahead)?;
                if c == ')' {
                    cursor.index += ahead + 1;
                    break;
                }
                if !(c.is_ascii_alphanumeric() || c == '_') {
                    break;
                }
                ahead += 1;
            }
        }
        let bits = match target {
            Target::Double => 0x7FF8_0000_0000_0000,
            Target::Single => 0x7FC0_0000,
        };
        return Ok(Parsed::Value {
            bits: with_sign(bits, negative, target),
            range_error: false,
            end: cursor.index,
        });
    }

    if cursor.peek()? == '0' && matches!(cursor.peek_at(1)?, 'x' | 'X') {
        let next = cursor.peek_at(2)?;
        if next.is_ascii_hexdigit() || (next == '.' && cursor.peek_at(3)?.is_ascii_hexdigit()) {
            return Err(unsupported());
        }
    }

    let mut digits = cursor.take_digits(&mut text)?;
    let mut nonzero = text.bytes().any(|b| (b'1'..=b'9').contains(&b));
    if cursor.peek()? == '.' {
        let mark = text.len();
        text.push('.');
        cursor.index += 1;
        let fraction = cursor.take_digits(&mut text)?;
        nonzero |= text[mark..].bytes().any(|b| (b'1'..=b'9').contains(&b));
        digits += fraction;
        if digits == 0 {
            return Ok(Parsed::None);
        }
    }
    if digits == 0 {
        return Ok(Parsed::None);
    }
    // An exponent counts only when at least one digit follows it.
    if matches!(cursor.peek()?, 'e' | 'E') {
        let sign = matches!(cursor.peek_at(1)?, '+' | '-');
        let first = cursor.peek_at(if sign { 2 } else { 1 })?;
        if first.is_ascii_digit() {
            text.push('e');
            if sign {
                text.push(cursor.peek_at(1)?);
            }
            cursor.index += if sign { 2 } else { 1 };
            cursor.take_digits(&mut text)?;
        }
    }

    let (bits, overflow, zero) = match target {
        Target::Double => {
            let value: f64 = text.parse().map_err(|_| unsupported())?;
            (value.to_bits(), value.is_infinite(), value == 0.0)
        }
        Target::Single => {
            let value: f32 = text.parse().map_err(|_| unsupported())?;
            (
                u64::from(value.to_bits()),
                value.is_infinite(),
                value == 0.0,
            )
        }
    };
    Ok(Parsed::Value {
        bits,
        range_error: overflow || (zero && nonzero),
        end: cursor.index,
    })
}

fn with_sign(bits: u64, negative: bool, target: Target) -> u64 {
    if !negative {
        return bits;
    }
    match target {
        Target::Double => bits | (1 << 63),
        Target::Single => bits | (1 << 31),
    }
}

fn convert(
    memory: &mut GuestMemory,
    cpu: &mut CpuState,
    string: u64,
    end_pointer: u64,
    width: Width,
    target: Target,
) -> Result<u64, ExitReason> {
    if string == 0 {
        return Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(0)));
    }
    let (bits, end) = match parse(memory, string, width, target)? {
        Parsed::None => (0, 0),
        Parsed::Value {
            bits,
            range_error,
            end,
        } => {
            if range_error {
                let errno = memory
                    .crt_errno_address(cpu.gs_base)
                    .map_err(ExitReason::MemoryFault)?;
                memory
                    .write_u32(errno, ERANGE)
                    .map_err(ExitReason::MemoryFault)?;
            }
            (bits, end)
        }
    };
    if end_pointer != 0 {
        let step = if width == Width::Wide { 2 } else { 1 };
        memory
            .write_u64(end_pointer, string + end as u64 * step)
            .map_err(ExitReason::MemoryFault)?;
    }
    Ok(match target {
        Target::Double => Float64(bits).place(cpu),
        Target::Single => Float32(bits as u32).place(cpu),
    })
}

pub(super) fn register(registry: &mut ImportRegistry) {
    for (symbol, width, target) in [
        ("strtod", Width::Byte, Target::Double),
        ("strtof", Width::Byte, Target::Single),
        ("wcstod", Width::Wide, Target::Double),
        ("wcstof", Width::Wide, Target::Single),
    ] {
        registry.register_with_status(
            UCRT,
            symbol,
            ServiceStatus::Partial,
            move |args, cpu, memory| convert(memory, cpu, args.rcx, args.rdx, width, target),
        );
    }
    for (symbol, width) in [("atof", Width::Byte), ("_wtof", Width::Wide)] {
        registry.register_with_status(
            UCRT,
            symbol,
            ServiceStatus::Partial,
            move |args, cpu, memory| convert(memory, cpu, args.rcx, 0, width, Target::Double),
        );
    }
    for symbol in ["strtod", "strtof", "wcstod", "wcstof", "atof", "_wtof"] {
        registry.alias(CONVERT_API, UCRT, symbol);
    }
}
