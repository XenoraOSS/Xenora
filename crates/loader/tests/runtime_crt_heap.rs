// SPDX-License-Identifier: GPL-3.0-or-later

//! UCRT block-size, zeroing and aligned allocation through the guest import
//! table. Expectations follow the public UCRT reference pages for `_msize`,
//! `_recalloc`, `_aligned_malloc`, `_aligned_realloc`, `_aligned_msize`,
//! `_aligned_free` and the `errno` constants.

mod support;

use support::*;
use xenora_loader::cpu::ExitReason;

const UCRT: &str = "ucrtbase.dll";
const IMPORTS: &[(&str, &str)] = &[
    (UCRT, "malloc"),
    (UCRT, "free"),
    (UCRT, "_errno"),
    (UCRT, "_msize"),
    (UCRT, "_recalloc"),
    (UCRT, "_aligned_malloc"),
    (UCRT, "_aligned_realloc"),
    (UCRT, "_aligned_msize"),
    (UCRT, "_aligned_free"),
];

const ENOMEM: u32 = 12;
const EINVAL: u32 = 22;
const SIZE_FAILURE: u64 = u64::MAX;

fn errno(guest: &mut Guest) -> u32 {
    let address = guest.call("_errno", &[]);
    guest.u32_at(address)
}

fn clear_errno(guest: &mut Guest) {
    let address = guest.call("_errno", &[]);
    guest.memory.write_u32(address, 0).unwrap();
}

fn faults(guest: &mut Guest, symbol: &str, args: &[u64]) {
    assert!(
        matches!(guest.fails(symbol, args), ExitReason::MemoryFault(_)),
        "{symbol}"
    );
}

#[test]
fn msize_reports_the_requested_size_and_rejects_null() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let block = guest.call("malloc", &[100]);
    assert_eq!(guest.call("_msize", &[block]), 100);
    clear_errno(&mut guest);
    assert_eq!(guest.call("_msize", &[0]), SIZE_FAILURE);
    assert_eq!(errno(&mut guest), EINVAL);
    guest.call("free", &[block]);
    faults(&mut guest, "_msize", &[block]);
}

#[test]
fn recalloc_zeroes_new_bytes_and_keeps_old_ones() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let block = guest.call("_recalloc", &[0, 4, 4]);
    assert_ne!(block, 0);
    assert_eq!(guest.call("_msize", &[block]), 16);
    guest.memory.write_bytes(block, &[0xAB; 16]).unwrap();
    let grown = guest.call("_recalloc", &[block, 8, 4]);
    assert_eq!(guest.call("_msize", &[grown]), 32);
    let mut bytes = [0u8; 32];
    guest.memory.read_bytes(grown, &mut bytes).unwrap();
    assert_eq!(&bytes[..16], &[0xAB; 16]);
    assert_eq!(&bytes[16..], &[0; 16], "new bytes are zero");

    clear_errno(&mut guest);
    assert_eq!(guest.call("_recalloc", &[grown, u64::MAX, 2]), 0);
    assert_eq!(errno(&mut guest), ENOMEM);
    assert_eq!(guest.call("_msize", &[grown]), 32, "unchanged on overflow");
    assert_eq!(
        guest.call("_recalloc", &[grown, 0, 4]),
        0,
        "size zero frees"
    );
    faults(&mut guest, "_msize", &[grown]);
}

#[test]
fn aligned_blocks_honour_alignment_size_and_reallocation() {
    let mut guest = Guest::new(IMPORTS, &[]);
    for alignment in [1, 16, 4096, 65536] {
        let block = guest.call("_aligned_malloc", &[40, alignment]);
        assert_ne!(block, 0, "{alignment}");
        assert_eq!(block % alignment, 0, "{alignment}");
        assert_eq!(guest.call("_aligned_msize", &[block, alignment, 0]), 40);
        guest.call("_aligned_free", &[block]);
    }

    let block = guest.call("_aligned_malloc", &[64, 64]);
    guest.memory.write_bytes(block, &[0x5A; 64]).unwrap();
    let grown = guest.call("_aligned_realloc", &[block, 1 << 20, 64]);
    assert_ne!(grown, 0);
    assert_eq!(grown % 64, 0);
    assert_eq!(guest.call("_aligned_msize", &[grown, 64, 0]), 1 << 20);
    let mut bytes = [0u8; 64];
    guest.memory.read_bytes(grown, &mut bytes).unwrap();
    assert_eq!(bytes, [0x5A; 64], "contents move with the block");
    let fresh = guest.call("_aligned_realloc", &[0, 8, 32]);
    assert_eq!(fresh % 32, 0);
    assert_eq!(guest.call("_aligned_realloc", &[fresh, 0, 32]), 0);
    guest.call("_aligned_free", &[grown]);
}

#[test]
fn aligned_allocators_reject_bad_alignment_and_bad_frees() {
    let mut guest = Guest::new(IMPORTS, &[]);
    for alignment in [0, 3, 48] {
        clear_errno(&mut guest);
        assert_eq!(guest.call("_aligned_malloc", &[16, alignment]), 0);
        assert_eq!(errno(&mut guest), EINVAL, "{alignment}");
    }
    // More alignment than the heap provides fails rather than misaligns.
    clear_errno(&mut guest);
    assert_eq!(guest.call("_aligned_malloc", &[16, 1 << 20]), 0);
    assert_eq!(errno(&mut guest), ENOMEM);

    let block = guest.call("_aligned_malloc", &[16, 16]);
    clear_errno(&mut guest);
    assert_eq!(guest.call("_aligned_msize", &[block, 16, 8]), SIZE_FAILURE);
    assert_eq!(errno(&mut guest), EINVAL);
    assert_eq!(guest.call("_aligned_free", &[0]), 0, "null is a no-op");
    guest.call("_aligned_free", &[block]);
    faults(&mut guest, "_aligned_free", &[block]); // double free

    let plain = guest.call("malloc", &[16]);
    faults(&mut guest, "_aligned_free", &[plain]); // not from _aligned_malloc
    faults(&mut guest, "_aligned_realloc", &[plain, 32, 16]);
    faults(&mut guest, "_aligned_msize", &[plain, 16, 0]);
    guest.call("free", &[plain]);
}

#[test]
fn heap_api_set_reaches_the_same_allocator() {
    const API: &[(&str, &str)] = &[
        ("api-ms-win-crt-heap-l1-1-0.dll", "_aligned_malloc"),
        ("api-ms-win-crt-heap-l1-1-0.dll", "_aligned_free"),
        ("api-ms-win-crt-heap-l1-1-0.dll", "_msize"),
    ];
    let mut guest = Guest::new(API, &[]);
    let block = guest.call("_aligned_malloc", &[24, 256]);
    assert_eq!(block % 256, 0);
    assert_eq!(guest.call("_msize", &[block]), 24);
    guest.call("_aligned_free", &[block]);
    faults(&mut guest, "_aligned_free", &[block]);
}
