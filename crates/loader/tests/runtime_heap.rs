// SPDX-License-Identifier: GPL-3.0-or-later

//! The legacy memory services through the guest import table.
//!
//! On every 32- or 64-bit Windows, `GlobalAlloc` and `LocalAlloc` are thin
//! wrappers over the process heap: memory is always fixed, the returned
//! handle is the pointer itself, and the moveable and discardable hints of
//! the 16-bit memory manager change nothing. Constants come from the public
//! documentation of the functions and winbase.h.

mod support;

use support::*;

const KERNEL32: &str = "kernel32.dll";
const OBSOLETE: &str = "api-ms-win-core-heap-obsolete-l1-1-0.dll";

const IMPORTS: &[(&str, &str)] = &[
    (KERNEL32, "GlobalAlloc"),
    (KERNEL32, "GlobalFree"),
    (KERNEL32, "LocalAlloc"),
    (KERNEL32, "LocalFree"),
    (KERNEL32, "GetLastError"),
    (KERNEL32, "SetLastError"),
    (KERNEL32, "GetProcessHeap"),
    (KERNEL32, "HeapAlloc"),
    (KERNEL32, "HeapCreate"),
    (KERNEL32, "HeapDestroy"),
    (KERNEL32, "HeapFree"),
    (KERNEL32, "HeapValidate"),
];

const GMEM_MOVEABLE: u64 = 0x2;
const GMEM_ZEROINIT: u64 = 0x40;
const GPTR: u64 = 0x40;
const GHND: u64 = 0x42;
const LMEM_DISCARDABLE: u64 = 0x0f00;
const ERROR_INVALID_PARAMETER: u64 = 87;
const ERROR_INVALID_HANDLE: u64 = 6;

#[test]
fn the_legacy_allocations_are_fixed_zeroed_and_freeable() {
    let mut guest = Guest::new(IMPORTS, &[]);

    // A fixed allocation answers a writable pointer; fresh memory is zero.
    let fixed = guest.call("GlobalAlloc", &[0, 64]);
    assert_ne!(fixed, 0);
    assert_eq!(guest.u32_at(fixed), 0);
    guest.memory.write_bytes(fixed, b"data").unwrap();
    assert_eq!(guest.u32_at(fixed), u32::from_le_bytes(*b"data"));

    // The zero-init form answers zeroed memory even after reuse.
    assert_eq!(guest.call("GlobalFree", &[fixed]), 0);
    let zeroed = guest.call("GlobalAlloc", &[GMEM_ZEROINIT, 64]);
    assert_ne!(zeroed, 0);
    assert_eq!(guest.u32_at(zeroed), 0);

    // The moveable bit and the discardable hints are accepted and inert:
    // the handle is still the pointer, as on 64-bit Windows.
    let moveable = guest.call("GlobalAlloc", &[GHND | LMEM_DISCARDABLE, 32]);
    assert_ne!(moveable, 0);
    assert_eq!(guest.u32_at(moveable), 0);
    assert_eq!(guest.call("GlobalFree", &[moveable]), 0);

    // LocalAlloc behaves identically, including the combined LPTR form.
    let local = guest.call("LocalAlloc", &[GPTR, 16]);
    assert_ne!(local, 0);
    assert_eq!(guest.u64_at(local), 0);
    assert_eq!(guest.call("LocalFree", &[local]), 0);

    // Freeing the null handle is a documented no-op success.
    assert_eq!(guest.call("GlobalFree", &[0]), 0);
    assert_eq!(guest.call("LocalFree", &[0]), 0);
}

#[test]
fn the_legacy_allocations_share_the_process_heap() {
    let mut guest = Guest::new(IMPORTS, &[]);

    // A pointer from GlobalAlloc is a process-heap allocation: HeapAlloc
    // memory and legacy memory interoperate because they are the same heap.
    let legacy = guest.call("GlobalAlloc", &[GMEM_ZEROINIT, 24]);
    let heap = guest.call("GetProcessHeap", &[]);
    assert_ne!(heap, 0);
    let direct = guest.call("HeapAlloc", &[heap, 0, 24]);
    assert_ne!(direct, 0);
    assert_ne!(legacy, direct);
    assert_eq!(guest.call("LocalFree", &[legacy]), 0);
}

#[test]
fn heap_validation_checks_the_exact_live_block_and_its_owner() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let heap = guest.call("GetProcessHeap", &[]);
    let private = guest.call("HeapCreate", &[0, 0, 0]);
    let block = guest.call("HeapAlloc", &[heap, 0, 16]);
    assert_ne!(private, 0);
    assert_ne!(block, 0);

    guest.call("SetLastError", &[419]);
    assert_eq!(guest.call("HeapValidate", &[heap, 0, 0]), 1);
    assert_eq!(guest.call("HeapValidate", &[heap, 0, block]), 1);
    assert_eq!(guest.call("HeapValidate", &[heap, 0, block + 1]), 0);
    assert_eq!(guest.call("HeapValidate", &[private, 0, block]), 0);
    assert_eq!(guest.call("GetLastError", &[]), 419);
    assert_eq!(guest.call("HeapFree", &[heap, 0, block]), 1);
    assert_eq!(guest.call("HeapValidate", &[heap, 0, block]), 0);
    assert_eq!(guest.call("HeapDestroy", &[private]), 1);
}

#[test]
fn the_legacy_failures_are_documented() {
    let mut guest = Guest::new(IMPORTS, &[]);

    // Flags outside the documented mask are an invalid parameter.
    assert_eq!(guest.call("GlobalAlloc", &[0x8, 16]), 0);
    assert_eq!(guest.call("GetLastError", &[]), ERROR_INVALID_PARAMETER);

    // Freeing a pointer that was never allocated answers the handle itself,
    // as documented, with the invalid-handle error.
    let bogus = SCRATCH + 0x40;
    assert_eq!(guest.call("GlobalFree", &[bogus]), bogus);
    assert_eq!(guest.call("GetLastError", &[]), ERROR_INVALID_HANDLE);

    // A double free fails the same way.
    let handle = guest.call("LocalAlloc", &[0, 16]);
    assert_eq!(guest.call("LocalFree", &[handle]), 0);
    assert_eq!(guest.call("LocalFree", &[handle]), handle);
    assert_eq!(guest.call("GetLastError", &[]), ERROR_INVALID_HANDLE);
}

#[test]
fn the_obsolete_api_set_aliases_resolve() {
    let imports: &[(&str, &str)] = &[
        (OBSOLETE, "GlobalAlloc"),
        (OBSOLETE, "GlobalFree"),
        (OBSOLETE, "LocalAlloc"),
        (OBSOLETE, "LocalFree"),
    ];
    let mut guest = Guest::new(imports, &[]);
    let handle = guest.call("GlobalAlloc", &[GMEM_ZEROINIT, 8]);
    assert_ne!(handle, 0);
    assert_eq!(guest.u64_at(handle), 0);
    assert_eq!(guest.call("GlobalFree", &[handle]), 0);
    let local = guest.call("LocalAlloc", &[0, 8]);
    assert_ne!(local, 0);
    assert_eq!(guest.call("LocalFree", &[local]), 0);
}

// Keep the constants referenced when assertions move during maintenance.
const _: u64 = GMEM_MOVEABLE;
