// SPDX-License-Identifier: GPL-3.0-or-later

mod support;

use support::{Guest, SCRATCH, SCRATCH_LEN};

const IMPORTS: &[(&str, &str)] = &[
    ("winhttp.dll", "WinHttpCrackUrl"),
    ("winhttp.dll", "WinHttpCreateUrl"),
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
const ICU_DECODE: u64 = 0x1000_0000;

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
fn decode_copies_each_component_once_after_finding_url_boundaries() {
    let mut guest = setup();
    let count = put_url(
        &mut guest,
        "https://example.test/a%20b/%2520/%23mark?q=%4a%",
    );
    output(&mut guest, PATH, BUFFER, 15);
    output(&mut guest, EXTRA, BUFFER + 64, 6);

    assert_eq!(
        guest.call("WinHttpCrackUrl", &[URL, count, ICU_DECODE, OUTPUT]),
        1
    );
    for (field, address, decoded) in [
        (PATH, BUFFER, "/a b/%20/#mark"),
        (EXTRA, BUFFER + 64, "?q=J%"),
    ] {
        let bytes: Vec<_> = decoded
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(guest.u32_at(OUTPUT + field + 8), decoded.len() as u32);
        assert_eq!(contents(&guest, address, bytes.len()), bytes);
    }
}

#[test]
fn requested_view_with_decode_is_rejected_before_any_copied_output() {
    let mut guest = setup();
    put_url(&mut guest, "https://example.test/a%20b?q=%42");
    output(&mut guest, PATH, BUFFER, 8);
    output(&mut guest, EXTRA, 0, 1);
    guest.memory.write_bytes(BUFFER, &[0x7d; 16]).unwrap();
    let fields = contents(&guest, OUTPUT, STRUCT_SIZE);

    assert_eq!(
        guest.call("WinHttpCrackUrl", &[URL, 0, ICU_DECODE, OUTPUT]),
        0
    );
    assert_eq!(guest.error(), 87);
    assert_eq!(contents(&guest, OUTPUT, STRUCT_SIZE), fields);
    assert_eq!(contents(&guest, BUFFER, 16), [0x7d; 16]);
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

// ---- WinHttpCreateUrl ----

const COMPONENTS: u64 = OUTPUT;
const CREATED: u64 = SCRATCH + 0x800;
const LENGTH: u64 = SCRATCH + 0x40;
const STRINGS: u64 = SCRATCH + 0xa00;

/// Writes a UTF-16 component string plus terminator at `storage` and points
/// the record field at it. `counted` selects the exact-length form; otherwise
/// the zero length means the zero-terminated form.
fn component(guest: &mut Guest, field: u64, text: &str, counted: bool, storage: u64) {
    let units: Vec<u16> = text.encode_utf16().collect();
    for (index, unit) in units.iter().copied().chain([0]).enumerate() {
        guest
            .memory
            .write_u16(storage + index as u64 * 2, unit)
            .unwrap();
    }
    guest.memory.write_u64(COMPONENTS + field, storage).unwrap();
    guest
        .memory
        .write_u32(
            COMPONENTS + field + 8,
            if counted { units.len() as u32 } else { 0 },
        )
        .unwrap();
}

fn create_setup() -> Guest {
    let mut guest = Guest::new(IMPORTS, &[]);
    let mut fields = [0; STRUCT_SIZE];
    fields[..4].copy_from_slice(&(STRUCT_SIZE as u32).to_le_bytes());
    guest.memory.write_bytes(COMPONENTS, &fields).unwrap();
    guest
}

fn created(guest: &Guest) -> String {
    let len = guest.u32_at(LENGTH) as usize;
    let mut bytes = vec![0u8; len * 2];
    guest.memory.read_bytes(CREATED, &mut bytes).unwrap();
    let (words, _) = bytes.as_chunks::<2>();
    String::from_utf16(
        &words
            .iter()
            .copied()
            .map(u16::from_le_bytes)
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

#[test]
fn a_cracked_canonical_url_round_trips_through_creation() {
    let mut guest = setup();
    put_url(&mut guest, "http://user:pass@example.test:8080/p?q#f");
    for field in [SCHEME, HOST, USER, PASSWORD, PATH, EXTRA] {
        output(&mut guest, field, 0, u32::MAX);
    }
    assert_eq!(guest.call("WinHttpCrackUrl", &[URL, 0, 0, COMPONENTS]), 1);
    guest.memory.write_u32(LENGTH, 256).unwrap();
    assert_eq!(
        guest.call("WinHttpCreateUrl", &[COMPONENTS, 0, CREATED, LENGTH]),
        1
    );
    let expected = "http://user:pass@example.test:8080/p?q#f";
    assert_eq!(created(&guest), expected);
    assert_eq!(guest.u32_at(LENGTH), expected.len() as u32);
    // The terminator is written but not counted.
    assert_eq!(guest.u16_at(CREATED + 2 * expected.len() as u64), 0);
}

#[test]
fn creation_composes_scheme_port_and_path_with_canonical_choices() {
    let mut guest = create_setup();
    component(&mut guest, HOST, "example.test", false, STRINGS);
    component(&mut guest, PATH, "p", true, STRINGS + 0x40);
    // nScheme 2 selects https when no scheme string is present.
    guest.memory.write_u32(COMPONENTS + SCHEME_KIND, 2).unwrap();
    // The default port is elided; a non-default port is emitted.
    for (port, expected) in [
        (443u16, "https://example.test/p"),
        (0, "https://example.test/p"),
        (444, "https://example.test:444/p"),
    ] {
        guest.memory.write_u16(COMPONENTS + PORT, port).unwrap();
        guest.memory.write_u32(LENGTH, 256).unwrap();
        assert_eq!(
            guest.call("WinHttpCreateUrl", &[COMPONENTS, 0, CREATED, LENGTH]),
            1,
            "port {port}"
        );
        assert_eq!(created(&guest), expected, "port {port}");
    }
    // A scheme string wins over nScheme and is emitted verbatim.
    component(&mut guest, SCHEME, "http", false, STRINGS + 0x80);
    guest.memory.write_u16(COMPONENTS + PORT, 80).unwrap();
    guest.memory.write_u32(LENGTH, 256).unwrap();
    assert_eq!(
        guest.call("WinHttpCreateUrl", &[COMPONENTS, 0, CREATED, LENGTH]),
        1
    );
    assert_eq!(created(&guest), "http://example.test/p");
}

#[test]
fn creation_reports_required_size_before_writing() {
    let mut guest = create_setup();
    component(&mut guest, HOST, "example.test", false, STRINGS);
    guest.memory.write_u32(COMPONENTS + SCHEME_KIND, 1).unwrap();
    // "http://example.test" is 19 units; 20 with the terminator.
    for (buffer, capacity) in [(0u64, 0u32), (CREATED, 0), (CREATED, 19)] {
        guest.memory.write_u32(LENGTH, capacity).unwrap();
        let before = contents(&guest, CREATED, 40);
        assert_eq!(
            guest.call("WinHttpCreateUrl", &[COMPONENTS, 0, buffer, LENGTH]),
            0
        );
        assert_eq!(guest.error(), 122);
        assert_eq!(
            guest.u32_at(LENGTH),
            20,
            "buffer {buffer} capacity {capacity}"
        );
        assert_eq!(contents(&guest, CREATED, 40), before, "nothing written");
    }
    guest.memory.write_u32(LENGTH, 20).unwrap();
    assert_eq!(
        guest.call("WinHttpCreateUrl", &[COMPONENTS, 0, CREATED, LENGTH]),
        1
    );
    assert_eq!(guest.u32_at(LENGTH), 19);
    assert_eq!(created(&guest), "http://example.test");
}

#[test]
fn creation_escapes_the_documented_unsafe_set_in_path_and_extra() {
    let mut guest = create_setup();
    component(&mut guest, HOST, "example.test", false, STRINGS);
    component(&mut guest, PATH, "/a b%c<d>\u{1f980}", true, STRINGS + 0x40);
    component(&mut guest, EXTRA, "?q=x y#f", true, STRINGS + 0x100);
    guest.memory.write_u32(COMPONENTS + SCHEME_KIND, 1).unwrap();
    guest.memory.write_u32(LENGTH, 256).unwrap();
    assert_eq!(
        guest.call(
            "WinHttpCreateUrl",
            &[COMPONENTS, 0x8000_0000, CREATED, LENGTH]
        ),
        1
    );
    assert_eq!(
        created(&guest),
        "http://example.test/a%20b%25c%3Cd%3E%F0%9F%A6%80?q=x%20y%23f"
    );
    // Without ICU_ESCAPE the same components pass through verbatim.
    guest.memory.write_u32(LENGTH, 256).unwrap();
    assert_eq!(
        guest.call("WinHttpCreateUrl", &[COMPONENTS, 0, CREATED, LENGTH]),
        1
    );
    assert_eq!(
        created(&guest),
        "http://example.test/a b%c<d>\u{1f980}?q=x y#f"
    );
}

#[test]
fn creation_rejects_credentials_and_invalid_input_explicitly() {
    let mut guest = create_setup();
    component(&mut guest, HOST, "example.test", false, STRINGS);
    component(&mut guest, USER, "alice", false, STRINGS + 0x40);
    component(&mut guest, PASSWORD, "secret", false, STRINGS + 0x80);
    guest.memory.write_u32(COMPONENTS + SCHEME_KIND, 1).unwrap();
    guest.memory.write_u32(LENGTH, 256).unwrap();
    // ICU_REJECT_USERPWD fails with the documented invalid-URL error.
    assert_eq!(
        guest.call("WinHttpCreateUrl", &[COMPONENTS, 0x4000, CREATED, LENGTH]),
        0
    );
    assert_eq!(guest.error(), 12005);
    // Credentials compose without the flag.
    assert_eq!(
        guest.call("WinHttpCreateUrl", &[COMPONENTS, 0, CREATED, LENGTH]),
        1
    );
    assert_eq!(created(&guest), "http://alice:secret@example.test");
    // A password without a user name is an invalid URL.
    guest.memory.write_u64(COMPONENTS + USER, 0).unwrap();
    guest.memory.write_u32(LENGTH, 256).unwrap();
    assert_eq!(
        guest.call("WinHttpCreateUrl", &[COMPONENTS, 0, CREATED, LENGTH]),
        0
    );
    assert_eq!(guest.error(), 12005);
    guest.memory.write_u64(COMPONENTS + PASSWORD, 0).unwrap();
    // Unknown flags, schemes and missing input are explicit.
    guest.memory.write_u32(LENGTH, 256).unwrap();
    assert_eq!(
        guest.call(
            "WinHttpCreateUrl",
            &[COMPONENTS, 0x1000_0000, CREATED, LENGTH]
        ),
        0
    );
    assert_eq!(guest.error(), 50);
    guest.memory.write_u32(COMPONENTS + SCHEME_KIND, 3).unwrap();
    assert_eq!(
        guest.call("WinHttpCreateUrl", &[COMPONENTS, 0, CREATED, LENGTH]),
        0
    );
    assert_eq!(guest.error(), 12006);
    guest.memory.write_u32(COMPONENTS + SCHEME_KIND, 1).unwrap();
    guest.memory.write_u64(COMPONENTS + HOST, 0).unwrap();
    assert_eq!(
        guest.call("WinHttpCreateUrl", &[COMPONENTS, 0, CREATED, LENGTH]),
        0
    );
    assert_eq!(guest.error(), 12005);
    for (record, length) in [(0, LENGTH), (COMPONENTS, 0), (u64::MAX - 1, LENGTH)] {
        assert_eq!(
            guest.call("WinHttpCreateUrl", &[record, 0, CREATED, length]),
            0
        );
        assert_eq!(guest.error(), 87);
    }
    guest.memory.write_u32(COMPONENTS, 84).unwrap();
    assert_eq!(
        guest.call("WinHttpCreateUrl", &[COMPONENTS, 0, CREATED, LENGTH]),
        0
    );
    assert_eq!(guest.error(), 87);
}
