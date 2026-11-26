// SPDX-License-Identifier: GPL-3.0-or-later

//! Synthetic guest calls for the file type, flush and ANSI-query forms.
//! Expectations use Microsoft's public contracts:
//! https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-getfiletype
//! https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers
//! https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-arefileapisansi
//!
//! FILE_TYPE_CHAR for the console handles is a Xenora-hosted-console fact, not a
//! claim about every Windows console host.

mod support;

use support::{Guest, SCRATCH};

const KERNEL32: &str = "kernel32.dll";

const IMPORTS: &[(&str, &str)] = &[
    (KERNEL32, "GetLastError"),
    (KERNEL32, "GetStdHandle"),
    (KERNEL32, "CreateFileW"),
    (KERNEL32, "CloseHandle"),
    (KERNEL32, "CreateEventW"),
    (KERNEL32, "GetFileType"),
    (KERNEL32, "FlushFileBuffers"),
    (KERNEL32, "AreFileApisANSI"),
    (KERNEL32, "GetLogicalDrives"),
    (KERNEL32, "GetDriveTypeA"),
    (KERNEL32, "GetDiskFreeSpaceA"),
    (KERNEL32, "GetDiskFreeSpaceW"),
    (KERNEL32, "GetDiskFreeSpaceExW"),
    (KERNEL32, "SetHandleInformation"),
    (KERNEL32, "GetFileSize"),
    (KERNEL32, "WriteFile"),
    (KERNEL32, "MoveFileExA"),
    (KERNEL32, "LockFileEx"),
    (KERNEL32, "UnlockFileEx"),
    (KERNEL32, "ReadFile"),
    (KERNEL32, "CreateFileA"),
    (KERNEL32, "GetFileAttributesA"),
    (KERNEL32, "SetFileAttributesA"),
    (KERNEL32, "MoveFileExW"),
    (KERNEL32, "GetCompressedFileSizeA"),
    ("msvcrt.dll", "strstr"),
    ("msvcrt.dll", "strtok"),
    ("msvcrt.dll", "strcspn"),
    ("msvcrt.dll", "strspn"),
    ("msvcrt.dll", "strpbrk"),
    ("msvcrt.dll", "_stricmp"),
    ("msvcrt.dll", "_strnicmp"),
    ("msvcrt.dll", "strcpy"),
];

const WIDE: u64 = SCRATCH;
const FILE_TYPE_DISK: u64 = 1;
const FILE_TYPE_CHAR: u64 = 2;
const FILE_TYPE_UNKNOWN: u64 = 0;
const GENERIC_READ: u64 = 0x8000_0000;
const GENERIC_WRITE: u64 = 0x4000_0000;
const CREATE_ALWAYS: u64 = 2;
const INVALID_HANDLE_VALUE: u64 = u64::MAX;
const ERROR_INVALID_HANDLE: u64 = 6;
const ERROR_ACCESS_DENIED: u64 = 5;
const ERROR_NOT_LOCKED: u64 = 158;
const ERROR_NOACCESS: u64 = 998;
const STD_OUTPUT_HANDLE: u64 = 0xFFFF_FFF5; // -11

fn wide(guest: &mut Guest, text: &str) -> u64 {
    let units: Vec<u8> = text
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    guest.memory.write_bytes(WIDE, &units).unwrap();
    WIDE
}

fn create_file(guest: &mut Guest, name: &str, access: u64) -> u64 {
    create_file_disposition(guest, name, access, CREATE_ALWAYS)
}

fn create_file_disposition(guest: &mut Guest, name: &str, access: u64, disposition: u64) -> u64 {
    let name = wide(guest, name);
    guest.call("CreateFileW", &[name, access, 3, 0, disposition, 0, 0])
}

fn last_error(guest: &mut Guest) -> u64 {
    guest.call("GetLastError", &[]) & 0xFFFF_FFFF
}

#[test]
fn narrow_file_operations_are_ansi() {
    let mut guest = Guest::new(IMPORTS, &[]);
    assert_eq!(guest.call("AreFileApisANSI", &[]), 1);
}

#[test]
fn the_file_type_distinguishes_disks_consoles_and_other_objects() {
    let mut guest = Guest::new(IMPORTS, &[]);
    // A disk file answers FILE_TYPE_DISK.
    let handle = create_file(&mut guest, "typed.bin", GENERIC_WRITE);
    assert_ne!(handle, INVALID_HANDLE_VALUE);
    assert_eq!(guest.call("GetFileType", &[handle]), FILE_TYPE_DISK);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);
    // The console output handle answers FILE_TYPE_CHAR.
    let console = guest.call("GetStdHandle", &[STD_OUTPUT_HANDLE]);
    assert_ne!(console, 0);
    assert_ne!(console, INVALID_HANDLE_VALUE);
    assert_eq!(guest.call("GetFileType", &[console]), FILE_TYPE_CHAR);
    // A synchronization object is a valid handle that is not a file:
    // FILE_TYPE_UNKNOWN, and the last error is not set.
    let event = guest.call("CreateEventW", &[0, 0, 0, 0]);
    assert_ne!(event, 0);
    guest.call("GetLastError", &[]);
    assert_eq!(guest.call("GetFileType", &[event]), FILE_TYPE_UNKNOWN);
    assert_eq!(
        last_error(&mut guest),
        0,
        "a valid non-file is not an error"
    );
    assert_eq!(guest.call("CloseHandle", &[event]), 1);
    // An unknown handle is FILE_TYPE_UNKNOWN with ERROR_INVALID_HANDLE.
    assert_eq!(guest.call("GetFileType", &[0xDEAD]), FILE_TYPE_UNKNOWN);
    assert_eq!(last_error(&mut guest), ERROR_INVALID_HANDLE);
}

#[test]
fn flush_file_buffers_accepts_a_written_file_and_refuses_the_rest() {
    let mut guest = Guest::new(IMPORTS, &[]);
    // A file opened for writing flushes successfully.
    let handle = create_file(&mut guest, "flush.bin", GENERIC_WRITE);
    assert_ne!(handle, INVALID_HANDLE_VALUE);
    assert_eq!(guest.call("FlushFileBuffers", &[handle]), 1);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);
    // A read-only open is refused with ERROR_ACCESS_DENIED.
    // CREATE_ALWAYS requires write access, so the reader opens existing.
    let reader = create_file_disposition(&mut guest, "flush.bin", GENERIC_READ, 3);
    assert_ne!(reader, INVALID_HANDLE_VALUE);
    assert_eq!(guest.call("FlushFileBuffers", &[reader]), 0);
    assert_eq!(last_error(&mut guest), ERROR_ACCESS_DENIED);
    assert_eq!(guest.call("CloseHandle", &[reader]), 1);
    // An unknown handle is refused with ERROR_INVALID_HANDLE.
    assert_eq!(guest.call("FlushFileBuffers", &[0xDEAD]), 0);
    assert_eq!(last_error(&mut guest), ERROR_INVALID_HANDLE);
}

/// The hosted machine has one fixed disk, letter C: bit 2 of the drive mask,
/// DRIVE_FIXED for every path, and counts that fit a memory drive.
#[test]
fn the_volume_queries_describe_the_one_disk() {
    let mut guest = Guest::new(IMPORTS, &[]);
    assert_eq!(guest.call("GetLogicalDrives", &[]), 1 << 2);
    assert_eq!(guest.call("GetDriveTypeA", &[0]), 3);
    const FREE: u64 = SCRATCH + 0x10;
    const TOTAL: u64 = SCRATCH + 0x18;
    const TOTAL_FREE: u64 = SCRATCH + 0x20;
    wide(&mut guest, "C:\\");
    assert_eq!(
        guest.call("GetDiskFreeSpaceExW", &[WIDE, FREE, TOTAL, TOTAL_FREE]),
        1
    );
    let gig = 1u64 << 30;
    assert_eq!(guest.u64_at(FREE), gig);
    assert_eq!(guest.u64_at(TOTAL), gig);
    assert_eq!(guest.u64_at(TOTAL_FREE), gig);
}

#[test]
fn legacy_disk_space_queries_use_the_explicit_root_and_check_all_outputs() {
    const OUT: [u64; 4] = [
        SCRATCH + 0x100,
        SCRATCH + 0x104,
        SCRATCH + 0x108,
        SCRATCH + 0x10c,
    ];
    const NARROW: u64 = SCRATCH + 0x40;
    let mut guest = Guest::new(IMPORTS, &[]);
    let file = create_file(&mut guest, "one-byte.bin", GENERIC_WRITE);
    assert_ne!(file, INVALID_HANDLE_VALUE);
    guest.memory.write_bytes(SCRATCH + 0x80, b"x").unwrap();
    assert_eq!(
        guest.call("WriteFile", &[file, SCRATCH + 0x80, 1, SCRATCH + 0x84, 0]),
        1
    );
    let root = wide(&mut guest, "C:\\");
    narrow_at(&mut guest, NARROW, "C:\\");
    for (symbol, path) in [
        ("GetDiskFreeSpaceW", root),
        ("GetDiskFreeSpaceA", NARROW),
        ("GetDiskFreeSpaceW", 0),
    ] {
        assert_eq!(
            guest.call(symbol, &[path, OUT[0], OUT[1], OUT[2], OUT[3]]),
            1
        );
        assert_eq!(OUT.map(|at| guest.u32_at(at)), [1, 4096, 4095, 4096]);
    }
    guest.memory.write_u32(OUT[0], 123).unwrap();
    assert_eq!(
        guest.call(
            "GetDiskFreeSpaceW",
            &[root, OUT[0], OUT[1], OUT[2], u64::MAX]
        ),
        0
    );
    assert_eq!(last_error(&mut guest), ERROR_NOACCESS);
    assert_eq!(guest.u32_at(OUT[0]), 123);
}

/// SetHandleInformation accepts a valid handle and refuses an unknown one;
/// GetFileSize answers the low word with the high word through its pointer.
#[test]
fn handle_information_and_the_size_form() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let handle = create_file(&mut guest, "sized.bin", GENERIC_WRITE);
    assert_ne!(handle, INVALID_HANDLE_VALUE);
    // The inheritance flag is accepted: there is no child to inherit into.
    assert_eq!(guest.call("SetHandleInformation", &[handle, 1, 1]), 1);
    assert_eq!(guest.call("SetHandleInformation", &[0xDEAD, 1, 1]), 0);
    assert_eq!(last_error(&mut guest), ERROR_INVALID_HANDLE);
    // Write ten bytes and read the size back through the legacy form.
    const OUT: u64 = SCRATCH + 0x400;
    const WRITTEN: u64 = SCRATCH + 0x800;
    guest.memory.write_bytes(OUT, b"0123456789").unwrap();
    assert_eq!(guest.call("WriteFile", &[handle, OUT, 10, WRITTEN, 0]), 1);
    const HIGH: u64 = SCRATCH + 0x810;
    assert_eq!(guest.call("GetFileSize", &[handle, HIGH]), 10);
    assert_eq!(guest.u32_at(HIGH), 0, "ten bytes need no high word");
    assert_eq!(guest.call("GetFileSize", &[0xDEAD, 0]), 0xFFFF_FFFF);
    assert_eq!(last_error(&mut guest), ERROR_INVALID_HANDLE);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);
}

fn narrow_at(guest: &mut Guest, at: u64, text: &str) -> u64 {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    guest.memory.write_bytes(at, &bytes).unwrap();
    at
}

/// MoveFileExA renames a file after its lock is released and refuses the reboot flag.
#[test]
fn move_file_ex_and_the_byte_range_locks() {
    const NAME_A: u64 = SCRATCH;
    const NAME_B: u64 = SCRATCH + 0x40;
    const OUT2: u64 = SCRATCH + 0x400;
    const GOT: u64 = SCRATCH + 0x808;
    const OVERLAPPED: u64 = SCRATCH + 0x840;
    let mut guest = Guest::new(IMPORTS, &[]);
    let handle = create_file(&mut guest, "moved.bin", GENERIC_WRITE);
    assert_ne!(handle, INVALID_HANDLE_VALUE);
    guest.memory.write_bytes(OUT2, b"payload").unwrap();
    assert_eq!(guest.call("WriteFile", &[handle, OUT2, 7, GOT, 0]), 1);
    guest.memory.write_bytes(OVERLAPPED, &[0; 32]).unwrap();
    assert_eq!(
        guest.call("LockFileEx", &[handle, 0, 0, 1, 0, OVERLAPPED]),
        1
    );
    assert_eq!(
        guest.call("UnlockFileEx", &[handle, 0, 1, 0, OVERLAPPED]),
        1
    );
    assert_eq!(
        guest.call("UnlockFileEx", &[handle, 0, 1, 0, OVERLAPPED]),
        0
    );
    assert_eq!(last_error(&mut guest), ERROR_NOT_LOCKED);
    assert_eq!(
        guest.call("LockFileEx", &[0xDEAD, 0, 0, 1, 0, OVERLAPPED]),
        0
    );
    assert_eq!(last_error(&mut guest), ERROR_INVALID_HANDLE);
    // A valid file handle with metadata-only access cannot lock data.
    let metadata = create_file_disposition(&mut guest, "moved.bin", 0, 3); // OPEN_EXISTING
    assert_ne!(metadata, INVALID_HANDLE_VALUE);
    assert_eq!(
        guest.call("LockFileEx", &[metadata, 0, 0, 1, 0, OVERLAPPED]),
        0
    );
    assert_eq!(last_error(&mut guest), ERROR_ACCESS_DENIED);
    assert_eq!(guest.call("CloseHandle", &[metadata]), 1);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);
    // A plain rename, then one over an existing target without the flag
    // refused and with it accepted.
    narrow_at(&mut guest, NAME_A, "moved.bin");
    narrow_at(&mut guest, NAME_B, "renamed.bin");
    assert_eq!(guest.call("MoveFileExA", &[NAME_A, NAME_B, 0]), 1);
    // DELAY_UNTIL_REBOOT is honestly unsupported: there is no boot to wait for.
    assert_eq!(guest.call("MoveFileExA", &[NAME_B, NAME_A, 4]), 0);
    let error = last_error(&mut guest);
    assert_eq!(error, 50, "ERROR_NOT_SUPPORTED, got {error}");
    assert_eq!(guest.call("MoveFileExA", &[NAME_B, NAME_A, 0]), 1);
}

/// CreateFileA is the narrow form of the same open: the sharing rules, the
/// dispositions and the content are the drive's, whatever the width.
#[test]
fn create_file_of_the_narrow_width_shares_the_drive() {
    const NAME: u64 = SCRATCH;
    const OUT3: u64 = SCRATCH + 0x400;
    const GOT2: u64 = SCRATCH + 0x808;
    let mut guest = Guest::new(IMPORTS, &[]);
    narrow_at(&mut guest, NAME, "narrow.bin");
    let handle = guest.call(
        "CreateFileA",
        &[NAME, GENERIC_WRITE, 3, 0, CREATE_ALWAYS, 0, 0],
    );
    assert_ne!(handle, INVALID_HANDLE_VALUE);
    guest.memory.write_bytes(OUT3, b"narrow width").unwrap();
    assert_eq!(guest.call("WriteFile", &[handle, OUT3, 12, GOT2, 0]), 1);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);
    // The wide form reads back what the narrow wrote: one drive, two widths.
    let reader = create_file_disposition(&mut guest, "narrow.bin", GENERIC_READ, 3);
    assert_ne!(reader, INVALID_HANDLE_VALUE);
    assert_eq!(guest.call("ReadFile", &[reader, OUT3, 64, GOT2, 0]), 1);
    assert_eq!(guest.u32_at(GOT2), 12);
    let mut bytes = [0; 12];
    guest.memory.read_bytes(OUT3, &mut bytes).unwrap();
    assert_eq!(&bytes, b"narrow width");
    assert_eq!(guest.call("CloseHandle", &[reader]), 1);
}

/// The narrow attribute forms answer and set the same bits as the wide ones.
#[test]
fn narrow_attributes_round_trip() {
    const NAME4: u64 = SCRATCH;
    const OUT4: u64 = SCRATCH + 0x400;
    const GOT4: u64 = SCRATCH + 0x808;
    let mut guest = Guest::new(IMPORTS, &[]);
    let handle = create_file(&mut guest, "attr.bin", GENERIC_WRITE);
    assert_ne!(handle, INVALID_HANDLE_VALUE);
    guest.memory.write_bytes(OUT4, b"a").unwrap();
    assert_eq!(guest.call("WriteFile", &[handle, OUT4, 1, GOT4, 0]), 1);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);
    narrow_at(&mut guest, NAME4, "attr.bin");
    let attributes = guest.call("GetFileAttributesA", &[NAME4]) & 0xFFFF_FFFF;
    assert_ne!(attributes, 0xFFFF_FFFF, "the file exists");
    // The drive does not track attribute bits: the set accepts only the
    // normal and archive forms, as the wide width documents, and refuses
    // the rest honestly.
    assert_eq!(guest.call("SetFileAttributesA", &[NAME4, 0x80]), 1);
    assert_eq!(guest.call("SetFileAttributesA", &[NAME4, 1]), 0);
    assert_eq!(last_error(&mut guest), 50, "ERROR_NOT_SUPPORTED");
    // A missing file answers the documented invalid attributes.
    narrow_at(&mut guest, NAME4, "absent.bin");
    assert_eq!(
        guest.call("GetFileAttributesA", &[NAME4]) & 0xFFFF_FFFF,
        0xFFFF_FFFF
    );
}

/// MoveFileExW is the wide width of the same extended rename.
#[test]
fn move_file_ex_of_the_wide_width() {
    const GOT5: u64 = SCRATCH + 0x808;
    const OUT5: u64 = SCRATCH + 0x400;
    let mut guest = Guest::new(IMPORTS, &[]);
    let handle = create_file(&mut guest, "wide-move.bin", GENERIC_WRITE);
    assert_ne!(handle, INVALID_HANDLE_VALUE);
    guest.memory.write_bytes(OUT5, b"w").unwrap();
    assert_eq!(guest.call("WriteFile", &[handle, OUT5, 1, GOT5, 0]), 1);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);
    let source = wide(&mut guest, "wide-move.bin");
    // The target goes past the source in memory: use a second slot.
    const TARGET: u64 = SCRATCH + 0x200;
    let units: Vec<u8> = "wide-renamed.bin"
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    guest.memory.write_bytes(TARGET, &units).unwrap();
    assert_eq!(guest.call("MoveFileExW", &[source, TARGET, 0]), 1);
    // The rename happened: the narrow width sees the new name.
    narrow_at(&mut guest, OUT5, "wide-renamed.bin");
    assert_ne!(
        guest.call("GetFileAttributesA", &[OUT5]) & 0xFFFF_FFFF,
        0xFFFF_FFFF
    );
}

/// Nothing on the drive is compressed: the compressed size is the size.
#[test]
fn the_compressed_size_is_the_size() {
    const NAME6: u64 = SCRATCH;
    const OUT6: u64 = SCRATCH + 0x400;
    const GOT6: u64 = SCRATCH + 0x808;
    let mut guest = Guest::new(IMPORTS, &[]);
    let handle = create_file(&mut guest, "plain.bin", GENERIC_WRITE);
    assert_ne!(handle, INVALID_HANDLE_VALUE);
    guest.memory.write_bytes(OUT6, b"12345").unwrap();
    assert_eq!(guest.call("WriteFile", &[handle, OUT6, 5, GOT6, 0]), 1);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);
    narrow_at(&mut guest, NAME6, "plain.bin");
    assert_eq!(guest.call("GetCompressedFileSizeA", &[NAME6, 0]), 5);
    narrow_at(&mut guest, NAME6, "absent.bin");
    assert_eq!(
        guest.call("GetCompressedFileSizeA", &[NAME6, 0]) & 0xFFFF_FFFF,
        0xFFFF_FFFF
    );
}

/// The string search family: substring scans, span counts, the stateful
/// tokenizer and case-insensitive comparison, reached through the legacy
/// msvcrt.dll aliases.
#[test]
fn string_search_and_tokenizing_through_the_legacy_aliases() {
    const TEXT: u64 = SCRATCH + 0x300;
    const NEEDLE: u64 = SCRATCH + 0x380;
    const DELIMS: u64 = SCRATCH + 0x3C0;
    const OTHER: u64 = SCRATCH + 0x400;
    let mut guest = Guest::new(IMPORTS, &[]);
    narrow_at(&mut guest, TEXT, "the quick brown fox jumps");
    narrow_at(&mut guest, NEEDLE, "brown");
    // strstr finds the substring at its byte offset.
    let found = guest.call("strstr", &[TEXT, NEEDLE]);
    assert_eq!(found, TEXT + 10);
    narrow_at(&mut guest, NEEDLE, "zebra");
    assert_eq!(guest.call("strstr", &[TEXT, NEEDLE]), 0);
    // strcspn/strspn count the spans.
    narrow_at(&mut guest, DELIMS, "aeiou");
    assert_eq!(
        guest.call("strcspn", &[TEXT, DELIMS]),
        2,
        "first vowel at index 2"
    );
    narrow_at(&mut guest, DELIMS, "theq ");
    assert_eq!(guest.call("strspn", &[TEXT, DELIMS]), 5, "theq+space run");
    // strpbrk returns the pointer of the first matching byte.
    narrow_at(&mut guest, DELIMS, "xyz");
    let hit = guest.call("strpbrk", &[TEXT, DELIMS]);
    assert_eq!(hit, TEXT + 18, "the x in fox");
    // strtok tokenizes in place with CRT scan state.
    narrow_at(&mut guest, TEXT, "alpha,beta gamma");
    narrow_at(&mut guest, DELIMS, ", ");
    let first = guest.call("strtok", &[TEXT, DELIMS]);
    assert_eq!(first, TEXT);
    let second = guest.call("strtok", &[0, DELIMS]);
    assert_eq!(second, TEXT + 6);
    let third = guest.call("strtok", &[0, DELIMS]);
    assert_eq!(third, TEXT + 11);
    assert_eq!(guest.call("strtok", &[0, DELIMS]), 0);
    // Case-insensitive comparison folds ASCII letters.
    narrow_at(&mut guest, TEXT, "Hello");
    narrow_at(&mut guest, OTHER, "hELLO");
    assert_eq!(guest.call("_stricmp", &[TEXT, OTHER]) as u32 as i32, 0);
    narrow_at(&mut guest, OTHER, "hELLo");
    assert_eq!(guest.call("_stricmp", &[TEXT, OTHER]) as u32 as i32, 0);
    narrow_at(&mut guest, OTHER, "hep");
    let diff = guest.call("_stricmp", &[TEXT, OTHER]) as u32 as i32;
    assert!(diff < 0);
    // _strnicmp bounds the scan.
    narrow_at(&mut guest, TEXT, "abcdef");
    narrow_at(&mut guest, OTHER, "ABCxyz");
    assert_eq!(guest.call("_strnicmp", &[TEXT, OTHER, 3]) as u32 as i32, 0);
    assert!(guest.call("_strnicmp", &[TEXT, OTHER, 6]) as u32 as i32 != 0);
    // strcpy through the legacy alias copies and returns the destination.
    narrow_at(&mut guest, TEXT, "copied");
    let destination = guest.call("strcpy", &[OTHER, TEXT]);
    assert_eq!(destination, OTHER);
    let mut verify = [0u8; 8];
    guest.memory.read_bytes(OTHER, &mut verify).unwrap();
    assert_eq!(&verify[..7], b"copied\0");
}
