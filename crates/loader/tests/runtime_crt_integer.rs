// SPDX-License-Identifier: GPL-3.0-or-later

//! Integer abs and div through the guest import table, under the Windows
//! LLP64 model (`long` is 32 bits) and the Win64 return rules: an 8-byte
//! `div_t`/`ldiv_t` comes back in RAX with the quotient in the low half, and a
//! 16-byte `lldiv_t` through a hidden pointer in RCX that RAX returns.
//! Expected values follow ISO C (quotients truncate toward zero and
//! `quot * den + rem == num`) and were worked out by hand.

mod support;

use support::*;
use xenora_loader::cpu::ExitReason;

const UCRT: &str = "ucrtbase.dll";
const IMPORTS: &[(&str, &str)] = &[
    (UCRT, "abs"),
    (UCRT, "labs"),
    (UCRT, "llabs"),
    (UCRT, "div"),
    (UCRT, "ldiv"),
    (UCRT, "lldiv"),
];
const UTILITY: &[(&str, &str)] = &[
    ("api-ms-win-crt-utility-l1-1-0.dll", "labs"),
    ("api-ms-win-crt-utility-l1-1-0.dll", "lldiv"),
];

const RESULT: u64 = SCRATCH;

fn int(value: i32) -> u64 {
    // A 32-bit argument; the upper half of the register is garbage the
    // callee must ignore.
    0xDEAD_BEEF_0000_0000 | u64::from(value as u32)
}

fn split(rax: u64) -> (i32, i32) {
    (rax as u32 as i32, (rax >> 32) as u32 as i32)
}

#[test]
fn abs_and_labs_are_32_bit_and_llabs_is_64_bit() {
    let mut guest = Guest::new(IMPORTS, &[]);
    for symbol in ["abs", "labs"] {
        assert_eq!(guest.call(symbol, &[int(-5)]), 5, "{symbol}");
        assert_eq!(guest.call(symbol, &[int(7)]), 7, "{symbol}");
        // The minimum wraps to itself, as x86 neg does.
        assert_eq!(
            guest.call(symbol, &[int(i32::MIN)]),
            0x8000_0000,
            "{symbol}"
        );
    }
    assert_eq!(
        guest.call("llabs", &[(-5_000_000_000i64) as u64]),
        5_000_000_000
    );
    assert_eq!(guest.call("llabs", &[i64::MIN as u64]), i64::MIN as u64);
}

#[test]
fn div_and_ldiv_pack_quotient_and_remainder_into_rax() {
    let mut guest = Guest::new(IMPORTS, &[]);
    for symbol in ["div", "ldiv"] {
        for (num, den, quot, rem) in [
            (7, 2, 3, 1),
            (-7, 2, -3, -1),
            (7, -2, -3, 1),
            (-7, -2, 3, -1),
            (i32::MIN, 1, i32::MIN, 0),
        ] {
            let rax = guest.call(symbol, &[int(num), int(den)]);
            assert_eq!(split(rax), (quot, rem), "{symbol}({num}, {den})");
        }
    }
}

#[test]
fn lldiv_returns_through_the_hidden_pointer() {
    for imports in [IMPORTS, UTILITY] {
        let mut guest = Guest::new(imports, &[]);
        let num = -10_000_000_007i64;
        let rax = guest.call("lldiv", &[RESULT, num as u64, 1_000_000_000]);
        assert_eq!(rax, RESULT);
        assert_eq!(guest.u64_at(RESULT) as i64, -10);
        assert_eq!(guest.u64_at(RESULT + 8) as i64, -7);
        // The API set reaches the same LLP64 labs.
        if imports == UTILITY {
            assert_eq!(guest.call("labs", &[int(-3)]), 3);
        }
    }
}

#[test]
fn division_by_zero_and_overflow_raise_a_divide_error() {
    let mut guest = Guest::new(IMPORTS, &[]);
    for symbol in ["div", "ldiv"] {
        assert_eq!(
            guest.fails(symbol, &[int(1), int(0)]),
            ExitReason::DivideError
        );
        assert_eq!(
            guest.fails(symbol, &[int(i32::MIN), int(-1)]),
            ExitReason::DivideError
        );
    }
    assert_eq!(
        guest.fails("lldiv", &[RESULT, 1, 0]),
        ExitReason::DivideError
    );
    assert_eq!(
        guest.fails("lldiv", &[RESULT, i64::MIN as u64, u64::MAX]),
        ExitReason::DivideError
    );
}
