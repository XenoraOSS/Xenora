// SPDX-License-Identifier: GPL-3.0-or-later

//! Transactional batch mapping for explicit PE32+ module images.
//!
//! This narrow seam connects a completed [`ModuleLayoutPlan`] to the existing
//! single-image [`ParsedPe::map_into_memory`] primitive. It checks that the
//! plan belongs to the exact ordered input batch before touching memory, maps
//! images in input order at their planned bases and returns logical
//! reservation records for inspection. Those complete spans are retained in
//! `GuestMemory` without committing their holes, so later allocations cannot
//! reuse bytes reserved for the image.
//!
//! A complete batch plan has exactly one successful placement for every input
//! image index, with no missing, duplicate or extra indexes. This layer also
//! checks that each planned descriptor matches the parsed image supplied with
//! it and that the half-open placement intervals do not overlap. The overlap
//! and index checks are defensive even though [`ModuleLayoutPlan::build`]
//! currently preserves those invariants: the memory boundary must not depend
//! on mutation order or on a caller composing the wrong plan for a batch.
//!
//! Rollback is deliberately exact and deliberately expensive. For a nonempty
//! batch, after the whole plan validates, the batch clones the entire
//! [`GuestMemory`] once. If image
//! validation or mapping fails later, that clone is assigned back, restoring
//! the prior address space and its metadata rather than merely unmapping the
//! successful images. Each single-image primitive can additionally snapshot
//! the whole address space for its own partial-section rollback. In the worst
//! case this is one whole-memory copy before the batch plus one nested
//! whole-memory copy for every image attempted: time cost scales with the
//! batch length times the committed guest bytes and metadata, while peak host
//! memory includes the live address space, the batch snapshot and one nested
//! image snapshot. This is not the final memory-ownership or efficient
//! reservation design.
//!
//! The returned reservations describe `[base, end)` spans derived from the
//! planned `SizeOfImage`. Only headers and sections are committed, but the
//! complete span is retained as a lightweight ownership record so holes remain
//! unmapped and unavailable to later mappings or dynamic allocations. The
//! records still do not implement unloading, per-section ownership or host OS
//! guard pages. Batch mapping performs no relocation, import or IAT
//! binding, API-set resolution, service registration, exception or TLS
//! initialization, unloading, reference counting or guest execution.

use std::fmt;

use crate::memory::{GuestMemory, MemoryError};
use crate::module_layout::{
    AddressInterval, IMAGE_BASE_ALIGNMENT, ImageLayoutOutcome, ImagePlacement,
    ModuleImageDescriptor, ModuleLayoutPlan,
};
use crate::pe::{ParsedPe, PeError};
use crate::reloc::{RelocationError, RelocationReport, apply_relocations};

/// One explicit in-memory module image in batch input order.
///
/// `bytes` must be the complete original PE32+ byte slice represented by `pe`.
/// The batch layer does not parse a second copy. It passes both values to
/// [`ParsedPe::map_into_memory`], which validates header agreement, section
/// extents and mapping arithmetic before insertion.
#[derive(Clone, Copy, Debug)]
pub struct ExplicitModuleImage<'a> {
    pub pe: &'a ParsedPe,
    pub bytes: &'a [u8],
}

/// Logical half-open reservation metadata for one successfully mapped image.
///
/// `end` is the exclusive planned end of the `SizeOfImage` span, not a promise
/// that every byte below it was committed. `entry_point` is absent for a DLL
/// whose entry RVA is zero, matching [`ParsedPe::map_into_memory`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModuleReservation {
    pub image_index: usize,
    pub base: u64,
    pub end: u64,
    pub used_preferred_base: bool,
    pub entry_point: Option<u64>,
}

/// Errors that prevent a module batch from being mapped as one transaction.
/// Every image-specific failure carries the input `image_index`.
#[derive(Debug, PartialEq, Eq)]
pub enum ModuleMemoryError {
    /// The plan does not have one record for each input image. Because the
    /// exact-one rule is checked before memory mutation, too few and too many
    /// plan records are both batch-shape errors rather than partial mappings.
    PlanInputMismatch {
        plan_images: usize,
        module_images: usize,
    },
    /// A plan record index is outside the input batch or repeats an earlier
    /// record. A repeat necessarily leaves another input index missing.
    InvalidPlanIndex {
        plan_position: usize,
        image_index: usize,
    },
    /// The record for an input image is an explicit planner failure rather
    /// than a successful placement.
    MissingPlacement { image_index: usize },
    /// The descriptor retained by the plan does not match the parsed image
    /// supplied at the same input index.
    DescriptorMismatch { image_index: usize },
    /// A placement span overlaps memory that existed before the batch. The
    /// complete span is module-owned, so even an uncommitted hole cannot
    /// contain unrelated memory.
    PlacementConflict {
        image_index: usize,
        placement: AddressInterval,
        existing: AddressInterval,
    },
    /// Two half-open placement intervals overlap. Adjacent intervals do not.
    OverlappingPlacements {
        first_index: usize,
        first: AddressInterval,
        second_index: usize,
        second: AddressInterval,
        overlap: AddressInterval,
    },
    /// The image's own validation or insertion failed. The batch restored the
    /// exact prior [`GuestMemory`] snapshot before returning this error.
    MappingFailed { image_index: usize, source: PeError },
    /// Retaining the image's complete span conflicted with an existing module
    /// reservation. The batch restored the exact prior [`GuestMemory`]
    /// snapshot before returning this error.
    ReservationFailed {
        image_index: usize,
        source: MemoryError,
    },
}

impl fmt::Display for ModuleMemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PlanInputMismatch {
                plan_images,
                module_images,
            } => write!(
                f,
                "layout plan has {plan_images} image records for {module_images} module images"
            ),
            Self::InvalidPlanIndex {
                plan_position,
                image_index,
            } => write!(
                f,
                "layout plan record {plan_position} has missing, repeated or out-of-batch image index {image_index}"
            ),
            Self::MissingPlacement { image_index } => {
                write!(f, "image {image_index} has no successful placement")
            }
            Self::DescriptorMismatch { image_index } => {
                write!(f, "layout descriptor does not describe image {image_index}")
            }
            Self::PlacementConflict {
                image_index,
                placement,
                existing,
            } => write!(
                f,
                "placement for image {image_index} at [{:#x}, {:#x}) overlaps existing memory at [{:#x}, {:#x})",
                placement.start, placement.end, existing.start, existing.end
            ),
            Self::OverlappingPlacements {
                first_index,
                second_index,
                overlap,
                ..
            } => write!(
                f,
                "placements for images {first_index} and {second_index} overlap at [{:#x}, {:#x})",
                overlap.start, overlap.end
            ),
            Self::MappingFailed {
                image_index,
                source,
            } => write!(
                f,
                "image {image_index} failed validation or mapping: {source}"
            ),
            Self::ReservationFailed {
                image_index,
                source,
            } => write!(
                f,
                "image {image_index} failed reservation ownership: {source}"
            ),
        }
    }
}

impl std::error::Error for ModuleMemoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MappingFailed { source, .. } => Some(source),
            Self::ReservationFailed { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Why one reservation cannot describe the image with the same input index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleReservationError {
    /// Planned module bases are required to be 64 KiB aligned.
    UnalignedBase { base: u64 },
    /// `base + SizeOfImage` is not representable as a guest address.
    EndOverflow { base: u64, size_of_image: u32 },
    /// The half-open reservation must exactly cover the parsed `SizeOfImage`.
    WrongEnd {
        base: u64,
        end: u64,
        expected_end: u64,
    },
    /// The entry point must be the reserved base plus the nonzero entry RVA,
    /// or absent when the image has no entry point.
    WrongEntryPoint {
        entry_point: Option<u64>,
        expected_entry_point: Option<u64>,
    },
    /// The preferred-base marker must say whether the reservation uses the
    /// parsed image's preferred base.
    WrongPreferredBase {
        used_preferred_base: bool,
        expected_used_preferred_base: bool,
    },
    /// The entry RVA is nonzero but its address is not representable at the
    /// reservation base.
    EntryPointOverflow { base: u64, entry_rva: u32 },
}

impl fmt::Display for ModuleReservationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnalignedBase { base } => {
                write!(
                    f,
                    "reservation base {base:#x} is not {IMAGE_BASE_ALIGNMENT}-byte aligned"
                )
            }
            Self::EndOverflow {
                base,
                size_of_image,
            } => write!(
                f,
                "reservation base {base:#x} plus image size {size_of_image:#x} overflows"
            ),
            Self::WrongEnd {
                base,
                end,
                expected_end,
            } => write!(
                f,
                "reservation [{base:#x}, {end:#x}) does not end at the image extent {expected_end:#x}"
            ),
            Self::WrongEntryPoint {
                entry_point,
                expected_entry_point,
            } => write!(
                f,
                "reservation entry {entry_point:?} does not match image entry {expected_entry_point:?}"
            ),
            Self::WrongPreferredBase {
                used_preferred_base,
                expected_used_preferred_base,
            } => write!(
                f,
                "reservation preferred-base marker {used_preferred_base} does not match expected {expected_used_preferred_base}"
            ),
            Self::EntryPointOverflow { base, entry_rva } => write!(
                f,
                "reservation base {base:#x} plus entry RVA {entry_rva:#x} overflows the guest address space"
            ),
        }
    }
}

impl std::error::Error for ModuleReservationError {}

/// Shared crate-internal validation failures for a reservation set. Each
/// batch phase translates these into its own public error type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReservationSetError {
    ReservationInputMismatch {
        reservations: usize,
        module_images: usize,
    },
    InvalidReservationIndex {
        position: usize,
        image_index: usize,
    },
    InvalidReservation {
        image_index: usize,
        source: ModuleReservationError,
    },
    OverlappingReservations {
        first_image_index: usize,
        second_image_index: usize,
    },
}

impl From<ReservationSetError> for ModuleRelocationError {
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

/// Errors that prevent transactional relocation of an explicit module batch.
#[derive(Debug, PartialEq, Eq)]
pub enum ModuleRelocationError {
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
    /// The original bytes and parsed headers no longer agree on the image-base
    /// field location. The batch was not mutated.
    ValidationFailed { image_index: usize, source: PeError },
    /// Applying one image's relocation directory failed. The complete prior
    /// address space was restored before this error was returned.
    RelocationFailed {
        image_index: usize,
        source: RelocationError,
    },
    /// Writing the mapped image-base field failed after relocation. The
    /// complete prior address space was restored before this error was returned.
    ImageBaseWriteFailed {
        image_index: usize,
        source: MemoryError,
    },
}

impl fmt::Display for ModuleRelocationError {
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
            } => {
                write!(
                    f,
                    "reservation for image {image_index} is inconsistent: {source}"
                )
            }
            Self::OverlappingReservations {
                first_image_index,
                second_image_index,
            } => write!(
                f,
                "reservations for images {first_image_index} and {second_image_index} overlap"
            ),
            Self::ValidationFailed {
                image_index,
                source,
            } => {
                write!(
                    f,
                    "image {image_index} relocation validation failed: {source}"
                )
            }
            Self::RelocationFailed {
                image_index,
                source,
            } => {
                write!(f, "image {image_index} relocation failed: {source}")
            }
            Self::ImageBaseWriteFailed {
                image_index,
                source,
            } => {
                write!(
                    f,
                    "image {image_index} image-base field write failed: {source}"
                )
            }
        }
    }
}

impl std::error::Error for ModuleRelocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidReservation { source, .. } => Some(source),
            Self::ValidationFailed { source, .. } => Some(source),
            Self::RelocationFailed { source, .. } => Some(source),
            Self::ImageBaseWriteFailed { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Apply every explicit module image's base relocations as one transaction.
///
/// Reservation records are matched by `image_index`, so the caller's slice
/// order does not matter. The exact-one index rule, reservation shape, image
/// extents, alignment, entry and preferred-base metadata, span overlap and
/// mapped image-base field locations are validated before mutation. Relocation
/// then runs in input order through the existing PE32+ DIR64 engine. Because that
/// engine is intentionally small and can discover malformed later entries only
/// after earlier writes, this batch layer snapshots the whole address space
/// once; any failure assigns that snapshot back before returning. The rollback
/// cost and peak host-memory use therefore scale with the existing address
/// space, and this is not the final memory-ownership design.
///
/// After each image's relocation succeeds, its mapped optional-header image
/// base is written to the reserved base, matching startup's ordering. Reports
/// are returned in input order. No import/IAT write, delayed import, exception
/// registration, TLS callback, entry execution or ownership reservation is
/// performed.
pub fn relocate_module_batch(
    images: &[ExplicitModuleImage<'_>],
    reservations: &[ModuleReservation],
    memory: &mut GuestMemory,
) -> Result<Vec<RelocationReport>, ModuleRelocationError> {
    let ordered =
        validate_reservations(images, reservations).map_err(ModuleRelocationError::from)?;
    if images.is_empty() {
        return Ok(Vec::new());
    }
    let mut image_base_fields = Vec::with_capacity(images.len());
    for (image_index, image) in images.iter().enumerate() {
        let reservation = ordered[image_index];
        let base = reservation.base;
        let source = image
            .pe
            .image_base_field_address(image.bytes, base)
            .map_err(|source| ModuleRelocationError::ValidationFailed {
                image_index,
                source,
            })?;
        image_base_fields.push(source);
    }

    let before = memory.clone();
    let mut reports = Vec::with_capacity(images.len());
    for (image_index, image) in images.iter().enumerate() {
        let base = ordered[image_index].base;
        let report = match apply_relocations(image.pe, image.bytes, base, memory) {
            Ok(report) => report,
            Err(source) => {
                *memory = before;
                return Err(ModuleRelocationError::RelocationFailed {
                    image_index,
                    source,
                });
            }
        };
        if let Err(source) = memory.write_u64_bypass(image_base_fields[image_index], base) {
            *memory = before;
            return Err(ModuleRelocationError::ImageBaseWriteFailed {
                image_index,
                source,
            });
        }
        reports.push(report);
    }
    Ok(reports)
}

pub(crate) fn validate_reservations(
    images: &[ExplicitModuleImage<'_>],
    reservations: &[ModuleReservation],
) -> Result<Vec<ModuleReservation>, ReservationSetError> {
    if reservations.len() != images.len() {
        return Err(ReservationSetError::ReservationInputMismatch {
            reservations: reservations.len(),
            module_images: images.len(),
        });
    }
    let mut positions = vec![None; images.len()];
    for (position, reservation) in reservations.iter().enumerate() {
        if reservation.image_index >= images.len() || positions[reservation.image_index].is_some() {
            return Err(ReservationSetError::InvalidReservationIndex {
                position,
                image_index: reservation.image_index,
            });
        }
        positions[reservation.image_index] = Some(position);
    }

    let mut ordered = Vec::with_capacity(images.len());
    for (image_index, image) in images.iter().enumerate() {
        let position = positions[image_index].expect("the exact-one index check passed");
        let reservation = reservations[position];
        validate_reservation(image_index, *image, reservation)?;
        ordered.push(reservation);
    }

    let mut spans: Vec<_> = ordered
        .iter()
        .map(|reservation| (reservation.image_index, reservation.base, reservation.end))
        .collect();
    spans.sort_unstable_by_key(|&(image_index, base, end)| (base, end, image_index));
    for pair in spans.windows(2) {
        if pair[1].1 < pair[0].2 {
            return Err(ReservationSetError::OverlappingReservations {
                first_image_index: pair[0].0,
                second_image_index: pair[1].0,
            });
        }
    }
    Ok(ordered)
}

fn validate_reservation(
    image_index: usize,
    image: ExplicitModuleImage<'_>,
    reservation: ModuleReservation,
) -> Result<(), ReservationSetError> {
    let inconsistent = |source| ReservationSetError::InvalidReservation {
        image_index,
        source,
    };
    if !reservation.base.is_multiple_of(IMAGE_BASE_ALIGNMENT) {
        return Err(inconsistent(ModuleReservationError::UnalignedBase {
            base: reservation.base,
        }));
    }
    let expected_end = reservation
        .base
        .checked_add(u64::from(image.pe.size_of_image))
        .ok_or(inconsistent(ModuleReservationError::EndOverflow {
            base: reservation.base,
            size_of_image: image.pe.size_of_image,
        }))?;
    if reservation.end != expected_end {
        return Err(inconsistent(ModuleReservationError::WrongEnd {
            base: reservation.base,
            end: reservation.end,
            expected_end,
        }));
    }
    let expected_entry_point = if image.pe.entry_point_rva == 0 {
        None
    } else {
        Some(
            reservation
                .base
                .checked_add(u64::from(image.pe.entry_point_rva))
                .ok_or_else(|| {
                    inconsistent(ModuleReservationError::EntryPointOverflow {
                        base: reservation.base,
                        entry_rva: image.pe.entry_point_rva,
                    })
                })?,
        )
    };
    if reservation.entry_point != expected_entry_point {
        return Err(inconsistent(ModuleReservationError::WrongEntryPoint {
            entry_point: reservation.entry_point,
            expected_entry_point,
        }));
    }
    let expected_used_preferred_base = reservation.base == image.pe.image_base;
    if reservation.used_preferred_base != expected_used_preferred_base {
        return Err(inconsistent(ModuleReservationError::WrongPreferredBase {
            used_preferred_base: reservation.used_preferred_base,
            expected_used_preferred_base,
        }));
    }
    Ok(())
}

/// Map a complete explicit module batch into `memory` as one transaction.
///
/// Validation of the complete plan finishes before the batch snapshot is made
/// and before any image can mutate memory. Images are then mapped in input
/// order through [`ParsedPe::map_into_memory`]. A failure at any image assigns
/// the pre-batch snapshot back to `memory`, so successful earlier images do
/// not remain mapped.
///
/// On success the reservations are returned in input order. They are logical
/// inspection records only; see the module documentation for the snapshot
/// cost, lack of ownership and absent guard-gap semantics.
pub fn map_module_batch(
    images: &[ExplicitModuleImage<'_>],
    plan: &ModuleLayoutPlan,
    memory: &mut GuestMemory,
) -> Result<Vec<ModuleReservation>, ModuleMemoryError> {
    let placements = validate_complete_plan(images, plan)?;
    if images.is_empty() {
        return Ok(Vec::new());
    }
    for (image_index, placement) in placements.iter().enumerate() {
        if let Some((existing_base, existing_end)) =
            memory.first_region_overlap(placement.base, placement.end)
        {
            return Err(ModuleMemoryError::PlacementConflict {
                image_index,
                placement: AddressInterval::new(placement.base, placement.end),
                existing: AddressInterval::new(existing_base, existing_end),
            });
        }
    }

    let before = memory.clone();
    let mut reservations = Vec::with_capacity(images.len());
    for (image_index, image) in images.iter().enumerate() {
        let placement = placements[image_index];
        match image
            .pe
            .map_into_memory(image.bytes, memory, placement.base)
        {
            Ok(entry_point) => reservations.push(ModuleReservation {
                image_index,
                base: placement.base,
                end: placement.end,
                used_preferred_base: placement.used_preferred_base,
                entry_point,
            }),
            Err(source) => {
                *memory = before;
                return Err(ModuleMemoryError::MappingFailed {
                    image_index,
                    source,
                });
            }
        }
    }
    for reservation in &reservations {
        if let Err(source) = memory.reserve_module_image_span(reservation.base, reservation.end) {
            *memory = before;
            return Err(ModuleMemoryError::ReservationFailed {
                image_index: reservation.image_index,
                source,
            });
        }
    }
    Ok(reservations)
}

fn validate_complete_plan(
    images: &[ExplicitModuleImage<'_>],
    plan: &ModuleLayoutPlan,
) -> Result<Vec<ImagePlacement>, ModuleMemoryError> {
    let positions = validate_indexes(images.len(), plan)?;
    let mut placements = Vec::with_capacity(images.len());

    for (image_index, image) in images.iter().enumerate() {
        let position = positions[image_index].expect("the exact-one index check passed");
        let planned_image = &plan.images()[position];
        let placement = match planned_image.outcome {
            ImageLayoutOutcome::Placed(placement) => placement,
            ImageLayoutOutcome::Failed(_) => {
                return Err(ModuleMemoryError::MissingPlacement { image_index });
            }
        };
        if planned_image.descriptor != ModuleImageDescriptor::from_parsed_pe(image.pe) {
            return Err(ModuleMemoryError::DescriptorMismatch { image_index });
        }
        placements.push(placement);
    }

    reject_overlapping_placements(&placements)?;
    Ok(placements)
}

fn validate_indexes(
    module_images: usize,
    plan: &ModuleLayoutPlan,
) -> Result<Vec<Option<usize>>, ModuleMemoryError> {
    if plan.images().len() != module_images {
        return Err(ModuleMemoryError::PlanInputMismatch {
            plan_images: plan.images().len(),
            module_images,
        });
    }

    let mut positions = vec![None; module_images];
    for (plan_position, image) in plan.images().iter().enumerate() {
        if image.index >= module_images || positions[image.index].is_some() {
            return Err(ModuleMemoryError::InvalidPlanIndex {
                plan_position,
                image_index: image.index,
            });
        }
        positions[image.index] = Some(plan_position);
    }
    Ok(positions)
}

fn reject_overlapping_placements(placements: &[ImagePlacement]) -> Result<(), ModuleMemoryError> {
    let mut ordered: Vec<_> = placements.iter().copied().enumerate().collect();
    ordered.sort_by_key(|&(index, placement)| (placement.base, placement.end, index));

    for pair in ordered.windows(2) {
        let (first_index, first) = pair[0];
        let (second_index, second) = pair[1];
        if second.base >= first.end {
            continue;
        }
        return Err(ModuleMemoryError::OverlappingPlacements {
            first_index,
            first: AddressInterval::new(first.base, first.end),
            second_index,
            second: AddressInterval::new(second.base, second.end),
            overlap: AddressInterval::new(first.base.max(second.base), first.end.min(second.end)),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(base: u64, end: u64) -> ImagePlacement {
        ImagePlacement {
            base,
            end,
            used_preferred_base: false,
        }
    }

    #[test]
    fn placement_overlap_check_uses_half_open_intervals() {
        let adjacent = [placement(0x1000, 0x2000), placement(0x2000, 0x3000)];
        assert_eq!(reject_overlapping_placements(&adjacent), Ok(()));

        let overlapping = [placement(0x1000, 0x2001), placement(0x2000, 0x3000)];
        assert_eq!(
            reject_overlapping_placements(&overlapping),
            Err(ModuleMemoryError::OverlappingPlacements {
                first_index: 0,
                first: AddressInterval::new(0x1000, 0x2001),
                second_index: 1,
                second: AddressInterval::new(0x2000, 0x3000),
                overlap: AddressInterval::new(0x2000, 0x2001),
            })
        );
    }

    #[test]
    fn placement_overlap_check_is_independent_of_input_order() {
        let unordered = [placement(0x3000, 0x4000), placement(0x1000, 0x3001)];
        assert_eq!(
            reject_overlapping_placements(&unordered),
            Err(ModuleMemoryError::OverlappingPlacements {
                first_index: 1,
                first: AddressInterval::new(0x1000, 0x3001),
                second_index: 0,
                second: AddressInterval::new(0x3000, 0x4000),
                overlap: AddressInterval::new(0x3000, 0x3001),
            })
        );
    }
}
