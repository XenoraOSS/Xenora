// SPDX-License-Identifier: GPL-3.0-or-later

//! Base relocation regression tests (#81).
//!
//! Images are built byte by byte from the PE/COFF layout: data directory 5
//! (Base Relocation Table) at optional-header offset 152, and blocks made of a
//! 4-byte page RVA, a 4-byte block size and 2-byte entries whose top 4 bits
//! are the type (0 = ABSOLUTE padding, 3 = HIGHLOW, 10 = DIR64).

use xenora_loader::{GuestMemory, ParsedPe, RelocationError, apply_relocations};

const PREFERRED: u64 = 0x1_4000_0000;
const RELOC_FILE: usize = 0x400;
const COFF_CHARACTERISTICS: usize = 0x44 + 18;
const RELOCS_STRIPPED: u16 = 1;
const OPTIONAL_HEADER: usize = 0x44 + 20;
const RELOC_DIRECTORY_RVA: usize = OPTIONAL_HEADER + 152;
const RELOC_SECTION: usize = OPTIONAL_HEADER + 240 + 40;

/// .text at RVA 0x1000 (file 0x200) holding two pointers, .reloc at RVA 0x2000 (file 0x400).
/// `NumberOfRvaAndSizes` and the relocation table bytes are parameters.
fn build(rva_and_sizes: u32, table: &[u8], dir_size: u32) -> Vec<u8> {
    let mut pe = vec![0u8; 0x600];
    pe[0..2].copy_from_slice(b"MZ");
    pe[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
    pe[0x40..0x44].copy_from_slice(b"PE\0\0");
    let coff = 0x44;
    pe[coff..coff + 2].copy_from_slice(&0x8664u16.to_le_bytes());
    pe[coff + 2..coff + 4].copy_from_slice(&2u16.to_le_bytes());
    pe[coff + 16..coff + 18].copy_from_slice(&240u16.to_le_bytes());
    let opt = coff + 20;
    pe[opt..opt + 2].copy_from_slice(&0x020Bu16.to_le_bytes());
    pe[opt + 16..opt + 20].copy_from_slice(&0x1000u32.to_le_bytes());
    pe[opt + 24..opt + 32].copy_from_slice(&PREFERRED.to_le_bytes());
    pe[opt + 32..opt + 36].copy_from_slice(&0x1000u32.to_le_bytes());
    pe[opt + 36..opt + 40].copy_from_slice(&0x200u32.to_le_bytes());
    pe[opt + 56..opt + 60].copy_from_slice(&0x3000u32.to_le_bytes());
    pe[opt + 60..opt + 64].copy_from_slice(&0x200u32.to_le_bytes());
    pe[opt + 108..opt + 112].copy_from_slice(&rva_and_sizes.to_le_bytes());
    pe[opt + 152..opt + 156].copy_from_slice(&0x2000u32.to_le_bytes());
    pe[opt + 156..opt + 160].copy_from_slice(&dir_size.to_le_bytes());

    let sections: [(&[u8], u32, u32, u32, u32); 2] = [
        (b".text", 0x1000, 0x200, 0x200, 0x6000_0020),
        // .reloc fills the image up to SizeOfImage (0x3000).
        (b".reloc", 0x2000, 0x1000, RELOC_FILE as u32, 0x4200_0040),
    ];
    for (i, (name, va, vsize, raw, flags)) in sections.into_iter().enumerate() {
        let h = opt + 240 + i * 40;
        pe[h..h + name.len()].copy_from_slice(name);
        pe[h + 8..h + 12].copy_from_slice(&vsize.to_le_bytes());
        pe[h + 12..h + 16].copy_from_slice(&va.to_le_bytes());
        pe[h + 16..h + 20].copy_from_slice(&0x200u32.to_le_bytes());
        pe[h + 20..h + 24].copy_from_slice(&raw.to_le_bytes());
        pe[h + 36..h + 40].copy_from_slice(&flags.to_le_bytes());
    }

    // Two absolute pointers into the image, at RVAs 0x1010 and 0x1018.
    pe[0x210..0x218].copy_from_slice(&(PREFERRED + 0x1100).to_le_bytes());
    pe[0x218..0x220].copy_from_slice(&(PREFERRED + 0x2000).to_le_bytes());
    pe[RELOC_FILE..RELOC_FILE + table.len()].copy_from_slice(table);
    pe
}

fn block(page_rva: u32, entries: &[u16]) -> Vec<u8> {
    let mut b = page_rva.to_le_bytes().to_vec();
    b.extend_from_slice(&(8 + 2 * entries.len() as u32).to_le_bytes());
    for e in entries {
        b.extend_from_slice(&e.to_le_bytes());
    }
    b
}

/// DIR64 at 0x1010 and 0x1018, plus one ABSOLUTE padding entry to keep 4-byte alignment.
fn standard_table() -> Vec<u8> {
    block(0x1000, &[0xA010, 0xA018, 0x0000])
}

fn load(bytes: &[u8], base: u64) -> (ParsedPe, GuestMemory) {
    let pe = ParsedPe::parse(bytes).expect("parse");
    let mut memory = GuestMemory::new();
    for s in &pe.sections {
        let raw = &bytes[s.raw_data_ptr as usize..(s.raw_data_ptr + s.raw_data_size) as usize];
        memory
            .map_region_with_data(
                base + u64::from(s.virtual_address),
                raw.len(),
                s.permissions,
                raw,
            )
            .expect("map");
    }
    (pe, memory)
}

fn relocate(
    bytes: &[u8],
    base: u64,
) -> (
    Result<xenora_loader::RelocationReport, RelocationError>,
    GuestMemory,
) {
    let (pe, mut memory) = load(bytes, base);
    let result = apply_relocations(&pe, bytes, base, &mut memory);
    (result, memory)
}

#[test]
fn positive_rebase_adds_delta_to_each_dir64_target() {
    let table = standard_table();
    let bytes = build(16, &table, table.len() as u32);
    let base = PREFERRED + 0x1_0000_0000;
    let (result, memory) = relocate(&bytes, base);
    let report = result.expect("relocate");
    assert_eq!(report.delta, 0x1_0000_0000);
    assert_eq!(report.applied_dir64, 2);
    assert_eq!(report.skipped_absolute, 1);
    assert_eq!(report.total_entries, 3);
    assert_eq!(
        memory.read_u64_bypass(base + 0x1010).unwrap(),
        base + 0x1100
    );
    assert_eq!(
        memory.read_u64_bypass(base + 0x1018).unwrap(),
        base + 0x2000
    );
}

#[test]
fn negative_rebase_subtracts_delta() {
    let table = standard_table();
    let bytes = build(16, &table, table.len() as u32);
    let base = 0x1000_0000;
    let (result, memory) = relocate(&bytes, base);
    assert_eq!(
        result.expect("relocate").delta,
        base as i64 - PREFERRED as i64
    );
    assert_eq!(
        memory.read_u64_bypass(base + 0x1010).unwrap(),
        base + 0x1100
    );
    assert_eq!(
        memory.read_u64_bypass(base + 0x1018).unwrap(),
        base + 0x2000
    );
}

#[test]
fn preferred_base_leaves_targets_unchanged() {
    let table = standard_table();
    let bytes = build(16, &table, table.len() as u32);
    let (result, memory) = relocate(&bytes, PREFERRED);
    assert_eq!(result.expect("relocate").applied_dir64, 0);
    assert_eq!(
        memory.read_u64_bypass(PREFERRED + 0x1010).unwrap(),
        PREFERRED + 0x1100
    );
}

#[test]
fn fixed_image_at_its_preferred_base_needs_no_relocations() {
    let mut bytes = build(16, &[], 0);
    bytes[COFF_CHARACTERISTICS..COFF_CHARACTERISTICS + 2]
        .copy_from_slice(&RELOCS_STRIPPED.to_le_bytes());
    let (result, memory) = relocate(&bytes, PREFERRED);
    let report = result.expect("load at preferred base");
    assert_eq!(report.delta, 0);
    assert_eq!(report.applied_dir64, 0);
    assert_eq!(
        memory.read_u64_bypass(PREFERRED + 0x1010).unwrap(),
        PREFERRED + 0x1100
    );
}

#[test]
fn fixed_image_rejects_a_different_base_before_writing_targets() {
    // A relocation directory cannot override the fixed-base flag.
    for table in [Vec::new(), standard_table()] {
        let mut bytes = build(16, &table, table.len() as u32);
        bytes[COFF_CHARACTERISTICS..COFF_CHARACTERISTICS + 2]
            .copy_from_slice(&RELOCS_STRIPPED.to_le_bytes());
        for base in [PREFERRED + 0x10_000, PREFERRED - 0x10_000] {
            let (result, memory) = relocate(&bytes, base);
            assert_eq!(
                result.unwrap_err(),
                RelocationError::FixedImageBase {
                    preferred_base: PREFERRED,
                    loaded_base: base,
                }
            );
            assert_eq!(
                memory.read_u64_bypass(base + 0x1010).unwrap(),
                PREFERRED + 0x1100
            );
            assert_eq!(
                memory.read_u64_bypass(base + 0x1018).unwrap(),
                PREFERRED + 0x2000
            );
        }
    }
}

#[test]
fn delay_import_fixture_rebases_all_three_initial_iat_pointers() {
    let bytes = include_bytes!("../../../fixtures/pe/delay-imports.exe");
    let base = PREFERRED + 0x80_000;
    let (result, memory) = relocate(bytes, base);
    let report = result.expect("rebase delay-import fixture");
    assert_eq!(report.delta, 0x80_000);
    assert_eq!(report.applied_dir64, 3);
    assert_eq!(report.skipped_absolute, 1);
    assert_eq!(report.total_entries, 4);
    for (slot_rva, stub_rva) in [(0x3000, 0x1010), (0x3008, 0x1018), (0x3020, 0x1020)] {
        assert_eq!(
            memory.read_u64_bypass(base + slot_rva).unwrap(),
            base + stub_rva,
            "delay IAT slot RVA {slot_rva:#x}"
        );
    }
    for terminator_rva in [0x3010, 0x3028] {
        assert_eq!(memory.read_u64_bypass(base + terminator_rva).unwrap(), 0);
    }
}

#[test]
fn directory_beyond_declared_count_is_ignored() {
    // NumberOfRvaAndSizes = 5 means directory index 5 does not exist.
    let table = standard_table();
    let bytes = build(5, &table, table.len() as u32);
    let base = PREFERRED + 0x1_0000_0000;
    let (result, memory) = relocate(&bytes, base);
    assert_eq!(result.expect("relocate").applied_dir64, 0);
    assert_eq!(
        memory.read_u64_bypass(base + 0x1010).unwrap(),
        PREFERRED + 0x1100
    );
}

#[test]
fn unsupported_type_is_rejected() {
    let table = block(0x1000, &[0x3010, 0x0000]); // HIGHLOW is not a PE32+ fixup here
    let bytes = build(16, &table, table.len() as u32);
    let (result, _) = relocate(&bytes, PREFERRED + 0x1000);
    assert_eq!(
        result.unwrap_err(),
        RelocationError::UnsupportedRelocationType(3)
    );
}

#[test]
fn malformed_block_sizes_are_rejected() {
    let base = PREFERRED + 0x1000;
    for size in [0u32, 4, 7, 11] {
        let mut table = 0x1000u32.to_le_bytes().to_vec();
        table.extend_from_slice(&size.to_le_bytes());
        table.extend_from_slice(&[0; 8]);
        let bytes = build(16, &table, table.len() as u32);
        let (result, _) = relocate(&bytes, base);
        assert_eq!(
            result.unwrap_err(),
            RelocationError::MalformedBlockHeader { offset: RELOC_FILE },
            "block size {size}"
        );
    }
}

#[test]
fn misaligned_initial_block_start_is_rejected_before_fixups() {
    let table = standard_table();
    let mut bytes = build(16, &[], table.len() as u32);
    bytes[RELOC_DIRECTORY_RVA..RELOC_DIRECTORY_RVA + 4].copy_from_slice(&0x2002u32.to_le_bytes());
    bytes[RELOC_FILE + 2..RELOC_FILE + 2 + table.len()].copy_from_slice(&table);

    let base = PREFERRED + 0x1000;
    let (result, memory) = relocate(&bytes, base);
    assert_eq!(
        result.unwrap_err(),
        RelocationError::MalformedBlockHeader {
            offset: RELOC_FILE + 2
        }
    );
    assert_eq!(
        memory.read_u64_bypass(base + 0x1010).unwrap(),
        PREFERRED + 0x1100
    );
}

#[test]
fn short_first_block_leaving_misaligned_next_start_is_rejected() {
    let mut table = block(0x1000, &[0x0000]);
    table.extend(block(0x1000, &[0xA018, 0x0000]));
    let bytes = build(16, &table, table.len() as u32);

    let base = PREFERRED + 0x1000;
    let (result, memory) = relocate(&bytes, base);
    assert_eq!(
        result.unwrap_err(),
        RelocationError::MalformedBlockHeader {
            offset: RELOC_FILE + 10
        }
    );
    assert_eq!(
        memory.read_u64_bypass(base + 0x1018).unwrap(),
        PREFERRED + 0x2000
    );
}

#[test]
fn block_larger_than_directory_is_rejected() {
    let table = standard_table(); // 14 bytes
    let bytes = build(16, &table, 12);
    let (result, _) = relocate(&bytes, PREFERRED + 0x1000);
    assert_eq!(
        result.unwrap_err(),
        RelocationError::BlockSizeOutOfBounds {
            block_size: 14,
            max_size: 12
        }
    );
}

#[test]
fn trailing_partial_block_header_is_rejected() {
    let mut table = block(0x1000, &[0xA010, 0x0000]);
    table.extend_from_slice(&[0; 4]);
    let bytes = build(16, &table, table.len() as u32);
    let (result, _) = relocate(&bytes, PREFERRED + 0x1000);
    assert_eq!(
        result.unwrap_err(),
        RelocationError::MalformedBlockHeader {
            offset: RELOC_FILE + 12
        }
    );
}

#[test]
fn directory_past_end_of_file_is_rejected() {
    let table = standard_table();
    let bytes = build(16, &table, 0x300); // .reloc raw data starts at 0x400 in a 0x600-byte file
    let (result, _) = relocate(&bytes, PREFERRED + 0x1000);
    assert_eq!(
        result.unwrap_err(),
        RelocationError::DirectoryOutOfBounds {
            rva: 0x2000,
            size: 0x300
        }
    );
}

#[test]
fn directory_entering_unbacked_virtual_space_is_rejected_before_fixups() {
    const DIRECTORY_RVA: u32 = 0x21f4;
    let mut table = block(0x1000, &[0xA010, 0x0000]);
    table.extend(block(0x1000, &[0xA018, 0x0000]));
    let size = table.len() as u32;
    let base = PREFERRED + 0x1000;

    for virtual_size in [0x200u32, 0x1000] {
        let mut bytes = build(16, &[], size);
        bytes[RELOC_DIRECTORY_RVA..RELOC_DIRECTORY_RVA + 4]
            .copy_from_slice(&DIRECTORY_RVA.to_le_bytes());
        bytes[RELOC_SECTION + 8..RELOC_SECTION + 12].copy_from_slice(&virtual_size.to_le_bytes());
        // The first block fits. The next block's bytes exist physically, but
        // its RVA lies in a gap (0x200) or the zero-filled tail (0x1000).
        bytes[0x5f4..].copy_from_slice(&table[..12]);
        bytes.extend_from_slice(&table[12..]);

        let (result, memory) = relocate(&bytes, base);
        assert_eq!(
            result.unwrap_err(),
            RelocationError::DirectoryOutOfBounds {
                rva: DIRECTORY_RVA,
                size,
            },
            "virtual size {virtual_size:#x}"
        );
        assert_eq!(
            memory.read_u64_bypass(base + 0x1010).unwrap(),
            PREFERRED + 0x1100
        );
        assert_eq!(
            memory.read_u64_bypass(base + 0x1018).unwrap(),
            PREFERRED + 0x2000
        );
    }
}

const CONTINUATION_RVA: u32 = 0x3000;
const CONTINUATION_FILE: usize = 0x1800;

fn split_directory(table: &[u8], split_at: usize) -> Vec<u8> {
    let mut bytes = build(16, &[], table.len() as u32);
    bytes.resize(CONTINUATION_FILE + 0x200, 0);
    bytes[0x46..0x48].copy_from_slice(&3u16.to_le_bytes());
    bytes[OPTIONAL_HEADER + 56..OPTIONAL_HEADER + 60].copy_from_slice(&0x4000u32.to_le_bytes());
    bytes[RELOC_SECTION + 16..RELOC_SECTION + 20].copy_from_slice(&0x1000u32.to_le_bytes());

    let continuation = RELOC_SECTION + 40;
    bytes[continuation..continuation + 6].copy_from_slice(b".cont\0");
    for (offset, value) in [
        (8, 0x200u32),
        (12, CONTINUATION_RVA),
        (16, 0x200),
        (20, CONTINUATION_FILE as u32),
        (36, 0x4200_0040),
    ] {
        bytes[continuation + offset..continuation + offset + 4]
            .copy_from_slice(&value.to_le_bytes());
    }

    let rva = CONTINUATION_RVA - split_at as u32;
    bytes[RELOC_DIRECTORY_RVA..RELOC_DIRECTORY_RVA + 4].copy_from_slice(&rva.to_le_bytes());
    let (prefix, suffix) = table.split_at(split_at);
    let section_end = RELOC_FILE + 0x1000;
    bytes[section_end - prefix.len()..section_end].copy_from_slice(prefix);
    bytes[CONTINUATION_FILE..CONTINUATION_FILE + suffix.len()].copy_from_slice(suffix);
    bytes
}

#[test]
fn split_block_uses_each_sections_raw_offset() {
    let table = block(0x1000, &[0xA010, 0x0000, 0xA018, 0x0000]);

    // Split either the header or the entries across two virtual sections
    // whose bytes are separated by a gap in the file.
    for split_at in [4usize, 12] {
        let mut bytes = split_directory(&table, split_at);
        let next_block_offset = CONTINUATION_FILE + table.len() - split_at;
        let base = PREFERRED + 0x1000;
        let (result, memory) = relocate(&bytes, base);
        let report = result.expect("relocate split directory");
        assert_eq!(report.total_entries, 4);
        assert_eq!(report.applied_dir64, 2);
        assert_eq!(report.skipped_absolute, 2);
        assert_eq!(
            memory.read_u64_bypass(base + 0x1010).unwrap(),
            base + 0x1100
        );
        assert_eq!(
            memory.read_u64_bypass(base + 0x1018).unwrap(),
            base + 0x2000
        );

        // Include the next, empty block header. The earlier fixups stay applied.
        bytes[RELOC_DIRECTORY_RVA + 4..RELOC_DIRECTORY_RVA + 8]
            .copy_from_slice(&(table.len() as u32 + 8).to_le_bytes());
        let (result, memory) = relocate(&bytes, base);
        assert_eq!(
            result.unwrap_err(),
            RelocationError::MalformedBlockHeader {
                offset: next_block_offset,
            }
        );
        assert_eq!(
            memory.read_u64_bypass(base + 0x1018).unwrap(),
            base + 0x2000
        );
    }
}

#[test]
fn misaligned_later_block_start_is_rejected_across_raw_sections() {
    let mut table = block(0x1000, &[0xA010, 0, 0]);
    let first_size = table.len() as u32;
    table.extend(block(0x1000, &[0xA018, 0]));
    let base = PREFERRED + 0x1000;

    for (split_at, offset) in [(12, CONTINUATION_FILE + 2), (16, RELOC_FILE + 0xffe)] {
        let mut bytes = split_directory(&table, split_at);
        let (result, memory) = relocate(&bytes, base);
        assert_eq!(
            result.unwrap_err(),
            RelocationError::MalformedBlockHeader { offset }
        );
        assert_eq!(
            memory.read_u64_bypass(base + 0x1010).unwrap(),
            base + 0x1100
        );
        assert_eq!(
            memory.read_u64_bypass(base + 0x1018).unwrap(),
            PREFERRED + 0x2000
        );

        bytes[RELOC_DIRECTORY_RVA + 4..RELOC_DIRECTORY_RVA + 8]
            .copy_from_slice(&first_size.to_le_bytes());
        let (result, memory) = relocate(&bytes, base);
        assert_eq!(result.unwrap().applied_dir64, 1);
        assert_eq!(
            memory.read_u64_bypass(base + 0x1010).unwrap(),
            base + 0x1100
        );
        assert_eq!(
            memory.read_u64_bypass(base + 0x1018).unwrap(),
            PREFERRED + 0x2000
        );
    }
}

#[test]
fn target_outside_image_is_rejected_even_if_mapped() {
    // SizeOfImage is 0x3000; RVA 0x3000 is past the image but mapped by another region.
    let table = block(0x3000, &[0xA000, 0x0000]);
    let bytes = build(16, &table, table.len() as u32);
    let base = PREFERRED + 0x1000;
    let (pe, mut memory) = load(&bytes, base);
    let rw = pe.sections[1].permissions;
    memory
        .map_region(base + 0x3000, 0x1000, rw)
        .expect("adjacent mapping");
    memory.write_u64_bypass(base + 0x3000, 0x55).unwrap();
    let result = apply_relocations(&pe, &bytes, base, &mut memory);
    assert_eq!(
        result.unwrap_err(),
        RelocationError::TargetOutOfBounds {
            target_va: base + 0x3000
        }
    );
    assert_eq!(memory.read_u64_bypass(base + 0x3000).unwrap(), 0x55);
}

#[test]
fn target_in_an_image_hole_is_rejected_even_if_memory_was_reused() {
    // .text ends at RVA 0x1200 and .reloc begins at 0x2000. RVA 0x1800 is
    // inside SizeOfImage but is a hole rather than image-owned backing.
    let table = block(0x1000, &[0xA800, 0x0000]);
    let bytes = build(16, &table, table.len() as u32);
    let base = PREFERRED + 0x1000;
    let (pe, mut memory) = load(&bytes, base);
    memory
        .map_region(
            base + 0x1800,
            0x100,
            xenora_loader::MemoryPermissions::READ_WRITE,
        )
        .expect("unrelated hole mapping");
    memory.write_u64_bypass(base + 0x1800, 0x55).unwrap();

    let result = apply_relocations(&pe, &bytes, base, &mut memory);

    assert_eq!(
        result.unwrap_err(),
        RelocationError::TargetNotImageBacked {
            target_rva: 0x1800,
            target_va: base + 0x1800,
        }
    );
    assert_eq!(memory.read_u64_bypass(base + 0x1800).unwrap(), 0x55);
}

#[test]
fn header_target_is_image_backed() {
    let table = block(0, &[0xA1F0, 0x0000]);
    let bytes = build(16, &table, table.len() as u32);
    let base = PREFERRED + 0x1000;
    let pe = ParsedPe::parse(&bytes).expect("parse");
    let mut memory = GuestMemory::new();
    pe.map_into_memory(&bytes, &mut memory, base).expect("map");
    let target = base + 0x1F0;
    let original = memory.read_u64_bypass(target).unwrap();

    let report = apply_relocations(&pe, &bytes, base, &mut memory).expect("relocate");

    assert_eq!(report.applied_dir64, 1);
    assert_eq!(
        memory.read_u64_bypass(target).unwrap(),
        original.wrapping_add(report.delta as u64)
    );
}

#[test]
fn target_crossing_adjacent_header_and_section_backing_is_accepted() {
    let table = block(0, &[0xAFFC, 0x0000]);
    let bytes = build(16, &table, table.len() as u32);
    let base = PREFERRED + 0x1000;
    let pe = ParsedPe::parse(&bytes).expect("parse");
    let mut memory = GuestMemory::new();
    pe.map_into_memory(&bytes, &mut memory, base).expect("map");
    let target = base + 0xFFC;
    let original = memory.read_u64_bypass(target).unwrap();

    let report = apply_relocations(&pe, &bytes, base, &mut memory).expect("relocate");

    assert_eq!(report.applied_dir64, 1);
    assert_eq!(
        memory.read_u64_bypass(target).unwrap(),
        original.wrapping_add(report.delta as u64)
    );
}

#[test]
fn virtual_zero_fill_target_is_image_backed() {
    // `.reloc` has 0x200 bytes of file data and a 0x1000-byte virtual extent.
    // RVA 0x2800 is zero-filled section backing rather than a hole.
    let table = block(0x2000, &[0xA800, 0x0000]);
    let bytes = build(16, &table, table.len() as u32);
    let base = PREFERRED + 0x1000;
    let pe = ParsedPe::parse(&bytes).expect("parse");
    let mut memory = GuestMemory::new();
    pe.map_into_memory(&bytes, &mut memory, base).expect("map");
    assert_eq!(memory.read_u64_bypass(base + 0x2800).unwrap(), 0);

    let report = apply_relocations(&pe, &bytes, base, &mut memory).expect("relocate");

    assert_eq!(report.applied_dir64, 1);
    assert_eq!(memory.read_u64_bypass(base + 0x2800).unwrap(), 0x1000);
}

#[test]
fn target_rva_at_top_of_address_space_is_rejected() {
    let table = block(0xFFFF_F000, &[0xAFFC, 0x0000]);
    let bytes = build(16, &table, table.len() as u32);
    let (result, _) = relocate(&bytes, PREFERRED + 0x1000);
    assert!(matches!(
        result,
        Err(RelocationError::TargetOutOfBounds { .. })
    ));
}
