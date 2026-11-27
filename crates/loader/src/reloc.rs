// SPDX-License-Identifier: GPL-3.0-or-later

//! PE32+ Base Relocation Engine for Xenora X15.
//!
//! Applies `IMAGE_REL_BASED_DIR64` relocations to guest virtual memory when an image
//! is loaded at a base address different from its preferred PE `image_base`.

use crate::memory::{GuestMemory, MemoryError};
use crate::pe::{MappedImageBacking, ParsedPe, PeSection};
use std::collections::BTreeMap;
use std::fmt;

/// Type of base relocation entry.
pub const IMAGE_REL_BASED_ABSOLUTE: u8 = 0;
pub const IMAGE_REL_BASED_DIR64: u8 = 10;

// COFF Characteristics flag for an image that must use its preferred base.
const IMAGE_FILE_RELOCS_STRIPPED: u16 = 0x0001;

/// Errors that can occur during base relocation parsing or application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelocationError {
    FixedImageBase {
        preferred_base: u64,
        loaded_base: u64,
    },
    DirectoryOutOfBounds {
        rva: u32,
        size: u32,
    },
    MalformedBlockHeader {
        offset: usize,
    },
    BlockSizeOutOfBounds {
        block_size: u32,
        max_size: usize,
    },
    UnsupportedRelocationType(u8),
    TargetOutOfBounds {
        target_va: u64,
    },
    TargetNotImageBacked {
        target_rva: u32,
        target_va: u64,
    },
    Memory(MemoryError),
    ArithmeticOverflow,
}

impl fmt::Display for RelocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FixedImageBase {
                preferred_base,
                loaded_base,
            } => write!(
                f,
                "PE relocations are stripped: image requires base {preferred_base:#x}, requested {loaded_base:#x}"
            ),
            Self::DirectoryOutOfBounds { rva, size } => {
                write!(
                    f,
                    "relocation directory (RVA 0x{rva:08x}, size 0x{size:x}) is out of bounds"
                )
            }
            Self::MalformedBlockHeader { offset } => {
                write!(f, "malformed relocation block header at offset {offset}")
            }
            Self::BlockSizeOutOfBounds {
                block_size,
                max_size,
            } => {
                write!(
                    f,
                    "relocation block size 0x{block_size:x} exceeds boundary {max_size}"
                )
            }
            Self::UnsupportedRelocationType(t) => {
                write!(f, "unsupported base relocation type: {t}")
            }
            Self::TargetOutOfBounds { target_va } => {
                write!(
                    f,
                    "relocation target VA 0x{target_va:016x} is outside the image"
                )
            }
            Self::TargetNotImageBacked {
                target_rva,
                target_va,
            } => {
                write!(
                    f,
                    "relocation target RVA 0x{target_rva:08x} (VA 0x{target_va:016x}) is not continuously backed by image headers or sections"
                )
            }
            Self::Memory(e) => write!(f, "guest memory fault during relocation: {e}"),
            Self::ArithmeticOverflow => write!(f, "arithmetic overflow in relocation computation"),
        }
    }
}

impl std::error::Error for RelocationError {}

impl From<MemoryError> for RelocationError {
    fn from(e: MemoryError) -> Self {
        Self::Memory(e)
    }
}

/// Statistics and counters from applying base relocations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelocationReport {
    pub total_entries: usize,
    pub applied_dir64: usize,
    pub skipped_absolute: usize,
    pub type_counts: BTreeMap<u8, usize>,
    pub delta: i64,
}

struct RelocationSpan<'a> {
    rva: u32,
    file_offset: usize,
    bytes: &'a [u8],
}

impl<'a> RelocationSpan<'a> {
    fn from_section(section: &PeSection, file: &'a [u8], rva: u32, end: u32) -> Option<Self> {
        let section_offset = rva.checked_sub(section.virtual_address)?;
        let available = section.raw_data_size.checked_sub(section_offset)?;
        if available == 0 || section.raw_data_ptr == 0 {
            return None;
        }
        let len = available.min(end - rva) as usize;
        let file_offset = (section.raw_data_ptr as usize).checked_add(section_offset as usize)?;
        Some(Self {
            rva,
            file_offset,
            bytes: file.get(file_offset..file_offset.checked_add(len)?)?,
        })
    }

    fn end_rva(&self) -> u32 {
        self.rva + self.bytes.len() as u32
    }
}

struct RelocationReader<'a> {
    current: RelocationSpan<'a>,
    following: std::vec::IntoIter<RelocationSpan<'a>>,
}

impl<'a> RelocationReader<'a> {
    fn new(pe: &ParsedPe, file: &'a [u8], rva: u32, size: u32) -> Option<Self> {
        let end = rva.checked_add(size)?;
        if end > pe.size_of_image {
            return None;
        }
        let current = pe
            .sections
            .iter()
            .find_map(|section| RelocationSpan::from_section(section, file, rva, end))?;
        let mut covered = current.end_rva();
        let mut following = Vec::new();
        if covered < end {
            // Section headers can be out of RVA order. Verify every handoff
            // before applying any fixups, and borrow the bytes from the file.
            following.extend(pe.sections.iter().filter_map(|section| {
                let start = section.virtual_address;
                if start < covered || start >= end {
                    return None;
                }
                RelocationSpan::from_section(section, file, start, end)
            }));
            following.sort_unstable_by_key(|span| span.rva);
            for span in &following {
                if span.rva != covered {
                    return None;
                }
                covered = span.end_rva();
            }
            if covered != end {
                return None;
            }
        }
        Some(Self {
            current,
            following: following.into_iter(),
        })
    }

    // The caller checks how many directory bytes remain before every read.
    fn advance_if_empty(&mut self) {
        if self.current.bytes.is_empty() {
            self.current = self
                .following
                .next()
                .expect("validated relocation directory has more bytes");
        }
    }

    fn take_contiguous(&mut self, len: usize) -> Option<&'a [u8]> {
        let (bytes, rest) = self.current.bytes.split_at_checked(len)?;
        self.current.bytes = rest;
        self.current.file_offset += len;
        Some(bytes)
    }

    fn read<const N: usize>(&mut self) -> [u8; N] {
        if let Some((value, rest)) = self.current.bytes.split_first_chunk::<N>() {
            self.current.bytes = rest;
            self.current.file_offset += N;
            return *value;
        }

        let mut result = [0; N];
        let mut output = result.as_mut_slice();
        while !output.is_empty() {
            self.advance_if_empty();
            let len = output.len().min(self.current.bytes.len());
            let (chunk, rest) = self.current.bytes.split_at(len);
            output[..len].copy_from_slice(chunk);
            output = &mut output[len..];
            self.current.bytes = rest;
            self.current.file_offset += len;
        }
        result
    }
}

/// Applies PE base relocations when the image is loaded at a nonpreferred base.
///
/// An image with the COFF relocations-stripped flag can use only its preferred base.
pub fn apply_relocations(
    pe: &ParsedPe,
    bytes: &[u8],
    loaded_base: u64,
    memory: &mut GuestMemory,
) -> Result<RelocationReport, RelocationError> {
    let mut report = RelocationReport::default();

    let delta = (loaded_base as i128) - (pe.image_base as i128);
    if delta == 0 {
        // Zero delta: image is loaded at preferred base, no DIR64 fixup needed
        return Ok(report);
    }
    let reloc_dir = find_reloc_directory(bytes, pe.image_base, loaded_base)?;
    if delta > (i64::MAX as i128) || delta < (i64::MIN as i128) {
        return Err(RelocationError::ArithmeticOverflow);
    }
    let delta_i64 = delta as i64;
    report.delta = delta_i64;

    let Some((reloc_rva, reloc_size)) = reloc_dir else {
        return Ok(report);
    };

    if reloc_size == 0 {
        return Ok(report);
    }

    let mut reader = RelocationReader::new(pe, bytes, reloc_rva, reloc_size).ok_or(
        RelocationError::DirectoryOutOfBounds {
            rva: reloc_rva,
            size: reloc_size,
        },
    )?;
    let backing = MappedImageBacking::new(pe);

    let mut read_bytes = 0usize;
    let max_read = reloc_size as usize;

    while read_bytes < max_read {
        reader.advance_if_empty();
        let block_start = reader.current.file_offset;
        let block_rva = reloc_rva as usize + read_bytes;
        if !block_rva.is_multiple_of(4) {
            return Err(RelocationError::MalformedBlockHeader {
                offset: block_start,
            });
        }
        // A partial block header after the last block is malformed, not padding.
        if max_read - read_bytes < 8 {
            return Err(RelocationError::MalformedBlockHeader {
                offset: block_start,
            });
        }
        let page_rva = u32::from_le_bytes(reader.read());
        let block_size = u32::from_le_bytes(reader.read());

        // A block is an 8-byte header followed by whole 2-byte entries.
        if block_size < 8 || block_size % 2 != 0 {
            return Err(RelocationError::MalformedBlockHeader {
                offset: block_start,
            });
        }
        if (block_size as usize) > max_read - read_bytes {
            return Err(RelocationError::BlockSizeOutOfBounds {
                block_size,
                max_size: max_read - read_bytes,
            });
        }

        let entries_size = block_size as usize - 8;
        if let Some(raw_entries) = reader.take_contiguous(entries_size) {
            let entries = raw_entries.as_chunks::<2>().0.iter().copied();
            apply_entries(
                entries.map(u16::from_le_bytes),
                page_rva,
                pe,
                loaded_base,
                memory,
                &backing,
                &mut report,
            )?;
        } else {
            let entries = (0..entries_size / 2).map(|_| u16::from_le_bytes(reader.read()));
            apply_entries(
                entries,
                page_rva,
                pe,
                loaded_base,
                memory,
                &backing,
                &mut report,
            )?;
        }

        read_bytes += block_size as usize;
    }

    // Only these two types can reach a successful return. Build the public
    // histogram once instead of searching its tree for every relocation.
    for (kind, count) in [
        (IMAGE_REL_BASED_ABSOLUTE, report.skipped_absolute),
        (IMAGE_REL_BASED_DIR64, report.applied_dir64),
    ] {
        if count != 0 {
            report.type_counts.insert(kind, count);
        }
    }

    Ok(report)
}

fn apply_entries(
    entries: impl Iterator<Item = u16>,
    page_rva: u32,
    pe: &ParsedPe,
    loaded_base: u64,
    memory: &mut GuestMemory,
    backing: &MappedImageBacking,
    report: &mut RelocationReport,
) -> Result<(), RelocationError> {
    for entry_val in entries {
        let reloc_type = (entry_val >> 12) as u8;
        let offset_in_page = (entry_val & 0x0FFF) as u32;

        report.total_entries += 1;

        match reloc_type {
            IMAGE_REL_BASED_ABSOLUTE => {
                // Padding entry, ignore
                report.skipped_absolute += 1;
            }
            IMAGE_REL_BASED_DIR64 => {
                // Target VA = loaded_base + page_rva + offset_in_page
                let target_rva = page_rva
                    .checked_add(offset_in_page)
                    .ok_or(RelocationError::ArithmeticOverflow)?;
                // The 8-byte target must lie inside the image, not in other mapped memory.
                if target_rva
                    .checked_add(8)
                    .is_none_or(|end| end > pe.size_of_image)
                {
                    return Err(RelocationError::TargetOutOfBounds {
                        target_va: loaded_base.wrapping_add(u64::from(target_rva)),
                    });
                }
                let target_va = loaded_base
                    .checked_add(target_rva as u64)
                    .ok_or(RelocationError::ArithmeticOverflow)?;
                // SizeOfImage includes holes that the image mapping does not own.
                // Only headers and sections are committed backing; adjacent
                // committed intervals may jointly cover a crossing target.
                if !backing.covers(target_rva, 8) {
                    return Err(RelocationError::TargetNotImageBacked {
                        target_rva,
                        target_va,
                    });
                }

                // Read current 64-bit value at target_va
                // Use write_u64_bypass / read_u64_bypass if section is read-only
                let orig_val = memory.read_u64_bypass(target_va).map_err(|e| match e {
                    MemoryError::UnmappedAddress(_) => {
                        RelocationError::TargetOutOfBounds { target_va }
                    }
                    other => RelocationError::Memory(other),
                })?;

                // Rebase: new_val = orig_val + delta_i64
                let new_val = ((orig_val as i128) + (report.delta as i128)) as u64;

                memory
                    .write_u64_bypass(target_va, new_val)
                    .map_err(|e| match e {
                        MemoryError::UnmappedAddress(_) => {
                            RelocationError::TargetOutOfBounds { target_va }
                        }
                        other => RelocationError::Memory(other),
                    })?;

                report.applied_dir64 += 1;
            }
            other => {
                return Err(RelocationError::UnsupportedRelocationType(other));
            }
        }
    }

    Ok(())
}

// Called only for loads at a nonpreferred base.
fn find_reloc_directory(
    bytes: &[u8],
    preferred_base: u64,
    loaded_base: u64,
) -> Result<Option<(u32, u32)>, RelocationError> {
    if bytes.len() < 0x40 {
        return Ok(None);
    }
    let e_lfanew = u32::from_le_bytes(bytes[0x3C..0x40].try_into().unwrap()) as usize;
    let coff_offset = e_lfanew + 4;
    let opt_offset = coff_offset + 20;
    let Some(coff) = bytes.get(coff_offset..opt_offset) else {
        return Ok(None);
    };
    let characteristics = u16::from_le_bytes([coff[18], coff[19]]);
    if characteristics & IMAGE_FILE_RELOCS_STRIPPED != 0 {
        return Err(RelocationError::FixedImageBase {
            preferred_base,
            loaded_base,
        });
    }
    let size_of_opt = usize::from(u16::from_le_bytes([coff[16], coff[17]]));

    // Relocation directory is directory index 5 (offset 112 + 5 * 8 = 152 in Optional Header).
    // It exists only when NumberOfRvaAndSizes (offset 108) covers index 5.
    if size_of_opt < 160 || bytes.len() < opt_offset + 160 {
        return Ok(None);
    }
    let number_of_rva_and_sizes = u32::from_le_bytes(
        bytes[opt_offset + 108..opt_offset + 112]
            .try_into()
            .unwrap(),
    );
    if number_of_rva_and_sizes < 6 {
        return Ok(None);
    }

    let reloc_rva = u32::from_le_bytes(
        bytes[opt_offset + 152..opt_offset + 156]
            .try_into()
            .unwrap(),
    );
    let reloc_size = u32::from_le_bytes(
        bytes[opt_offset + 156..opt_offset + 160]
            .try_into()
            .unwrap(),
    );

    if reloc_rva == 0 || reloc_size == 0 {
        Ok(None)
    } else {
        Ok(Some((reloc_rva, reloc_size)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryPermissions;
    use crate::pe::PeSection;
    use std::hint::black_box;
    use std::time::Instant;

    const PREFERRED: u64 = 0x140000000;
    const LOADED: u64 = PREFERRED + 0x200000;
    const DATA_RVA: u32 = 0x1000;
    const DIRECTORY_RVA: u32 = 0x10000000;
    const DIRECTORY_OFFSET: usize = 0x200;

    // Original synthetic directory, laid out from Microsoft's PE Format,
    // .reloc section: eight-byte block headers and two-byte type/offset words.
    // https://learn.microsoft.com/windows/win32/debug/pe-format#the-reloc-section-image-only
    // Header fields and metadata are supplied directly: PE parsing is not timed.
    fn fixture(blocks: &[(u32, Vec<u16>)], data: &[u8]) -> (ParsedPe, Vec<u8>, GuestMemory) {
        let mut bytes = vec![0u8; DIRECTORY_OFFSET];
        bytes[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        bytes[0x94..0x96].copy_from_slice(&240u16.to_le_bytes());
        bytes[0x104..0x108].copy_from_slice(&16u32.to_le_bytes());
        bytes[0x130..0x134].copy_from_slice(&DIRECTORY_RVA.to_le_bytes());
        for (page, entries) in blocks {
            bytes.extend_from_slice(&page.to_le_bytes());
            bytes.extend_from_slice(&(8u32 + entries.len() as u32 * 2).to_le_bytes());
            for entry in entries {
                bytes.extend_from_slice(&entry.to_le_bytes());
            }
        }
        let size = (bytes.len() - DIRECTORY_OFFSET) as u32;
        bytes[0x134..0x138].copy_from_slice(&size.to_le_bytes());
        let pe = ParsedPe {
            machine: 0x8664,
            number_of_sections: 2,
            time_date_stamp: 0,
            characteristics: crate::pe::IMAGE_FILE_DLL,
            image_base: PREFERRED,
            section_alignment: 4096,
            file_alignment: 512,
            size_of_image: DIRECTORY_RVA + size.max(4096),
            size_of_headers: 512,
            entry_point_rva: 0,
            sections: vec![
                PeSection {
                    name: ".data".into(),
                    virtual_address: DATA_RVA,
                    virtual_size: 4096,
                    raw_data_ptr: 0,
                    raw_data_size: 0,
                    characteristics: 0xC0000040,
                    permissions: MemoryPermissions::READ_ONLY,
                },
                PeSection {
                    name: ".reloc".into(),
                    virtual_address: DIRECTORY_RVA,
                    virtual_size: size,
                    raw_data_ptr: DIRECTORY_OFFSET as u32,
                    raw_data_size: size,
                    characteristics: 0x40000040,
                    permissions: MemoryPermissions::READ_ONLY,
                },
            ],
        };
        let mut memory = GuestMemory::new();
        memory
            .map_region_with_data(
                LOADED + u64::from(DATA_RVA),
                data.len(),
                MemoryPermissions::READ_ONLY,
                data,
            )
            .unwrap();
        (pe, bytes, memory)
    }

    #[test]
    fn counts_padding_and_rebases_read_only_targets() {
        for (preferred, initial, expected) in [
            (PREFERRED, 0x140001234, 0x140201234),
            (LOADED + 0x200000, 0x140001234, 0x13fe01234),
            (LOADED - 8, u64::MAX - 3, 4),
            (LOADED + 8, 3, u64::MAX - 4),
        ] {
            let (mut pe, bytes, mut memory) =
                fixture(&[(DATA_RVA, vec![0x0fff, 0xa000])], &initial.to_le_bytes());
            pe.image_base = preferred;
            let report = apply_relocations(&pe, &bytes, LOADED, &mut memory).unwrap();
            assert_eq!(memory.read_u64(LOADED + 0x1000).unwrap(), expected);
            assert_eq!(report.total_entries, 2);
            assert_eq!(report.applied_dir64, 1);
            assert_eq!(report.skipped_absolute, 1);
            assert_eq!(report.type_counts, BTreeMap::from([(0, 1), (10, 1)]));
        }
    }

    #[test]
    fn histogram_omits_types_that_did_not_occur() {
        for (entries, counts) in [
            (vec![], BTreeMap::new()),
            (vec![0x0fff, 0x0000], BTreeMap::from([(0, 2)])),
            (vec![0xa000, 0xa008], BTreeMap::from([(10, 2)])),
        ] {
            let (pe, bytes, mut memory) = fixture(&[(DATA_RVA, entries)], &[0; 16]);
            let report = apply_relocations(&pe, &bytes, LOADED, &mut memory).unwrap();
            assert_eq!(report.type_counts, counts);
        }
    }

    #[test]
    fn preserves_duplicate_and_overlapping_entry_order() {
        let (pe, bytes, mut memory) = fixture(
            &[(DATA_RVA, vec![0xa000, 0xa000, 0xa004, 0x0000])],
            &[0; 16],
        );
        let report = apply_relocations(&pe, &bytes, LOADED, &mut memory).unwrap();
        // Two additions of 0x200000 at offset zero, then one at offset four.
        let mut actual = [0; 16];
        memory.read_bytes(LOADED + 0x1000, &mut actual).unwrap();
        assert_eq!(
            actual,
            [0, 0, 0x40, 0, 0, 0, 0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(report.type_counts, BTreeMap::from([(0, 1), (10, 3)]));
    }

    #[test]
    fn overlapping_fields_observe_prior_carries_in_directory_order() {
        let initial = [255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 0];
        for (entries, expected) in [
            (vec![0xa000, 0xa004], [0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]),
            (vec![0xa004, 0xa000], [0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]),
        ] {
            let (mut pe, bytes, mut memory) = fixture(&[(DATA_RVA, entries)], &initial);
            pe.image_base = LOADED - 1;
            apply_relocations(&pe, &bytes, LOADED, &mut memory).unwrap();
            let mut actual = [0; 12];
            memory.read_bytes(LOADED + 0x1000, &mut actual).unwrap();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn preserves_prior_writes_and_rejects_unsupported_types() {
        let (pe, bytes, mut memory) = fixture(&[(DATA_RVA, vec![0xa000, 0xf008])], &[0; 16]);
        assert_eq!(
            apply_relocations(&pe, &bytes, LOADED, &mut memory),
            Err(RelocationError::UnsupportedRelocationType(15))
        );
        assert_eq!(memory.read_u64(LOADED + 0x1000).unwrap(), 0x200000);
        assert_eq!(memory.read_u64(LOADED + 0x1008).unwrap(), 0);
    }

    #[test]
    fn zero_delta_keeps_the_empty_report_without_parsing() {
        let (mut pe, _, mut memory) = fixture(&[], &[0; 8]);
        pe.image_base = LOADED;
        assert_eq!(
            apply_relocations(&pe, &[], LOADED, &mut memory).unwrap(),
            RelocationReport::default()
        );
    }

    #[test]
    #[ignore = "manual relocation timing run; release build on an otherwise idle host"]
    fn relocation_profile() {
        for pages in [1u32, 128] {
            for padding_every in [0usize, 2, 1] {
                let blocks: Vec<_> = (0..pages)
                    .map(|page| {
                        let entries = (0..512)
                            .map(|slot| {
                                let padding = padding_every != 0 && slot % padding_every == 0;
                                (if padding { 0 } else { 0xa000 }) | (slot as u16 * 8)
                            })
                            .collect();
                        (DATA_RVA + page * 4096, entries)
                    })
                    .collect();
                let (pe, bytes, initial) = fixture(&blocks, &vec![0; pages as usize * 4096]);
                let mut samples = Vec::new();
                for run in 0..36 {
                    let mut memory = initial.clone();
                    let start = Instant::now();
                    let report = black_box(
                        apply_relocations(
                            black_box(&pe),
                            black_box(&bytes),
                            LOADED,
                            black_box(&mut memory),
                        )
                        .unwrap(),
                    );
                    let elapsed = start.elapsed().as_secs_f64() * 1e6;
                    assert_eq!(report.total_entries, pages as usize * 512);
                    let padding = match padding_every {
                        0 => 0,
                        2 => report.total_entries / 2,
                        _ => report.total_entries,
                    };
                    assert_eq!(report.skipped_absolute, padding);
                    assert_eq!(report.applied_dir64, report.total_entries - padding);
                    // Check every output against the fixture's independently specified pattern.
                    for slot in 0..pages as usize * 512 {
                        let expected = if padding_every != 0 && slot % padding_every == 0 {
                            0
                        } else {
                            0x200000
                        };
                        assert_eq!(
                            memory.read_u64(LOADED + 0x1000 + slot as u64 * 8).unwrap(),
                            expected
                        );
                    }
                    if run >= 5 {
                        samples.push(elapsed);
                    }
                }
                samples.sort_by(f64::total_cmp);
                let mean = samples.iter().sum::<f64>() / samples.len() as f64;
                let variance =
                    samples.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / samples.len() as f64;
                println!(
                    "pages={pages} padding_every={padding_every} n={} median_us={:.3} variance_us2={variance:.3} min_us={:.3} max_us={:.3}",
                    samples.len(),
                    samples[samples.len() / 2],
                    samples[0],
                    samples[samples.len() - 1]
                );
            }
        }
    }
}
