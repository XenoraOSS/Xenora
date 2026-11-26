// SPDX-License-Identifier: GPL-3.0-or-later

//! PE image mapping budget tests (#80).
//!
//! A section's mapped size is max(VirtualSize, SizeOfRawData) and the loader
//! backs it with zero-filled host memory, so a small file can request a large
//! allocation. The loader's documented budget is 256 MiB per image, and the
//! page of headers mapped at the image base (#333) counts toward it.

use xenora_loader::{MemoryError, ParsedPe, PeError};

const BUDGET: u32 = 256 * 1024 * 1024;
/// The image's 0x200 bytes of headers are mapped as one 4 KiB page.
const HEADERS: u32 = 0x1000;

/// An original synthetic image: one 0x200-byte code section at RVA 0x1000 followed by
/// uninitialized sections (no raw data) of the given virtual sizes, packed back to back.
fn build(bss_sizes: &[u32]) -> Vec<u8> {
    let mut pe = vec![0u8; 0x400];
    pe[0..2].copy_from_slice(b"MZ");
    pe[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
    pe[0x40..0x44].copy_from_slice(b"PE\0\0");
    let coff = 0x44;
    pe[coff..coff + 2].copy_from_slice(&0x8664u16.to_le_bytes());
    pe[coff + 2..coff + 4].copy_from_slice(&(1 + bss_sizes.len() as u16).to_le_bytes());
    pe[coff + 16..coff + 18].copy_from_slice(&112u16.to_le_bytes());
    let opt = coff + 20;
    pe[opt..opt + 2].copy_from_slice(&0x020Bu16.to_le_bytes());
    pe[opt + 16..opt + 20].copy_from_slice(&0x1000u32.to_le_bytes());
    pe[opt + 24..opt + 32].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
    // SizeOfHeaders: everything before the code's raw data.
    pe[opt + 60..opt + 64].copy_from_slice(&0x200u32.to_le_bytes());

    let mut section = |i: usize, va: u32, vsize: u32, raw: u32, flags: u32| {
        let h = opt + 112 + i * 40;
        pe[h + 8..h + 12].copy_from_slice(&vsize.to_le_bytes());
        pe[h + 12..h + 16].copy_from_slice(&va.to_le_bytes());
        pe[h + 16..h + 20].copy_from_slice(&(if raw == 0 { 0 } else { 0x200u32 }).to_le_bytes());
        pe[h + 20..h + 24].copy_from_slice(&raw.to_le_bytes());
        pe[h + 36..h + 40].copy_from_slice(&flags.to_le_bytes());
    };
    section(0, 0x1000, 0x200, 0x200, 0x6000_0020);
    let mut va = 0x2000u32;
    for (i, &size) in bss_sizes.iter().enumerate() {
        section(i + 1, va, size, 0, 0xC000_0080);
        va += size;
    }
    // The image span includes the gaps and is independent of allocated bytes.
    pe[opt + 56..opt + 60].copy_from_slice(&va.next_multiple_of(0x1000).to_le_bytes());
    pe
}

fn over_budget() -> PeError {
    PeError::SectionMappingError(MemoryError::AllocationFailed)
}

#[test]
fn small_image_loads() {
    let bytes = build(&[0x1000]);
    let pe = ParsedPe::parse(&bytes).unwrap();
    assert!(pe.load_into_memory(&bytes).is_ok());
}

#[test]
fn image_exactly_at_budget_loads() {
    let bytes = build(&[BUDGET - HEADERS - 0x200]);
    let pe = ParsedPe::parse(&bytes).unwrap();
    assert!(pe.load_into_memory(&bytes).is_ok());
}

#[test]
fn one_byte_over_budget_is_rejected() {
    let bytes = build(&[BUDGET - HEADERS - 0x1FF]);
    let pe = ParsedPe::parse(&bytes).unwrap();
    assert_eq!(pe.load_into_memory(&bytes).err(), Some(over_budget()));
}

#[test]
fn budget_counts_all_sections_together() {
    // Three 128 MiB sections: each fits alone, the total (384 MiB) does not.
    let bytes = build(&[128 << 20; 3]);
    let pe = ParsedPe::parse(&bytes).unwrap();
    assert_eq!(pe.load_into_memory(&bytes).err(), Some(over_budget()));
}

#[test]
fn near_4_gib_request_from_a_1_kib_file_is_rejected_before_allocation() {
    let bytes = build(&[0xF000_0000]);
    assert_eq!(bytes.len(), 0x400);
    let pe = ParsedPe::parse(&bytes).expect("headers are valid; only the size is excessive");
    assert_eq!(pe.load_into_memory(&bytes).err(), Some(over_budget()));
}

#[test]
fn repository_fixture_is_within_budget() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/pe/trusted-runner.exe"
    );
    let bytes = std::fs::read(path).unwrap();
    let pe = ParsedPe::parse(&bytes).unwrap();
    assert!(pe.load_into_memory(&bytes).is_ok());
}
