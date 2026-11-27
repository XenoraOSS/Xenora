// SPDX-License-Identifier: GPL-3.0-or-later

//! Transactional exception-directory registration for explicit module batches.
//!
//! This layer connects mapped module reservations to the existing x64 exception
//! function-table machinery. It does not execute module code, invoke `DllMain`,
//! implement dynamic function-table APIs or provide unloading.

use std::fmt;

use crate::exceptions::register_exception_directory_for_image;
use crate::memory::GuestMemory;
use crate::module_memory::{
    ExplicitModuleImage, ModuleReservation, ModuleReservationError, ReservationSetError,
    validate_reservations,
};

/// Registration outcome for one input image. A valid image with no exception
/// directory is an honest no-op with zero entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModuleExceptionReport {
    pub image_index: usize,
    pub function_entries: u64,
}

/// Errors that prevent transactional registration of module exception data.
#[derive(Debug, PartialEq, Eq)]
pub enum ModuleExceptionError {
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
    /// One image's exception directory is malformed, unreadable or not backed
    /// by committed image memory. Earlier registrations were rolled back.
    RegistrationFailed { image_index: usize, reason: String },
}

impl From<ReservationSetError> for ModuleExceptionError {
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

impl fmt::Display for ModuleExceptionError {
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
            Self::RegistrationFailed {
                image_index,
                reason,
            } => {
                write!(
                    f,
                    "image {image_index} exception registration failed: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for ModuleExceptionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidReservation { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Register every explicit module image's x64 exception directory as one
/// transaction. Reports are returned in input order.
///
/// Reservation shape is validated before mutation. Registration itself does
/// not write guest bytes; it only appends validated function-table metadata.
/// If a later image fails, the table list is truncated to its original length,
/// so registrations from earlier images do not remain visible to exception
/// lookup. No unwind behavior, entry-point execution, dynamic registration,
/// unloading or reference counting is implemented here.
pub fn register_module_exception_directories(
    images: &[ExplicitModuleImage<'_>],
    reservations: &[ModuleReservation],
    memory: &mut GuestMemory,
) -> Result<Vec<ModuleExceptionReport>, ModuleExceptionError> {
    let ordered =
        validate_reservations(images, reservations).map_err(ModuleExceptionError::from)?;
    if images.is_empty() {
        return Ok(Vec::new());
    }

    let original_table_count = memory.function_tables.len();
    let mut reports = Vec::with_capacity(images.len());
    for (image_index, image) in images.iter().enumerate() {
        let base = ordered[image_index].base;
        match register_exception_directory_for_image(image.pe, image.bytes, base, memory) {
            Ok(function_entries) => reports.push(ModuleExceptionReport {
                image_index,
                function_entries,
            }),
            Err(reason) => {
                memory.function_tables.truncate(original_table_count);
                return Err(ModuleExceptionError::RegistrationFailed {
                    image_index,
                    reason,
                });
            }
        }
    }
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exceptions::lookup;
    use crate::memory::MemoryPermissions;
    use crate::pe::{
        IMAGE_FILE_DLL, IMAGE_SCN_MEM_EXECUTE, IMAGE_SCN_MEM_READ, ParsedPe, PeSection,
    };

    const SIZE_OF_IMAGE: u32 = 0x3000;
    const ENTRY_RVA: u32 = 0x1000;
    const EXCEPTION_RVA: u32 = 0x2000;
    const UNWIND_RVA: u32 = 0x2100;

    fn pe(base: u64, include_pdata: bool) -> ParsedPe {
        let mut sections = vec![PeSection {
            name: ".text".into(),
            virtual_address: 0x1000,
            virtual_size: 0x1000,
            raw_data_size: 0,
            raw_data_ptr: 0,
            permissions: MemoryPermissions::READ_EXECUTE,
            characteristics: IMAGE_SCN_MEM_EXECUTE | IMAGE_SCN_MEM_READ,
        }];
        if include_pdata {
            sections.push(PeSection {
                name: ".pdata".into(),
                virtual_address: 0x2000,
                virtual_size: 0x1000,
                raw_data_size: 0,
                raw_data_ptr: 0,
                permissions: MemoryPermissions::READ_ONLY,
                characteristics: IMAGE_SCN_MEM_READ,
            });
        }
        ParsedPe {
            machine: 0x8664,
            number_of_sections: sections.len() as u16,
            time_date_stamp: 0,
            characteristics: IMAGE_FILE_DLL,
            image_base: base,
            section_alignment: 0x1000,
            file_alignment: 0x200,
            size_of_image: SIZE_OF_IMAGE,
            size_of_headers: 0x200,
            entry_point_rva: ENTRY_RVA,
            sections,
        }
    }

    fn headers(exception_rva: u32, exception_size: u32) -> Vec<u8> {
        let mut bytes = vec![0; 0x200];
        bytes[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        bytes[0x94..0x96].copy_from_slice(&240u16.to_le_bytes());
        let optional = 0x84 + 20;
        bytes[optional + 108..optional + 112].copy_from_slice(&4u32.to_le_bytes());
        bytes[optional + 136..optional + 140].copy_from_slice(&exception_rva.to_le_bytes());
        bytes[optional + 140..optional + 144].copy_from_slice(&exception_size.to_le_bytes());
        bytes
    }

    fn reservation(image_index: usize, base: u64) -> ModuleReservation {
        ModuleReservation {
            image_index,
            base,
            end: base + u64::from(SIZE_OF_IMAGE),
            used_preferred_base: true,
            entry_point: Some(base + u64::from(ENTRY_RVA)),
        }
    }

    fn map_committed_image(memory: &mut GuestMemory, base: u64, include_pdata: bool) {
        memory
            .map_region(base, 0x1000, MemoryPermissions::READ_ONLY)
            .unwrap();
        memory
            .map_region(base + 0x1000, 0x1000, MemoryPermissions::READ_EXECUTE)
            .unwrap();
        if include_pdata {
            memory
                .map_region(base + 0x2000, 0x1000, MemoryPermissions::READ_ONLY)
                .unwrap();
        }
    }

    fn write_function_table(memory: &mut GuestMemory, base: u64) {
        for (index, value) in [0x1000u32, 0x1100, UNWIND_RVA].into_iter().enumerate() {
            memory
                .write_u32_bypass(base + u64::from(EXCEPTION_RVA) + 4 * index as u64, value)
                .unwrap();
        }
    }

    #[test]
    fn valid_and_absent_directories_register_honestly() {
        let base = 0x1_0000_0000;
        let valid_pe = pe(base, true);
        let valid_bytes = headers(EXCEPTION_RVA, 12);
        let absent_pe = pe(base + 0x1_0000, true);
        let absent_bytes = headers(0, 0);
        let images = [
            ExplicitModuleImage {
                pe: &valid_pe,
                bytes: &valid_bytes,
            },
            ExplicitModuleImage {
                pe: &absent_pe,
                bytes: &absent_bytes,
            },
        ];
        let reservations = [reservation(0, base), reservation(1, base + 0x1_0000)];
        let mut memory = GuestMemory::new();
        map_committed_image(&mut memory, base, true);
        map_committed_image(&mut memory, base + 0x1_0000, true);
        write_function_table(&mut memory, base);

        let reports = register_module_exception_directories(&images, &reservations, &mut memory)
            .expect("register");

        assert_eq!(
            reports,
            [
                ModuleExceptionReport {
                    image_index: 0,
                    function_entries: 1,
                },
                ModuleExceptionReport {
                    image_index: 1,
                    function_entries: 0,
                },
            ]
        );
        assert_eq!(memory.function_tables.len(), 1);
        assert_eq!(
            lookup(&memory, base + 0x1040).unwrap(),
            Some((base, base + u64::from(EXCEPTION_RVA)))
        );
    }

    #[test]
    fn later_hole_backed_directory_rolls_back_earlier_registration() {
        let first_base = 0x1_0000_0000;
        let second_base = 0x1_0001_0000;
        let first_pe = pe(first_base, true);
        let first_bytes = headers(EXCEPTION_RVA, 12);
        let second_pe = pe(second_base, false);
        let second_bytes = headers(0x2800, 12);
        let images = [
            ExplicitModuleImage {
                pe: &first_pe,
                bytes: &first_bytes,
            },
            ExplicitModuleImage {
                pe: &second_pe,
                bytes: &second_bytes,
            },
        ];
        let reservations = [reservation(0, first_base), reservation(1, second_base)];
        let mut memory = GuestMemory::new();
        map_committed_image(&mut memory, first_base, true);
        map_committed_image(&mut memory, second_base, false);
        write_function_table(&mut memory, first_base);
        let hole = second_base + 0x2800;
        memory
            .map_region(hole, 0x100, MemoryPermissions::READ_WRITE)
            .unwrap();
        for (index, value) in [0x1000u32, 0x1100, UNWIND_RVA].into_iter().enumerate() {
            memory.write_u32(hole + 4 * index as u64, value).unwrap();
        }

        let error =
            register_module_exception_directories(&images, &reservations, &mut memory).unwrap_err();

        assert!(
            matches!(
                error,
                ModuleExceptionError::RegistrationFailed { image_index: 1, .. }
            ),
            "{error:?}"
        );
        assert_eq!(memory.function_tables.len(), 0);
        assert_eq!(lookup(&memory, first_base + 0x1040).unwrap(), None);
    }
}
