// SPDX-License-Identifier: GPL-3.0-or-later

mod support;

use support::{Guest, SCRATCH, SCRATCH_LEN};

const IMPORTS: &[(&str, &str)] = &[
    ("winhttp.dll", "WinHttpCrackUrl"),
    ("kernel32.dll", "GetLastError"),
    ("kernel32.dll", "SetLastError"),
];
const URL: u64 = SCRATCH + 0x100;
const OUTPUT: u64 = SCRATCH + 0x400;
const BUFFER: u64 = SCRATCH + 0x600;
const STRUCT_SIZE: usize = 104;
// Offsets from the Win64 SDK's eight-byte-packed URL_COMPONENTS definition.
const SCHEME: u64 = 8;
const SCHEME_KIND: u64 = 20;
const HOST: u64 = 24;
const PORT: u64 = 36;
const USER: u64 = 40;
const PASSWORD: u64 = 56;
const PATH: u64 = 72;
const EXTRA: u64 = 88;

fn setup() -> Guest {
    let mut guest = Guest::new(IMPORTS, &[]);
    let mut fields = [0; STRUCT_SIZE];
    fields[..4].copy_from_slice(&(STRUCT_SIZE as u32).to_le_bytes());
    fields[4..8].fill(0xa5);
    fields[38..40].fill(0x5a);
    fields[100..104].fill(0xc3);
    guest.memory.write_bytes(OUTPUT, &fields).unwrap();
    guest
}

fn put_url(guest: &mut Guest, text: &str) -> u64 {
    for (index, unit) in text.encode_utf16().chain([0]).enumerate() {
        guest
            .memory
            .write_u16(URL + index as u64 * 2, unit)
            .unwrap();
    }
    text.encode_utf16().count() as u64
}

fn output(guest: &mut Guest, field: u64, ptr: u64, capacity: u32) {
    guest.memory.write_u64(OUTPUT + field, ptr).unwrap();
    guest
        .memory
        .write_u32(OUTPUT + field + 8, capacity)
        .unwrap();
}

fn contents(guest: &Guest, start: u64, len: usize) -> Vec<u8> {
    let mut result = vec![0; len];
    guest.memory.read_bytes(start, &mut result).unwrap();
    result
}

fn view(guest: &Guest, field: u64, start: u64, len: u32) {
    assert_eq!(guest.u64_at(OUTPUT + field), URL + start * 2);
    assert_eq!(guest.u32_at(OUTPUT + field + 8), len);
}

#[test]
fn counted_url_returns_exact_guest_views_and_preserves_win64_padding() {
    let mut guest = setup();
    let count = put_url(&mut guest, "HTTP://example.test/a🦀?q=%23#frag");
    // Counted input ends here even when the next WCHAR is nonzero.
    guest
        .memory
        .write_u16(URL + 2 * count, b'X'.into())
        .unwrap();
    for field in [SCHEME, HOST, PATH, EXTRA] {
        output(&mut guest, field, 0, u32::MAX);
    }
    guest.call("SetLastError", &[0xfeed]);
    assert_eq!(
        guest.call("WinHttpCrackUrl", &[URL, 0x1_0000_0000 | count, 0, OUTPUT]),
        1
    );
    assert_eq!(guest.error(), 0xfeed);
    assert_eq!(guest.u32_at(OUTPUT + SCHEME_KIND), 1);
    assert_eq!(guest.u16_at(OUTPUT + PORT), 80);
    view(&guest, SCHEME, 0, 4);
    view(&guest, HOST, 7, 12);
    view(&guest, PATH, 19, 4);
    view(&guest, EXTRA, 23, 11);
    assert_eq!(guest.u64_at(OUTPUT + USER), 0);
    assert_eq!(guest.u32_at(OUTPUT + PASSWORD + 8), 0);
    assert_eq!(contents(&guest, OUTPUT + 4, 4), [0xa5; 4]);
    assert_eq!(contents(&guest, OUTPUT + 38, 2), [0x5a; 2]);
    assert_eq!(contents(&guest, OUTPUT + 100, 4), [0xc3; 4]);
}

#[test]
fn copied_fields_terminate_and_the_path_can_include_query_and_fragment() {
    let mut guest = setup();
    put_url(&mut guest, "https://alice:secret@example.test:8443/p?q#f");
    output(&mut guest, HOST, BUFFER, 13);
    output(&mut guest, PATH, BUFFER + 64, 7);
    output(&mut guest, EXTRA, u64::MAX, 0);
    for field in [USER, PASSWORD] {
        output(&mut guest, field, 0, 1);
    }
    guest.memory.write_bytes(BUFFER - 1, &[0x7d; 96]).unwrap();
    assert_eq!(guest.call("WinHttpCrackUrl", &[URL, 0, 0, OUTPUT]), 1);
    assert_eq!(guest.u32_at(OUTPUT + SCHEME_KIND), 2);
    assert_eq!(guest.u16_at(OUTPUT + PORT), 8443);
    view(&guest, USER, 8, 5);
    view(&guest, PASSWORD, 14, 6);
    assert_eq!(guest.u64_at(OUTPUT + EXTRA), u64::MAX);
    assert_eq!(guest.u32_at(OUTPUT + EXTRA + 8), 0);
    for (field, address, text) in [
        (HOST, BUFFER, "example.test"),
        (PATH, BUFFER + 64, "/p?q#f"),
    ] {
        let bytes: Vec<_> = text
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(guest.u64_at(OUTPUT + field), address);
        assert_eq!(
            guest.u32_at(OUTPUT + field + 8),
            (bytes.len() / 2 - 1) as u32
        );
        assert_eq!(contents(&guest, address, bytes.len()), bytes);
        assert_eq!(contents(&guest, address - 1, 1), [0x7d]);
        assert_eq!(contents(&guest, address + bytes.len() as u64, 1), [0x7d]);
    }
}

#[test]
fn empty_components_are_deterministic_and_ipv6_authority_stays_in_the_input() {
    let mut guest = setup();
    put_url(&mut guest, "https://[::1]:8443");
    output(&mut guest, HOST, 0, 1);
    output(&mut guest, PATH, BUFFER, 1);
    output(&mut guest, EXTRA, 0, 1);
    guest.memory.write_u32(BUFFER, 0x5a5a_5a5a).unwrap();
    assert_eq!(guest.call("WinHttpCrackUrl", &[URL, 0, 0, OUTPUT]), 1);
    view(&guest, HOST, 8, 5);
    assert_eq!(guest.u16_at(OUTPUT + PORT), 8443);
    assert_eq!(guest.u32_at(OUTPUT + PATH + 8), 0);
    assert_eq!(guest.u64_at(OUTPUT + EXTRA), 0);
    assert_eq!(guest.u32_at(OUTPUT + EXTRA + 8), 0);
    assert_eq!(guest.u32_at(BUFFER), 0x5a5a_0000);
}

#[test]
fn a_short_or_unwritable_later_destination_leaves_all_output_unchanged() {
    let mut guest = setup();
    put_url(&mut guest, "https://example.test/file?q=x");
    guest.memory.write_bytes(BUFFER, &[0x6b; 72]).unwrap();
    output(&mut guest, HOST, BUFFER, 32);
    for (destination, capacity, error) in [
        (BUFFER + 56, 4, 122),
        (SCRATCH + SCRATCH_LEN - 1, 20, 87),
        (BUFFER + 4, 20, 87),
        (OUTPUT + 88, 20, 87),
        (URL, 20, 87),
    ] {
        output(&mut guest, PATH, destination, capacity);
        let record = contents(&guest, OUTPUT, STRUCT_SIZE);
        assert_eq!(guest.call("WinHttpCrackUrl", &[URL, 0, 0, OUTPUT]), 0);
        assert_eq!(guest.error(), error);
        assert_eq!(contents(&guest, OUTPUT, STRUCT_SIZE), record);
        assert_eq!(contents(&guest, BUFFER, 72), [0x6b; 72]);
    }
}

#[test]
fn unsupported_scheme_invalid_url_and_guest_pointer_failures_are_explicit() {
    let mut guest = setup();
    for (text, flags, error) in [
        ("ftp://example.test/a", 0, 12006),
        ("https://alice:secret@example.test/a", 0x4000, 12005),
        ("https://example.test:65536/a", 0, 12005),
        ("https://example.test/a", 0x8000_0000, 50),
    ] {
        put_url(&mut guest, text);
        let before = contents(&guest, OUTPUT, STRUCT_SIZE);
        assert_eq!(guest.call("WinHttpCrackUrl", &[URL, 0, flags, OUTPUT]), 0);
        assert_eq!(guest.error(), error);
        assert_eq!(contents(&guest, OUTPUT, STRUCT_SIZE), before);
    }
    for (input, count, record) in [
        (0, 0, OUTPUT),
        (u64::MAX - 1, 2, OUTPUT),
        (URL, 32_769, OUTPUT),
        (URL, 0, SCRATCH + SCRATCH_LEN - 1),
    ] {
        assert_eq!(guest.call("WinHttpCrackUrl", &[input, count, 0, record]), 0);
        assert_eq!(guest.error(), 87);
    }

    guest.memory.write_u32(OUTPUT, 84).unwrap();
    assert_eq!(guest.call("WinHttpCrackUrl", &[URL, 0, 0, OUTPUT]), 0);
    assert_eq!(guest.error(), 87);
}
