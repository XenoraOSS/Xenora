// SPDX-License-Identifier: GPL-3.0-or-later

//! Exercise the CRT stdio API-set imports through original PE call stubs.
//! https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/locking
//! https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/commit

mod support;

use support::{Guest, SCRATCH};

const STDIO_API_SET: &str = "api-ms-win-crt-stdio-l1-1-0.dll";
const UCRT: &str = "ucrtbase.dll";
const IMPORTS: &[(&str, &str)] = &[
    (STDIO_API_SET, "_locking"),
    (STDIO_API_SET, "_commit"),
    (UCRT, "_open"),
    (UCRT, "_close"),
    (UCRT, "_lseek"),
    (UCRT, "_write"),
    (UCRT, "_read"),
    (UCRT, "_errno"),
];

const FILE_NAME: u64 = SCRATCH;
const BUFFER: u64 = SCRATCH + 0x100;
const O_RDONLY: u64 = 0;
const O_RDWR: u64 = 2;
const O_CREAT: u64 = 0x100;
const O_BINARY: u64 = 0x8000;
const S_IREAD_WRITE: u64 = 0x180;
const SEEK_SET: u64 = 0;
const SEEK_CUR: u64 = 1;
const LK_UNLCK: u64 = 0;
const LK_LOCK: u64 = 1;
const LK_NBLCK: u64 = 2;
const EBADF: u32 = 9;
const EINVAL: u32 = 22;
const EDEADLOCK: u32 = 36;

fn open(guest: &mut Guest, flags: u64) -> u64 {
    let fd = guest.call("_open", &[FILE_NAME, flags | O_BINARY, S_IREAD_WRITE]) as i32;
    assert!(fd >= 3, "expected a file descriptor, got {fd}");
    fd as u64
}

fn expect_errno(guest: &mut Guest, symbol: &str, args: &[u64], expected: u32) {
    let address = guest.call("_errno", &[]);
    guest.memory.write_u32(address, 0).unwrap();
    assert_eq!(guest.call(symbol, args) as i32, -1, "{symbol}");
    assert_eq!(guest.u32_at(address), expected, "{symbol} errno");
}

#[test]
fn guest_crt_file_locking_tracks_ranges_until_close() {
    let mut guest = Guest::new(IMPORTS, &[]);
    guest
        .memory
        .write_bytes(FILE_NAME, b"range-locks.bin\0")
        .unwrap();
    let first = open(&mut guest, O_RDWR | O_CREAT);
    let second = open(&mut guest, O_RDWR);
    assert_ne!(first, second);

    // [2, 6) starts at the current offset and can extend past EOF.
    assert_eq!(guest.call("_lseek", &[first, 2, SEEK_SET]) as i32, 2);
    assert_eq!(guest.call("_locking", &[first, LK_NBLCK, 4]) as i32, 0);
    assert_eq!(guest.call("_lseek", &[first, 0, SEEK_CUR]) as i32, 2);
    assert_eq!(guest.call("_lseek", &[second, 5, SEEK_SET]) as i32, 5);
    // Xenora returns EDEADLOCK immediately for both modes. Windows retries
    // _LK_LOCK and reports EACCES for a nonblocking locking violation.
    for mode in [LK_LOCK, LK_NBLCK] {
        expect_errno(&mut guest, "_locking", &[second, mode, 2], EDEADLOCK);
    }

    // An adjacent range is independent and unlock requires the exact length.
    assert_eq!(guest.call("_lseek", &[second, 6, SEEK_SET]) as i32, 6);
    assert_eq!(guest.call("_locking", &[second, LK_NBLCK, 2]) as i32, 0);
    expect_errno(&mut guest, "_locking", &[second, LK_UNLCK, 1], EINVAL);
    assert_eq!(guest.call("_locking", &[second, LK_UNLCK, 2]) as i32, 0);
    assert_eq!(guest.call("_lseek", &[second, 5, SEEK_SET]) as i32, 5);
    expect_errno(&mut guest, "_locking", &[second, LK_NBLCK, 2], EDEADLOCK);

    // Xenora releases the remaining range when its owning stream closes.
    assert_eq!(guest.call("_close", &[first]) as i32, 0);
    expect_errno(&mut guest, "_locking", &[first, LK_NBLCK, 1], EBADF);
    assert_eq!(guest.call("_locking", &[second, LK_NBLCK, 2]) as i32, 0);
    assert_eq!(guest.call("_locking", &[second, LK_UNLCK, 2]) as i32, 0);
    assert_eq!(guest.call("_close", &[second]) as i32, 0);
}

#[test]
fn guest_crt_commit_requires_file_backing_and_preserves_bytes() {
    const PAYLOAD: &[u8] = b"committed";
    let mut guest = Guest::new(IMPORTS, &[]);
    guest
        .memory
        .write_bytes(FILE_NAME, b"commit-test.bin\0")
        .unwrap();
    let writer = open(&mut guest, O_RDWR | O_CREAT);
    guest.memory.write_bytes(BUFFER, PAYLOAD).unwrap();
    assert_eq!(
        guest.call("_write", &[writer, BUFFER, PAYLOAD.len() as u64]) as i32,
        PAYLOAD.len() as i32
    );
    assert_eq!(guest.call("_commit", &[writer]) as i32, 0);

    // The synthetic drive is already the storage. No host file is flushed.
    let reader = open(&mut guest, O_RDONLY);
    let mut bytes = [0; PAYLOAD.len()];
    guest.memory.write_bytes(BUFFER, &bytes).unwrap();
    assert_eq!(
        guest.call("_read", &[reader, BUFFER, PAYLOAD.len() as u64]) as i32,
        PAYLOAD.len() as i32
    );
    guest.memory.read_bytes(BUFFER, &mut bytes).unwrap();
    assert_eq!(&bytes, PAYLOAD);
    assert_eq!(guest.call("_close", &[reader]) as i32, 0);
    assert_eq!(guest.call("_close", &[writer]) as i32, 0);
    expect_errno(&mut guest, "_commit", &[writer], EBADF);
}
