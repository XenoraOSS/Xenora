// SPDX-License-Identifier: GPL-3.0-or-later

//! Save slot sizing through a translated guest's platform imports.

mod support;

use support::*;

const PLATFORM: &str = "xenora_fixture.dll";
const KERNEL32: &str = "kernel32.dll";
const IMPORTS: &[(&str, &str)] = &[
    (PLATFORM, "OpenFixturePlatform"),
    (PLATFORM, "CloseFixturePlatform"),
    (PLATFORM, "GetFixtureSaveSize"),
    (PLATFORM, "WriteFixtureSave"),
    (PLATFORM, "ReadFixtureSave"),
    (PLATFORM, "DeleteFixtureSave"),
    (KERNEL32, "GetLastError"),
];

const SLOT: u64 = 3;
const INPUT: u64 = SCRATCH;
const OUTPUT: u64 = SCRATCH + 0x100;
const ERROR_INVALID_STATE: u64 = 5023;
const ERROR_NOT_FOUND: u64 = 1168;
const ERROR_NOACCESS: u64 = 998;

fn write(guest: &mut Guest, data: &[u8]) {
    guest.memory.write_bytes(INPUT, data).unwrap();
    assert_eq!(
        guest.call("WriteFixtureSave", &[SLOT, INPUT, data.len() as u64]),
        1
    );
}

fn query_and_read(guest: &mut Guest, expected: &[u8]) {
    let size = guest.call("GetFixtureSaveSize", &[SLOT]);
    assert_eq!(size, expected.len() as u64);

    guest.memory.write_u8(OUTPUT + size, 0xA5).unwrap();
    assert_eq!(guest.call("ReadFixtureSave", &[SLOT, OUTPUT, size]), size);
    let mut data = vec![0; size as usize];
    guest.memory.read_bytes(OUTPUT, &mut data).unwrap();
    assert_eq!(data, expected);
    assert_eq!(guest.memory.read_u8(OUTPUT + size).unwrap(), 0xA5);
}

#[test]
fn save_sizes_follow_replacement_and_survive_reopening_the_platform() {
    let mut guest = Guest::new(IMPORTS, &[]);
    assert_eq!(guest.call("GetFixtureSaveSize", &[SLOT]), 0);
    assert_eq!(guest.error(), ERROR_INVALID_STATE);
    assert_eq!(guest.call("OpenFixturePlatform", &[]), 1);
    assert_eq!(guest.call("GetFixtureSaveSize", &[SLOT]), 0);
    assert_eq!(guest.error(), ERROR_NOT_FOUND);

    write(&mut guest, b"first");
    query_and_read(&mut guest, b"first");
    write(&mut guest, b"second save");
    query_and_read(&mut guest, b"second save");

    assert_eq!(guest.call("WriteFixtureSave", &[SLOT, 0, 4]), 0);
    assert_eq!(guest.error(), ERROR_NOACCESS);
    query_and_read(&mut guest, b"second save");

    assert_eq!(guest.call("CloseFixturePlatform", &[]), 1);
    assert_eq!(guest.call("GetFixtureSaveSize", &[SLOT]), 0);
    assert_eq!(guest.error(), ERROR_INVALID_STATE);
    assert_eq!(guest.call("OpenFixturePlatform", &[]), 1);
    query_and_read(&mut guest, b"second save");
    assert_eq!(guest.call("DeleteFixtureSave", &[SLOT]), 1);
    assert_eq!(guest.call("GetFixtureSaveSize", &[SLOT]), 0);
    assert_eq!(guest.error(), ERROR_NOT_FOUND);
}
