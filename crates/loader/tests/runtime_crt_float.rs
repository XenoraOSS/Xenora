// SPDX-License-Identifier: GPL-3.0-or-later

//! Floating-point string conversion through the guest import table. Results
//! are read from XMM0. Expected values are the IEEE 754 nearest values of the
//! decimal inputs, written as bit patterns or exact Rust literals, and follow
//! the ISO C grammar for strtod in the "C" locale.

mod support;

use support::*;
use xenora_loader::cpu::{CpuState, ExitReason};

const UCRT: &str = "ucrtbase.dll";
const CONVERT: &str = "api-ms-win-crt-convert-l1-1-0.dll";
const IMPORTS: &[(&str, &str)] = &[
    (UCRT, "strtod"),
    (UCRT, "strtof"),
    (UCRT, "wcstod"),
    (UCRT, "wcstof"),
    (UCRT, "atof"),
    (UCRT, "_wtof"),
    (UCRT, "_errno"),
];
const ALIASES: &[(&str, &str)] = &[(CONVERT, "strtod"), (CONVERT, "atof")];

const TEXT: u64 = SCRATCH;
const END: u64 = SCRATCH + 0x800;

fn xmm0(cpu: &CpuState) -> u64 {
    cpu.xmm[0] as u64
}

/// Run `symbol` and return XMM0's low 64 bits and RAX.
fn float_call(guest: &mut Guest, symbol: &str, args: &[u64]) -> u64 {
    let (cpu, exit) = guest.run_state(symbol, args);
    assert_eq!(exit, ExitReason::Halt, "{symbol}");
    xmm0(&cpu)
}

fn put(guest: &mut Guest, text: &str) {
    guest.memory.write_bytes(TEXT, text.as_bytes()).unwrap();
    guest.memory.write_u8(TEXT + text.len() as u64, 0).unwrap();
}

fn put_wide(guest: &mut Guest, text: &str) {
    for (i, unit) in text.encode_utf16().chain([0]).enumerate() {
        guest.memory.write_u16(TEXT + 2 * i as u64, unit).unwrap();
    }
}

fn errno(guest: &mut Guest) -> u32 {
    let at = guest.call("_errno", &[]);
    guest.u32_at(at)
}

fn clear_errno(guest: &mut Guest) {
    let at = guest.call("_errno", &[]);
    guest.memory.write_u32(at, 0).unwrap();
}

/// strtod `text`, returning the bits and how many bytes were consumed.
fn strtod(guest: &mut Guest, text: &str) -> (u64, u64) {
    put(guest, text);
    let bits = float_call(guest, "strtod", &[TEXT, END]);
    (bits, guest.u64_at(END) - TEXT)
}

#[test]
fn decimal_forms_parse_to_the_nearest_double() {
    let mut guest = Guest::new(IMPORTS, &[]);
    for (text, value, consumed) in [
        ("1.5", 1.5f64, 3),
        ("  \t-42", -42.0, 6),
        ("+.25x", 0.25, 4),
        ("7.", 7.0, 2),
        ("1e3", 1000.0, 3),
        ("2.5E-1", 0.25, 6),
        // The exponent needs a digit; "e" and "e+" are left unconsumed.
        ("3e", 3.0, 1),
        ("3e+", 3.0, 1),
        // 0.1 is not exact: the nearest double is 0x3FB999999999999A.
        ("0.1", 0.1, 3),
        ("-0", -0.0, 2),
    ] {
        let (bits, used) = strtod(&mut guest, text);
        assert_eq!(bits, value.to_bits(), "{text:?}");
        assert_eq!(used, consumed, "{text:?}");
    }
    assert_eq!(0.1f64.to_bits(), 0x3FB9_9999_9999_999A);
}

#[test]
fn no_conversion_returns_zero_and_the_start() {
    let mut guest = Guest::new(IMPORTS, &[]);
    for text in ["", "abc", "  .", "-", "+e5", "  x1"] {
        let (bits, used) = strtod(&mut guest, text);
        assert_eq!((bits, used), (0, 0), "{text:?}");
    }
}

#[test]
fn infinity_and_nan_are_case_insensitive() {
    let mut guest = Guest::new(IMPORTS, &[]);
    for (text, bits, consumed) in [
        ("inf", f64::INFINITY.to_bits(), 3),
        ("-INFINITY!", f64::NEG_INFINITY.to_bits(), 9),
        ("Infinit", f64::INFINITY.to_bits(), 3),
        ("nan", 0x7FF8_0000_0000_0000, 3),
        ("-NaN(abc_1)z", 0xFFF8_0000_0000_0000, 11),
        // An unclosed payload is not part of the number.
        ("nan(ab", 0x7FF8_0000_0000_0000, 3),
    ] {
        assert_eq!(strtod(&mut guest, text), (bits, consumed), "{text:?}");
    }
}

#[test]
fn out_of_range_results_set_erange() {
    let mut guest = Guest::new(IMPORTS, &[]);
    clear_errno(&mut guest);
    assert_eq!(strtod(&mut guest, "1e400").0, f64::INFINITY.to_bits());
    assert_eq!(errno(&mut guest), 34);
    clear_errno(&mut guest);
    assert_eq!(strtod(&mut guest, "-1e-400").0, (-0.0f64).to_bits());
    assert_eq!(errno(&mut guest), 34, "nonzero digits underflowed to zero");
    clear_errno(&mut guest);
    assert_eq!(strtod(&mut guest, "0.000e-400").0, 0);
    assert_eq!(errno(&mut guest), 0, "an exact zero is not a range error");
    // 3.4e38 fits a float; 3.5e38 overflows only in single precision.
    clear_errno(&mut guest);
    put(&mut guest, "3.5e38");
    let single = float_call(&mut guest, "strtof", &[TEXT, 0]);
    assert_eq!(single as u32, f32::INFINITY.to_bits());
    assert_eq!(errno(&mut guest), 34);
}

#[test]
fn strtof_rounds_once_from_the_decimal() {
    let mut guest = Guest::new(IMPORTS, &[]);
    // 1.00000005960464477539062500001 lies just above the midpoint between
    // 1.0f and the next float, so it rounds up. Rounding through double first
    // lands exactly on the midpoint and then rounds to even, giving 1.0f.
    let text = "1.00000005960464477539062500001";
    put(&mut guest, text);
    let single = float_call(&mut guest, "strtof", &[TEXT, END]) as u32;
    assert_eq!(single, 0x3F80_0001);
    let through_double = text.parse::<f64>().unwrap() as f32;
    assert_eq!(through_double.to_bits(), 0x3F80_0000, "the hazard is real");
}

#[test]
fn wide_forms_and_atof_share_the_parser() {
    let mut guest = Guest::new(IMPORTS, &[]);
    put_wide(&mut guest, " -2.5e2z");
    assert_eq!(
        float_call(&mut guest, "wcstod", &[TEXT, END]),
        (-250.0f64).to_bits()
    );
    assert_eq!(
        guest.u64_at(END),
        TEXT + 2 * 7,
        "end counts wide characters"
    );
    assert_eq!(
        float_call(&mut guest, "wcstof", &[TEXT, 0]) as u32,
        (-250.0f32).to_bits()
    );
    assert_eq!(
        float_call(&mut guest, "_wtof", &[TEXT]),
        (-250.0f64).to_bits()
    );
    put(&mut guest, "6.25 apples");
    assert_eq!(float_call(&mut guest, "atof", &[TEXT]), 6.25f64.to_bits());
    let mut aliased = Guest::new(ALIASES, &[]);
    put(&mut aliased, "0.5");
    assert_eq!(float_call(&mut aliased, "atof", &[TEXT]), 0.5f64.to_bits());
    assert_eq!(
        float_call(&mut aliased, "strtod", &[TEXT, 0]),
        0.5f64.to_bits()
    );
}

#[test]
fn hexadecimal_constants_stop_the_guest() {
    let mut guest = Guest::new(IMPORTS, &[]);
    put(&mut guest, "0x1.8p3");
    assert_eq!(guest.fails("strtod", &[TEXT, END]), RUNTIME_LIMIT);
    // Without a hex digit after it, "0x" is just the number zero.
    assert_eq!(strtod(&mut guest, "0xg"), (0, 1));
}
