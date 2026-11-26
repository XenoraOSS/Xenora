// SPDX-License-Identifier: GPL-3.0-or-later

//! Paging-file-backed memory mappings through the guest import table.

mod support;

use support::*;

const KERNEL32: &str = "kernel32.dll";

const IMPORTS: &[(&str, &str)] = &[
    (KERNEL32, "CreateFileMappingA"),
    (KERNEL32, "MapViewOfFile"),
    (KERNEL32, "UnmapViewOfFile"),
    (KERNEL32, "FlushViewOfFile"),
    (KERNEL32, "CreateProcessA"),
    (KERNEL32, "CloseHandle"),
    (KERNEL32, "GetLastError"),
];

const INVALID_FILE: u64 = u64::MAX;

#[test]
fn shared_memory_mappings_are_zeroed_views_the_program_releases() {
    let mut guest = Guest::new(IMPORTS, &[]);

    // A paging-file-backed object opens and its view reads as zero.
    let object = guest.call("CreateFileMappingA", &[INVALID_FILE, 0, 4, 0, 4096, 0]);
    assert_ne!(object, 0);
    let view = guest.call("MapViewOfFile", &[object, 2, 0, 0, 0]);
    assert_ne!(view, 0);
    assert_eq!(guest.u64_at(view), 0);
    // The memory is usable and unmaps once.
    guest.memory.write_u64(view, 42).unwrap();
    assert_eq!(guest.u64_at(view), 42);
    assert_eq!(guest.call("UnmapViewOfFile", &[view]), 1);
    assert_eq!(guest.call("UnmapViewOfFile", &[view]), 0);
    // The object closes through the uniform handle path.
    assert_eq!(guest.call("CloseHandle", &[object]), 1);

    // A real file is honest: not supported.
    assert_eq!(
        guest.call("CreateFileMappingA", &[0x2000, 0, 4, 0, 4096, 0]),
        0
    );
    assert_eq!(guest.error(), 50);
    // A zero size is the documented file-invalid answer.
    assert_eq!(
        guest.call("CreateFileMappingA", &[INVALID_FILE, 0, 4, 0, 0, 0]),
        0
    );
    assert_eq!(guest.error(), 1006);
    // A bogus object cannot map.
    assert_eq!(guest.call("MapViewOfFile", &[0xdead, 2, 0, 0, 0]), 0);
    assert_eq!(guest.error(), 6);
}

#[test]
fn flushing_checks_the_range_of_a_live_view() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let object = guest.call("CreateFileMappingA", &[INVALID_FILE, 0, 4, 0, 4096, 0]);
    assert_ne!(object, 0);
    let view = guest.call("MapViewOfFile", &[object, 2, 0, 0, 2048]);
    assert_ne!(view, 0);
    guest.memory.write_u64(view + 32, 42).unwrap();

    assert_eq!(guest.call("FlushViewOfFile", &[view + 32, 0]), 1);
    assert_eq!(guest.call("FlushViewOfFile", &[view + 32, 2016]), 1);
    assert_eq!(guest.u64_at(view + 32), 42);
    for args in [[view + 32, 2017], [view + 2048, 0], [view + 32, u64::MAX]] {
        assert_eq!(guest.call("FlushViewOfFile", &args), 0);
        assert_eq!(guest.error(), 87);
    }

    assert_eq!(guest.call("UnmapViewOfFile", &[view]), 1);
    assert_eq!(guest.call("FlushViewOfFile", &[view + 32, 0]), 0);
    assert_eq!(guest.error(), 87);
    assert_eq!(guest.call("CloseHandle", &[object]), 1);
}

#[test]
fn create_process_fails_honestly_and_zeroes_the_information() {
    let mut guest = Guest::new(IMPORTS, &[]);
    guest.memory.write_u64(SCRATCH + 0x100, u64::MAX).unwrap();
    let info = SCRATCH + 0x100;
    // Ten arguments: the last two are the startup and process information.
    assert_eq!(
        guest.call("CreateProcessA", &[0, 0, 0, 0, 0, 0, 0, 0, 0, info]),
        0
    );
    assert_eq!(guest.error(), 50);
    assert_eq!(guest.u64_at(info), 0);
}
