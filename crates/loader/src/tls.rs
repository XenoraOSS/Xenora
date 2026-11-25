// SPDX-License-Identifier: GPL-3.0-or-later

//! Thread Local Storage (TLS) and Process Setup for Xenora X15.
//!
//! Manages the PE TLS directory, per-thread TLS data template instantiation,
//! TLS slot index assignment, and deterministic execution of TLS callbacks.

use crate::cpu::{CpuState, ExitReason, Register};
use crate::dbt::DbtEngine;
use crate::memory::{GuestMemory, MemoryError, MemoryPermissions};
use crate::pe::ParsedPe;
use std::fmt;

/// Most callbacks one TLS directory may list.
pub const MAX_TLS_CALLBACKS: usize = 64;

/// Bytes below the stack top that a callback is entered with: the return
/// address and the 32-byte home space the Win64 convention gives every callee.
const CALLBACK_FRAME: u64 = 40;

/// Errors arising during TLS setup or callback invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TlsError {
    DirectoryOutOfBounds {
        rva: u32,
    },
    InvalidTemplateRange {
        start: u64,
        end: u64,
    },
    /// `AddressOfCallbacks` is not zero and not inside a section of the image.
    CallbackArrayOutOfBounds {
        address: u64,
    },
    /// The callback array leaves its section before a null entry.
    CallbackArrayUnterminated {
        address: u64,
    },
    /// The callback array lists more than [`MAX_TLS_CALLBACKS`] entries.
    TooManyCallbacks {
        limit: usize,
    },
    /// The stack cannot hold a callback's entry frame or is not 16-byte aligned.
    InvalidCallbackStack {
        stack_top: u64,
    },
    CallbackExecutionFault(ExitReason),
    Memory(MemoryError),
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DirectoryOutOfBounds { rva } => {
                write!(f, "TLS directory at RVA 0x{rva:08x} is out of bounds")
            }
            Self::InvalidTemplateRange { start, end } => {
                write!(
                    f,
                    "invalid TLS template range: 0x{start:016x}..0x{end:016x}"
                )
            }
            Self::CallbackArrayOutOfBounds { address } => {
                write!(
                    f,
                    "TLS callback array at 0x{address:016x} is outside the image"
                )
            }
            Self::CallbackArrayUnterminated { address } => {
                write!(
                    f,
                    "TLS callback array at 0x{address:016x} has no null entry in its section"
                )
            }
            Self::TooManyCallbacks { limit } => {
                write!(f, "TLS callback array lists more than {limit} callbacks")
            }
            Self::InvalidCallbackStack { stack_top } => {
                write!(
                    f,
                    "stack top 0x{stack_top:016x} cannot hold an aligned TLS callback frame"
                )
            }
            Self::CallbackExecutionFault(e) => {
                write!(f, "fault during TLS callback execution: {e}")
            }
            Self::Memory(e) => write!(f, "guest memory fault during TLS setup: {e}"),
        }
    }
}

impl std::error::Error for TlsError {}

impl From<MemoryError> for TlsError {
    fn from(e: MemoryError) -> Self {
        Self::Memory(e)
    }
}

/// Parsed PE TLS directory metadata.
#[derive(Clone, Debug, Default)]
pub struct TlsDirectory {
    pub raw_data_start_va: u64,
    pub raw_data_end_va: u64,
    pub index_address: u64,
    pub callbacks_address: u64,
    pub size_of_zero_fill: u32,
    pub characteristics: u32,
    pub callbacks: Vec<u64>,
}

/// Parse the TLS directory from raw PE file bytes.
pub fn parse_tls_directory(bytes: &[u8], pe: &ParsedPe) -> Result<Option<TlsDirectory>, TlsError> {
    if bytes.len() < 0x40 {
        return Ok(None);
    }
    let e_lfanew = u32::from_le_bytes(bytes[0x3C..0x40].try_into().unwrap()) as usize;
    let opt_offset = e_lfanew + 24;
    let coff_offset = e_lfanew + 4;
    if bytes.len() < coff_offset + 18 {
        return Ok(None);
    }
    let size_of_opt = u16::from_le_bytes(
        bytes[coff_offset + 16..coff_offset + 18]
            .try_into()
            .unwrap(),
    ) as usize;

    // TLS directory is directory index 9 (offset 112 + 9 * 8 = 184 in Optional Header)
    if size_of_opt < 192 || bytes.len() < opt_offset + 192 {
        return Ok(None);
    }

    let tls_rva = u32::from_le_bytes(
        bytes[opt_offset + 184..opt_offset + 188]
            .try_into()
            .unwrap(),
    );
    let tls_size = u32::from_le_bytes(
        bytes[opt_offset + 188..opt_offset + 192]
            .try_into()
            .unwrap(),
    );

    if tls_rva == 0 || tls_size == 0 {
        return Ok(None);
    }

    let tls_off = pe
        .rva_to_file_offset(tls_rva)
        .ok_or(TlsError::DirectoryOutOfBounds { rva: tls_rva })?;

    if tls_off + 40 > bytes.len() {
        return Err(TlsError::DirectoryOutOfBounds { rva: tls_rva });
    }

    let raw_data_start_va = u64::from_le_bytes(bytes[tls_off..tls_off + 8].try_into().unwrap());
    let raw_data_end_va = u64::from_le_bytes(bytes[tls_off + 8..tls_off + 16].try_into().unwrap());
    let index_address = u64::from_le_bytes(bytes[tls_off + 16..tls_off + 24].try_into().unwrap());
    let callbacks_address =
        u64::from_le_bytes(bytes[tls_off + 24..tls_off + 32].try_into().unwrap());
    let size_of_zero_fill =
        u32::from_le_bytes(bytes[tls_off + 32..tls_off + 36].try_into().unwrap());
    let characteristics = u32::from_le_bytes(bytes[tls_off + 36..tls_off + 40].try_into().unwrap());

    let callbacks = read_callbacks(bytes, pe, callbacks_address)?;

    Ok(Some(TlsDirectory {
        raw_data_start_va,
        raw_data_end_va,
        index_address,
        callbacks_address,
        size_of_zero_fill,
        characteristics,
        callbacks,
    }))
}

/// Read the null-terminated callback array that `address` names.
///
/// A null `address` means the image has no callbacks. Any other value must be a
/// virtual address inside one section, and the array must reach its null entry
/// inside that section. Bytes past the section's file data, up to its virtual
/// size, are zero in the loaded image and are read as zero here.
fn read_callbacks(bytes: &[u8], pe: &ParsedPe, address: u64) -> Result<Vec<u64>, TlsError> {
    if address == 0 {
        return Ok(Vec::new());
    }
    let (section, start) = address
        .checked_sub(pe.image_base)
        .and_then(|rva| u32::try_from(rva).ok())
        .and_then(|rva| {
            pe.sections.iter().find_map(|section| {
                let offset = rva.checked_sub(section.virtual_address)?;
                let extent = section.virtual_size.max(section.raw_data_size);
                (offset < extent).then_some((section, u64::from(offset)))
            })
        })
        .ok_or(TlsError::CallbackArrayOutOfBounds { address })?;
    let extent = u64::from(section.virtual_size.max(section.raw_data_size));
    let byte_at = |offset: u64| -> Option<u8> {
        if offset >= extent {
            None
        } else if offset >= u64::from(section.raw_data_size) {
            Some(0)
        } else {
            let file = u64::from(section.raw_data_ptr) + offset;
            bytes.get(usize::try_from(file).ok()?).copied()
        }
    };

    let mut callbacks = Vec::new();
    let mut offset = start;
    loop {
        let mut entry = [0u8; 8];
        for byte in &mut entry {
            *byte = byte_at(offset).ok_or(TlsError::CallbackArrayUnterminated { address })?;
            offset += 1;
        }
        let target = u64::from_le_bytes(entry);
        if target == 0 {
            return Ok(callbacks);
        }
        if callbacks.len() == MAX_TLS_CALLBACKS {
            return Err(TlsError::TooManyCallbacks {
                limit: MAX_TLS_CALLBACKS,
            });
        }
        callbacks.push(target);
    }
}

/// Bytes reserved at `thread_tls_base` for the thread's TLS pointer array.
const TLS_ARRAY_SIZE: usize = 0x1000;

/// Bytes read per step when a template is checked and then copied into its
/// block.
const TEMPLATE_COPY_CHUNK: usize = 4096;

/// Smallest template copied into the zeroed block instead of a staging buffer.
///
/// Release timings of original synthetic templates on an Apple M2 Pro put the
/// point where a second buffer dominates at 64 KiB. Below that, one read into
/// a staging buffer is as fast or faster.
const DIRECT_TEMPLATE_MIN: usize = 64 * 1024;

/// Templates at least this big keep the single staged read. On the same
/// machine, proving the bytes readable and then copying them was no faster
/// than the staging buffer once the template reached 1 MiB.
const DIRECT_TEMPLATE_LIMIT: usize = 1024 * 1024;

/// Set up TLS runtime state in guest memory:
/// 1. Assign TLS index 0 at `index_address`.
/// 2. Map the thread's TLS pointer array at `thread_tls_base`.
/// 3. Map the thread's data block one page above it, initialized from the
///    template and zero fill, and store its address in the array slot of the
///    index.
///
/// `thread_tls_base` is the value the thread environment block holds as its
/// TLS pointer. Compiled code loads that pointer, indexes it by the module's
/// TLS index to find the data block, and then adds a variable's offset, so the
/// array and the data are separate regions.
///
/// The block is the template followed by `SizeOfZeroFill` zero bytes. A
/// template of at least 64 KiB and below 1 MiB is proved readable and then
/// copied into a zeroed block. Every other size is read once into a staging
/// buffer that initializes the block. Either way, a template that cannot be
/// read leaves the pointer array unmapped.
pub fn setup_tls(
    tls: &TlsDirectory,
    loaded_base: u64,
    preferred_base: u64,
    memory: &mut GuestMemory,
    thread_tls_base: u64,
) -> Result<(), TlsError> {
    let delta = (loaded_base as i128) - (preferred_base as i128);
    let rebase = |va: u64| -> u64 {
        if va == 0 {
            0
        } else {
            ((va as i128) + delta) as u64
        }
    };

    let actual_index_addr = rebase(tls.index_address);
    // Set TLS index = 0 in guest memory
    memory.write_u32_bypass(actual_index_addr, 0)?;

    let actual_start = rebase(tls.raw_data_start_va);
    let actual_end = rebase(tls.raw_data_end_va);

    if actual_end < actual_start {
        return Err(TlsError::InvalidTemplateRange {
            start: actual_start,
            end: actual_end,
        });
    }

    let template_size = (actual_end - actual_start) as usize;
    let total_size = template_size + (tls.size_of_zero_fill as usize);

    if total_size > 0 {
        let direct = (DIRECT_TEMPLATE_MIN..DIRECT_TEMPLATE_LIMIT).contains(&template_size);
        // The read happens before anything is mapped, so a missing or
        // unreadable template does not leave a pointer array behind.
        let staged = if direct {
            prove_template_readable(memory, actual_start, template_size)?;
            None
        } else {
            let mut staged = vec![0u8; template_size];
            if template_size > 0 {
                memory.read_bytes(actual_start, &mut staged)?;
            }
            Some(staged)
        };

        // Allocate the pointer array and the per-thread TLS block.
        let block_base = thread_tls_base
            .checked_add(TLS_ARRAY_SIZE as u64)
            .ok_or(MemoryError::Overflow)?;
        let page_size = 4096;
        let map_size = (total_size + page_size - 1) & !(page_size - 1);
        memory.map_region(
            thread_tls_base,
            TLS_ARRAY_SIZE,
            MemoryPermissions::READ_WRITE,
        )?;
        if let Some(staged) = staged.as_deref() {
            memory.map_region_with_data(
                block_base,
                map_size,
                MemoryPermissions::READ_WRITE,
                staged,
            )?;
        } else {
            memory.map_region(block_base, map_size, MemoryPermissions::READ_WRITE)?;
            copy_template(memory, actual_start, block_base, template_size)?;
        }
        // Slot 0 belongs to the index assigned above.
        memory.write_u64(thread_tls_base, block_base)?;
    }

    Ok(())
}

/// Read every template byte, discarding the contents.
///
/// Failure here means the template is not available. Callers map the block
/// only after this returns.
fn prove_template_readable(memory: &GuestMemory, addr: u64, len: usize) -> Result<(), TlsError> {
    let mut chunk = [0u8; TEMPLATE_COPY_CHUNK];
    let mut offset = 0usize;
    while offset < len {
        let n = (len - offset).min(TEMPLATE_COPY_CHUNK);
        let at = addr
            .checked_add(offset as u64)
            .ok_or(MemoryError::Overflow)?;
        memory.read_bytes(at, &mut chunk[..n])?;
        offset += n;
    }
    Ok(())
}

/// Copy `len` bytes from `src` to `dst`. `dst` is already zeroed, which is the
/// template's `SizeOfZeroFill` and the rounded tail of the block.
fn copy_template(memory: &mut GuestMemory, src: u64, dst: u64, len: usize) -> Result<(), TlsError> {
    let mut chunk = [0u8; TEMPLATE_COPY_CHUNK];
    let mut offset = 0usize;
    while offset < len {
        let n = (len - offset).min(TEMPLATE_COPY_CHUNK);
        let from = src
            .checked_add(offset as u64)
            .ok_or(MemoryError::Overflow)?;
        let to = dst
            .checked_add(offset as u64)
            .ok_or(MemoryError::Overflow)?;
        memory.read_bytes(from, &mut chunk[..n])?;
        memory.write_bytes(to, &chunk[..n])?;
        offset += n;
    }
    Ok(())
}

/// Execute all registered TLS callbacks in sequence.
/// Signature: void (NTAPI *PIMAGE_TLS_CALLBACK)(PVOID DllHandle, DWORD Reason, PVOID Reserved);
/// - RCX = DllHandle (loaded_base)
/// - RDX = Reason (DLL_PROCESS_ATTACH = 1)
/// - R8  = Reserved (0)
///
/// Each callback is entered as after a CALL from a 16-byte aligned stack: RSP
/// is `stack_top - 40`, holding the return address below 32 bytes of home
/// space. A `stack_top` that is not 16-byte aligned or cannot hold that frame
/// is rejected before any callback runs.
pub fn execute_tls_callbacks(
    callbacks: &[u64],
    loaded_base: u64,
    preferred_base: u64,
    stack_top: u64,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
    dbt: &mut DbtEngine,
) -> Result<usize, TlsError> {
    if callbacks.is_empty() {
        return Ok(0);
    }
    if stack_top < CALLBACK_FRAME || !stack_top.is_multiple_of(16) {
        return Err(TlsError::InvalidCallbackStack { stack_top });
    }
    let delta = (loaded_base as i128) - (preferred_base as i128);
    let mut executed_count = 0;

    for &cb in callbacks {
        let actual_cb_va = ((cb as i128) + delta) as u64;

        // Reset CPU state for callback execution adhering to Win64 ABI
        cpu.rip = actual_cb_va;
        // Stack pointer aligned with 32-byte shadow space + return address
        let rsp = stack_top - CALLBACK_FRAME;
        cpu.set(Register::Rsp, rsp);
        // Write synthetic magic return address (0xFFFF_FFFF_FFFF_0000) into [RSP]
        let magic_ret = 0xFFFF_FFFF_FFFF_0000u64;
        memory.write_u64(rsp, magic_ret)?;

        // Set Win64 arguments:
        // RCX = DllHandle (image base)
        cpu.set(Register::Rcx, loaded_base);
        // RDX = DLL_PROCESS_ATTACH (1)
        cpu.set(Register::Rdx, 1);
        // R8 = 0
        cpu.set(Register::R8, 0);

        // RET lands at the sentinel, which has no guest code mapped.
        let mut steps = 0;
        while cpu.rip != magic_ret {
            let res = dbt.run(cpu, memory, 100);
            if cpu.rip == magic_ret {
                break;
            }
            if res.exit_reason != ExitReason::StepLimitExceeded {
                return Err(TlsError::CallbackExecutionFault(res.exit_reason));
            }
            steps += res.instructions_executed;
            if steps >= 100_000 || res.instructions_executed == 0 {
                return Err(TlsError::CallbackExecutionFault(
                    ExitReason::StepLimitExceeded,
                ));
            }
        }

        executed_count += 1;
    }

    Ok(executed_count)
}

#[cfg(test)]
mod tests {
    //! Expectations come from Microsoft's public PE Format page (the TLS
    //! directory: `AddressOfCallbacks` is a virtual address of a null-terminated
    //! array, and a callback is `void cb(PVOID DllHandle, DWORD Reason, PVOID
    //! Reserved)`) and its x64 calling convention page (the stack is 16-byte
    //! aligned before a call, the caller provides 32 bytes of home space, and
    //! the first three integer arguments are in RCX, RDX and R8). The headers
    //! and code bytes below are original. Nothing here ran on Windows.

    use super::*;
    use crate::pe::PeSection;
    use std::hint::black_box;
    use std::time::Instant;

    const IMAGE_BASE: u64 = 0x1_4000_0000;
    const SECTION_RVA: u32 = 0x1000;
    const SECTION_FILE: usize = 0x400;
    const SECTION_SIZE: u32 = 0x400;
    /// Offset of the optional header: `e_lfanew` (0x80), signature, COFF header.
    const OPTIONAL_HEADER: usize = 0x80 + 24;

    fn parsed(virtual_size: u32) -> ParsedPe {
        ParsedPe {
            machine: 0x8664,
            number_of_sections: 1,
            time_date_stamp: 0,
            image_base: IMAGE_BASE,
            section_alignment: 0x1000,
            file_alignment: 0x200,
            size_of_image: 0x3000,
            size_of_headers: 0x400,
            entry_point_rva: 0,
            sections: vec![PeSection {
                name: ".rdata".into(),
                virtual_address: SECTION_RVA,
                virtual_size,
                raw_data_ptr: SECTION_FILE as u32,
                raw_data_size: SECTION_SIZE,
                characteristics: crate::pe::IMAGE_SCN_MEM_READ,
                permissions: MemoryPermissions::READ_ONLY,
            }],
        }
    }

    /// A file with one section holding a TLS directory at its start and the
    /// callback `entries` at `array_rva`. The directory names `callbacks_address`.
    /// Every byte after the section's file data is 0xCC.
    fn file(callbacks_address: u64, array_rva: u32, entries: &[u64]) -> Vec<u8> {
        let section_end = SECTION_FILE + SECTION_SIZE as usize;
        let mut bytes = vec![0u8; section_end];
        bytes.resize(section_end + 0x200, 0xCC);
        bytes[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        // SizeOfOptionalHeader: a PE32+ optional header with 16 data directories.
        bytes[0x94..0x96].copy_from_slice(&240u16.to_le_bytes());
        // Data directory 9 (TLS): RVA and size.
        let entry = OPTIONAL_HEADER + 112 + 9 * 8;
        bytes[entry..entry + 4].copy_from_slice(&SECTION_RVA.to_le_bytes());
        bytes[entry + 4..entry + 8].copy_from_slice(&40u32.to_le_bytes());
        // IMAGE_TLS_DIRECTORY64.AddressOfCallBacks is its fourth 8-byte field.
        bytes[SECTION_FILE + 24..SECTION_FILE + 32]
            .copy_from_slice(&callbacks_address.to_le_bytes());
        let mut at = SECTION_FILE + (array_rva - SECTION_RVA) as usize;
        for entry in entries {
            bytes[at..at + 8].copy_from_slice(&entry.to_le_bytes());
            at += 8;
        }
        bytes
    }

    fn callbacks_of(bytes: &[u8], pe: &ParsedPe) -> Result<Vec<u64>, TlsError> {
        parse_tls_directory(bytes, pe).map(|tls| tls.expect("a TLS directory").callbacks)
    }

    const ARRAY_RVA: u32 = 0x1100;
    const ARRAY: u64 = IMAGE_BASE + ARRAY_RVA as u64;
    const FIRST: u64 = IMAGE_BASE + 0x2000;
    const SECOND: u64 = IMAGE_BASE + 0x2010;

    #[test]
    fn callbacks_are_read_in_order_up_to_the_null_entry() {
        let bytes = file(ARRAY, ARRAY_RVA, &[FIRST, SECOND, 0, IMAGE_BASE + 0x2020]);
        assert_eq!(
            callbacks_of(&bytes, &parsed(SECTION_SIZE)),
            Ok(vec![FIRST, SECOND])
        );
    }

    #[test]
    fn a_null_callbacks_address_means_no_callbacks() {
        let bytes = file(0, ARRAY_RVA, &[FIRST, 0]);
        assert_eq!(callbacks_of(&bytes, &parsed(SECTION_SIZE)), Ok(vec![]));
    }

    #[test]
    fn a_callback_array_outside_the_image_is_rejected() {
        for address in [8, IMAGE_BASE - 8, IMAGE_BASE + 0x800, IMAGE_BASE + 0x5000] {
            let bytes = file(address, ARRAY_RVA, &[FIRST, 0]);
            assert_eq!(
                callbacks_of(&bytes, &parsed(SECTION_SIZE)),
                Err(TlsError::CallbackArrayOutOfBounds { address }),
                "{address:#x}"
            );
        }
    }

    #[test]
    fn a_callback_array_address_is_not_truncated_to_32_bits() {
        // The low 32 bits of the offset name the real array.
        let address = ARRAY + (1 << 32);
        let bytes = file(address, ARRAY_RVA, &[FIRST, 0]);
        assert_eq!(
            callbacks_of(&bytes, &parsed(SECTION_SIZE)),
            Err(TlsError::CallbackArrayOutOfBounds { address })
        );
    }

    #[test]
    fn a_callback_array_must_end_inside_its_section() {
        // Two entries fill the last 16 bytes of the section; the file goes on.
        let rva = SECTION_RVA + SECTION_SIZE - 16;
        let address = IMAGE_BASE + u64::from(rva);
        let bytes = file(address, rva, &[FIRST, SECOND]);
        assert_eq!(
            callbacks_of(&bytes, &parsed(SECTION_SIZE)),
            Err(TlsError::CallbackArrayUnterminated { address })
        );
    }

    #[test]
    fn zero_fill_after_the_section_file_data_ends_the_array() {
        // The section is larger in memory than in the file, so the bytes after
        // its file data are zero when loaded, whatever follows in the file.
        let rva = SECTION_RVA + SECTION_SIZE - 16;
        let address = IMAGE_BASE + u64::from(rva);
        let bytes = file(address, rva, &[FIRST, SECOND]);
        assert_eq!(
            callbacks_of(&bytes, &parsed(SECTION_SIZE + 0x100)),
            Ok(vec![FIRST, SECOND])
        );
    }

    #[test]
    fn a_callback_array_over_the_limit_is_rejected() {
        let entries: Vec<u64> = (0..65).map(|index| FIRST + index * 16).collect();
        let mut at_limit = entries[..MAX_TLS_CALLBACKS].to_vec();
        at_limit.push(0);
        let bytes = file(ARRAY, ARRAY_RVA, &at_limit);
        assert_eq!(
            callbacks_of(&bytes, &parsed(SECTION_SIZE)),
            Ok(entries[..64].to_vec())
        );

        let mut over = entries.clone();
        over.push(0);
        let bytes = file(ARRAY, ARRAY_RVA, &over);
        assert_eq!(
            callbacks_of(&bytes, &parsed(SECTION_SIZE)),
            Err(TlsError::TooManyCallbacks { limit: 64 })
        );
    }

    #[test]
    fn a_file_cut_inside_the_tls_data_directory_has_no_tls_directory() {
        let bytes = file(ARRAY, ARRAY_RVA, &[FIRST, 0]);
        let entry = OPTIONAL_HEADER + 112 + 9 * 8;
        for length in entry..entry + 8 {
            let directory = parse_tls_directory(&bytes[..length], &parsed(SECTION_SIZE));
            assert!(matches!(directory, Ok(None)), "length {length:#x}");
        }
    }

    const CODE: u64 = 0x40_0000;
    const STACK_TOP: u64 = 0x80_0000;
    const SENTINEL: u64 = 0xFFFF_FFFF_FFFF_0000;

    /// Code at `CODE`, a 4 KiB stack below `STACK_TOP` and a fresh translator.
    fn guest(code: &[u8]) -> (CpuState, GuestMemory, DbtEngine) {
        let mut memory = GuestMemory::new();
        memory
            .map_region_with_data(CODE, 0x1000, MemoryPermissions::READ_EXECUTE, code)
            .unwrap();
        memory
            .map_region(STACK_TOP - 0x1000, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();
        (
            CpuState::new(0, 0),
            memory,
            DbtEngine::new(64 * 1024).unwrap(),
        )
    }

    /// Two callbacks 16 bytes apart. Each shifts RAX left by four bits and sets
    /// its own number in the low bits, so RAX records the order they ran in.
    const TAGGING: [u8; 25] = [
        0x48, 0xC1, 0xE0, 0x04, // shl rax, 4
        0x48, 0x83, 0xC8, 0x01, // or rax, 1
        0xC3, // ret
        0, 0, 0, 0, 0, 0, 0, // padding
        0x48, 0xC1, 0xE0, 0x04, // shl rax, 4
        0x48, 0x83, 0xC8, 0x02, // or rax, 2
        0xC3, // ret
    ];

    #[test]
    fn a_callback_is_entered_with_the_win64_arguments_and_frame() {
        let code = [0x48, 0x89, 0xE0, 0xC3]; // mov rax, rsp; ret
        let (mut cpu, mut memory, mut dbt) = guest(&code);
        // Loaded 64 KiB above the preferred base: the listed address is rebased.
        let loaded = IMAGE_BASE + 0x1_0000;
        let listed = [CODE - 0x1_0000];
        let ran = execute_tls_callbacks(
            &listed,
            loaded,
            IMAGE_BASE,
            STACK_TOP,
            &mut cpu,
            &mut memory,
            &mut dbt,
        );
        assert_eq!(ran, Ok(1));
        let entry_rsp = cpu.get(Register::Rax);
        assert_eq!(entry_rsp, STACK_TOP - 40);
        // A callee is entered 8 bytes off 16-byte alignment, as after a CALL.
        assert_eq!(entry_rsp % 16, 8);
        assert_eq!(memory.read_u64(entry_rsp), Ok(SENTINEL));
        assert_eq!(cpu.get(Register::Rcx), loaded);
        assert_eq!(cpu.get(Register::Rdx), 1); // DLL_PROCESS_ATTACH
        assert_eq!(cpu.get(Register::R8), 0);
        // RET popped the return address and nothing else.
        assert_eq!(cpu.get(Register::Rsp), entry_rsp + 8);
        assert_eq!(cpu.rip, SENTINEL);
    }

    #[test]
    fn callbacks_run_in_array_order() {
        for (order, recorded) in [([CODE, CODE + 16], 0x12), ([CODE + 16, CODE], 0x21)] {
            let (mut cpu, mut memory, mut dbt) = guest(&TAGGING);
            let ran = execute_tls_callbacks(
                &order,
                IMAGE_BASE,
                IMAGE_BASE,
                STACK_TOP,
                &mut cpu,
                &mut memory,
                &mut dbt,
            );
            assert_eq!(ran, Ok(2));
            assert_eq!(cpu.get(Register::Rax), recorded);
        }
    }

    #[test]
    fn a_callback_over_the_instruction_budget_stops_the_sequence() {
        let mut code = TAGGING.to_vec();
        code.resize(32, 0);
        code.extend([0xEB, 0xFE]); // jmp to itself
        let (mut cpu, mut memory, mut dbt) = guest(&code);
        let ran = execute_tls_callbacks(
            &[CODE, CODE + 32, CODE + 16],
            IMAGE_BASE,
            IMAGE_BASE,
            STACK_TOP,
            &mut cpu,
            &mut memory,
            &mut dbt,
        );
        assert_eq!(
            ran,
            Err(TlsError::CallbackExecutionFault(
                ExitReason::StepLimitExceeded
            ))
        );
        // The first callback ran; the one after the endless one did not.
        assert_eq!(cpu.get(Register::Rax), 0x1);
    }

    #[test]
    fn an_unusable_callback_stack_is_rejected_before_anything_runs() {
        for stack_top in [0, 32, STACK_TOP - 8, STACK_TOP - 1] {
            let (mut cpu, mut memory, mut dbt) = guest(&TAGGING);
            let ran = execute_tls_callbacks(
                &[CODE],
                IMAGE_BASE,
                IMAGE_BASE,
                stack_top,
                &mut cpu,
                &mut memory,
                &mut dbt,
            );
            assert_eq!(
                ran,
                Err(TlsError::InvalidCallbackStack { stack_top }),
                "{stack_top:#x}"
            );
            assert_eq!(cpu, CpuState::new(0, 0));
            assert_eq!(memory.read_u64(STACK_TOP - 48), Ok(0));
            // No callback, no frame: the stack is not looked at.
            let none = execute_tls_callbacks(&[], 0, 0, stack_top, &mut cpu, &mut memory, &mut dbt);
            assert_eq!(none, Ok(0));
        }
    }

    /// Bytes that do not repeat every 256 or every 4096 bytes, so a chunk
    /// copied from the wrong offset cannot match.
    fn synthetic_template(len: usize) -> Vec<u8> {
        (0..len)
            .map(|index| {
                let mixed = index ^ (index >> 8) ^ (index >> 16);
                mixed.wrapping_mul(131).wrapping_add(17) as u8
            })
            .collect()
    }

    /// Image template at `template_at`, with `sentinel` 0xA5 bytes after it, and
    /// a writable index that does not start at zero.
    fn template_memory(
        template_at: u64,
        template: &[u8],
        sentinel: usize,
        index_at: u64,
    ) -> GuestMemory {
        let mut image = template.to_vec();
        image.extend(std::iter::repeat_n(0xA5, sentinel));
        let map = (image.len() + 0xFFF) & !0xFFF;
        image.resize(map, 0);
        let mut memory = GuestMemory::new();
        memory
            .map_region_with_data(template_at, map, MemoryPermissions::READ_ONLY, &image)
            .unwrap();
        memory
            .map_region_with_data(index_at, 0x1000, MemoryPermissions::READ_WRITE, &[0xFF; 8])
            .unwrap();
        memory
    }

    fn directory_at(
        template_at: u64,
        template_len: u64,
        index_at: u64,
        zero_fill: u32,
    ) -> TlsDirectory {
        TlsDirectory {
            raw_data_start_va: template_at,
            raw_data_end_va: template_at + template_len,
            index_address: index_at,
            size_of_zero_fill: zero_fill,
            ..TlsDirectory::default()
        }
    }

    fn assert_block(
        memory: &GuestMemory,
        thread: u64,
        index: u64,
        template: &[u8],
        zero_fill: usize,
    ) {
        assert_eq!(memory.read_u32(index).unwrap(), 0);
        let block = memory.read_u64(thread).unwrap();
        assert_eq!(block, thread + 0x1000, "the data block is the next page");
        // The block is mapped up to the next page boundary and no further.
        let mapped = (template.len() + zero_fill + 0xFFF) & !0xFFF;
        let mut got = vec![0xEEu8; mapped];
        memory.read_bytes(block, &mut got).unwrap();
        assert_eq!(&got[..template.len()], template);
        assert!(got[template.len()..].iter().all(|byte| *byte == 0));
        let end = block + mapped as u64;
        assert_eq!(memory.read_u8(end), Err(MemoryError::UnmappedAddress(end)));
    }

    const TEMPLATE_AT: u64 = 0x2_0020_0000;
    const INDEX_AT: u64 = 0x2_0000_1000;
    const THREAD_AT: u64 = 0x5_0000_0000;

    #[test]
    fn a_small_template_is_copied_and_zero_fill_does_not_take_the_following_bytes() {
        let template = synthetic_template(48);
        let mut memory = template_memory(TEMPLATE_AT, &template, 16, INDEX_AT);
        setup_tls(
            &directory_at(TEMPLATE_AT, template.len() as u64, INDEX_AT, 16),
            IMAGE_BASE,
            IMAGE_BASE,
            &mut memory,
            THREAD_AT,
        )
        .unwrap();
        assert_block(&memory, THREAD_AT, INDEX_AT, &template, 16);
        let mut tail = [0u8; 16];
        memory
            .read_bytes(TEMPLATE_AT + template.len() as u64, &mut tail)
            .unwrap();
        assert_eq!(tail, [0xA5; 16]);
        let block = memory.read_u64(THREAD_AT).unwrap();
        memory.write_u64(block + 8, 0x0102_0304_0506_0708).unwrap();
        let mut image = [0u8; 8];
        memory.read_bytes(TEMPLATE_AT + 8, &mut image).unwrap();
        assert_eq!(image, template[8..16]);
    }

    #[test]
    fn the_staged_and_direct_copy_boundaries_keep_every_template_byte() {
        for len in [
            DIRECT_TEMPLATE_MIN - 1,
            DIRECT_TEMPLATE_MIN,
            DIRECT_TEMPLATE_MIN + TEMPLATE_COPY_CHUNK + 100,
            DIRECT_TEMPLATE_LIMIT - 1,
            DIRECT_TEMPLATE_LIMIT,
        ] {
            let template = synthetic_template(len);
            let mut memory = template_memory(TEMPLATE_AT, &template, 24, INDEX_AT);
            setup_tls(
                &directory_at(TEMPLATE_AT, template.len() as u64, INDEX_AT, 24),
                IMAGE_BASE,
                IMAGE_BASE,
                &mut memory,
                THREAD_AT,
            )
            .unwrap();
            assert_block(&memory, THREAD_AT, INDEX_AT, &template, 24);
            let mut tail = [0u8; 24];
            memory
                .read_bytes(TEMPLATE_AT + template.len() as u64, &mut tail)
                .unwrap();
            assert_eq!(tail, [0xA5; 24], "len {len}");
        }
    }

    #[test]
    fn a_rebased_direct_copy_reads_the_loaded_image() {
        let template = synthetic_template(DIRECT_TEMPLATE_MIN);
        let preferred = 0x1_4000_0000u64;
        let loaded = preferred + 0x80_0000;
        let template_at = preferred + 0x20_0000;
        let index_at = preferred + 0x1000;
        let mut memory =
            template_memory(template_at + 0x80_0000, &template, 0, index_at + 0x80_0000);
        setup_tls(
            &directory_at(template_at, template.len() as u64, index_at, 8),
            loaded,
            preferred,
            &mut memory,
            THREAD_AT,
        )
        .unwrap();
        assert_block(&memory, THREAD_AT, index_at + 0x80_0000, &template, 8);
    }

    #[test]
    fn an_unreadable_template_maps_no_pointer_array() {
        for len in [32usize, DIRECT_TEMPLATE_MIN, DIRECT_TEMPLATE_MIN + 50] {
            let mut missing = GuestMemory::new();
            missing
                .map_region_with_data(INDEX_AT, 0x1000, MemoryPermissions::READ_WRITE, &[0xFF; 8])
                .unwrap();
            let missing_result = setup_tls(
                &directory_at(TEMPLATE_AT, len as u64, INDEX_AT, 0),
                IMAGE_BASE,
                IMAGE_BASE,
                &mut missing,
                THREAD_AT,
            );
            assert_eq!(
                missing_result,
                Err(TlsError::Memory(MemoryError::UnmappedAddress(TEMPLATE_AT))),
                "{len}"
            );
            assert_eq!(missing.read_u32(INDEX_AT).unwrap(), 0);
            assert!(matches!(
                missing.read_u64(THREAD_AT),
                Err(MemoryError::UnmappedAddress(_))
            ));

            let mut denied = GuestMemory::new();
            denied
                .map_region(TEMPLATE_AT, len.max(0x1000), MemoryPermissions::NO_ACCESS)
                .unwrap();
            denied
                .map_region_with_data(INDEX_AT, 0x1000, MemoryPermissions::READ_WRITE, &[0xFF; 8])
                .unwrap();
            let denied_result = setup_tls(
                &directory_at(TEMPLATE_AT, len as u64, INDEX_AT, 0),
                IMAGE_BASE,
                IMAGE_BASE,
                &mut denied,
                THREAD_AT,
            );
            assert_eq!(
                denied_result,
                Err(TlsError::Memory(MemoryError::PermissionViolation {
                    address: TEMPLATE_AT,
                    required: "read",
                })),
                "{len}"
            );
            assert_eq!(denied.read_u32(INDEX_AT).unwrap(), 0);
            assert!(matches!(
                denied.read_u64(THREAD_AT),
                Err(MemoryError::UnmappedAddress(_))
            ));
        }
    }

    #[test]
    fn a_direct_copy_template_that_runs_out_of_its_mapping_maps_nothing() {
        let len = DIRECT_TEMPLATE_MIN + 100;
        let mut memory = GuestMemory::new();
        // Only the first chunk is mapped. The rest of the template is not.
        memory
            .map_region_with_data(
                TEMPLATE_AT,
                TEMPLATE_COPY_CHUNK,
                MemoryPermissions::READ_ONLY,
                &synthetic_template(TEMPLATE_COPY_CHUNK),
            )
            .unwrap();
        memory
            .map_region_with_data(INDEX_AT, 0x1000, MemoryPermissions::READ_WRITE, &[0xFF; 8])
            .unwrap();
        let result = setup_tls(
            &directory_at(TEMPLATE_AT, len as u64, INDEX_AT, 0),
            IMAGE_BASE,
            IMAGE_BASE,
            &mut memory,
            THREAD_AT,
        );
        // The same first unreadable byte the single staged read reports.
        assert_eq!(
            result,
            Err(TlsError::Memory(MemoryError::UnmappedAddress(
                TEMPLATE_AT + TEMPLATE_COPY_CHUNK as u64
            )))
        );
        assert_eq!(memory.read_u32(INDEX_AT).unwrap(), 0);
        assert!(matches!(
            memory.read_u64(THREAD_AT),
            Err(MemoryError::UnmappedAddress(_))
        ));
    }

    #[test]
    fn an_empty_template_and_zero_fill_only_assigns_the_index() {
        let mut memory = template_memory(TEMPLATE_AT, &[], 0, INDEX_AT);
        setup_tls(
            &directory_at(TEMPLATE_AT, 0, INDEX_AT, 0),
            IMAGE_BASE,
            IMAGE_BASE,
            &mut memory,
            THREAD_AT,
        )
        .unwrap();
        assert_eq!(memory.read_u32(INDEX_AT).unwrap(), 0);
        assert!(matches!(
            memory.read_u64(THREAD_AT),
            Err(MemoryError::UnmappedAddress(_))
        ));
    }

    #[test]
    fn a_zero_fill_only_block_is_zero_and_separate_from_the_array() {
        let mut memory = template_memory(TEMPLATE_AT, &[], 0, INDEX_AT);
        setup_tls(
            &directory_at(TEMPLATE_AT, 0, INDEX_AT, 24),
            IMAGE_BASE,
            IMAGE_BASE,
            &mut memory,
            THREAD_AT,
        )
        .unwrap();
        assert_block(&memory, THREAD_AT, INDEX_AT, &[], 24);
    }

    /// Byte pattern from the before/after timing runs. It is not the pattern
    /// the correctness tests use to catch a chunk copied from the wrong offset.
    fn timed_template(len: usize) -> Vec<u8> {
        (0..len)
            .map(|index| (index.wrapping_mul(131).wrapping_add(17)) as u8)
            .collect()
    }

    fn timed_guest(template_len: usize, zero_fill: u32) -> (GuestMemory, TlsDirectory) {
        let template = timed_template(template_len);
        let mut memory = GuestMemory::new();
        if template_len > 0 {
            let map = (template_len + 0xFFF) & !0xFFF;
            memory
                .map_region_with_data(TEMPLATE_AT, map, MemoryPermissions::READ_ONLY, &template)
                .unwrap();
        }
        memory
            .map_region_with_data(INDEX_AT, 0x1000, MemoryPermissions::READ_WRITE, &[0xFF; 8])
            .unwrap();
        (
            memory,
            directory_at(TEMPLATE_AT, template_len as u64, INDEX_AT, zero_fill),
        )
    }

    #[test]
    #[ignore = "manual TLS template timing run; release build on an otherwise idle host"]
    fn tls_template_initialization_profile() {
        // Sizes, zero fill and batch counts from the interleaved before/after runs.
        let cases = [
            (64usize, 0u32, 80usize),
            (4096, 0, 40),
            (65535, 0, 20),
            (65536, 128, 16),
            (262144, 0, 6),
            (786432, 0, 4),
            (1048576, 0, 4),
            (0, 1048576, 4),
        ];
        for (template_len, zero_fill, batch) in cases {
            let (mut memory, directory) = timed_guest(template_len, zero_fill);
            setup_tls(&directory, IMAGE_BASE, IMAGE_BASE, &mut memory, THREAD_AT).unwrap();
            if template_len > 0 {
                let template = timed_template(template_len);
                let block = memory.read_u64(THREAD_AT).unwrap();
                let mut byte = [0u8; 1];
                memory.read_bytes(block, &mut byte).unwrap();
                assert_eq!(byte[0], template[0]);
                memory
                    .read_bytes(block + template_len as u64 - 1, &mut byte)
                    .unwrap();
                assert_eq!(byte[0], template[template_len - 1]);
            }

            let mut samples = Vec::new();
            for run in 0..33 {
                let mut guests: Vec<_> = (0..batch)
                    .map(|_| timed_guest(template_len, zero_fill))
                    .collect();
                let start = Instant::now();
                for (memory, directory) in &mut guests {
                    setup_tls(directory, IMAGE_BASE, IMAGE_BASE, memory, THREAD_AT).unwrap();
                    black_box(memory);
                }
                if run >= 2 {
                    let per_ns = start.elapsed().as_secs_f64() * 1e9 / batch as f64;
                    samples.push(per_ns);
                }
            }
            samples.sort_by(f64::total_cmp);
            let n = samples.len();
            let mean = samples.iter().sum::<f64>() / n as f64;
            let sample_variance = samples
                .iter()
                .map(|sample| (sample - mean).powi(2))
                .sum::<f64>()
                / (n - 1) as f64;
            println!(
                "template={template_len} zero_fill={zero_fill} batch={batch} n={n} median_ns={:.1} sample_variance_ns2={sample_variance:.1}",
                samples[n / 2]
            );
        }
    }
}
