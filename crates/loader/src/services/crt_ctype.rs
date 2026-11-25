// SPDX-License-Identifier: GPL-3.0-or-later

//! C-locale character classification and case mapping (`ctype.h`).
//!
//! Predicates follow the MSVC contract: the argument is an `int` that is either
//! `EOF` (-1) or representable as `unsigned char`. Out-of-range values stop the
//! guest rather than invoking undefined behaviour.

use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};

const UCRT: &str = "ucrtbase.dll";
const STRING_API: &str = "api-ms-win-crt-string-l1-1-0.dll";
const EOF: i32 = -1;

fn as_char(value: u64) -> Result<Option<u8>, ExitReason> {
    let value = value as i32;
    if value == EOF {
        return Ok(None);
    }
    if !(0..=255).contains(&value) {
        return Err(ExitReason::IllegalInstruction(0xFE));
    }
    Ok(Some(value as u8))
}

fn classify(value: u64, pred: fn(u8) -> bool) -> Result<u64, ExitReason> {
    Ok(u64::from(as_char(value)?.is_some_and(pred)))
}

fn map_case(value: u64, map: fn(u8) -> u8) -> Result<u64, ExitReason> {
    Ok(match as_char(value)? {
        None => value as i32 as u64,
        Some(byte) => u64::from(map(byte)),
    })
}

fn is_alnum(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
}
fn is_alpha(byte: u8) -> bool {
    byte.is_ascii_alphabetic()
}
fn is_digit(byte: u8) -> bool {
    byte.is_ascii_digit()
}
fn is_lower(byte: u8) -> bool {
    byte.is_ascii_lowercase()
}
fn is_upper(byte: u8) -> bool {
    byte.is_ascii_uppercase()
}
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c')
}
fn is_xdigit(byte: u8) -> bool {
    byte.is_ascii_hexdigit()
}
fn is_print(byte: u8) -> bool {
    (0x20..=0x7e).contains(&byte)
}
fn is_cntrl(byte: u8) -> bool {
    byte < 0x20 || byte == 0x7f
}
fn is_punct(byte: u8) -> bool {
    is_print(byte) && !is_alnum(byte) && !is_space(byte)
}

pub(super) fn register(registry: &mut ImportRegistry) {
    for (symbol, pred) in [
        ("isalnum", is_alnum as fn(u8) -> bool),
        ("isalpha", is_alpha),
        ("isdigit", is_digit),
        ("islower", is_lower),
        ("isupper", is_upper),
        ("isspace", is_space),
        ("isxdigit", is_xdigit),
        ("isprint", is_print),
        ("iscntrl", is_cntrl),
        ("ispunct", is_punct),
    ] {
        registry.register(UCRT, symbol, move |args, _, _| classify(args.rcx, pred));
        registry.alias(STRING_API, UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
    registry.register(UCRT, "tolower", |args, _, _| {
        map_case(args.rcx, |byte| byte.to_ascii_lowercase())
    });
    registry.register(UCRT, "toupper", |args, _, _| {
        map_case(args.rcx, |byte| byte.to_ascii_uppercase())
    });
    for symbol in ["tolower", "toupper"] {
        registry.alias(STRING_API, UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::CpuState;
    use crate::import_table::ImportName;
    use crate::imports::Win64Args;
    use crate::memory::GuestMemory;

    #[test]
    fn ctype_follows_c_locale_ascii_rules() {
        let mut registry = ImportRegistry::new();
        register(&mut registry);
        let mut memory = GuestMemory::new();
        let mut cpu = CpuState::default();
        let mut call = |symbol: &str, value: i32| {
            cpu.set(crate::cpu::Register::Rcx, value as u64);
            let service = registry
                .resolve(UCRT, &ImportName::Name(symbol.into()))
                .unwrap();
            (service.handler)(&Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap()
        };
        assert_ne!(call("isalpha", b'A' as i32), 0);
        assert_eq!(call("isalpha", b'1' as i32), 0);
        assert_ne!(call("isdigit", b'9' as i32), 0);
        assert_eq!(call("isdigit", EOF), 0);
        assert_eq!(call("tolower", b'Z' as i32), u64::from(b'z'));
        assert_eq!(call("toupper", b'a' as i32), u64::from(b'A'));
        assert_eq!(call("tolower", EOF), EOF as u64);
    }
}
