// SPDX-License-Identifier: GPL-3.0-or-later

//! The environment block services through the guest import table.
//!
//! The environment is empty, so expansion is the identity by the documented
//! rule that an undefined variable is copied verbatim. The mechanics -
//! the terminator-counting sizes, the small-buffer answers, the block
//! ownership - are the documented ones.

mod support;

use support::*;

const KERNEL32: &str = "kernel32.dll";

const IMPORTS: &[(&str, &str)] = &[
    (KERNEL32, "ExpandEnvironmentStringsA"),
    (KERNEL32, "ExpandEnvironmentStringsW"),
    (KERNEL32, "GetEnvironmentStringsW"),
    (KERNEL32, "FreeEnvironmentStringsW"),
    (KERNEL32, "GetLastError"),
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

#[test]
fn expansion_is_the_identity_over_the_empty_environment() {
    let mut guest = Guest::new(IMPORTS, &[]);
    // Variables undefined everywhere - including the percent signs of a
    // path a program would expand on Windows - come through verbatim.
    narrow(&mut guest, SCRATCH, "%TEMP%\\file.txt");
    let written = guest.call("ExpandEnvironmentStringsA", &[SCRATCH, BUFFER, 64]);
    // The count includes the terminator, as documented.
    assert_eq!(written, "%TEMP%\\file.txt".len() as u64 + 1);
    assert_eq!(narrow_string(&guest, BUFFER), "%TEMP%\\file.txt");

    // A buffer too small answers the required size and writes nothing useful.
    assert_eq!(
        guest.call("ExpandEnvironmentStringsA", &[SCRATCH, BUFFER, 4]),
        16
    );

    // The wide form counts wide units the same way.
    let wide_text: Vec<u16> = "%PATH%".encode_utf16().chain([0]).collect();
    for (i, unit) in wide_text.iter().enumerate() {
        guest
            .memory
            .write_u16(SCRATCH + 2 * i as u64, *unit)
            .unwrap();
    }
    let written = guest.call("ExpandEnvironmentStringsW", &[SCRATCH, BUFFER, 64]);
    assert_eq!(written, 7);
    assert_eq!(guest.u16_at(BUFFER), u16::from(b'%'));
}

#[test]
fn the_environment_block_is_empty_owned_and_freeable() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let block = guest.call("GetEnvironmentStringsW", &[]);
    assert_ne!(block, 0);
    // An empty block is two null units: the empty string, its terminator.
    assert_eq!(guest.u16_at(block), 0);
    assert_eq!(guest.u16_at(block + 2), 0);

    // Each call hands a fresh owned copy, and freeing it succeeds.
    let second = guest.call("GetEnvironmentStringsW", &[]);
    assert_ne!(second, 0);
    assert_ne!(second, block);
    assert_eq!(guest.call("FreeEnvironmentStringsW", &[block]), 1);
    assert_eq!(guest.call("FreeEnvironmentStringsW", &[second]), 1);

    // Freeing a non-allocation fails the documented way.
    assert_eq!(guest.call("FreeEnvironmentStringsW", &[SCRATCH]), 0);
    assert_eq!(guest.call("GetLastError", &[]), 87);
}
