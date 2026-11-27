// SPDX-License-Identifier: GPL-3.0-or-later

//! PE32+ (64-bit Portable Executable) format parser and section loader for Xenora.
//!
//! Parses standard PE32+ headers (DOS header, COFF header, Optional Header 64)
//! and loads the headers and each section into `GuestMemory` with appropriate
//! permissions.

use crate::memory::{GuestMemory, MemoryError, MemoryPermissions};
use std::borrow::Cow;
use std::fmt;

/// Errors that can occur during PE32+ parsing and loading.
#[derive(Debug, PartialEq, Eq)]
pub enum PeError {
    TooSmall,
    InvalidDosSignature,
    InvalidPeSignature,
    UnsupportedMachine(u16),
    InvalidMagic(u16),
    InvalidSectionHeaders,
    SectionMappingError(MemoryError),
    InvalidEntryPoint,
    /// The headers cannot be mapped at the image base: `SizeOfHeaders` is
    /// zero, ends before the section table does, runs past the end of the
    /// file, or its pages reach into a section; or the bytes given to the
    /// loader do not hold the PE32+ headers that were parsed.
    InvalidHeaderSize,
    /// `SizeOfImage` is smaller than the highest mapped RVA plus one.
    InvalidImageSize {
        declared: u32,
        required: u64,
    },
}

impl fmt::Display for PeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooSmall => write!(f, "executable image is smaller than required headers"),
            Self::InvalidDosSignature => write!(f, "invalid DOS signature (expected 'MZ')"),
            Self::InvalidPeSignature => write!(f, "invalid PE signature (expected 'PE\\0\\0')"),
            Self::UnsupportedMachine(m) => {
                write!(
                    f,
                    "unsupported machine type 0x{m:04x} (expected AMD64 0x8664)"
                )
            }
            Self::InvalidMagic(magic) => {
                write!(
                    f,
                    "invalid optional header magic 0x{magic:04x} (expected PE32+ 0x020b)"
                )
            }
            Self::InvalidSectionHeaders => write!(f, "invalid or corrupt section headers"),
            Self::SectionMappingError(e) => write!(f, "failed to map section: {e}"),
            Self::InvalidEntryPoint => write!(f, "entry point address is outside executable code"),
            Self::InvalidHeaderSize => write!(
                f,
                "the PE headers cannot be mapped: SizeOfHeaders is zero, shorter than the section \
                 table, longer than the file or reaches into a section, or the bytes do not match \
                 the parsed headers"
            ),
            Self::InvalidImageSize { declared, required } => write!(
                f,
                "SizeOfImage {declared:#x} is too small: the PE mappings require {required:#x} bytes of image space"
            ),
        }
    }
}

impl std::error::Error for PeError {}

/// An individual section extracted from a PE32+ header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeSection {
    pub name: String,
    pub virtual_address: u32,
    pub virtual_size: u32,
    pub raw_data_ptr: u32,
    pub raw_data_size: u32,
    pub characteristics: u32,
    pub permissions: MemoryPermissions,
}

/// Parsed metadata from a PE32+ executable or DLL image.
#[derive(Clone, Debug)]
pub struct ParsedPe {
    pub machine: u16,
    pub number_of_sections: u16,
    pub time_date_stamp: u32,
    /// The COFF characteristics word, including `IMAGE_FILE_DLL`.
    pub characteristics: u16,
    pub image_base: u64,
    pub section_alignment: u32,
    pub file_alignment: u32,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub entry_point_rva: u32,
    pub sections: Vec<PeSection>,
}

/// Result of loading a PE32+ image into guest memory.
pub struct LoadedImage {
    pub pe: ParsedPe,
    pub memory: GuestMemory,
    /// Load base plus `entry_point_rva`. A zero entry RVA means the image has
    /// no entry point; it is valid only for a DLL.
    pub entry_point: u64,
    /// Guest address of the `ImageBase` field in the mapped optional header.
    /// Startup writes the load base there once relocations have been applied.
    pub(crate) image_base_field: u64,
}

// Windows PE Characteristic Constants
pub const IMAGE_SCN_CNT_CODE: u32 = 0x00000020;
pub const IMAGE_SCN_MEM_EXECUTE: u32 = 0x20000000;
pub const IMAGE_SCN_MEM_READ: u32 = 0x40000000;
pub const IMAGE_SCN_MEM_WRITE: u32 = 0x80000000;

// Standard Constants
pub const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
/// COFF characteristics bit identifying a DLL rather than an executable image.
pub const IMAGE_FILE_DLL: u16 = 0x2000;
pub const PE32_PLUS_MAGIC: u16 = 0x020B;
/// PE32+ optional header length up to and including NumberOfRvaAndSizes.
const PE32_PLUS_MIN_OPTIONAL_HEADER: usize = 112;
const SECTION_HEADER_SIZE: usize = 40;
/// Host memory budget for the mapped headers and sections of one image. This is a
/// loader policy limit, not a property of the PE format.
const MAX_MAPPED_IMAGE_BYTES: u64 = 256 * 1024 * 1024;
/// The mapped headers end on a guest page boundary.
const HEADER_PAGE: u64 = 0x1000;
/// Field offsets within the PE32+ optional header.
const OPTIONAL_IMAGE_BASE: usize = 24;
const OPTIONAL_SIZE_OF_IMAGE: usize = 56;
const OPTIONAL_SIZE_OF_HEADERS: usize = 60;

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// A nonzero entry must lie inside executable image code. Entry RVA zero is
/// the documented absence of an entry point and is admissible only for a DLL.
fn entry_point_is_valid(
    characteristics: u16,
    entry_point_rva: u32,
    sections: &[PeSection],
) -> bool {
    if entry_point_rva == 0 {
        return characteristics & IMAGE_FILE_DLL != 0;
    }
    sections.iter().any(|section| {
        section.permissions.execute
            && entry_point_rva >= section.virtual_address
            && entry_point_rva - section.virtual_address
                < section.virtual_size.max(section.raw_data_size)
    })
}

fn section_initial_data<'a>(bytes: &'a [u8], section: &PeSection) -> Result<&'a [u8], PeError> {
    if section.raw_data_size == 0 || section.raw_data_ptr == 0 {
        return Ok(&[]);
    }
    let start = section.raw_data_ptr as usize;
    let end = start
        .checked_add(section.raw_data_size as usize)
        .ok_or(PeError::InvalidSectionHeaders)?;
    bytes.get(start..end).ok_or(PeError::InvalidSectionHeaders)
}

struct SectionMapping<'a> {
    base: u64,
    size: usize,
    permissions: MemoryPermissions,
    initial_data: &'a [u8],
}

struct MappingPlan<'a> {
    headers: &'a [u8],
    header_map_size: usize,
    image_base_field: u64,
    entry_point: u64,
    sections: Vec<SectionMapping<'a>>,
}

struct MappingOutcome {
    entry_point: u64,
    image_base_field: u64,
}

fn map_plan(
    base: u64,
    memory: &mut GuestMemory,
    plan: MappingPlan<'_>,
) -> Result<MappingOutcome, PeError> {
    // Windows maps the headers read-only at the image base, and programs read
    // them: a C runtime finds its own sections this way before `main`. The
    // file bytes are the initial data of the mapping.
    memory
        .map_region_with_data(
            base,
            plan.header_map_size,
            MemoryPermissions::READ_ONLY,
            plan.headers,
        )
        .map_err(PeError::SectionMappingError)?;

    for section in plan.sections {
        memory
            .map_region_with_data(
                section.base,
                section.size,
                section.permissions,
                section.initial_data,
            )
            .map_err(PeError::SectionMappingError)?;
    }

    Ok(MappingOutcome {
        entry_point: plan.entry_point,
        image_base_field: plan.image_base_field,
    })
}

impl ParsedPe {
    /// Parse PE32+ headers from an in-memory byte slice.
    pub fn parse(bytes: &[u8]) -> Result<Self, PeError> {
        Self::parse_headers(bytes.len() as u64, |offset, len| {
            let start = offset as usize;
            Ok(Cow::Borrowed(&bytes[start..start + len]))
        })
    }

    /// Read only the fields needed for PE metadata. The reader receives
    /// in-range requests and returns exactly the requested bytes.
    pub(crate) fn parse_headers<'data, E: From<PeError>>(
        file_len: u64,
        mut read: impl FnMut(u64, usize) -> Result<Cow<'data, [u8]>, E>,
    ) -> Result<Self, E> {
        const DOS_HEADER_SIZE: usize = 0x40;
        const PE_AND_COFF_SIZE: usize = 24;
        if file_len < DOS_HEADER_SIZE as u64 {
            return Err(PeError::TooSmall.into());
        }

        let dos = read(0, DOS_HEADER_SIZE)?;
        if &dos[..2] != b"MZ" {
            return Err(PeError::InvalidDosSignature.into());
        }

        let pe_offset = u64::from(read_u32(&dos, 0x3c));
        let opt_offset = pe_offset + PE_AND_COFF_SIZE as u64;
        if file_len < opt_offset {
            return Err(PeError::TooSmall.into());
        }

        let coff = read(pe_offset, PE_AND_COFF_SIZE)?;
        if &coff[..4] != b"PE\0\0" {
            return Err(PeError::InvalidPeSignature.into());
        }

        let machine = read_u16(&coff, 4);
        if machine != IMAGE_FILE_MACHINE_AMD64 {
            return Err(PeError::UnsupportedMachine(machine).into());
        }
        let number_of_sections = read_u16(&coff, 6);
        let time_date_stamp = read_u32(&coff, 8);
        let coff_characteristics = read_u16(&coff, 22);
        let size_of_optional_header = usize::from(read_u16(&coff, 20));

        let section_table_offset = opt_offset + size_of_optional_header as u64;
        if file_len < opt_offset + 2 {
            return Err(PeError::TooSmall.into());
        }
        let full_optional_header = size_of_optional_header >= PE32_PLUS_MIN_OPTIONAL_HEADER
            && file_len >= section_table_offset;
        // Diagnose the magic first even if the advertised header is incomplete.
        let optional = read(
            opt_offset,
            if full_optional_header {
                PE32_PLUS_MIN_OPTIONAL_HEADER
            } else {
                2
            },
        )?;
        let magic = read_u16(&optional, 0);
        if magic != PE32_PLUS_MAGIC {
            return Err(PeError::InvalidMagic(magic).into());
        }
        if !full_optional_header {
            return Err(PeError::TooSmall.into());
        }

        let entry_point_rva = read_u32(&optional, 16);
        let image_base = u64::from_le_bytes(
            optional[OPTIONAL_IMAGE_BASE..OPTIONAL_IMAGE_BASE + 8]
                .try_into()
                .unwrap(),
        );
        let section_alignment = read_u32(&optional, 32);
        let file_alignment = read_u32(&optional, 36);
        let size_of_image = read_u32(&optional, OPTIONAL_SIZE_OF_IMAGE);
        let size_of_headers = read_u32(&optional, OPTIONAL_SIZE_OF_HEADERS);

        // A 32-bit PE offset and 16-bit optional size and section count all
        // fit in these file coordinates. Read only the table itself.
        let table_size = usize::from(number_of_sections) * SECTION_HEADER_SIZE;
        if file_len < section_table_offset + table_size as u64 {
            return Err(PeError::InvalidSectionHeaders.into());
        }
        let table = read(section_table_offset, table_size)?;
        let mut sections = Vec::with_capacity(usize::from(number_of_sections));
        for sec_bytes in table.as_chunks::<SECTION_HEADER_SIZE>().0 {
            // Section name (up to 8 bytes UTF-8 or null-padded ASCII)
            let name_raw = &sec_bytes[0..8];
            let name_len = name_raw.iter().position(|&b| b == 0).unwrap_or(8);
            let name = String::from_utf8_lossy(&name_raw[..name_len]).into_owned();

            let virtual_size = read_u32(sec_bytes, 8);
            let virtual_address = read_u32(sec_bytes, 12);
            let raw_data_size = read_u32(sec_bytes, 16);
            let raw_data_ptr = read_u32(sec_bytes, 20);
            let characteristics = read_u32(sec_bytes, 36);

            // The mapped range must fit in the 32-bit RVA space, and raw data must be in the file.
            virtual_address
                .checked_add(virtual_size.max(raw_data_size))
                .ok_or(PeError::InvalidSectionHeaders)?;
            if raw_data_size > 0 {
                let raw_end = u64::from(raw_data_ptr) + u64::from(raw_data_size);
                if raw_end > file_len {
                    return Err(PeError::InvalidSectionHeaders.into());
                }
            }

            let permissions = MemoryPermissions {
                read: (characteristics & IMAGE_SCN_MEM_READ) != 0,
                write: (characteristics & IMAGE_SCN_MEM_WRITE) != 0,
                execute: (characteristics & IMAGE_SCN_MEM_EXECUTE) != 0,
            };

            sections.push(PeSection {
                name,
                virtual_address,
                virtual_size,
                raw_data_ptr,
                raw_data_size,
                characteristics,
                permissions,
            });
        }

        // Mapped section ranges must not overlap. An ascending, disjoint
        // table needs neither a temporary allocation nor sorting. Preserve
        // section-table order in the result, even for an unsorted table.
        let ranges = sections.iter().filter_map(|section| {
            let size = section.virtual_size.max(section.raw_data_size);
            (size != 0).then_some((section.virtual_address, section.virtual_address + size))
        });
        if !ranges.clone().is_sorted_by(|left, right| left.1 <= right.0) {
            let mut ranges: Vec<_> = ranges.collect();
            ranges.sort_unstable();
            if ranges.windows(2).any(|pair| pair[1].0 < pair[0].1) {
                return Err(PeError::InvalidSectionHeaders.into());
            }
        }

        if !entry_point_is_valid(coff_characteristics, entry_point_rva, &sections) {
            return Err(PeError::InvalidEntryPoint.into());
        }

        Ok(ParsedPe {
            machine,
            number_of_sections,
            time_date_stamp,
            characteristics: coff_characteristics,
            image_base,
            section_alignment,
            file_alignment,
            size_of_image,
            size_of_headers,
            entry_point_rva,
            sections,
        })
    }

    /// Whether the COFF characteristics mark this image as a DLL.
    pub fn is_dll(&self) -> bool {
        self.characteristics & IMAGE_FILE_DLL != 0
    }

    fn validate_entry_point(&self) -> Result<(), PeError> {
        if entry_point_is_valid(self.characteristics, self.entry_point_rva, &self.sections) {
            Ok(())
        } else {
            Err(PeError::InvalidEntryPoint)
        }
    }

    /// Translate a Relative Virtual Address (RVA) to a raw file offset.
    pub fn rva_to_file_offset(&self, rva: u32) -> Option<usize> {
        for sec in &self.sections {
            // Sections are public and may not come from `parse`, so stay checked here.
            let Some(diff) = rva.checked_sub(sec.virtual_address) else {
                continue;
            };
            if diff < sec.virtual_size.max(sec.raw_data_size) && diff < sec.raw_data_size {
                return (sec.raw_data_ptr as usize).checked_add(diff as usize);
            }
        }
        None
    }

    /// Where the headers are and how much to map for them: the file length of
    /// the headers, the page-rounded length of the mapping, and the file offset
    /// of the optional header's `ImageBase` field. Nothing is copied or
    /// allocated here.
    ///
    /// A caller may pass bytes other than the ones parsed, so the header
    /// layout is read again from `bytes` with every access checked, and must
    /// be a PE32+ optional header that agrees with the parsed metadata.
    fn headers_for(&self, bytes: &[u8]) -> Result<(usize, u64, usize), PeError> {
        let field = |at: usize, len: usize| at.checked_add(len).and_then(|end| bytes.get(at..end));
        let u16_at = |at: usize| Some(u16::from_le_bytes(field(at, 2)?.try_into().ok()?));
        let u32_at = |at: usize| Some(u32::from_le_bytes(field(at, 4)?.try_into().ok()?));
        let layout = || {
            let coff = (u32_at(0x3C)? as usize).checked_add(4)?;
            let sections = u16_at(coff.checked_add(2)?)?;
            let optional_size = usize::from(u16_at(coff.checked_add(16)?)?);
            let optional = coff.checked_add(20)?;
            let image_base = optional.checked_add(OPTIONAL_IMAGE_BASE)?;
            let agrees = u16_at(optional)? == PE32_PLUS_MAGIC
                && optional_size >= PE32_PLUS_MIN_OPTIONAL_HEADER
                && sections == self.number_of_sections
                && u16_at(coff.checked_add(18)?)? == self.characteristics
                && u32_at(optional.checked_add(16)?)? == self.entry_point_rva
                && u64::from_le_bytes(field(image_base, 8)?.try_into().ok()?) == self.image_base
                && u32_at(optional.checked_add(OPTIONAL_SIZE_OF_IMAGE)?)? == self.size_of_image
                && u32_at(optional.checked_add(OPTIONAL_SIZE_OF_HEADERS)?)? == self.size_of_headers;
            let table_end = optional
                .checked_add(optional_size)?
                .checked_add(usize::from(sections).checked_mul(SECTION_HEADER_SIZE)?)?;
            agrees.then_some((image_base, table_end))
        };
        let (image_base, table_end) = layout().ok_or(PeError::InvalidHeaderSize)?;

        // A size of zero is shorter than any section table.
        let len = self.size_of_headers as usize;
        if len < table_end || len > bytes.len() {
            return Err(PeError::InvalidHeaderSize);
        }
        let mapped = u64::from(self.size_of_headers).next_multiple_of(HEADER_PAGE);
        let reaches_a_section = self.sections.iter().any(|sec| {
            sec.virtual_size.max(sec.raw_data_size) > 0 && u64::from(sec.virtual_address) < mapped
        });
        if reaches_a_section {
            return Err(PeError::InvalidHeaderSize);
        }
        Ok((len, mapped, image_base))
    }

    /// Validate the input and arithmetic of a mapping without changing memory.
    /// The plan borrows section data from `bytes`, so the mapping itself does
    /// not need to rediscover file-slice errors.
    fn mapping_plan<'a>(&self, bytes: &'a [u8], base: u64) -> Result<MappingPlan<'a>, PeError> {
        let (headers_len, headers_mapped, image_base_field_offset) = self.headers_for(bytes)?;

        // Count allocated bytes and measure the full image span separately.
        // RVA gaps do not consume host memory.
        let mut mapped = headers_mapped;
        let mut required = headers_mapped;
        for sec in &self.sections {
            let size = u64::from(sec.virtual_size.max(sec.raw_data_size));
            mapped = mapped
                .checked_add(size)
                .ok_or(PeError::SectionMappingError(MemoryError::AllocationFailed))?;
            if size != 0 {
                required = required.max(u64::from(sec.virtual_address) + size);
            }
        }
        if mapped > MAX_MAPPED_IMAGE_BYTES {
            return Err(PeError::SectionMappingError(MemoryError::AllocationFailed));
        }
        let declared = self.size_of_image;
        if required > u64::from(declared) {
            return Err(PeError::InvalidImageSize { declared, required });
        }

        base.checked_add(headers_mapped)
            .ok_or(PeError::SectionMappingError(MemoryError::Overflow))?;
        let image_base_field = base
            .checked_add(image_base_field_offset as u64)
            .ok_or(PeError::InvalidHeaderSize)?;
        let header_map_size = usize::try_from(headers_mapped)
            .map_err(|_| PeError::SectionMappingError(MemoryError::AllocationFailed))?;

        let mut sections = Vec::with_capacity(self.sections.len());
        for sec in &self.sections {
            let size = u64::from(sec.virtual_size.max(sec.raw_data_size));
            if size == 0 {
                continue;
            }
            let section_base = base
                .checked_add(u64::from(sec.virtual_address))
                .ok_or(PeError::SectionMappingError(MemoryError::Overflow))?;
            section_base
                .checked_add(size)
                .ok_or(PeError::SectionMappingError(MemoryError::Overflow))?;
            let size = usize::try_from(size)
                .map_err(|_| PeError::SectionMappingError(MemoryError::AllocationFailed))?;
            sections.push(SectionMapping {
                base: section_base,
                size,
                permissions: sec.permissions,
                initial_data: section_initial_data(bytes, sec)?,
            });
        }

        let entry_point = base
            .checked_add(u64::from(self.entry_point_rva))
            .ok_or(PeError::InvalidEntryPoint)?;
        self.validate_entry_point()?;

        Ok(MappingPlan {
            headers: &bytes[..headers_len],
            header_map_size,
            image_base_field,
            entry_point,
            sections,
        })
    }

    pub(crate) fn image_base_field_address(&self, bytes: &[u8], base: u64) -> Result<u64, PeError> {
        Ok(self.mapping_plan(bytes, base)?.image_base_field)
    }

    /// Map this parsed image into an existing guest address space at `base`.
    ///
    /// Only the image headers and sections are mapped. Relocations, import
    /// binding, exception-directory registration, TLS initialization and entry
    /// execution remain separate steps. The mapping is transactional: if any
    /// region cannot be mapped, `memory` is restored to its prior state.
    ///
    /// This insertion-only implementation snapshots the address space before
    /// mapping, so its cost and peak host memory use scale with the existing
    /// memory rather than only the new image. It is a batch-local mechanism,
    /// not the ownership or rollback design for dynamic module loading.
    ///
    /// The result is the entry address for a nonzero entry RVA, or `None` for
    /// a DLL whose entry RVA is zero.
    pub fn map_into_memory(
        &self,
        bytes: &[u8],
        memory: &mut GuestMemory,
        base: u64,
    ) -> Result<Option<u64>, PeError> {
        let mapped = self.map_transactional(bytes, memory, base)?;
        Ok((self.entry_point_rva != 0).then_some(mapped.entry_point))
    }

    fn map_transactional(
        &self,
        bytes: &[u8],
        memory: &mut GuestMemory,
        base: u64,
    ) -> Result<MappingOutcome, PeError> {
        let plan = self.mapping_plan(bytes, base)?;
        let before = memory.clone();
        match map_plan(base, memory, plan) {
            Ok(mapped) => Ok(mapped),
            Err(error) => {
                *memory = before;
                Err(error)
            }
        }
    }

    /// Load this parsed PE into a fresh `GuestMemory` instance, mapping the
    /// headers and all sections.
    pub fn load_into_memory(&self, bytes: &[u8]) -> Result<LoadedImage, PeError> {
        let (memory, entry_point, image_base_field) = self.map_at_base(bytes, self.image_base)?;
        Ok(LoadedImage {
            pe: self.clone(),
            memory,
            entry_point,
            image_base_field,
        })
    }

    pub(crate) fn load_at_base(self, bytes: &[u8], base: u64) -> Result<LoadedImage, PeError> {
        let (memory, entry_point, image_base_field) = self.map_at_base(bytes, base)?;
        Ok(LoadedImage {
            pe: self,
            memory,
            entry_point,
            image_base_field,
        })
    }

    fn map_at_base(&self, bytes: &[u8], base: u64) -> Result<(GuestMemory, u64, u64), PeError> {
        let mut memory = GuestMemory::new();
        let mapped = self.map_transactional(bytes, &mut memory, base)?;
        Ok((memory, mapped.entry_point, mapped.image_base_field))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../../../fixtures/pe/runtime-services.exe");

    fn section_table(bytes: &[u8]) -> usize {
        let coff = read_u32(bytes, 0x3c) as usize + 4;
        coff + 20 + usize::from(read_u16(bytes, coff + 16))
    }

    #[test]
    fn header_reads_skip_the_stub_optional_padding_and_raw_sections() {
        const PE_AT: u64 = 0x8000_0000;
        const OPTIONAL_SIZE: u16 = u16::MAX;
        const RAW_AT: u32 = 0xffff_f000;
        const RAW_SIZE: u32 = 0x2000;
        let original = ParsedPe::parse(FIXTURE).unwrap();
        let file_len = u64::from(RAW_AT) + u64::from(RAW_SIZE);
        let old_pe_at = read_u32(FIXTURE, 0x3c) as usize;
        let mut dos = FIXTURE[..64].to_vec();
        dos[0x3c..0x40].copy_from_slice(&(PE_AT as u32).to_le_bytes());
        let mut coff = FIXTURE[old_pe_at..old_pe_at + 24].to_vec();
        coff[20..22].copy_from_slice(&OPTIONAL_SIZE.to_le_bytes());
        let optional = &FIXTURE[old_pe_at + 24..old_pe_at + 24 + 112];
        let table_len = usize::from(original.number_of_sections) * SECTION_HEADER_SIZE;
        let table_at = section_table(FIXTURE);
        let mut table = FIXTURE[table_at..table_at + table_len].to_vec();
        let last = table_len - SECTION_HEADER_SIZE;
        table[last + 16..last + 20].copy_from_slice(&RAW_SIZE.to_le_bytes());
        table[last + 20..last + 24].copy_from_slice(&RAW_AT.to_le_bytes());
        let ranges: [(u64, &[u8]); 4] = [
            (0, &dos),
            (PE_AT, &coff),
            (PE_AT + 24, optional),
            (PE_AT + 24 + u64::from(OPTIONAL_SIZE), &table),
        ];

        for physical_len in [file_len, file_len - 1] {
            let mut expected = ranges.into_iter();
            let result = ParsedPe::parse_headers(physical_len, |offset, size| {
                let (expected_offset, data) = expected.next().expect("unexpected header read");
                assert_eq!((offset, size), (expected_offset, data.len()));
                Ok::<_, PeError>(std::borrow::Cow::Borrowed(data))
            });
            assert!(expected.next().is_none());
            if physical_len == file_len {
                let parsed = result.unwrap();
                let mut sections = original.sections.clone();
                sections.last_mut().unwrap().raw_data_ptr = RAW_AT;
                sections.last_mut().unwrap().raw_data_size = RAW_SIZE;
                assert_eq!(parsed.sections, sections);
                assert_eq!(parsed.entry_point_rva, original.entry_point_rva);
            } else {
                assert_eq!(result.unwrap_err(), PeError::InvalidSectionHeaders);
            }
        }
    }

    #[test]
    fn header_read_errors_keep_slice_parser_precedence() {
        let pe_at = read_u32(FIXTURE, 0x3c) as usize;
        let optional = pe_at + 24;
        let coff_reads = [(0, 64), (pe_at as u64, 24)];
        let magic_reads = [(0, 64), (pe_at as u64, 24), (optional as u64, 2)];
        let mut bytes = FIXTURE.to_vec();
        bytes[pe_at..pe_at + 4].fill(0);
        for (len, error, reads) in [
            (optional - 1, PeError::TooSmall, &coff_reads[..1]),
            (optional, PeError::InvalidPeSignature, &coff_reads[..]),
        ] {
            check_header_error(&bytes[..len], error, reads);
        }

        bytes[pe_at..pe_at + 4].copy_from_slice(b"PE\0\0");
        bytes[optional..optional + 2].copy_from_slice(&0x010bu16.to_le_bytes());
        check_header_error(&bytes[..optional + 1], PeError::TooSmall, &coff_reads);
        check_header_error(
            &bytes[..optional + 2],
            PeError::InvalidMagic(0x010b),
            &magic_reads,
        );
        // The magic check also precedes a too-short *declared* optional header.
        bytes[pe_at + 20..pe_at + 22].copy_from_slice(&1u16.to_le_bytes());
        check_header_error(&bytes, PeError::InvalidMagic(0x010b), &magic_reads);
        bytes[optional..optional + 2].copy_from_slice(&PE32_PLUS_MAGIC.to_le_bytes());
        check_header_error(&bytes, PeError::TooSmall, &magic_reads);
    }

    fn check_header_error(bytes: &[u8], error: PeError, reads: &[(u64, usize)]) {
        let mut expected = reads.iter().copied();
        let positional = ParsedPe::parse_headers(bytes.len() as u64, |offset, size| {
            assert_eq!(Some((offset, size)), expected.next());
            let start = offset as usize;
            Ok::<_, PeError>(std::borrow::Cow::Borrowed(&bytes[start..start + size]))
        });
        assert!(expected.next().is_none());
        assert_eq!(positional.unwrap_err(), error);
        assert_eq!(ParsedPe::parse(bytes).unwrap_err(), error);
    }

    #[test]
    fn failed_existing_memory_mapping_restores_layout_and_metadata() {
        const BASE: u64 = 0x1_6000_0000;
        const BLOCKED_BYTE: u8 = 0xEE;
        let pe = ParsedPe::parse(FIXTURE).unwrap();
        let first = pe
            .sections
            .iter()
            .find(|section| section.virtual_size.max(section.raw_data_size) != 0)
            .expect("fixture has a nonempty section");
        let blocker = BASE + u64::from(first.virtual_address);
        let mut memory = GuestMemory::new();
        memory
            .map_region_with_data(blocker, 1, MemoryPermissions::READ_WRITE, &[BLOCKED_BYTE])
            .unwrap();
        memory.main_image = Some(crate::memory::MainImage {
            base: 0x1234_0000,
            size: 0x5678,
            entry_point: 0x1234_1000,
        });
        memory.write_fault = Some(0x2222_3333);
        memory.unhandled_filter = 0x4444_5555;
        let usage = memory.usage();

        let error = pe
            .map_into_memory(FIXTURE, &mut memory, BASE)
            .expect_err("the blocker overlaps the first section");
        assert!(matches!(
            error,
            PeError::SectionMappingError(MemoryError::OverlappingRegion { .. })
        ));
        assert_eq!(memory.usage(), usage);
        assert_eq!(memory.read_u8(blocker), Ok(BLOCKED_BYTE));
        assert_eq!(
            memory.read_u8(BASE),
            Err(MemoryError::UnmappedAddress(BASE))
        );
        let main = memory.main_image.expect("main image metadata survives");
        assert_eq!(main.base, 0x1234_0000);
        assert_eq!(main.size, 0x5678);
        assert_eq!(main.entry_point, 0x1234_1000);
        assert_eq!(memory.write_fault, Some(0x2222_3333));
        assert_eq!(memory.unhandled_filter, 0x4444_5555);
    }

    #[test]
    fn unsorted_disjoint_sections_retain_table_order() {
        let original = ParsedPe::parse(FIXTURE).unwrap();
        let mut bytes = FIXTURE.to_vec();
        let first = section_table(&bytes);
        for offset in 0..SECTION_HEADER_SIZE {
            bytes.swap(first + offset, first + SECTION_HEADER_SIZE + offset);
        }

        let parsed = ParsedPe::parse(&bytes).unwrap();
        let mut expected = original.sections;
        expected.swap(0, 1);
        assert_eq!(parsed.sections, expected);
        assert_eq!(parsed.entry_point_rva, original.entry_point_rva);
    }

    #[test]
    fn entry_point_uses_the_half_open_mapped_section_range() {
        let mut bytes = FIXTURE.to_vec();
        let optional_header = read_u32(&bytes, 0x3c) as usize + 24;
        let entry_field = optional_header + 16..optional_header + 20;
        let text = section_table(&bytes);
        let contents_end = read_u32(&bytes, text + 12) + read_u32(&bytes, text + 16);

        // The mapped span includes the raw bytes beyond the declared virtual size.
        bytes[text + 8..text + 12].copy_from_slice(&1_u32.to_le_bytes());
        bytes[entry_field.clone()].copy_from_slice(&(contents_end - 1).to_le_bytes());
        assert!(ParsedPe::parse(&bytes).is_ok());

        // Start a non-executable section immediately after the executable one.
        let rdata = text + SECTION_HEADER_SIZE;
        bytes[rdata + 12..rdata + 16].copy_from_slice(&contents_end.to_le_bytes());
        bytes[entry_field.clone()].copy_from_slice(&contents_end.to_le_bytes());
        assert_eq!(
            ParsedPe::parse(&bytes).unwrap_err(),
            PeError::InvalidEntryPoint
        );

        // A zero-length executable section at the entry cannot admit it or
        // make the nonempty ranges overlap, even out of table order.
        let empty_rva = contents_end + 1;
        let empty = rdata + 2 * SECTION_HEADER_SIZE;
        bytes[empty + 8..empty + 12].copy_from_slice(&0_u32.to_le_bytes());
        bytes[empty + 12..empty + 16].copy_from_slice(&empty_rva.to_le_bytes());
        bytes[empty + 16..empty + 20].copy_from_slice(&0_u32.to_le_bytes());
        bytes[empty + 36..empty + 40].copy_from_slice(&IMAGE_SCN_MEM_EXECUTE.to_le_bytes());
        bytes[entry_field.clone()].copy_from_slice(&empty_rva.to_le_bytes());
        assert_eq!(
            ParsedPe::parse(&bytes).unwrap_err(),
            PeError::InvalidEntryPoint
        );

        bytes[entry_field].copy_from_slice(&(contents_end - 1).to_le_bytes());
        assert!(ParsedPe::parse(&bytes).is_ok());
    }

    #[test]
    fn overlapping_sections_are_rejected_regardless_of_table_order() {
        let mut bytes = FIXTURE.to_vec();
        let first = section_table(&bytes);
        let second = first + SECTION_HEADER_SIZE;
        // Extend .text's mapped range into .rdata without changing its raw data.
        bytes[first + 8..first + 12].copy_from_slice(&0x1200_u32.to_le_bytes());
        assert_eq!(
            ParsedPe::parse(&bytes).unwrap_err(),
            PeError::InvalidSectionHeaders
        );

        for offset in 0..SECTION_HEADER_SIZE {
            bytes.swap(first + offset, second + offset);
        }
        assert_eq!(
            ParsedPe::parse(&bytes).unwrap_err(),
            PeError::InvalidSectionHeaders
        );
    }
}
