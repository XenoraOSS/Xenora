// SPDX-License-Identifier: GPL-3.0-or-later

mod support;

use support::{Guest, SCRATCH};

const IMPORTS: &[(&str, &str)] = &[
    ("winhttp.dll", "WinHttpOpen"),
    ("winhttp.dll", "WinHttpConnect"),
    ("winhttp.dll", "WinHttpOpenRequest"),
    ("winhttp.dll", "WinHttpSetOption"),
    ("winhttp.dll", "WinHttpQueryOption"),
    ("winhttp.dll", "WinHttpReadData"),
    ("winhttp.dll", "WinHttpCloseHandle"),
    ("kernel32.dll", "GetLastError"),
];
const SERVER: u64 = SCRATCH;
const VALUE: u64 = SCRATCH + 64;
const SIZE: u64 = SCRATCH + 72;
const WINHTTP_OPTION_RESOLVE_TIMEOUT: u64 = 3;
const WINHTTP_OPTION_CONTEXT_VALUE: u64 = 78;
const ERROR_INVALID_HANDLE: u64 = 6;
const CONTEXT: u64 = 0x0123_4567_89ab_cdef;

fn setup() -> Guest {
    let mut guest = Guest::new(IMPORTS, &[]);
    for (index, unit) in "example.test".encode_utf16().chain([0]).enumerate() {
        guest
            .memory
            .write_u16(SERVER + 2 * index as u64, unit)
            .unwrap();
    }
    guest
}

fn open_session(guest: &mut Guest) -> u64 {
    let session = guest.call("WinHttpOpen", &[0, 1, 0, 0, 0]);
    assert_ne!(session, 0);
    session
}

fn connect(guest: &mut Guest, session: u64) -> u64 {
    let connection = guest.call("WinHttpConnect", &[session, SERVER, 80, 0]);
    assert_ne!(connection, 0);
    connection
}

fn open_request(guest: &mut Guest, connection: u64) -> u64 {
    let request = guest.call("WinHttpOpenRequest", &[connection, 0, 0, 0, 0, 0, 0]);
    assert_ne!(request, 0);
    request
}

fn assert_invalid(guest: &mut Guest, symbol: &str, args: &[u64]) {
    assert_eq!(guest.call(symbol, args), 0, "{symbol}");
    assert_eq!(guest.error(), ERROR_INVALID_HANDLE, "{symbol}");
}

fn close_once(guest: &mut Guest, handle: u64) {
    assert_eq!(guest.call("WinHttpCloseHandle", &[handle]), 1);
    assert_invalid(guest, "WinHttpCloseHandle", &[handle]);
}

fn assert_context_round_trip(guest: &mut Guest, handle: u64) {
    guest.memory.write_u64(VALUE, CONTEXT).unwrap();
    assert_eq!(
        guest.call(
            "WinHttpSetOption",
            &[handle, WINHTTP_OPTION_CONTEXT_VALUE, VALUE, 8]
        ),
        1
    );
    guest.memory.write_u64(VALUE, 0).unwrap();
    guest.memory.write_u32(SIZE, 8).unwrap();
    assert_eq!(
        guest.call(
            "WinHttpQueryOption",
            &[handle, WINHTTP_OPTION_CONTEXT_VALUE, VALUE, SIZE]
        ),
        1
    );
    assert_eq!(guest.u64_at(VALUE), CONTEXT);
}

fn assert_context_inaccessible(guest: &mut Guest, handle: u64) {
    guest.memory.write_u64(VALUE, !CONTEXT).unwrap();
    guest.memory.write_u32(SIZE, 8).unwrap();
    assert_invalid(
        guest,
        "WinHttpQueryOption",
        &[handle, WINHTTP_OPTION_CONTEXT_VALUE, VALUE, SIZE],
    );
    assert_invalid(
        guest,
        "WinHttpSetOption",
        &[handle, WINHTTP_OPTION_CONTEXT_VALUE, VALUE, 8],
    );
}

#[test]
fn closing_a_connection_invalidates_its_requests_only() {
    let mut guest = setup();
    let session = open_session(&mut guest);
    let connection = connect(&mut guest, session);
    let request = open_request(&mut guest, connection);
    let sibling = connect(&mut guest, session);
    assert_context_round_trip(&mut guest, request);

    close_once(&mut guest, connection);
    assert_eq!(guest.state.open_handle_count(), 3);
    guest.memory.write_u32(SIZE, 4).unwrap();
    assert_invalid(
        &mut guest,
        "WinHttpQueryOption",
        &[request, WINHTTP_OPTION_RESOLVE_TIMEOUT, VALUE, SIZE],
    );
    assert_invalid(&mut guest, "WinHttpReadData", &[request, VALUE, 1, SIZE]);
    assert_invalid(
        &mut guest,
        "WinHttpOpenRequest",
        &[connection, 0, 0, 0, 0, 0, 0],
    );
    assert_context_inaccessible(&mut guest, request);

    let sibling_request = open_request(&mut guest, sibling);
    assert_context_round_trip(&mut guest, sibling_request);
    close_once(&mut guest, request);
    close_once(&mut guest, sibling_request);
    close_once(&mut guest, sibling);
    close_once(&mut guest, session);
    assert_eq!(guest.state.open_handle_count(), 0);
}

#[test]
fn closing_a_session_invalidates_descendants_until_each_is_closed() {
    let mut guest = setup();
    let session = open_session(&mut guest);
    let connection = connect(&mut guest, session);
    let request = open_request(&mut guest, connection);
    let independent_session = open_session(&mut guest);
    let independent_connection = connect(&mut guest, independent_session);
    let independent_request = open_request(&mut guest, independent_connection);
    assert_context_round_trip(&mut guest, connection);
    assert_context_round_trip(&mut guest, request);

    close_once(&mut guest, session);
    assert_eq!(guest.state.open_handle_count(), 5);
    assert_invalid(&mut guest, "WinHttpConnect", &[session, SERVER, 80, 0]);
    assert_invalid(
        &mut guest,
        "WinHttpOpenRequest",
        &[connection, 0, 0, 0, 0, 0, 0],
    );
    assert_invalid(&mut guest, "WinHttpReadData", &[request, VALUE, 1, SIZE]);
    assert_context_inaccessible(&mut guest, connection);
    assert_context_inaccessible(&mut guest, request);
    assert_context_round_trip(&mut guest, independent_request);

    close_once(&mut guest, connection);
    close_once(&mut guest, request);
    close_once(&mut guest, independent_request);
    close_once(&mut guest, independent_connection);
    close_once(&mut guest, independent_session);
    assert_eq!(guest.state.open_handle_count(), 0);
}
