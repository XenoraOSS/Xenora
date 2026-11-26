// SPDX-License-Identifier: GPL-3.0-or-later

//! The PE headers are mapped read-only at the image base (#333).
//!
//! Windows maps the first `SizeOfHeaders` bytes of an image at its base, and
//! programs read them: a C runtime walks its own section table before `main`.
//!
//! Every image here is original and built byte by byte in this file. Expected
//! values are the bytes the builder wrote, never values read back from a run.

use xenora_loader::cpu::{ExitReason, Register};
use xenora_loader::imports::ImportRegistry;
use xenora_loader::startup::{
    StartupEvent, StartupLayout, StartupResult, run_synthetic_startup,
    run_synthetic_startup_with_observer,
};
use xenora_loader::{MemoryError, ParsedPe, PeError};

const PREFERRED_BASE: u64 = 0x1_4000_0000;
const OTHER_BASE: u64 = 0x2_5000_0000;
/// Deliberately not the usual 0x40 or 0x80, so a read of it is distinctive.
const E_LFANEW: u32 = 0x90;
const OPTIONAL: usize = E_LFANEW as usize + 4 + 20;
const OPTIONAL_SIZE: usize = 240;
const SECTIONS: u16 = 2;
/// The first byte after the section table.
const TABLE_END: u32 = (OPTIONAL + OPTIONAL_SIZE) as u32 + 40 * SECTIONS as u32;

/// One image: `.text` holding `code` and an uninitialized `.data`.
struct Image {
    size_of_image: u32,
    size_of_headers: u32,
    text_rva: u32,
    data_rva: u32,
    data_size: u32,
    /// File offset of the code, and so the length of the file before it.
    text_offset: u32,
    code: Vec<u8>,
    /// Base relocation blocks, placed in `.text` 0x100 bytes after the code.
    relocations: Vec<u8>,
}

impl Image {
    fn new(code: &[u8]) -> Self {
        Self {
            size_of_image: 0x4000,
            size_of_headers: 0x400,
            text_rva: 0x2000,
            data_rva: 0x3000,
            data_size: 0x100,
            text_offset: 0x400,
            code: code.to_vec(),
            relocations: Vec::new(),
        }
    }

    fn build(&self) -> Vec<u8> {
        let mut pe = vec![0u8; self.text_offset as usize + 0x200];
        pe[0..2].copy_from_slice(b"MZ");
        pe[0x3C..0x40].copy_from_slice(&E_LFANEW.to_le_bytes());
        // A recognisable pattern where a DOS stub would be, and in the padding
        // after the section table.
        for (i, byte) in pe[0x40..E_LFANEW as usize].iter_mut().enumerate() {
            *byte = (i * 7 + 3) as u8;
        }
        // The pattern stops after two pages so that a very large header stays cheap.
        let padding_end = (self.text_offset as usize).min(0x2000);
        for (i, byte) in pe[TABLE_END as usize..padding_end].iter_mut().enumerate() {
            *byte = (i * 5 + 1) as u8;
        }
        let pe_at = E_LFANEW as usize;
        pe[pe_at..pe_at + 4].copy_from_slice(b"PE\0\0");
        let coff = pe_at + 4;
        pe[coff..coff + 2].copy_from_slice(&0x8664u16.to_le_bytes());
        pe[coff + 2..coff + 4].copy_from_slice(&SECTIONS.to_le_bytes());
        pe[coff + 16..coff + 18].copy_from_slice(&(OPTIONAL_SIZE as u16).to_le_bytes());
        pe[coff + 18..coff + 20].copy_from_slice(&0x0022u16.to_le_bytes());
        let opt = OPTIONAL;
        pe[opt..opt + 2].copy_from_slice(&0x020Bu16.to_le_bytes());
        pe[opt + 16..opt + 20].copy_from_slice(&self.text_rva.to_le_bytes());
        pe[opt + 24..opt + 32].copy_from_slice(&PREFERRED_BASE.to_le_bytes());
        pe[opt + 32..opt + 36].copy_from_slice(&0x1000u32.to_le_bytes());
        pe[opt + 36..opt + 40].copy_from_slice(&0x200u32.to_le_bytes());
        pe[opt + 56..opt + 60].copy_from_slice(&self.size_of_image.to_le_bytes());
        pe[opt + 60..opt + 64].copy_from_slice(&self.size_of_headers.to_le_bytes());
        pe[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());

        let mut section = |i: usize, name: &[u8], rva: u32, size: u32, raw: u32, flags: u32| {
            let h = opt + OPTIONAL_SIZE + i * 40;
            pe[h..h + name.len()].copy_from_slice(name);
            pe[h + 8..h + 12].copy_from_slice(&size.to_le_bytes());
            pe[h + 12..h + 16].copy_from_slice(&rva.to_le_bytes());
            pe[h + 16..h + 20]
                .copy_from_slice(&(if raw == 0 { 0 } else { 0x200u32 }).to_le_bytes());
            pe[h + 20..h + 24].copy_from_slice(&raw.to_le_bytes());
            pe[h + 36..h + 40].copy_from_slice(&flags.to_le_bytes());
        };
        section(
            0,
            b".text",
            self.text_rva,
            0x200,
            self.text_offset,
            0x6000_0020,
        );
        section(1, b".data", self.data_rva, self.data_size, 0, 0xC000_0080);

        let at = self.text_offset as usize;
        pe[at..at + self.code.len()].copy_from_slice(&self.code);
        if !self.relocations.is_empty() {
            // Data directory 5 is the base relocation table.
            let directory = OPTIONAL + 112 + 5 * 8;
            pe[directory..directory + 4].copy_from_slice(&(self.text_rva + 0x100).to_le_bytes());
            pe[directory + 4..directory + 8]
                .copy_from_slice(&(self.relocations.len() as u32).to_le_bytes());
            pe[at + 0x100..at + 0x100 + self.relocations.len()].copy_from_slice(&self.relocations);
        }
        pe
    }
}

/// `lea rbx, [rip - (text_rva + 7)]`: the image base, wherever it was loaded.
fn base_into_rbx(text_rva: u32) -> Vec<u8> {
    let mut code = vec![0x48, 0x8D, 0x1D];
    code.extend((-(text_rva as i32 + 7)).to_le_bytes());
    code
}

fn run(bytes: &[u8], base: u64) -> StartupResult {
    let layout = StartupLayout {
        image_base: base,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    run_synthetic_startup(bytes, layout, ImportRegistry::new(), 1_000).expect("startup")
}

fn load_error(image: &Image) -> Option<PeError> {
    let bytes = image.build();
    let pe = ParsedPe::parse(&bytes).expect("the parser does not judge SizeOfHeaders");
    pe.load_into_memory(&bytes).err()
}

#[test]
fn a_guest_reads_its_own_headers_at_the_preferred_and_at_a_relocated_base() {
    let mut code = base_into_rbx(0x2000);
    code.extend([
        0x0F, 0xB7, 0x03, // movzx eax, word [rbx]          DOS signature
        0x8B, 0x4B, 0x3C, // mov ecx, [rbx + 0x3C]          e_lfanew
        0x8B, 0x14, 0x0B, // mov edx, [rbx + rcx]           PE signature
        0x0F, 0xB7, 0x74, 0x0B, 0x06, // movzx esi, word [rbx + rcx + 6]  NumberOfSections
        0x48, 0x8B, 0x7C, 0x0B, 0x30, // mov rdi, [rbx + rcx + 0x30]      ImageBase
        0xF4, // hlt
    ]);
    let bytes = Image::new(&code).build();
    for base in [PREFERRED_BASE, OTHER_BASE] {
        let result = run(&bytes, base);
        let cpu = &result.execution.final_state;
        assert_eq!(result.execution.exit_reason, ExitReason::Halt, "{base:#x}");
        assert_eq!(cpu.get(Register::Rbx), base, "{base:#x}");
        assert_eq!(cpu.get(Register::Rax), 0x5A4D, "{base:#x}");
        assert_eq!(cpu.get(Register::Rcx), u64::from(E_LFANEW), "{base:#x}");
        assert_eq!(cpu.get(Register::Rdx), 0x0000_4550, "{base:#x}");
        assert_eq!(cpu.get(Register::Rsi), u64::from(SECTIONS), "{base:#x}");
        // The mapped header names the base the image is at, not the preferred one.
        assert_eq!(cpu.get(Register::Rdi), base, "{base:#x}");
    }
}

#[test]
fn the_mapping_is_the_file_headers_then_zeros_to_the_end_of_the_page() {
    let mut code = base_into_rbx(0x2000);
    code.push(0xF4);
    let bytes = Image::new(&code).build();
    for base in [PREFERRED_BASE, OTHER_BASE] {
        let result = run(&bytes, base);
        let memory = &result.image.memory;
        let mut page = [0u8; 0x1000];
        memory.read_bytes(base, &mut page).unwrap();

        let mut wanted = bytes[..0x400].to_vec();
        wanted[OPTIONAL + 24..OPTIONAL + 32].copy_from_slice(&base.to_le_bytes());
        assert_eq!(&page[..0x400], wanted.as_slice(), "{base:#x}");
        assert!(page[0x400..].iter().all(|&byte| byte == 0), "{base:#x}");

        // One page and no more: `.text` starts a page later.
        assert_eq!(
            memory.read_u8(base + 0x1000),
            Err(MemoryError::UnmappedAddress(base + 0x1000)),
            "{base:#x}"
        );
    }
}

#[test]
fn a_guest_write_to_the_headers_faults_and_changes_nothing() {
    let mut code = base_into_rbx(0x2000);
    code.extend([0xC6, 0x03, 0x00, 0xF4]); // mov byte [rbx], 0; hlt
    let result = run(&Image::new(&code).build(), OTHER_BASE);
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::MemoryFault(MemoryError::PermissionViolation {
            address: OTHER_BASE,
            required: "write",
        })
    );
    assert_eq!(result.image.memory.read_u8(OTHER_BASE), Ok(b'M'));
}

#[test]
fn the_headers_cannot_be_executed() {
    let mut code = base_into_rbx(0x2000);
    code.extend([0xFF, 0xE3]); // jmp rbx
    let result = run(&Image::new(&code).build(), OTHER_BASE);
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::MemoryFault(MemoryError::PermissionViolation {
            address: OTHER_BASE,
            required: "execute",
        })
    );
    assert_eq!(result.execution.final_state.rip, OTHER_BASE);
}

#[test]
fn headers_longer_than_a_page_take_whole_pages() {
    // 0x1001 bytes of headers need two pages, so `.text` must start at 0x2000.
    let mut image = Image::new(&[0xF4]);
    image.size_of_headers = 0x1001;
    image.text_offset = 0x1200;
    let bytes = image.build();
    let Ok(loaded) = ParsedPe::parse(&bytes).unwrap().load_into_memory(&bytes) else {
        panic!("two pages of headers below a section at 0x2000 must load");
    };
    let memory = &loaded.memory;
    assert_eq!(memory.read_u8(PREFERRED_BASE + 0x1000), Ok(bytes[0x1000]));
    assert_eq!(memory.read_u8(PREFERRED_BASE + 0x1001), Ok(0));
    assert_eq!(memory.read_u8(PREFERRED_BASE + 0x1FFF), Ok(0));
    assert_eq!(memory.read_u8(PREFERRED_BASE + 0x2000), Ok(0xF4));

    // With `.text` a page earlier the headers would reach into it.
    image.text_rva = 0x1000;
    assert_eq!(load_error(&image), Some(PeError::InvalidHeaderSize));

    // Exactly one page of headers ends where that section starts.
    image.size_of_headers = 0x1000;
    assert_eq!(load_error(&image), None);
}

#[test]
fn a_header_size_that_cannot_be_mapped_is_refused() {
    let mut image = Image::new(&[0xF4]);
    image.size_of_headers = 0;
    assert_eq!(load_error(&image), Some(PeError::InvalidHeaderSize));

    // One byte short of the section table, then exactly the section table.
    image.size_of_headers = TABLE_END - 1;
    assert_eq!(load_error(&image), Some(PeError::InvalidHeaderSize));
    image.size_of_headers = TABLE_END;
    assert_eq!(load_error(&image), None);

    // The file is 0x600 bytes: exactly that, then one byte more.
    image.size_of_headers = 0x600;
    assert_eq!(load_error(&image), None);
    image.size_of_headers = 0x601;
    assert_eq!(load_error(&image), Some(PeError::InvalidHeaderSize));
}

#[test]
fn an_empty_section_inside_the_header_page_is_not_an_overlap() {
    // A section that maps nothing has no range to collide with.
    let mut image = Image::new(&[0xF4]);
    image.data_rva = 0x200;
    image.data_size = 0;
    assert_eq!(load_error(&image), None);

    image.data_size = 1;
    assert_eq!(load_error(&image), Some(PeError::InvalidHeaderSize));
}

#[test]
fn declared_image_extent_bounds_the_virtual_section_end() {
    let mut image = Image::new(&[0xF4]);
    image.data_size = 0x1000;
    let bytes = image.build();
    let pe = ParsedPe::parse(&bytes).unwrap();
    let loaded = pe.load_into_memory(&bytes).unwrap();
    assert_eq!(loaded.memory.read_u8(PREFERRED_BASE + 0x3fff), Ok(0));
    assert_eq!(
        loaded.memory.read_u8(PREFERRED_BASE + 0x4000),
        Err(MemoryError::UnmappedAddress(PREFERRED_BASE + 0x4000))
    );

    image.data_size += 1;
    let bytes = image.build();
    let parsed = ParsedPe::parse(&bytes).expect("metadata remains inspectable");
    assert_eq!(parsed.size_of_image, 0x4000);
    let expected = PeError::InvalidImageSize {
        declared: 0x4000,
        required: 0x4001,
    };
    assert_eq!(
        parsed.load_into_memory(&bytes).err().as_ref(),
        Some(&expected)
    );

    let layout = StartupLayout {
        image_base: OTHER_BASE,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let mut events = Vec::new();
    let result = run_synthetic_startup_with_observer(
        &bytes,
        layout,
        ImportRegistry::new(),
        1_000,
        |event| events.push(event),
    );
    let error = result.err().expect("startup refused the image");
    assert_eq!(error.downcast_ref::<PeError>(), Some(&expected));
    assert!(matches!(&events[..], [StartupEvent::Parsed { .. }]));
}

#[test]
fn declared_image_extent_counts_raw_bytes_beyond_virtual_size() {
    let mut image = Image::new(&[0xF4]);
    image.size_of_image = 0x21ff;
    // Put an empty section beyond both candidate ends. It contributes nothing.
    image.data_size = 0;
    let mut bytes = image.build();
    let text = OPTIONAL + OPTIONAL_SIZE;
    bytes[text + 8..text + 12].copy_from_slice(&1u32.to_le_bytes());
    let parsed = ParsedPe::parse(&bytes).unwrap();
    assert_eq!(
        parsed.load_into_memory(&bytes).err(),
        Some(PeError::InvalidImageSize {
            declared: 0x21ff,
            required: 0x2200,
        })
    );

    bytes[OPTIONAL + 56..OPTIONAL + 60].copy_from_slice(&0x2200u32.to_le_bytes());
    let parsed = ParsedPe::parse(&bytes).unwrap();
    let loaded = parsed.load_into_memory(&bytes).unwrap();
    assert_eq!(loaded.memory.read_u8(PREFERRED_BASE + 0x21ff), Ok(0));
}

#[test]
fn declared_image_extent_counts_header_page_rounding() {
    let mut image = Image::new(&[0xF4]);
    image.size_of_image = 0x0fff;
    let bytes = image.build();
    let mut parsed = ParsedPe::parse(&bytes).unwrap();
    // Isolate the header mapping on the public mapping path.
    parsed.sections.clear();
    assert_eq!(
        parsed.load_into_memory(&bytes).err(),
        Some(PeError::InvalidImageSize {
            declared: 0x0fff,
            required: 0x1000,
        })
    );
}

#[test]
fn declared_image_extent_allows_gaps_larger_than_the_allocation_budget() {
    let mut image = Image::new(&[0xF4]);
    image.data_rva = 0x1000_2000;
    image.size_of_image = 0x1000_3000;
    let bytes = image.build();
    let parsed = ParsedPe::parse(&bytes).unwrap();
    let loaded = parsed.load_into_memory(&bytes).unwrap();
    let data = PREFERRED_BASE + u64::from(image.data_rva);
    assert_eq!(loaded.memory.read_u8(data), Ok(0));
    assert_eq!(
        loaded.memory.read_u8(data - 1),
        Err(MemoryError::UnmappedAddress(data - 1))
    );
}

#[test]
fn declared_image_extent_must_match_the_bytes_being_mapped() {
    let mut bytes = Image::new(&[0xF4]).build();
    let parsed = ParsedPe::parse(&bytes).unwrap();
    let mut altered = parsed.clone();
    altered.size_of_image = 0x5000;
    assert_eq!(
        altered.load_into_memory(&bytes).err(),
        Some(PeError::InvalidHeaderSize)
    );

    bytes[OPTIONAL + 56..OPTIONAL + 60].copy_from_slice(&0x5000u32.to_le_bytes());
    assert_eq!(
        parsed.load_into_memory(&bytes).err(),
        Some(PeError::InvalidHeaderSize)
    );
    assert!(altered.load_into_memory(&bytes).is_ok());
}

#[test]
fn loading_with_bytes_shorter_than_the_headers_is_an_error_not_a_panic() {
    let bytes = Image::new(&[0xF4]).build();
    let pe = ParsedPe::parse(&bytes).unwrap();
    for len in [0, 0x3F, OPTIONAL, TABLE_END as usize, 0x3FF] {
        assert_eq!(
            pe.load_into_memory(&bytes[..len]).err(),
            Some(PeError::InvalidHeaderSize),
            "{len:#x}"
        );
    }

    // Other bytes of the right length, whose own headers leave no room for the
    // `ImageBase` field inside the size that was parsed.
    let mut image = Image::new(&[0xF4]);
    image.size_of_headers = OPTIONAL as u32 + 8;
    let bytes = image.build();
    let pe = ParsedPe::parse(&bytes).unwrap();
    let mut other = bytes.clone();
    let coff = E_LFANEW as usize + 4;
    other[coff + 2..coff + 4].copy_from_slice(&0u16.to_le_bytes());
    other[coff + 16..coff + 18].copy_from_slice(&0u16.to_le_bytes());
    assert_eq!(
        pe.load_into_memory(&other).err(),
        Some(PeError::InvalidHeaderSize)
    );
}

#[test]
fn startup_reports_the_refusal() {
    let mut image = Image::new(&[0xF4]);
    image.size_of_headers = 0;
    let layout = StartupLayout {
        image_base: OTHER_BASE,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let error = run_synthetic_startup(&image.build(), layout, ImportRegistry::new(), 1_000)
        .err()
        .expect("refused");
    assert_eq!(error.to_string(), PeError::InvalidHeaderSize.to_string());
    assert_eq!(
        error.to_string(),
        "the PE headers cannot be mapped: SizeOfHeaders is zero, shorter than the section table, \
         longer than the file or reaches into a section, or the bytes do not match the parsed \
         headers"
    );
}

/// One base relocation block: `IMAGE_REL_BASED_DIR64` entries for the given
/// offsets in the page at `page_rva`, padded to a multiple of four bytes.
fn dir64_block(page_rva: u32, offsets: &[u16]) -> Vec<u8> {
    let mut entries: Vec<u16> = offsets.iter().map(|offset| 0xA000 | offset).collect();
    if entries.len() % 2 == 1 {
        entries.push(0); // IMAGE_REL_BASED_ABSOLUTE padding
    }
    let mut block = page_rva.to_le_bytes().to_vec();
    block.extend((8 + 2 * entries.len() as u32).to_le_bytes());
    for entry in entries {
        block.extend(entry.to_le_bytes());
    }
    block
}

#[test]
fn a_relocation_aimed_at_image_base_does_not_move_it_twice() {
    // The image carries two DIR64 entries: one for the `ImageBase` field in the
    // headers, and one for a pointer in `.text` so the table is seen to apply.
    let mut code = base_into_rbx(0x2000);
    code.extend([
        0x8B, 0x4B, 0x3C, // mov ecx, [rbx + 0x3C]
        0x48, 0x8B, 0x7C, 0x0B, 0x30, // mov rdi, [rbx + rcx + 0x30]   ImageBase
        // mov rax, [rip + 0x2A]: the pointer at 0x2040
        0x48, 0x8B, 0x05, 0x2A, 0x00, 0x00, 0x00, 0xF4, // hlt
    ]);
    // The load ends 0x16 bytes into the code, and 0x16 + 0x2A is 0x40.
    assert_eq!(code.len(), 0x17);
    code.resize(0x40, 0x90);
    code.extend((PREFERRED_BASE + 0x2040).to_le_bytes());
    let mut image = Image::new(&code);
    image.relocations = dir64_block(0, &[OPTIONAL as u16 + 24]);
    image.relocations.extend(dir64_block(0x2000, &[0x40]));
    let bytes = image.build();

    for base in [PREFERRED_BASE, OTHER_BASE] {
        let result = run(&bytes, base);
        assert_eq!(result.execution.exit_reason, ExitReason::Halt, "{base:#x}");
        // Nothing is applied when the image is already at its preferred base.
        let applied = if base == PREFERRED_BASE { 0 } else { 2 };
        assert_eq!(result.relocations.applied_dir64, applied, "{base:#x}");
        let cpu = &result.execution.final_state;
        assert_eq!(cpu.get(Register::Rax), base + 0x2040, "{base:#x}");
        // Not the preferred base, and not the load base plus the delta again.
        assert_eq!(cpu.get(Register::Rdi), base, "{base:#x}");
    }
}

#[test]
fn bytes_that_are_not_the_parsed_headers_are_refused() {
    let bytes = Image::new(&[0xF4]).build();
    let pe = ParsedPe::parse(&bytes).unwrap();
    let coff = E_LFANEW as usize + 4;
    let refused = |change: &dyn Fn(&mut Vec<u8>)| {
        let mut other = bytes.clone();
        change(&mut other);
        pe.load_into_memory(&other).err()
    };
    assert_eq!(refused(&|_| {}), None);

    // An optional header declared empty: the section table would start where
    // the optional header is, and `ImageBase` would be patched inside it.
    assert_eq!(
        refused(&|b| b[coff + 16..coff + 18].copy_from_slice(&0u16.to_le_bytes())),
        Some(PeError::InvalidHeaderSize)
    );
    // One byte short of the PE32+ fields the loader relies on.
    assert_eq!(
        refused(&|b| b[coff + 16..coff + 18].copy_from_slice(&111u16.to_le_bytes())),
        Some(PeError::InvalidHeaderSize)
    );
    // Not a PE32+ optional header.
    assert_eq!(
        refused(&|b| b[OPTIONAL..OPTIONAL + 2].copy_from_slice(&0x010Bu16.to_le_bytes())),
        Some(PeError::InvalidHeaderSize)
    );
    // A different section count, preferred base or header size.
    assert_eq!(
        refused(&|b| b[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes())),
        Some(PeError::InvalidHeaderSize)
    );
    assert_eq!(
        refused(&|b| b[OPTIONAL + 24] ^= 1),
        Some(PeError::InvalidHeaderSize)
    );
    assert_eq!(
        refused(&|b| b[OPTIONAL + 60..OPTIONAL + 64].copy_from_slice(&0x200u32.to_le_bytes())),
        Some(PeError::InvalidHeaderSize)
    );
}

#[test]
fn headers_over_the_mapping_budget_are_refused() {
    // 256 MiB of headers and one more page. The file really is that long, so
    // only the budget stands between this image and a mapping of that size.
    const BUDGET: u32 = 256 * 1024 * 1024;
    let mut image = Image::new(&[0xF4]);
    image.size_of_image = BUDGET + 0x3000;
    image.size_of_headers = BUDGET + 0x1000;
    image.text_offset = BUDGET + 0x1000;
    image.text_rva = BUDGET + 0x1000;
    image.data_rva = BUDGET + 0x2000;
    assert_eq!(
        load_error(&image),
        Some(PeError::SectionMappingError(MemoryError::AllocationFailed))
    );
}
