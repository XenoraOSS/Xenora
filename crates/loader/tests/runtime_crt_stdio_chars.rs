// SPDX-License-Identifier: GPL-3.0-or-later

//! Character and line stdio on in-memory CRT streams through the guest import
//! table. Expectations follow the public UCRT pages for fputc, putc, puts,
//! fgetc, getc and ungetc: the character functions return the byte as an
//! unsigned char or EOF, puts returns a non-negative value or EOF, and ungetc
//! returns the pushed-back byte or EOF.

mod support;

use support::*;

const UCRT: &str = "ucrtbase.dll";
const IMPORTS: &[(&str, &str)] = &[
    (UCRT, "fopen"),
    (UCRT, "fclose"),
    (UCRT, "feof"),
    (UCRT, "ferror"),
    (UCRT, "fputc"),
    (UCRT, "putc"),
    (UCRT, "puts"),
    (UCRT, "fgetc"),
    ("api-ms-win-crt-stdio-l1-1-0.dll", "getc"),
    (UCRT, "ungetc"),
    (UCRT, "fread"),
    (UCRT, "fseek"),
    (UCRT, "ftell"),
];

const EOF: u64 = u32::MAX as u64;
const NAME: u64 = SCRATCH;
const WRITE: u64 = SCRATCH + 0x40;
const READ: u64 = SCRATCH + 0x48;
const TEXT: u64 = SCRATCH + 0x80;
const UPDATE: u64 = SCRATCH + 0x50;
const LINE: u64 = SCRATCH + 0x200;

fn guest() -> Guest {
    let mut guest = Guest::new(IMPORTS, &[]);
    guest.memory.write_bytes(NAME, b"chars.txt\0").unwrap();
    guest.memory.write_bytes(WRITE, b"w\0").unwrap();
    guest.memory.write_bytes(READ, b"r\0").unwrap();
    guest.memory.write_bytes(UPDATE, b"r+\0").unwrap();
    guest
}

/// Write `contents` to chars.txt and open it with `mode`.
fn file_with(guest: &mut Guest, contents: &[u8], mode: u64) -> u64 {
    let out = guest.call("fopen", &[NAME, WRITE]);
    for &byte in contents {
        guest.call("fputc", &[u64::from(byte), out]);
    }
    assert_eq!(guest.call("fclose", &[out]), 0);
    let stream = guest.call("fopen", &[NAME, mode]);
    assert_ne!(stream, 0);
    stream
}

fn read_all(guest: &mut Guest, stream: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let byte = guest.call("fgetc", &[stream]);
        if byte == EOF {
            return bytes;
        }
        assert!(byte <= 0xFF, "fgetc returns an unsigned char");
        bytes.push(byte as u8);
    }
}

#[test]
fn characters_and_lines_round_trip_through_a_file() {
    let mut guest = guest();
    let out = guest.call("fopen", &[NAME, WRITE]);
    assert_ne!(out, 0);
    // fputc writes (unsigned char)c and returns it.
    assert_eq!(guest.call("fputc", &[0x141, out]), 0x41);
    assert_eq!(guest.call("putc", &[0xFF, out]), 0xFF);
    for &byte in b"line one" {
        guest.call("fputc", &[u64::from(byte), out]);
    }
    assert_eq!(guest.call("fclose", &[out]), 0);

    let input = guest.call("fopen", &[NAME, READ]);
    assert_eq!(read_all(&mut guest, input), b"A\xFFline one");
    assert_ne!(guest.call("feof", &[input]), 0);
    assert_eq!(guest.call("ferror", &[input]), 0);
    // Writing to a read-only stream fails and sets the error indicator.
    assert_eq!(guest.call("fputc", &[b'x' as u64, input]), EOF);
    assert_ne!(guest.call("ferror", &[input]), 0);
    assert_eq!(guest.call("fclose", &[input]), 0);
}

#[test]
fn reading_a_write_only_stream_fails() {
    let mut guest = guest();
    let out = guest.call("fopen", &[NAME, WRITE]);
    assert_eq!(guest.call("fgetc", &[out]), EOF);
    assert_ne!(guest.call("ferror", &[out]), 0);
    assert_eq!(
        guest.call("getc", &[out]),
        EOF,
        "the stdio API set reaches the same service"
    );
    assert_eq!(guest.call("fclose", &[out]), 0);
}

#[test]
fn puts_writes_a_line_to_standard_output() {
    let mut guest = guest();
    guest.memory.write_bytes(TEXT, b"hello\0").unwrap();
    // Standard output accepts and discards output; puts reports success.
    assert_eq!(guest.call("puts", &[TEXT]), 0);
}

#[test]
fn ungetc_pushes_back_one_byte_that_reads_next() {
    let mut guest = guest();
    let input = file_with(&mut guest, b"abc", READ);
    assert_eq!(guest.call("fgetc", &[input]), u64::from(b'a'));
    // The same byte: the position simply steps back.
    assert_eq!(
        guest.call("ungetc", &[u64::from(b'a'), input]),
        u64::from(b'a')
    );
    assert_eq!(guest.call("ftell", &[input]), 0);
    assert_eq!(guest.call("fgetc", &[input]), u64::from(b'a'));
    // A different byte reads in its place; ftell counts it as unread.
    assert_eq!(guest.call("fgetc", &[input]), u64::from(b'b'));
    assert_eq!(
        guest.call("ungetc", &[0x1_7A, input]),
        0x7A,
        "unsigned char"
    );
    assert_eq!(guest.call("ftell", &[input]), 1);
    // Only one byte of push-back is guaranteed.
    assert_eq!(guest.call("ungetc", &[u64::from(b'q'), input]), EOF);
    assert_eq!(read_all(&mut guest, input), b"zc");
    assert_eq!(guest.call("fclose", &[input]), 0);
}

#[test]
fn ungetc_rejects_eof_clears_the_eof_indicator_and_is_discarded_by_seeks() {
    let mut guest = guest();
    let input = file_with(&mut guest, b"xy", READ);
    // Nothing read yet: there is no byte to push back over.
    assert_eq!(guest.call("ungetc", &[u64::from(b'w'), input]), EOF);
    assert_eq!(read_all(&mut guest, input), b"xy");
    assert_ne!(guest.call("feof", &[input]), 0);
    assert_eq!(guest.call("ungetc", &[EOF, input]), EOF);
    assert_eq!(
        guest.call("ungetc", &[u64::from(b'Y'), input]),
        u64::from(b'Y')
    );
    assert_eq!(guest.call("feof", &[input]), 0);
    // fread sees the pushed-back byte first.
    assert_eq!(guest.call("fread", &[LINE, 1, 4, input]), 1);
    assert_eq!(guest.memory.read_u8(LINE).unwrap(), b'Y');
    // A seek discards a push-back.
    assert_eq!(
        guest.call("ungetc", &[u64::from(b'Q'), input]),
        u64::from(b'Q')
    );
    assert_eq!(guest.call("fseek", &[input, 0, 0]), 0);
    assert_eq!(read_all(&mut guest, input), b"xy");
    assert_eq!(guest.call("fclose", &[input]), 0);
}

#[test]
fn a_pushed_back_byte_never_reaches_the_file() {
    let mut guest = guest();
    let update = file_with(&mut guest, b"kept", UPDATE);
    assert_eq!(guest.call("fgetc", &[update]), u64::from(b'k'));
    assert_eq!(
        guest.call("ungetc", &[u64::from(b'X'), update]),
        u64::from(b'X')
    );
    assert_eq!(guest.call("fclose", &[update]), 0);
    let input = guest.call("fopen", &[NAME, READ]);
    assert_eq!(read_all(&mut guest, input), b"kept");
    assert_eq!(guest.call("fclose", &[input]), 0);
}
