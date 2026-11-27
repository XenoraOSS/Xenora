// SPDX-License-Identifier: GPL-3.0-or-later

//! Transactional TLS storage preparation for explicit module batches.
//!
//! This layer validates each module's TLS directory against the image-backed
//! memory committed by module mapping, assigns static TLS indexes in batch
//! input order and initializes one caller-selected thread's TLS pointer array
//! plus per-module writable storage. It deliberately does not execute TLS
//! callbacks or `DllMain`, choose thread-creation policy, implement unloading
//! or claim dynamic TLS APIs.
//!
//! Rollback uses the same deliberately expensive whole-memory snapshot as the
//! surrounding explicit-module batch seams. Validation of every image happens
//! before mutation; an allocation or copy failure restores even earlier
//! successful index writes, array mappings and data blocks exactly.

use std::fmt;

use crate::memory::{GuestMemory, MemoryError, MemoryPermissions};
use crate::module_memory::{
    ExplicitModuleImage, ModuleReservation, ModuleReservationError, ReservationSetError,
    validate_reservations,
};
use crate::pe::MappedImageBacking;
use crate::tls::{
    TLS_ARRAY_SIZE, TlsDirectory, TlsError, parse_tls_directory, tls_directory_entry,
};

const TLS_COPY_CHUNK: usize = 4096;
const TLS_INDEX_LIMIT: usize = TLS_ARRAY_SIZE / 8;

/// Writable static TLS storage initialized for one module on the selected
/// thread. `end` is exclusive and equals `base + template_bytes + zero_fill`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModuleTlsStorage {
    pub base: u64,
    pub end: u64,
    pub template_bytes: u64,
    pub zero_fill_bytes: u32,
}

/// TLS preparation for a module that has a TLS directory. Addresses are the
/// rebased guest addresses at the module's actual reservation base. Callbacks
/// are discovered and validated but are never executed by this layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedModuleTls {
    pub tls_index: u32,
    pub index_address: u64,
    pub template_start: u64,
    pub template_end: u64,
    pub storage: Option<ModuleTlsStorage>,
    pub callbacks: Vec<u64>,
}

/// Preparation outcome for one input image. `prepared` is `None` only when the
/// image honestly has no TLS directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleTlsReport {
    pub image_index: usize,
    pub prepared: Option<PreparedModuleTls>,
}

/// Validation failure for one module's TLS metadata before mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleTlsValidationError {
    /// The TLS directory parser rejected the directory or callback array.
    MalformedDirectory { source: TlsError },
    /// A nonzero preferred-image VA does not belong to the parsed image.
    AddressOutsideImage { field: &'static str, address: u64 },
    /// The range is inside `SizeOfImage` or arithmetic but is not covered by
    /// committed header/section backing.
    RangeNotImageBacked {
        field: &'static str,
        rva: u32,
        len: u64,
    },
    /// Exactly one of the raw-template start/end addresses is zero, or the
    /// end precedes the start.
    InvalidTemplateRange { start: u64, end: u64 },
    /// A callback target is not inside executable committed image code.
    CallbackTargetNotExecutable { address: u64 },
    /// The template plus zero-fill cannot be represented by this host.
    TemplateSizeOverflow,
}

impl fmt::Display for ModuleTlsValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedDirectory { source } => write!(f, "malformed TLS directory: {source}"),
            Self::AddressOutsideImage { field, address } => write!(
                f,
                "TLS {field} address {address:#x} is outside the preferred image address space"
            ),
            Self::RangeNotImageBacked { field, rva, len } => write!(
                f,
                "TLS {field} range at RVA {rva:#x} for {len} bytes is not image-backed"
            ),
            Self::InvalidTemplateRange { start, end } => {
                write!(f, "invalid TLS template range {start:#x}..{end:#x}")
            }
            Self::CallbackTargetNotExecutable { address } => write!(
                f,
                "TLS callback target {address:#x} is not executable image code"
            ),
            Self::TemplateSizeOverflow => {
                write!(
                    f,
                    "TLS template plus zero-fill size overflows host arithmetic"
                )
            }
        }
    }
}

impl std::error::Error for ModuleTlsValidationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MalformedDirectory { source } => Some(source),
            _ => None,
        }
    }
}

/// Errors that prevent transactional TLS preparation.
#[derive(Debug, PartialEq, Eq)]
pub enum ModuleTlsError {
    /// The reservation slice does not have exactly one record per input image.
    ReservationInputMismatch {
        reservations: usize,
        module_images: usize,
    },
    /// A reservation index is outside the input batch or repeats an earlier one.
    InvalidReservationIndex { position: usize, image_index: usize },
    /// A reservation does not match the parsed image with the same input index.
    InvalidReservation {
        image_index: usize,
        source: ModuleReservationError,
    },
    /// Two reservation spans overlap. Half-open adjacency is valid.
    OverlappingReservations {
        first_image_index: usize,
        second_image_index: usize,
    },
    /// The caller-selected TLS pointer-array base cannot hold u64 slots.
    InvalidThreadTlsBase { thread_tls_base: u64 },
    /// More modules need static TLS indexes than the current 4 KiB pointer
    /// array can hold. This is Xenora's present storage limit, not a Windows
    /// compatibility claim.
    TooManyTlsIndexes { count: usize, limit: usize },
    /// The caller-selected array does not contain enough zero slots. Zero is
    /// Xenora's free-slot ownership marker for this non-executing batch layer.
    ThreadTlsSlotsExhausted { needed: usize, free: usize },
    /// One image's TLS metadata is malformed or not backed by committed image
    /// memory. No memory was mutated because all images validate first.
    ValidationFailed {
        image_index: usize,
        source: ModuleTlsValidationError,
    },
    /// The thread array mapping, an allocation, a copy or a loader-phase write
    /// failed after validation. The exact pre-preparation memory was restored.
    PreparationFailed {
        image_index: Option<usize>,
        source: MemoryError,
    },
}

impl From<ReservationSetError> for ModuleTlsError {
    fn from(error: ReservationSetError) -> Self {
        match error {
            ReservationSetError::ReservationInputMismatch {
                reservations,
                module_images,
            } => Self::ReservationInputMismatch {
                reservations,
                module_images,
            },
            ReservationSetError::InvalidReservationIndex {
                position,
                image_index,
            } => Self::InvalidReservationIndex {
                position,
                image_index,
            },
            ReservationSetError::InvalidReservation {
                image_index,
                source,
            } => Self::InvalidReservation {
                image_index,
                source,
            },
            ReservationSetError::OverlappingReservations {
                first_image_index,
                second_image_index,
            } => Self::OverlappingReservations {
                first_image_index,
                second_image_index,
            },
        }
    }
}

impl fmt::Display for ModuleTlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReservationInputMismatch {
                reservations,
                module_images,
            } => write!(
                f,
                "{reservations} reservations do not describe {module_images} module images exactly once"
            ),
            Self::InvalidReservationIndex {
                position,
                image_index,
            } => write!(
                f,
                "reservation {position} has missing, repeated or out-of-batch image index {image_index}"
            ),
            Self::InvalidReservation {
                image_index,
                source,
            } => write!(
                f,
                "reservation for image {image_index} is inconsistent: {source}"
            ),
            Self::OverlappingReservations {
                first_image_index,
                second_image_index,
            } => write!(
                f,
                "reservations for images {first_image_index} and {second_image_index} overlap"
            ),
            Self::InvalidThreadTlsBase { thread_tls_base } => write!(
                f,
                "thread TLS array base {thread_tls_base:#x} is zero or not eight-byte aligned"
            ),
            Self::TooManyTlsIndexes { count, limit } => write!(
                f,
                "{count} modules need static TLS indexes but the current array holds only {limit}"
            ),
            Self::ThreadTlsSlotsExhausted { needed, free } => write!(
                f,
                "the thread TLS array needs {needed} slots but only {free} are free"
            ),
            Self::ValidationFailed {
                image_index,
                source,
            } => write!(f, "image {image_index} TLS validation failed: {source}"),
            Self::PreparationFailed {
                image_index,
                source,
            } => match image_index {
                Some(index) => write!(
                    f,
                    "image {index} TLS preparation failed and was rolled back: {source}"
                ),
                None => write!(
                    f,
                    "thread TLS array preparation failed and was rolled back: {source}"
                ),
            },
        }
    }
}

impl std::error::Error for ModuleTlsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidReservation { source, .. } => Some(source),
            Self::ValidationFailed { source, .. } => Some(source),
            Self::PreparationFailed { source, .. } => Some(source),
            _ => None,
        }
    }
}

struct ValidatedTls {
    directory: TlsDirectory,
    index_address: u64,
    template_start: u64,
    template_end: u64,
    template_len: usize,
    total_size: usize,
    callbacks: Vec<u64>,
}

fn image_rva(
    address: u64,
    preferred_base: u64,
    field: &'static str,
) -> Result<u32, ModuleTlsValidationError> {
    let rva = address
        .checked_sub(preferred_base)
        .ok_or(ModuleTlsValidationError::AddressOutsideImage { field, address })?;
    u32::try_from(rva).map_err(|_| ModuleTlsValidationError::AddressOutsideImage { field, address })
}

fn rebase(
    address: u64,
    preferred_base: u64,
    reservation_base: u64,
    field: &'static str,
) -> Result<(u32, u64), ModuleTlsValidationError> {
    let rva = image_rva(address, preferred_base, field)?;
    let actual = reservation_base
        .checked_add(u64::from(rva))
        .ok_or(ModuleTlsValidationError::AddressOutsideImage { field, address })?;
    Ok((rva, actual))
}

fn require_backing(
    backing: &MappedImageBacking,
    field: &'static str,
    rva: u32,
    len: u64,
) -> Result<(), ModuleTlsValidationError> {
    if backing.covers(rva, len) {
        Ok(())
    } else {
        Err(ModuleTlsValidationError::RangeNotImageBacked { field, rva, len })
    }
}

fn callback_target_is_executable(pe: &crate::pe::ParsedPe, rva: u32) -> bool {
    pe.sections.iter().any(|section| {
        section.permissions.execute
            && rva >= section.virtual_address
            && rva - section.virtual_address < section.virtual_size.max(section.raw_data_size)
    })
}

fn validate_image_tls(
    image: &ExplicitModuleImage<'_>,
    reservation_base: u64,
) -> Result<Option<ValidatedTls>, ModuleTlsValidationError> {
    let Some(directory) = parse_tls_directory(image.bytes, image.pe)
        .map_err(|source| ModuleTlsValidationError::MalformedDirectory { source })?
    else {
        return Ok(None);
    };
    let Some((directory_rva, directory_size)) = tls_directory_entry(image.bytes) else {
        return Ok(None);
    };
    let backing = MappedImageBacking::new(image.pe);
    require_backing(
        &backing,
        "directory",
        directory_rva,
        u64::from(directory_size),
    )?;

    if directory.index_address == 0 {
        return Err(ModuleTlsValidationError::AddressOutsideImage {
            field: "index",
            address: 0,
        });
    }
    let (index_rva, index_address) = rebase(
        directory.index_address,
        image.pe.image_base,
        reservation_base,
        "index",
    )?;
    require_backing(&backing, "index", index_rva, 4)?;

    let start = directory.raw_data_start_va;
    let end = directory.raw_data_end_va;
    if (start == 0) != (end == 0) || end < start {
        return Err(ModuleTlsValidationError::InvalidTemplateRange { start, end });
    }
    let (template_start, template_end, template_len) = if start == 0 {
        (0, 0, 0usize)
    } else {
        let (start_rva, actual_start) = rebase(
            start,
            image.pe.image_base,
            reservation_base,
            "template start",
        )?;
        let (_, actual_end) = rebase(end, image.pe.image_base, reservation_base, "template end")?;
        let len_u64 = end - start;
        require_backing(&backing, "template", start_rva, len_u64)?;
        let len =
            usize::try_from(len_u64).map_err(|_| ModuleTlsValidationError::TemplateSizeOverflow)?;
        (actual_start, actual_end, len)
    };
    let total_size = template_len
        .checked_add(directory.size_of_zero_fill as usize)
        .ok_or(ModuleTlsValidationError::TemplateSizeOverflow)?;

    let mut callbacks = Vec::with_capacity(directory.callbacks.len());
    if directory.callbacks_address != 0 {
        let (array_rva, _) = rebase(
            directory.callbacks_address,
            image.pe.image_base,
            reservation_base,
            "callback array",
        )?;
        let array_len = u64::try_from(directory.callbacks.len() + 1)
            .ok()
            .and_then(|entries| entries.checked_mul(8))
            .ok_or(ModuleTlsValidationError::TemplateSizeOverflow)?;
        require_backing(&backing, "callback array", array_rva, array_len)?;
    }
    for callback in &directory.callbacks {
        let (target_rva, actual) = rebase(
            *callback,
            image.pe.image_base,
            reservation_base,
            "callback target",
        )?;
        if !callback_target_is_executable(image.pe, target_rva) {
            return Err(ModuleTlsValidationError::CallbackTargetNotExecutable {
                address: *callback,
            });
        }
        callbacks.push(actual);
    }

    Ok(Some(ValidatedTls {
        directory,
        index_address,
        template_start,
        template_end,
        template_len,
        total_size,
        callbacks,
    }))
}

fn copy_template(
    memory: &mut GuestMemory,
    source: u64,
    destination: u64,
    len: usize,
) -> Result<(), MemoryError> {
    let mut chunk = [0u8; TLS_COPY_CHUNK];
    let mut copied = 0usize;
    while copied < len {
        let count = (len - copied).min(TLS_COPY_CHUNK);
        let offset = copied as u64;
        memory.read_bytes(
            source.checked_add(offset).ok_or(MemoryError::Overflow)?,
            &mut chunk[..count],
        )?;
        memory.write_bytes(
            destination
                .checked_add(offset)
                .ok_or(MemoryError::Overflow)?,
            &chunk[..count],
        )?;
        copied += count;
    }
    Ok(())
}

/// Validate and prepare static TLS storage for one explicit module batch on
/// the thread whose TLS pointer array starts at `thread_tls_base`.
///
/// Indexes are assigned in input order to the first zero slots in the selected
/// array. Zero is this layer's free-slot marker, so existing nonzero slots are
/// never overwritten. If the pointer array does not already exist as writable
/// memory, it is created as thread-owned dynamic memory. Per-module storage is
/// also thread-owned dynamic memory. The caller must supply the batch in
/// dependency order; this layer treats input order as the initialization-order
/// contract, while dependency sorting itself remains a separate loader
/// responsibility.
///
/// No TLS callback is executed. Callback addresses are rebased, validated as
/// executable image code and returned for a future initialization-order design.
pub fn prepare_module_tls(
    images: &[ExplicitModuleImage<'_>],
    reservations: &[ModuleReservation],
    memory: &mut GuestMemory,
    thread_tls_base: u64,
) -> Result<Vec<ModuleTlsReport>, ModuleTlsError> {
    let ordered = validate_reservations(images, reservations).map_err(ModuleTlsError::from)?;
    if thread_tls_base == 0 || thread_tls_base & 7 != 0 {
        return Err(ModuleTlsError::InvalidThreadTlsBase { thread_tls_base });
    }

    let mut validated = Vec::with_capacity(images.len());
    let mut tls_count = 0usize;
    for (image_index, image) in images.iter().enumerate() {
        let base = ordered[image_index].base;
        let prepared =
            validate_image_tls(image, base).map_err(|source| ModuleTlsError::ValidationFailed {
                image_index,
                source,
            })?;
        tls_count += usize::from(prepared.is_some());
        validated.push(prepared);
    }
    if tls_count > TLS_INDEX_LIMIT {
        return Err(ModuleTlsError::TooManyTlsIndexes {
            count: tls_count,
            limit: TLS_INDEX_LIMIT,
        });
    }
    if tls_count == 0 {
        return Ok(images
            .iter()
            .enumerate()
            .map(|(image_index, _)| ModuleTlsReport {
                image_index,
                prepared: None,
            })
            .collect());
    }

    let before = memory.clone();
    if !memory.can_write(thread_tls_base, TLS_ARRAY_SIZE)
        && let Err(source) = memory.map_thread_region(
            thread_tls_base,
            TLS_ARRAY_SIZE,
            MemoryPermissions::READ_WRITE,
        )
    {
        *memory = before;
        return Err(ModuleTlsError::PreparationFailed {
            image_index: None,
            source,
        });
    }

    let available_indexes = match find_free_tls_slots(memory, thread_tls_base, tls_count) {
        Ok(indexes) => indexes,
        Err(error) => {
            *memory = before;
            return Err(error);
        }
    };
    let mut reports = Vec::with_capacity(images.len());
    let mut available_indexes = available_indexes.into_iter();
    for (image_index, prepared) in validated.into_iter().enumerate() {
        let Some(prepared) = prepared else {
            reports.push(ModuleTlsReport {
                image_index,
                prepared: None,
            });
            continue;
        };
        let tls_index = available_indexes
            .next()
            .expect("one free index was reserved for each TLS directory");
        let storage = match prepare_one(memory, thread_tls_base, tls_index, &prepared) {
            Ok(storage) => storage,
            Err(source) => {
                *memory = before;
                return Err(ModuleTlsError::PreparationFailed {
                    image_index: Some(image_index),
                    source,
                });
            }
        };
        reports.push(ModuleTlsReport {
            image_index,
            prepared: Some(PreparedModuleTls {
                tls_index,
                index_address: prepared.index_address,
                template_start: prepared.template_start,
                template_end: prepared.template_end,
                storage,
                callbacks: prepared.callbacks,
            }),
        });
    }
    Ok(reports)
}

fn find_free_tls_slots(
    memory: &GuestMemory,
    thread_tls_base: u64,
    needed: usize,
) -> Result<Vec<u32>, ModuleTlsError> {
    let mut indexes = Vec::with_capacity(needed);
    for index in 0..TLS_INDEX_LIMIT {
        let slot = thread_tls_base.checked_add(index as u64 * 8).ok_or(
            ModuleTlsError::PreparationFailed {
                image_index: None,
                source: MemoryError::Overflow,
            },
        )?;
        let occupied =
            memory
                .read_u64(slot)
                .map_err(|source| ModuleTlsError::PreparationFailed {
                    image_index: None,
                    source,
                })?;
        if occupied == 0 {
            indexes.push(index as u32);
            if indexes.len() == needed {
                return Ok(indexes);
            }
        }
    }
    Err(ModuleTlsError::ThreadTlsSlotsExhausted {
        needed,
        free: indexes.len(),
    })
}

fn prepare_one(
    memory: &mut GuestMemory,
    thread_tls_base: u64,
    tls_index: u32,
    prepared: &ValidatedTls,
) -> Result<Option<ModuleTlsStorage>, MemoryError> {
    memory.write_u32_bypass(prepared.index_address, tls_index)?;
    if prepared.total_size == 0 {
        return Ok(None);
    }
    let storage_base =
        memory.map_thread_region(0, prepared.total_size, MemoryPermissions::READ_WRITE)?;
    copy_template(
        memory,
        prepared.template_start,
        storage_base,
        prepared.template_len,
    )?;
    let slot = thread_tls_base
        .checked_add(u64::from(tls_index) * 8)
        .ok_or(MemoryError::Overflow)?;
    memory.write_u64(slot, storage_base)?;
    let end = storage_base
        .checked_add(prepared.total_size as u64)
        .ok_or(MemoryError::Overflow)?;
    Ok(Some(ModuleTlsStorage {
        base: storage_base,
        end,
        template_bytes: prepared.template_len as u64,
        zero_fill_bytes: prepared.directory.size_of_zero_fill,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryPermissions;
    use crate::pe::{
        IMAGE_FILE_DLL, IMAGE_SCN_MEM_EXECUTE, IMAGE_SCN_MEM_READ, IMAGE_SCN_MEM_WRITE,
    };

    const PREFERRED: u64 = 0x1_4000_0000;
    const ACTUAL0: u64 = 0x2_0000_0000;
    const SIZE_OF_IMAGE: u32 = 0x3000;
    const TEXT_RVA: u32 = 0x1000;
    const DATA_RVA: u32 = 0x2000;
    const TEXT_FILE: usize = 0x200;
    const DATA_FILE: usize = 0x400;
    const CALLBACK_ARRAY_RVA: u32 = 0x2080;
    const TEMPLATE_RVA: u32 = 0x2100;
    const INDEX_RVA: u32 = 0x2180;
    const THREAD_TLS: u64 = 0x5000_0000;

    #[derive(Clone)]
    struct TlsSpec {
        start_rva: u32,
        end_rva: u32,
        index_rva: u32,
        callbacks_rva: u32,
        callbacks: Vec<u64>,
        zero_fill: u32,
    }

    impl TlsSpec {
        fn valid(template_len: u32, zero_fill: u32) -> Self {
            Self {
                start_rva: TEMPLATE_RVA,
                end_rva: TEMPLATE_RVA + template_len,
                index_rva: INDEX_RVA,
                callbacks_rva: CALLBACK_ARRAY_RVA,
                callbacks: vec![PREFERRED + u64::from(TEXT_RVA)],
                zero_fill,
            }
        }
    }

    fn pe() -> crate::pe::ParsedPe {
        crate::pe::ParsedPe {
            machine: 0x8664,
            number_of_sections: 2,
            time_date_stamp: 0,
            characteristics: IMAGE_FILE_DLL,
            image_base: PREFERRED,
            section_alignment: 0x1000,
            file_alignment: 0x200,
            size_of_image: SIZE_OF_IMAGE,
            size_of_headers: 0x200,
            entry_point_rva: TEXT_RVA,
            sections: vec![
                crate::pe::PeSection {
                    name: ".text".into(),
                    virtual_address: TEXT_RVA,
                    virtual_size: 0x200,
                    raw_data_size: 0x200,
                    raw_data_ptr: TEXT_FILE as u32,
                    permissions: MemoryPermissions::READ_EXECUTE,
                    characteristics: IMAGE_SCN_MEM_EXECUTE | IMAGE_SCN_MEM_READ,
                },
                crate::pe::PeSection {
                    name: ".data".into(),
                    virtual_address: DATA_RVA,
                    virtual_size: 0x400,
                    raw_data_size: 0x400,
                    raw_data_ptr: DATA_FILE as u32,
                    permissions: MemoryPermissions::READ_WRITE,
                    characteristics: IMAGE_SCN_MEM_READ | IMAGE_SCN_MEM_WRITE,
                },
            ],
        }
    }

    fn va(rva: u32) -> u64 {
        PREFERRED + u64::from(rva)
    }

    fn image_bytes(include_tls: bool, spec: &TlsSpec) -> Vec<u8> {
        let mut bytes = vec![0; 0x800];
        bytes[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        bytes[0x94..0x96].copy_from_slice(&240u16.to_le_bytes());
        let optional = 0x98;
        bytes[optional + 108..optional + 112].copy_from_slice(&16u32.to_le_bytes());
        if include_tls {
            let directory = optional + 112 + 9 * 8;
            bytes[directory..directory + 4].copy_from_slice(&DATA_RVA.to_le_bytes());
            bytes[directory + 4..directory + 8].copy_from_slice(&40u32.to_le_bytes());
            let fields = [
                spec.start_rva,
                spec.end_rva,
                spec.index_rva,
                spec.callbacks_rva,
            ];
            for (field, rva) in fields.iter().enumerate() {
                let value = if *rva == 0 { 0 } else { va(*rva) };
                bytes[DATA_FILE + field * 8..DATA_FILE + field * 8 + 8]
                    .copy_from_slice(&value.to_le_bytes());
            }
            bytes[DATA_FILE + 32..DATA_FILE + 36].copy_from_slice(&spec.zero_fill.to_le_bytes());
            if spec.callbacks_rva != 0 {
                let mut at = DATA_FILE + (spec.callbacks_rva - DATA_RVA) as usize;
                for callback in &spec.callbacks {
                    bytes[at..at + 8].copy_from_slice(&callback.to_le_bytes());
                    at += 8;
                }
            }
            if spec.end_rva > spec.start_rva {
                let start = DATA_FILE + (spec.start_rva - DATA_RVA) as usize;
                let len = (spec.end_rva - spec.start_rva) as usize;
                if start + len <= bytes.len() {
                    for (offset, byte) in bytes[start..start + len].iter_mut().enumerate() {
                        *byte = (offset + 1) as u8;
                    }
                }
            }
        }
        bytes
    }

    fn map_image(memory: &mut GuestMemory, base: u64, bytes: &[u8], pe: &crate::pe::ParsedPe) {
        memory
            .map_region_with_data(base, 0x1000, MemoryPermissions::READ_ONLY, &bytes[..0x200])
            .unwrap();
        for section in &pe.sections {
            let raw_start = section.raw_data_ptr as usize;
            let raw_end = raw_start + section.raw_data_size as usize;
            memory
                .map_region_with_data(
                    base + u64::from(section.virtual_address),
                    section.virtual_size as usize,
                    section.permissions,
                    &bytes[raw_start..raw_end],
                )
                .unwrap();
        }
    }

    fn reservation(image_index: usize, base: u64) -> ModuleReservation {
        ModuleReservation {
            image_index,
            base,
            end: base + u64::from(SIZE_OF_IMAGE),
            used_preferred_base: false,
            entry_point: Some(base + u64::from(TEXT_RVA)),
        }
    }

    struct Fixture {
        pes: Vec<crate::pe::ParsedPe>,
        bytes: Vec<Vec<u8>>,
        bases: Vec<u64>,
    }

    fn fixture(specs: &[Option<TlsSpec>]) -> (Fixture, GuestMemory) {
        let mut fixture = Fixture {
            pes: Vec::new(),
            bytes: Vec::new(),
            bases: Vec::new(),
        };
        let mut memory = GuestMemory::new();
        for (index, spec) in specs.iter().enumerate() {
            let pe = pe();
            let bytes = image_bytes(
                spec.is_some(),
                spec.as_ref().unwrap_or(&TlsSpec {
                    start_rva: 0,
                    end_rva: 0,
                    index_rva: 0,
                    callbacks_rva: 0,
                    callbacks: Vec::new(),
                    zero_fill: 0,
                }),
            );
            let base = ACTUAL0 + index as u64 * 0x1_0000;
            map_image(&mut memory, base, &bytes, &pe);
            fixture.pes.push(pe);
            fixture.bytes.push(bytes);
            fixture.bases.push(base);
        }
        (fixture, memory)
    }

    impl Fixture {
        fn images(&self) -> Vec<ExplicitModuleImage<'_>> {
            self.pes
                .iter()
                .zip(&self.bytes)
                .map(|(pe, bytes)| ExplicitModuleImage { pe, bytes })
                .collect()
        }

        fn reservations(&self) -> Vec<ModuleReservation> {
            let mut reservations: Vec<_> = self
                .bases
                .iter()
                .copied()
                .enumerate()
                .map(|(index, base)| reservation(index, base))
                .collect();
            reservations.reverse();
            reservations
        }
    }

    #[test]
    fn valid_and_absent_directories_prepare_storage_in_input_order() {
        let mut first = TlsSpec::valid(6, 3);
        first.callbacks = vec![PREFERRED + u64::from(TEXT_RVA)];
        let mut second = TlsSpec::valid(2, 0);
        second.callbacks = Vec::new();
        let (fixture, mut memory) = fixture(&[None, Some(first), Some(second)]);
        let images = fixture.images();
        let reservations = fixture.reservations();

        let reports = prepare_module_tls(&images, &reservations, &mut memory, THREAD_TLS).unwrap();
        assert_eq!(reports.len(), 3);
        assert!(reports[0].prepared.is_none());
        let first = reports[1].prepared.as_ref().unwrap();
        let second = reports[2].prepared.as_ref().unwrap();
        assert_eq!((first.tls_index, second.tls_index), (0, 1));
        assert_eq!(
            first.callbacks,
            vec![fixture.bases[1] + u64::from(TEXT_RVA)]
        );
        assert!(second.callbacks.is_empty());

        assert_eq!(
            memory
                .read_u32(fixture.bases[1] + u64::from(INDEX_RVA))
                .unwrap(),
            0
        );
        assert_eq!(
            memory
                .read_u32(fixture.bases[2] + u64::from(INDEX_RVA))
                .unwrap(),
            1
        );
        let first_storage = first.storage.unwrap();
        let second_storage = second.storage.unwrap();
        assert_eq!(memory.read_u64(THREAD_TLS).unwrap(), first_storage.base);
        assert_eq!(
            memory.read_u64(THREAD_TLS + 8).unwrap(),
            second_storage.base
        );
        assert_eq!(first_storage.end - first_storage.base, 9);
        assert_eq!(first_storage.template_bytes, 6);
        assert_eq!(first_storage.zero_fill_bytes, 3);
        let mut copied = [0; 9];
        memory.read_bytes(first_storage.base, &mut copied).unwrap();
        assert_eq!(copied, [1, 2, 3, 4, 5, 6, 0, 0, 0]);
        assert_eq!(second_storage.end - second_storage.base, 2);
    }

    #[test]
    fn nonzero_existing_tls_slots_are_owned_and_never_overwritten() {
        let spec = TlsSpec::valid(3, 0);
        let (fixture, mut memory) = fixture(&[Some(spec)]);
        memory
            .map_region(THREAD_TLS, TLS_ARRAY_SIZE, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory.write_u64(THREAD_TLS, 0x1111).unwrap();
        memory.write_u64(THREAD_TLS + 8, 0x2222).unwrap();
        let images = fixture.images();
        let reservations = fixture.reservations();

        let reports = prepare_module_tls(&images, &reservations, &mut memory, THREAD_TLS).unwrap();
        let prepared = reports[0].prepared.as_ref().unwrap();
        assert_eq!(prepared.tls_index, 2);
        assert_eq!(memory.read_u64(THREAD_TLS).unwrap(), 0x1111);
        assert_eq!(memory.read_u64(THREAD_TLS + 8).unwrap(), 0x2222);
        assert_eq!(
            memory.read_u64(THREAD_TLS + 16).unwrap(),
            prepared.storage.unwrap().base
        );
    }

    #[test]
    fn hole_backed_tls_data_is_rejected_before_any_mutation() {
        for expected_field in ["template", "index"] {
            let valid = TlsSpec::valid(4, 0);
            let mut hole = TlsSpec::valid(4, 0);
            if expected_field == "template" {
                hole.start_rva = 0x2500;
                hole.end_rva = 0x2504;
            } else {
                hole.index_rva = 0x2500;
            }
            let (fixture, mut memory) = fixture(&[Some(valid), Some(hole)]);
            let images = fixture.images();
            let reservations = fixture.reservations();
            memory
                .write_u32_bypass(fixture.bases[0] + u64::from(INDEX_RVA), 77)
                .unwrap();

            let error = prepare_module_tls(&images, &reservations, &mut memory, THREAD_TLS)
                .expect_err("TLS data in a logical image hole must be rejected");
            let ModuleTlsError::ValidationFailed {
                image_index: 1,
                source:
                    ModuleTlsValidationError::RangeNotImageBacked {
                        field: actual_field,
                        ..
                    },
            } = error
            else {
                panic!("unexpected hole-backed TLS error: {error:?}");
            };
            assert_eq!(actual_field, expected_field);
            assert_eq!(
                memory
                    .read_u32(fixture.bases[0] + u64::from(INDEX_RVA))
                    .unwrap(),
                77
            );
            assert!(memory.read_u64(THREAD_TLS).is_err());
        }
    }

    #[test]
    fn malformed_template_and_nonexecutable_callback_are_validation_errors() {
        for (spec, executable_case) in [
            {
                let mut spec = TlsSpec::valid(4, 0);
                spec.end_rva = spec.start_rva - 1;
                (spec, false)
            },
            {
                let mut spec = TlsSpec::valid(4, 0);
                spec.callbacks = vec![va(DATA_RVA)];
                (spec, true)
            },
        ] {
            let (fixture, mut memory) = fixture(&[Some(spec)]);
            let images = fixture.images();
            let reservations = fixture.reservations();
            let error = prepare_module_tls(&images, &reservations, &mut memory, THREAD_TLS)
                .expect_err("malformed TLS metadata must be rejected");
            if executable_case {
                assert!(
                    matches!(
                        error,
                        ModuleTlsError::ValidationFailed {
                            source: ModuleTlsValidationError::CallbackTargetNotExecutable { .. },
                            ..
                        }
                    ),
                    "{error:?}"
                );
            } else {
                assert!(
                    matches!(
                        error,
                        ModuleTlsError::ValidationFailed {
                            source: ModuleTlsValidationError::InvalidTemplateRange { .. },
                            ..
                        }
                    ),
                    "{error:?}"
                );
            }
            assert!(memory.read_u64(THREAD_TLS).is_err());
        }
    }

    #[test]
    fn allocation_failure_rolls_back_earlier_indexes_storage_and_array() {
        let first = TlsSpec::valid(4, 0);
        let mut huge = TlsSpec::valid(4, 0);
        huge.zero_fill = 64 * 1024 * 1024;
        let (fixture, mut memory) = fixture(&[Some(first), Some(huge)]);
        let images = fixture.images();
        let reservations = fixture.reservations();
        memory
            .write_u32_bypass(fixture.bases[0] + u64::from(INDEX_RVA), 77)
            .unwrap();

        let error = prepare_module_tls(&images, &reservations, &mut memory, THREAD_TLS)
            .expect_err("oversized TLS storage must exhaust the bounded thread budget");
        assert!(
            matches!(
                error,
                ModuleTlsError::PreparationFailed {
                    image_index: Some(1),
                    source: MemoryError::AllocationFailed,
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            memory
                .read_u32(fixture.bases[0] + u64::from(INDEX_RVA))
                .unwrap(),
            77
        );
        assert!(memory.read_u64(THREAD_TLS).is_err());
    }
}
