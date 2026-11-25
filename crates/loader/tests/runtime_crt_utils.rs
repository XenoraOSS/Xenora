// SPDX-License-Identifier: GPL-3.0-or-later

//! The small CRT utilities through the guest import table.

mod support;

use support::*;

const UCRT: &str = "ucrtbase.dll";

const IMPORTS: &[(&str, &str)] = &[
    (UCRT, "strrchr"),
    (UCRT, "_strdup"),
    (UCRT, "_strlwr"),
    (UCRT, "_strupr"),
    (UCRT, "_strrev"),
    (UCRT, "strcoll"),
    (UCRT, "strxfrm"),
    (UCRT, "memcpy_s"),
    (UCRT, "memmove_s"),
    (UCRT, "isblank"),
    (UCRT, "isgraph"),
    (UCRT, "_itoa"),
    (UCRT, "_i64toa"),
    (UCRT, "_ultoa"),
    (UCRT, "_itoa_s"),
    (UCRT, "getenv_s"),
    (UCRT, "_errno"),
];

const BUFFER: u64 = SCRATCH + 0x200;

fn narrow(guest: &mut Guest, at: u64, text: &str) {
    guest.memory.write_bytes(at, text.as_bytes()).unwrap();
    guest.memory.write_u8(at + text.len() as u64, 0).unwrap();
}

fn narrow_string(guest: &Guest, at: u64) -> String {
    let mut text = String::new();
    for i in 0.. {
        let byte = guest.memory.read_u8(at + i).unwrap();
        if byte == 0 {
            return text;
        }
        text.push(byte as char);
    }
    unreachable!()
}

fn errno(guest: &mut Guest) -> u32 {
    let address = guest.call("_errno", &[]);
    guest.u32_at(address)
}

#[test]
fn the_string_utilities_transform_and_find() {
    let mut guest = Guest::new(IMPORTS, &[]);

    // strrchr finds the last occurrence, the terminator counts.
    narrow(&mut guest, SCRATCH, "a.b.c");
    assert_eq!(
        guest.call("strrchr", &[SCRATCH, u64::from(b'.')]),
        SCRATCH + 3
    );
    assert_eq!(guest.call("strrchr", &[SCRATCH, u64::from(b'z')]), 0);
    assert_eq!(guest.call("strrchr", &[SCRATCH, 0]), SCRATCH + 5);

    // _strdup copies into owned memory.
    let copy = guest.call("_strdup", &[SCRATCH]);
    assert_ne!(copy, 0);
    assert_ne!(copy, SCRATCH);
    assert_eq!(narrow_string(&guest, copy), "a.b.c");

    // Case conversion in place answers the input.
    narrow(&mut guest, SCRATCH, "AbC");
    assert_eq!(guest.call("_strlwr", &[SCRATCH]), SCRATCH);
    assert_eq!(narrow_string(&guest, SCRATCH), "abc");
    assert_eq!(guest.call("_strupr", &[SCRATCH]), SCRATCH);
    assert_eq!(narrow_string(&guest, SCRATCH), "ABC");

    // Reversal in place.
    narrow(&mut guest, SCRATCH, "abcde");
    assert_eq!(guest.call("_strrev", &[SCRATCH]), SCRATCH);
    assert_eq!(narrow_string(&guest, SCRATCH), "edcba");
}

#[test]
fn collation_is_byte_wise_in_the_c_locale() {
    let mut guest = Guest::new(IMPORTS, &[]);
    narrow(&mut guest, SCRATCH, "abc");
    narrow(&mut guest, BUFFER, "abd");
    assert_ne!(guest.call("strcoll", &[SCRATCH, BUFFER]), 0);
    assert_eq!(guest.call("strcoll", &[SCRATCH, SCRATCH]), 0);

    // strxfrm copies with the documented length answer.
    let length = guest.call("strxfrm", &[BUFFER, SCRATCH, 16]);
    assert_eq!(length, 3);
    assert_eq!(narrow_string(&guest, BUFFER), "abc");
    // A zero-size query answers the length and writes nothing.
    assert_eq!(guest.call("strxfrm", &[BUFFER, SCRATCH, 0]), 3);
}

#[test]
fn the_annex_k_copies_enforce_their_constraints() {
    let mut guest = Guest::new(IMPORTS, &[]);
    narrow(&mut guest, SCRATCH, "source");

    // A clean copy succeeds and answers zero.
    assert_eq!(guest.call("memcpy_s", &[BUFFER, 16, SCRATCH, 7]), 0);
    assert_eq!(narrow_string(&guest, BUFFER), "source");

    // A count over the destination size zeroes it and answers ERANGE.
    guest
        .memory
        .write_bytes(BUFFER, b"keepkeepkeepkeep")
        .unwrap();
    assert_eq!(guest.call("memcpy_s", &[BUFFER, 8, SCRATCH, 12]), 34);
    assert_eq!(guest.u64_at(BUFFER), 0);
    assert_eq!(errno(&mut guest), 34);

    // Null pointers are invalid.
    assert_eq!(guest.call("memcpy_s", &[0, 8, SCRATCH, 4]), 22);
    assert_eq!(errno(&mut guest), 22);

    // memmove_s tolerates overlap; memcpy_s rejects it.
    assert_eq!(guest.call("memmove_s", &[BUFFER, 16, SCRATCH, 4]), 0);
    assert_eq!(guest.call("memcpy_s", &[SCRATCH + 1, 8, SCRATCH, 4]), 22);
}

#[test]
fn the_character_classes_cover_the_missing_ascii() {
    let mut guest = Guest::new(IMPORTS, &[]);
    assert_eq!(guest.call("isblank", &[u64::from(b' ')]), 1);
    assert_eq!(guest.call("isblank", &[u64::from(b'\t')]), 1);
    assert_eq!(guest.call("isblank", &[u64::from(b'\n')]), 0);
    assert_eq!(guest.call("isgraph", &[u64::from(b'!')]), 1);
    assert_eq!(guest.call("isgraph", &[u64::from(b' ')]), 0);
    assert_eq!(guest.call("isgraph", &[0x7f]), 0);
}

#[test]
fn the_integer_formatting_family_covers_radices_and_signs() {
    let mut guest = Guest::new(IMPORTS, &[]);

    // Decimal with sign, hexadecimal without, binary digits.
    assert_eq!(guest.call("_itoa", &[(-42i64) as u64, BUFFER, 10]), BUFFER);
    assert_eq!(narrow_string(&guest, BUFFER), "-42");
    assert_eq!(guest.call("_itoa", &[255, BUFFER, 16]), BUFFER);
    assert_eq!(narrow_string(&guest, BUFFER), "ff");
    assert_eq!(guest.call("_itoa", &[5, BUFFER, 2]), BUFFER);
    assert_eq!(narrow_string(&guest, BUFFER), "101");

    // Negative numbers in non-decimal radix are unsigned, as documented.
    assert_eq!(guest.call("_itoa", &[(-1i64) as u64, BUFFER, 16]), BUFFER);
    assert_eq!(narrow_string(&guest, BUFFER), "ffffffff");

    // The 64-bit and unsigned forms.
    assert_eq!(
        guest.call("_i64toa", &[(-9_000_000_000i64) as u64, BUFFER, 10]),
        BUFFER
    );
    assert_eq!(narrow_string(&guest, BUFFER), "-9000000000");
    assert_eq!(guest.call("_ultoa", &[4_000_000_000, BUFFER, 10]), BUFFER);
    assert_eq!(narrow_string(&guest, BUFFER), "4000000000");

    // The _s form enforces the buffer size.
    assert_eq!(guest.call("_itoa_s", &[12345, BUFFER, 4, 10]), 22);
    assert_eq!(errno(&mut guest), 22);
    assert_eq!(guest.call("_itoa_s", &[12345, BUFFER, 8, 10]), 0);
    assert_eq!(narrow_string(&guest, BUFFER), "12345");
}

#[test]
fn getenv_s_answers_the_documented_empty() {
    let mut guest = Guest::new(IMPORTS, &[]);
    narrow(&mut guest, SCRATCH, "PATH");
    assert_eq!(
        guest.call("getenv_s", &[BUFFER, BUFFER + 8, 16, SCRATCH]),
        0
    );
    assert_eq!(guest.u64_at(BUFFER), 0);
    assert_eq!(guest.memory.read_u8(BUFFER + 8).unwrap(), 0);
}
