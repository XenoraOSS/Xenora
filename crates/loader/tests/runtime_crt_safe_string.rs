// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounds-checked UCRT string copy and append through the guest import table.
//! Expectations follow the public UCRT pages for strcpy_s, strcat_s,
//! strncpy_s and strncat_s (and their wcs forms): EINVAL for a null or empty
//! destination or a null source, ERANGE when the result does not fit, and
//! STRUNCATE when _TRUNCATE shortens the copy. A failure after the
//! destination is validated leaves it as an empty string.

mod support;

use support::*;

const UCRT: &str = "ucrtbase.dll";
const IMPORTS: &[(&str, &str)] = &[
    (UCRT, "_errno"),
    (UCRT, "strcpy_s"),
    (UCRT, "strcat_s"),
    (UCRT, "strncpy_s"),
    (UCRT, "strncat_s"),
    (UCRT, "wcscpy_s"),
    (UCRT, "wcscat_s"),
    (UCRT, "wcsncpy_s"),
    ("api-ms-win-crt-string-l1-1-0.dll", "wcsncat_s"),
];

const EINVAL: u64 = 22;
const ERANGE: u64 = 34;
const STRUNCATE: u64 = 80;
const TRUNCATE: u64 = u64::MAX;
const DEST: u64 = SCRATCH;
const SRC: u64 = SCRATCH + 0x100;

fn put(guest: &mut Guest, at: u64, text: &str) {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    guest.memory.write_bytes(at, &bytes).unwrap();
}

fn put_wide(guest: &mut Guest, at: u64, text: &str) {
    for (index, unit) in text.encode_utf16().chain([0]).enumerate() {
        guest.memory.write_u16(at + 2 * index as u64, unit).unwrap();
    }
}

fn text(guest: &Guest, at: u64) -> String {
    let mut bytes = Vec::new();
    while let Ok(byte) = guest.memory.read_u8(at + bytes.len() as u64) {
        if byte == 0 {
            break;
        }
        bytes.push(byte);
    }
    String::from_utf8(bytes).unwrap()
}

fn wide(guest: &Guest, at: u64) -> String {
    let units: Vec<u16> = (0..)
        .map(|i| guest.u16_at(at + 2 * i))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16(&units).unwrap()
}

fn errno(guest: &mut Guest) -> u64 {
    let address = guest.call("_errno", &[]);
    u64::from(guest.u32_at(address))
}

fn clear_errno(guest: &mut Guest) {
    let address = guest.call("_errno", &[]);
    guest.memory.write_u32(address, 0).unwrap();
}

fn fill(guest: &mut Guest) {
    guest.memory.write_bytes(DEST, &[b'#'; 64]).unwrap();
}

#[test]
fn strcpy_s_copies_when_it_fits_and_reports_errors() {
    let mut guest = Guest::new(IMPORTS, &[]);
    put(&mut guest, SRC, "hello");
    fill(&mut guest);
    assert_eq!(guest.call("strcpy_s", &[DEST, 6, SRC]), 0);
    assert_eq!(text(&guest, DEST), "hello");

    // Needs six bytes with the terminator.
    fill(&mut guest);
    clear_errno(&mut guest);
    assert_eq!(guest.call("strcpy_s", &[DEST, 5, SRC]), ERANGE);
    assert_eq!(errno(&mut guest), ERANGE);
    assert_eq!(guest.memory.read_u8(DEST).unwrap(), 0, "emptied");

    fill(&mut guest);
    assert_eq!(guest.call("strcpy_s", &[DEST, 8, 0]), EINVAL);
    assert_eq!(guest.memory.read_u8(DEST).unwrap(), 0);
    fill(&mut guest);
    assert_eq!(guest.call("strcpy_s", &[DEST, 0, SRC]), EINVAL);
    assert_eq!(guest.memory.read_u8(DEST).unwrap(), b'#', "untouched");
    assert_eq!(guest.call("strcpy_s", &[0, 8, SRC]), EINVAL);
    assert_eq!(errno(&mut guest), EINVAL);
}

#[test]
fn strcat_s_appends_and_rejects_unterminated_destinations() {
    let mut guest = Guest::new(IMPORTS, &[]);
    put(&mut guest, DEST, "ab");
    put(&mut guest, SRC, "cde");
    assert_eq!(guest.call("strcat_s", &[DEST, 6, SRC]), 0);
    assert_eq!(text(&guest, DEST), "abcde");
    assert_eq!(guest.call("strcat_s", &[DEST, 8, SRC]), ERANGE);
    assert_eq!(text(&guest, DEST), "");

    // No terminator within the stated size.
    fill(&mut guest);
    assert_eq!(guest.call("strcat_s", &[DEST, 10, SRC]), EINVAL);
    assert_eq!(guest.memory.read_u8(DEST).unwrap(), 0);
}

#[test]
fn strncpy_s_counts_truncates_and_handles_zero() {
    let mut guest = Guest::new(IMPORTS, &[]);
    put(&mut guest, SRC, "abcdef");
    fill(&mut guest);
    assert_eq!(guest.call("strncpy_s", &[DEST, 8, SRC, 3]), 0);
    assert_eq!(text(&guest, DEST), "abc");
    // The count is larger than the string: the whole string is copied.
    assert_eq!(guest.call("strncpy_s", &[DEST, 8, SRC, 100]), 0);
    assert_eq!(text(&guest, DEST), "abcdef");
    assert_eq!(guest.call("strncpy_s", &[DEST, 4, SRC, 5]), ERANGE);
    assert_eq!(text(&guest, DEST), "");

    clear_errno(&mut guest);
    assert_eq!(
        guest.call("strncpy_s", &[DEST, 4, SRC, TRUNCATE]),
        STRUNCATE
    );
    assert_eq!(text(&guest, DEST), "abc");
    assert_eq!(errno(&mut guest), 0, "truncation is not an error");
    assert_eq!(guest.call("strncpy_s", &[DEST, 7, SRC, TRUNCATE]), 0);
    assert_eq!(text(&guest, DEST), "abcdef");

    // A count of zero empties the destination even with a null source.
    assert_eq!(guest.call("strncpy_s", &[DEST, 4, 0, 0]), 0);
    assert_eq!(text(&guest, DEST), "");
    assert_eq!(guest.call("strncpy_s", &[DEST, 4, 0, 2]), EINVAL);
}

#[test]
fn strncat_s_appends_up_to_count_or_truncates() {
    let mut guest = Guest::new(IMPORTS, &[]);
    put(&mut guest, DEST, "12");
    put(&mut guest, SRC, "34567");
    assert_eq!(guest.call("strncat_s", &[DEST, 8, SRC, 2]), 0);
    assert_eq!(text(&guest, DEST), "1234");
    assert_eq!(
        guest.call("strncat_s", &[DEST, 8, SRC, TRUNCATE]),
        STRUNCATE
    );
    assert_eq!(text(&guest, DEST), "1234345");
    put(&mut guest, DEST, "12");
    assert_eq!(guest.call("strncat_s", &[DEST, 5, SRC, 3]), ERANGE);
    assert_eq!(text(&guest, DEST), "");
}

#[test]
fn wide_forms_count_characters_not_bytes() {
    let mut guest = Guest::new(IMPORTS, &[]);
    put_wide(&mut guest, SRC, "héllo");
    assert_eq!(guest.call("wcscpy_s", &[DEST, 6, SRC]), 0);
    assert_eq!(wide(&guest, DEST), "héllo");
    assert_eq!(guest.call("wcscpy_s", &[DEST, 5, SRC]), ERANGE);
    assert_eq!(guest.u16_at(DEST), 0);
    put_wide(&mut guest, DEST, "ab");
    put_wide(&mut guest, SRC, "cd");
    assert_eq!(guest.call("wcscat_s", &[DEST, 5, SRC]), 0);
    assert_eq!(wide(&guest, DEST), "abcd");
    put_wide(&mut guest, SRC, "wxyz");
    assert_eq!(
        guest.call("wcsncpy_s", &[DEST, 3, SRC, TRUNCATE]),
        STRUNCATE
    );
    assert_eq!(wide(&guest, DEST), "wx");
    // The string API set reaches the same service.
    assert_eq!(guest.call("wcsncat_s", &[DEST, 4, SRC, 1]), 0);
    assert_eq!(wide(&guest, DEST), "wxw");
}
