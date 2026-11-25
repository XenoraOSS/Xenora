// SPDX-License-Identifier: GPL-3.0-or-later

//! C runtime file streams and the Win32 file services on one drive (issue
//! #402). On Windows the C runtime's streams sit on Win32 files, so a file
//! written through one is the file read through the other.
//!
//! In text mode a C stream writes each line feed as a carriage return and a
//! line feed and reads the pair back as a line feed (Microsoft Learn, "fopen,
//! _wfopen"). `ReadFile` and `WriteFile` transfer bytes as they are. Expected
//! bytes are what each test wrote, with that translation applied by hand.

mod support;

use support::*;
use xenora_loader::cpu::ExitReason;

const KERNEL32: &str = "kernel32.dll";
const UCRT: &str = "ucrtbase.dll";

const IMPORTS: &[(&str, &str)] = &[
    (KERNEL32, "CreateFileW"),
    (KERNEL32, "ReadFile"),
    (KERNEL32, "WriteFile"),
    (KERNEL32, "CloseHandle"),
    (KERNEL32, "CreateDirectoryW"),
    (KERNEL32, "DeleteFileW"),
    (UCRT, "fopen"),
    (UCRT, "fclose"),
    (UCRT, "fgets"),
    (UCRT, "fputs"),
    (UCRT, "_errno"),
    (UCRT, "malloc"),
    (UCRT, "free"),
];

const GENERIC_READ: u64 = 0x8000_0000;
const GENERIC_WRITE: u64 = 0x4000_0000;
const CREATE_ALWAYS: u64 = 2;
const OPEN_EXISTING: u64 = 3;
const INVALID_HANDLE: u64 = u64::MAX;
const ENOENT: u32 = 2;
const EBADF: u32 = 9;
const ENOMEM: u32 = 12;
const EACCES: u32 = 13;

// Scratch layout: a wide name, a narrow name, a mode, a transfer buffer and a
// count.
const WIDE: u64 = SCRATCH;
const NARROW: u64 = SCRATCH + 0x200;
const MODE: u64 = SCRATCH + 0x300;
const BUFFER: u64 = SCRATCH + 0x400;
const COUNT: u64 = SCRATCH + 0x800;

fn guest() -> Guest {
    Guest::new(IMPORTS, &[])
}

fn narrow(guest: &mut Guest, at: u64, text: &str) -> u64 {
    guest.memory.write_bytes(at, text.as_bytes()).unwrap();
    guest.memory.write_u8(at + text.len() as u64, 0).unwrap();
    at
}

fn wide(guest: &mut Guest, text: &str) -> u64 {
    let units: Vec<u8> = text
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    guest.memory.write_bytes(WIDE, &units).unwrap();
    WIDE
}

fn errno(guest: &mut Guest) -> u32 {
    let address = guest.call("_errno", &[]);
    guest.u32_at(address)
}

fn fopen(guest: &mut Guest, name: &str, mode: &str) -> u64 {
    let name = narrow(guest, NARROW, name);
    let mode = narrow(guest, MODE, mode);
    guest.call("fopen", &[name, mode])
}

/// `CreateFileW` with no sharing restriction on later opens.
fn create_file(guest: &mut Guest, name: &str, access: u64, disposition: u64) -> u64 {
    let name = wide(guest, name);
    guest.call("CreateFileW", &[name, access, 3, 0, disposition, 0, 0])
}

/// Everything `ReadFile` returns for a file, up to 256 bytes.
fn read_through_win32(guest: &mut Guest, name: &str) -> Vec<u8> {
    let handle = create_file(guest, name, GENERIC_READ, OPEN_EXISTING);
    assert_ne!(handle, INVALID_HANDLE, "{name}");
    assert_eq!(guest.call("ReadFile", &[handle, BUFFER, 256, COUNT, 0]), 1);
    let count = guest.u32_at(COUNT) as usize;
    let mut bytes = vec![0; count];
    guest.memory.read_bytes(BUFFER, &mut bytes).unwrap();
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);
    bytes
}

fn write_through_win32(guest: &mut Guest, name: &str, bytes: &[u8]) {
    let handle = create_file(guest, name, GENERIC_WRITE, CREATE_ALWAYS);
    assert_ne!(handle, INVALID_HANDLE, "{name}");
    guest.memory.write_bytes(BUFFER, bytes).unwrap();
    let count = bytes.len() as u64;
    assert_eq!(
        guest.call("WriteFile", &[handle, BUFFER, count, COUNT, 0]),
        1
    );
    assert_eq!(u64::from(guest.u32_at(COUNT)), count);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);
}

fn put(guest: &mut Guest, stream: u64, text: &str) -> u32 {
    let text = narrow(guest, BUFFER, text);
    guest.call("fputs", &[text, stream]) as u32
}

/// One `fgets` of up to 127 characters.
fn get(guest: &mut Guest, stream: u64) -> Option<Vec<u8>> {
    if guest.call("fgets", &[BUFFER, 128, stream]) == 0 {
        return None;
    }
    let mut bytes = vec![0; 128];
    guest.memory.read_bytes(BUFFER, &mut bytes).unwrap();
    let end = bytes.iter().position(|byte| *byte == 0).unwrap();
    bytes.truncate(end);
    Some(bytes)
}

#[test]
fn a_file_written_through_a_c_stream_is_read_through_win32() {
    let mut guest = guest();
    // Text mode stores a carriage return before each line feed.
    let stream = fopen(&mut guest, "from-c.txt", "w");
    assert_ne!(stream, 0);
    assert_eq!(put(&mut guest, stream, "line one\nline two\n"), 0);
    assert_eq!(guest.call("fclose", &[stream]) as u32, 0);
    assert_eq!(
        read_through_win32(&mut guest, "C:\\from-c.txt"),
        b"line one\r\nline two\r\n"
    );
    // Binary mode stores what was written.
    let stream = fopen(&mut guest, "from-c.bin", "wb");
    assert_eq!(put(&mut guest, stream, "raw\nbytes"), 0);
    assert_eq!(guest.call("fclose", &[stream]) as u32, 0);
    assert_eq!(read_through_win32(&mut guest, "from-c.bin"), b"raw\nbytes");
    // Nothing is held back: the bytes are on the drive before fclose.
    let stream = fopen(&mut guest, "unflushed.bin", "wb");
    assert_eq!(put(&mut guest, stream, "now"), 0);
    assert_eq!(read_through_win32(&mut guest, "unflushed.bin"), b"now");
    assert_eq!(guest.call("fclose", &[stream]) as u32, 0);
}

#[test]
fn a_file_written_through_win32_is_read_through_a_c_stream() {
    let mut guest = guest();
    write_through_win32(&mut guest, "C:\\from-win32.txt", b"win32\r\ndata\nlast");
    // Text mode reads the pair as one line feed and leaves a lone one alone.
    let stream = fopen(&mut guest, "from-win32.txt", "r");
    assert_ne!(stream, 0);
    assert_eq!(get(&mut guest, stream).as_deref(), Some(&b"win32\n"[..]));
    assert_eq!(get(&mut guest, stream).as_deref(), Some(&b"data\n"[..]));
    assert_eq!(get(&mut guest, stream).as_deref(), Some(&b"last"[..]));
    assert_eq!(get(&mut guest, stream), None);
    assert_eq!(guest.call("fclose", &[stream]) as u32, 0);
    // Binary mode reads the bytes as they are.
    let stream = fopen(&mut guest, "from-win32.txt", "rb");
    assert_eq!(get(&mut guest, stream).as_deref(), Some(&b"win32\r\n"[..]));
    assert_eq!(guest.call("fclose", &[stream]) as u32, 0);
    // A file Win32 created is there for "r", and "w" destroys its contents
    // for Win32 too.
    let stream = fopen(&mut guest, "from-win32.txt", "w");
    assert_ne!(stream, 0);
    assert_eq!(guest.call("fclose", &[stream]) as u32, 0);
    assert!(read_through_win32(&mut guest, "from-win32.txt").is_empty());
}

#[test]
fn directories_are_not_files_and_hold_files() {
    let mut guest = guest();
    let name = wide(&mut guest, "C:\\folder");
    assert_eq!(guest.call("CreateDirectoryW", &[name, 0]), 1);
    // A directory cannot be opened as a stream, for reading or writing.
    for mode in ["r", "w", "a", "r+"] {
        assert_eq!(fopen(&mut guest, "folder", mode), 0, "{mode}");
        assert_eq!(errno(&mut guest), EACCES, "{mode}");
    }
    // A file inside it can, by either slash.
    let stream = fopen(&mut guest, "folder/inside.txt", "wb");
    assert_ne!(stream, 0);
    assert_eq!(put(&mut guest, stream, "in"), 0);
    assert_eq!(guest.call("fclose", &[stream]) as u32, 0);
    assert_eq!(
        read_through_win32(&mut guest, "c:\\FOLDER\\inside.txt"),
        b"in"
    );
    // A directory that does not exist has no files.
    assert_eq!(fopen(&mut guest, "nowhere\\inside.txt", "w"), 0);
    assert_eq!(errno(&mut guest), ENOENT);
}

/// A C stream is not a Win32 handle: it does not stop the file being deleted.
/// Once the file is gone the stream's operations fail with `EBADF`, and
/// closing it still succeeds. This is Xenora's behaviour, not Windows's,
/// where the open stream would keep the file from being deleted.
#[test]
fn a_stream_whose_file_was_deleted_fails_cleanly() {
    let mut guest = guest();
    let stream = fopen(&mut guest, "gone.txt", "w+b");
    assert_ne!(stream, 0);
    assert_eq!(put(&mut guest, stream, "data\n"), 0);
    let name = wide(&mut guest, "C:\\gone.txt");
    assert_eq!(guest.call("DeleteFileW", &[name]), 1);
    assert_eq!(put(&mut guest, stream, "more"), u32::MAX);
    assert_eq!(errno(&mut guest), EBADF);
    assert_eq!(get(&mut guest, stream), None);
    assert_eq!(errno(&mut guest), EBADF);
    assert_eq!(guest.call("fclose", &[stream]) as u32, 0);
    // The closed stream is no stream any more.
    let exit = guest.fails("fclose", &[stream]);
    assert_eq!(exit, RUNTIME_LIMIT);
    assert!(matches!(exit, ExitReason::IllegalInstruction(0xFE)));
    // And the name is free again.
    assert_eq!(fopen(&mut guest, "gone.txt", "r"), 0);
    assert_eq!(errno(&mut guest), ENOENT);
}

/// A `FILE` object comes from the process heap. When the heap has no room for
/// one, `fopen` fails with `ENOMEM` and has neither created the file nor
/// destroyed the contents of an existing one. The heap holds 4,096 blocks.
#[test]
fn an_fopen_that_cannot_allocate_leaves_the_drive_alone() {
    let mut guest = guest();
    write_through_win32(&mut guest, "kept.txt", b"keep me");
    // Fill the heap one byte at a time until it refuses.
    let mut blocks = Vec::new();
    loop {
        let block = guest.call("malloc", &[1]);
        if block == 0 {
            break;
        }
        blocks.push(block);
        assert!(blocks.len() <= 4096, "the heap never filled");
    }
    for (name, mode) in [
        ("new.txt", "w"),
        ("new.txt", "a"),
        ("kept.txt", "w"),
        ("kept.txt", "r"),
    ] {
        assert_eq!(fopen(&mut guest, name, mode), 0, "{name} {mode}");
        assert_eq!(errno(&mut guest), ENOMEM, "{name} {mode}");
    }
    // With room for one block again: the new file was never created, and the
    // existing one still has its contents.
    guest.call("free", &[blocks.pop().unwrap()]);
    assert_eq!(fopen(&mut guest, "new.txt", "r"), 0);
    assert_eq!(errno(&mut guest), ENOENT);
    assert_eq!(read_through_win32(&mut guest, "kept.txt"), b"keep me");
}
