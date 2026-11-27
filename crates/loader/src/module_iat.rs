// SPDX-License-Identifier: GPL-3.0-or-later

//! Transactional import-address-table binding for explicit PE32+ module batches.
//!
//! This narrow memory-writing seam connects four artifacts that all use the
//! same positional package order: the explicit images supplied to
//! `map_module_batch`, that batch's logical reservations, a [`ModuleGraph`]
//! built from those image bytes and the [`ModuleBindingPlan`] built from that
//! graph at the reservation bases. Logical file names are not present in
//! [`ExplicitModuleImage`], so position is the package-identity seam. Before
//! memory mutation this layer re-parses the supplied bytes under the graph's
//! file names and requires the resulting graph, and the exact binding plan
//! rebuilt from the reservation bases, to agree with the inputs.
//!
//! Validation of counts, reservation indexes and metadata, image metadata,
//! graph agreement, binding-plan agreement and overlapping logical
//! reservations is complete before memory is mutated. Per-slot image-backing
//! validation and memory writes then run in deterministic binding-plan order.
//! Successful resolved slots are written as little-endian guest u64 values
//! with [`GuestMemory::write_u64_bypass`]. This is loader-phase mutation: the
//! full eight-byte mapping is still checked, but final section write
//! permission is not a guest-store permission and does not prevent the loader
//! from binding an IAT. Unresolved binding outcomes are never written.
//!
//! Rollback is deliberately exact and deliberately expensive. For a nonempty
//! package the whole [`GuestMemory`] is cloned once after initial validation.
//! If slot validation or any write fails, that snapshot is assigned back,
//! restoring bytes, permissions and metadata even when earlier slots were
//! written successfully. Peak host memory therefore includes both the live
//! address space and its snapshot. This is not the final memory-ownership or
//! efficient reservation design.
//!
//! The layer performs no relocation, service registration, API-set resolution,
//! exception or TLS initialization, DllMain or other entry execution,
//! unloading, reference counting, delayed-import binding, host file search or
//! guest instruction execution.

use std::fmt;

use crate::import_table::ImportName;
use crate::memory::{GuestMemory, MemoryError};
use crate::module_binding::{
    BindingOutcome, IAT_SLOT_BYTES, IatSlotPlan, ModuleBindingPlan, ModuleInspectionError,
    ResolvedBinding, UnresolvedBindingReason,
};
use crate::module_graph::{
    ForwarderStep, ModuleGraph, ModuleGraphError, ModuleReference, PackageModule,
};
use crate::module_layout::{AddressInterval, IMAGE_BASE_ALIGNMENT};
use crate::module_memory::{ExplicitModuleImage, ModuleReservation};
use crate::pe::{ParsedPe, PeError};

const IMAGE_FILE_RELOCS_STRIPPED: u16 = 0x0001;

/// Errors that prevent transactional IAT binding or roll it back completely.
#[derive(Debug, PartialEq, Eq)]
pub enum ModuleIatError {
    /// The three positional package inputs do not have the same module count.
    InputCountMismatch {
        module_images: usize,
        reservations: usize,
        graph_modules: usize,
    },
    /// Reservations are required in input order, so the record at each slice
    /// position must carry that same input image index.
    ReservationIndexMismatch { position: usize, image_index: usize },
    /// A reservation is not the nonzero checked half-open `SizeOfImage` span
    /// derived from its image and base.
    InvalidReservationSpan {
        image_index: usize,
        base: u64,
        end: u64,
        image_size: u32,
    },
    /// A successful module layout placement is always 64 KiB aligned.
    UnalignedReservationBase { image_index: usize, base: u64 },
    /// `used_preferred_base` does not agree with the reservation base and the
    /// preferred base in the parsed image.
    ReservationPreferredBaseMismatch {
        image_index: usize,
        base: u64,
        preferred_base: u64,
        used_preferred_base: bool,
    },
    /// A relocation-stripped fixed-base image cannot occupy a nonpreferred
    /// reservation in any batch produced by the layout planner and mapper.
    FixedBaseReservationMismatch {
        image_index: usize,
        base: u64,
        preferred_base: u64,
    },
    /// The image bytes and reservation base do not satisfy the nonmutating
    /// mapping validation that produced legitimate reservations.
    ImageNotMappableAtReservation {
        image_index: usize,
        base: u64,
        source: PeError,
    },
    /// The optional entry address is not exactly the value produced by
    /// mapping this parsed image at the reservation base.
    ReservationEntryPointMismatch {
        image_index: usize,
        expected: Option<u64>,
        actual: Option<u64>,
    },
    /// Two logical half-open reservation spans overlap. Adjacent spans do not.
    OverlappingReservations {
        first_index: usize,
        first: AddressInterval,
        second_index: usize,
        second: AddressInterval,
        overlap: AddressInterval,
    },
    /// The original bytes for an image no longer parse as a supported PE32+
    /// image.
    InvalidImage { image_index: usize, source: PeError },
    /// The independently reparsed PE metadata does not agree with the parsed
    /// image supplied at the same input position.
    ImageMetadataMismatch { image_index: usize },
    /// Rebuilding the positional package graph from the supplied image bytes
    /// either failed or produced different graph data. `image_index` is the
    /// first divergent package position when one is known.
    ImageGraphMismatch {
        image_index: Option<usize>,
        source: Option<ModuleGraphError>,
    },
    /// The supplied binding plan is not exactly the plan rebuilt from the
    /// validated graph and reservation bases.
    BindingPlanMismatch {
        /// First differing slot position, or the first missing/extra position
        /// when the slot counts differ. `None` means only inspection-error
        /// metadata differed.
        slot_position: Option<usize>,
        plan_slots: usize,
        expected_slots: usize,
        /// Whether the retained import/export table inspection errors differ.
        inspection_errors: bool,
    },
    /// A resolved slot is outside the logical half-open reservation of its
    /// consuming image.
    SlotOutsideReservation {
        consumer: ModuleReference,
        slot_rva: u32,
        slot_address: u64,
        image: AddressInterval,
    },
    /// A resolved slot is inside the logical image span but is not covered
    /// continuously by the header/section bytes that the image mapping
    /// primitive commits. Holes inside a reservation are not owned by the
    /// image and must not receive an IAT write.
    SlotImageBackingMissing {
        consumer: ModuleReference,
        slot_rva: u32,
        slot_address: u64,
    },
    /// Guest memory rejected the complete eight-byte write. Any earlier slots
    /// in the batch were rolled back before this error was returned.
    WriteFailed {
        consumer: ModuleReference,
        slot_rva: u32,
        slot_address: u64,
        source: MemoryError,
    },
}

impl fmt::Display for ModuleIatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputCountMismatch {
                module_images,
                reservations,
                graph_modules,
            } => write!(
                f,
                "IAT binding needs equal module counts, got {module_images} images, {reservations} reservations and {graph_modules} graph modules"
            ),
            Self::ReservationIndexMismatch {
                position,
                image_index,
            } => write!(
                f,
                "reservation at input position {position} carries image index {image_index}"
            ),
            Self::InvalidReservationSpan {
                image_index,
                base,
                end,
                image_size,
            } => write!(
                f,
                "reservation for image {image_index} is [{base:#x}, {end:#x}), not the checked nonzero SizeOfImage span of {image_size:#x} bytes"
            ),
            Self::UnalignedReservationBase { image_index, base } => write!(
                f,
                "reservation base {base:#x} for image {image_index} is not {IMAGE_BASE_ALIGNMENT}-byte aligned"
            ),
            Self::ReservationPreferredBaseMismatch {
                image_index,
                base,
                preferred_base,
                used_preferred_base,
            } => write!(
                f,
                "reservation for image {image_index} has base {base:#x}, preferred base {preferred_base:#x} and used_preferred_base={used_preferred_base}"
            ),
            Self::FixedBaseReservationMismatch {
                image_index,
                base,
                preferred_base,
            } => write!(
                f,
                "relocation-stripped image {image_index} cannot use nonpreferred reservation base {base:#x}; required preferred base is {preferred_base:#x}"
            ),
            Self::ImageNotMappableAtReservation {
                image_index,
                base,
                source,
            } => write!(
                f,
                "image {image_index} cannot map at reservation base {base:#x}: {source}"
            ),
            Self::ReservationEntryPointMismatch {
                image_index,
                expected,
                actual,
            } => write!(
                f,
                "reservation entry point for image {image_index} is {actual:?}, expected {expected:?}"
            ),
            Self::OverlappingReservations {
                first_index,
                second_index,
                overlap,
                ..
            } => write!(
                f,
                "reservations for images {first_index} and {second_index} overlap at [{:#x}, {:#x})",
                overlap.start, overlap.end
            ),
            Self::InvalidImage {
                image_index,
                source,
            } => write!(f, "image {image_index} does not reparse as PE32+: {source}"),
            Self::ImageMetadataMismatch { image_index } => write!(
                f,
                "reparsed PE metadata does not describe image {image_index}"
            ),
            Self::ImageGraphMismatch {
                image_index,
                source,
            } => {
                if let Some(source) = source {
                    write!(f, "image bytes do not rebuild the package graph: {source}")
                } else if let Some(image_index) = image_index {
                    write!(
                        f,
                        "image bytes rebuild a different package graph at input position {image_index}"
                    )
                } else {
                    f.write_str("image bytes rebuild a different package graph")
                }
            }
            Self::BindingPlanMismatch {
                slot_position,
                plan_slots,
                expected_slots,
                inspection_errors,
            } => write!(
                f,
                "binding plan does not match the validated package and bases (first slot difference {slot_position:?}, {plan_slots} slots versus {expected_slots}, inspection-error mismatch {inspection_errors})"
            ),
            Self::SlotOutsideReservation {
                consumer,
                slot_rva,
                slot_address,
                image,
            } => write!(
                f,
                "IAT slot RVA {slot_rva:#x} at {slot_address:#x} for {consumer} is outside the image reservation [{:#x}, {:#x})",
                image.start, image.end
            ),
            Self::SlotImageBackingMissing {
                consumer,
                slot_rva,
                slot_address,
            } => write!(
                f,
                "IAT slot RVA {slot_rva:#x} at {slot_address:#x} for {consumer} is not continuously backed by the mapped image"
            ),
            Self::WriteFailed {
                consumer,
                slot_rva,
                slot_address,
                source,
            } => write!(
                f,
                "writing IAT slot RVA {slot_rva:#x} at {slot_address:#x} for {consumer} failed: {source}"
            ),
        }
    }
}

impl std::error::Error for ModuleIatError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidImage { source, .. } => Some(source),
            Self::ImageNotMappableAtReservation { source, .. } => Some(source),
            Self::ImageGraphMismatch {
                source: Some(source),
                ..
            } => Some(source),
            Self::WriteFailed { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Whether one planned slot was written or remains an unresolved diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IatSlotOutcome {
    /// The complete eight-byte slot was written. The value is the checked
    /// binding data whose `target_address` is now stored at `slot_address`.
    Written(ResolvedBinding),
    /// The binding planner left this slot unresolved. The writer did not
    /// invent a zero, fallback or host address for it.
    Unresolved(UnresolvedBindingReason),
}

/// Deterministic binding evidence for one ordinary import-table entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IatSlotReport {
    /// Module whose image contains the import address table.
    pub consumer: ModuleReference,
    /// RVA of the eight-byte import address table slot.
    pub slot_rva: u32,
    /// Module spelling exactly as requested by the import descriptor.
    pub requested_module: String,
    /// Module spelling after the graph's Windows reference normalization.
    pub normalized_module: String,
    /// Requested exact-case symbol name or ordinal.
    pub requested_symbol: ImportName,
    /// Forwarder edges traversed before the terminal outcome.
    pub forwarder_chain: Vec<ForwarderStep>,
    /// Whether the slot was written or remains unresolved.
    pub outcome: IatSlotOutcome,
}

/// Slot reports for one input image, in ordinary import-table order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageIatReport {
    pub image_index: usize,
    pub slots: Vec<IatSlotReport>,
}

/// Deterministic result of a successful transactional binding batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleIatReport {
    /// Every input image in positional package order, including images with no
    /// ordinary imports.
    pub images: Vec<ImageIatReport>,
    /// Table-inspection errors retained unchanged from the binding plan. Their
    /// presence means the plan is partial inventory; it does not fabricate
    /// outcomes for entries the parser could not observe.
    pub inspection_errors: Vec<ModuleInspectionError>,
}

impl ModuleIatReport {
    /// Total ordinary import entries represented in the report.
    pub fn slot_count(&self) -> usize {
        self.images.iter().map(|image| image.slots.len()).sum()
    }

    /// Slots whose resolved target address was written successfully.
    pub fn written_slot_count(&self) -> usize {
        self.images
            .iter()
            .flat_map(|image| &image.slots)
            .filter(|slot| matches!(slot.outcome, IatSlotOutcome::Written(_)))
            .count()
    }

    /// Slots left unwritten because the binding plan was unresolved.
    pub fn unresolved_slot_count(&self) -> usize {
        self.slot_count() - self.written_slot_count()
    }

    /// Whether every ordinary import and export table parsed completely. This
    /// does not imply that referenced modules or exports exist.
    pub fn inspection_is_complete(&self) -> bool {
        self.inspection_errors.is_empty()
    }
}

/// Bind one explicit module package's planned ordinary IAT slots in
/// `memory` as one transaction.
///
/// The positional inputs must describe the same package: `reservations` is in
/// input order with exactly one successful record per image, `graph` was built
/// from the same image bytes in that order, and `plan` is exactly the binding
/// plan for that graph at the reservation bases. All package-level validation
/// finishes before mutation. On success, every resolved plan slot was written;
/// unresolved slots are reported and retain their prior memory bytes.
///
/// If slot validation or a memory write fails, the complete pre-binding memory
/// snapshot is restored before the structured error is returned.
pub fn bind_module_iat(
    images: &[ExplicitModuleImage<'_>],
    reservations: &[ModuleReservation],
    graph: &ModuleGraph,
    plan: &ModuleBindingPlan,
    memory: &mut GuestMemory,
) -> Result<ModuleIatReport, ModuleIatError> {
    validate_counts(images, reservations, graph)?;
    validate_reservations(images, reservations)?;
    validate_images(images)?;
    validate_image_graph(images, graph)?;

    let bases: Vec<u64> = reservations
        .iter()
        .map(|reservation| reservation.base)
        .collect();
    validate_binding_plan(graph, &bases, plan)?;

    if images.is_empty() {
        return write_slots(plan, reservations, images, memory);
    }

    let before = memory.clone();
    match write_slots(plan, reservations, images, memory) {
        Ok(report) => Ok(report),
        Err(error) => {
            *memory = before;
            Err(error)
        }
    }
}

fn validate_counts(
    images: &[ExplicitModuleImage<'_>],
    reservations: &[ModuleReservation],
    graph: &ModuleGraph,
) -> Result<(), ModuleIatError> {
    if reservations.len() == images.len() && graph.modules().len() == images.len() {
        return Ok(());
    }
    Err(ModuleIatError::InputCountMismatch {
        module_images: images.len(),
        reservations: reservations.len(),
        graph_modules: graph.modules().len(),
    })
}

fn validate_reservations(
    images: &[ExplicitModuleImage<'_>],
    reservations: &[ModuleReservation],
) -> Result<(), ModuleIatError> {
    for (position, reservation) in reservations.iter().enumerate() {
        let image_index = position;
        if reservation.image_index != image_index {
            return Err(ModuleIatError::ReservationIndexMismatch {
                position,
                image_index: reservation.image_index,
            });
        }

        let image_size = images[image_index].pe.size_of_image;
        let valid_span = image_size != 0
            && reservation
                .base
                .checked_add(u64::from(image_size))
                .is_some_and(|end| end == reservation.end);
        if !valid_span {
            return Err(ModuleIatError::InvalidReservationSpan {
                image_index,
                base: reservation.base,
                end: reservation.end,
                image_size,
            });
        }
        if reservation.base % IMAGE_BASE_ALIGNMENT != 0 {
            return Err(ModuleIatError::UnalignedReservationBase {
                image_index,
                base: reservation.base,
            });
        }

        let preferred_base = images[image_index].pe.image_base;
        if reservation.used_preferred_base != (reservation.base == preferred_base) {
            return Err(ModuleIatError::ReservationPreferredBaseMismatch {
                image_index,
                base: reservation.base,
                preferred_base,
                used_preferred_base: reservation.used_preferred_base,
            });
        }
        if images[image_index].pe.characteristics & IMAGE_FILE_RELOCS_STRIPPED != 0
            && reservation.base != preferred_base
        {
            return Err(ModuleIatError::FixedBaseReservationMismatch {
                image_index,
                base: reservation.base,
                preferred_base,
            });
        }
        images[image_index]
            .pe
            .validate_mapping_at(images[image_index].bytes, reservation.base)
            .map_err(|source| ModuleIatError::ImageNotMappableAtReservation {
                image_index,
                base: reservation.base,
                source,
            })?;

        let entry_rva = images[image_index].pe.entry_point_rva;
        let expected = (entry_rva != 0)
            .then(|| reservation.base.checked_add(u64::from(entry_rva)))
            .flatten();
        if reservation.entry_point != expected {
            return Err(ModuleIatError::ReservationEntryPointMismatch {
                image_index,
                expected,
                actual: reservation.entry_point,
            });
        }
    }

    reject_overlapping_reservations(reservations)
}

fn reject_overlapping_reservations(
    reservations: &[ModuleReservation],
) -> Result<(), ModuleIatError> {
    let mut ordered: Vec<_> = reservations.iter().copied().enumerate().collect();
    ordered.sort_by_key(|&(index, reservation)| (reservation.base, reservation.end, index));

    for pair in ordered.windows(2) {
        let (first_index, first) = pair[0];
        let (second_index, second) = pair[1];
        if second.base >= first.end {
            continue;
        }
        return Err(ModuleIatError::OverlappingReservations {
            first_index,
            first: AddressInterval::new(first.base, first.end),
            second_index,
            second: AddressInterval::new(second.base, second.end),
            overlap: AddressInterval::new(first.base.max(second.base), first.end.min(second.end)),
        });
    }
    Ok(())
}

fn same_parsed_pe(left: &ParsedPe, right: &ParsedPe) -> bool {
    left.machine == right.machine
        && left.number_of_sections == right.number_of_sections
        && left.time_date_stamp == right.time_date_stamp
        && left.characteristics == right.characteristics
        && left.image_base == right.image_base
        && left.section_alignment == right.section_alignment
        && left.file_alignment == right.file_alignment
        && left.size_of_image == right.size_of_image
        && left.size_of_headers == right.size_of_headers
        && left.entry_point_rva == right.entry_point_rva
        && left.sections == right.sections
}

fn validate_images(images: &[ExplicitModuleImage<'_>]) -> Result<(), ModuleIatError> {
    for (image_index, image) in images.iter().enumerate() {
        let parsed =
            ParsedPe::parse(image.bytes).map_err(|source| ModuleIatError::InvalidImage {
                image_index,
                source,
            })?;
        if !same_parsed_pe(&parsed, image.pe) {
            return Err(ModuleIatError::ImageMetadataMismatch { image_index });
        }
    }
    Ok(())
}

fn graph_error_image_index(error: &ModuleGraphError) -> Option<usize> {
    match error {
        ModuleGraphError::TooManyModules { .. } => None,
        ModuleGraphError::InvalidFileName { index, .. } => Some(*index),
        ModuleGraphError::DuplicateModuleIdentity { second_index, .. } => Some(*second_index),
        ModuleGraphError::InvalidPe { module, .. }
        | ModuleGraphError::TruncatedDataDirectory { module, .. } => Some(module.index),
    }
}

fn validate_image_graph(
    images: &[ExplicitModuleImage<'_>],
    graph: &ModuleGraph,
) -> Result<(), ModuleIatError> {
    let package: Vec<_> = images
        .iter()
        .zip(graph.modules())
        .map(|(image, node)| PackageModule {
            file_name: node.file_name.as_str(),
            bytes: image.bytes,
        })
        .collect();
    let rebuilt =
        ModuleGraph::build(&package).map_err(|source| ModuleIatError::ImageGraphMismatch {
            image_index: graph_error_image_index(&source),
            source: Some(source),
        })?;
    if rebuilt == *graph {
        return Ok(());
    }

    let image_index = rebuilt
        .modules()
        .iter()
        .zip(graph.modules())
        .position(|(rebuilt_node, input_node)| rebuilt_node != input_node);
    Err(ModuleIatError::ImageGraphMismatch {
        image_index,
        source: None,
    })
}

fn validate_binding_plan(
    graph: &ModuleGraph,
    bases: &[u64],
    plan: &ModuleBindingPlan,
) -> Result<(), ModuleIatError> {
    let expected = ModuleBindingPlan::build(graph, bases)
        .expect("validated base count matches the graph module count");
    if expected == *plan {
        return Ok(());
    }

    let slot_position = expected
        .slots()
        .iter()
        .zip(plan.slots())
        .position(|(expected_slot, plan_slot)| expected_slot != plan_slot)
        .or_else(|| {
            (expected.slots().len() != plan.slots().len())
                .then_some(expected.slots().len().min(plan.slots().len()))
        });
    Err(ModuleIatError::BindingPlanMismatch {
        slot_position,
        plan_slots: plan.slots().len(),
        expected_slots: expected.slots().len(),
        inspection_errors: expected.inspection_errors() != plan.inspection_errors(),
    })
}

fn validate_slot_location(
    slot: &IatSlotPlan,
    binding: &ResolvedBinding,
    reservation: &ModuleReservation,
    image: &ExplicitModuleImage<'_>,
) -> Result<(), ModuleIatError> {
    let image_interval = AddressInterval::new(reservation.base, reservation.end);
    let Some(slot_end) = binding.slot_address.checked_add(IAT_SLOT_BYTES) else {
        return Err(ModuleIatError::SlotOutsideReservation {
            consumer: slot.consumer.clone(),
            slot_rva: slot.slot_rva,
            slot_address: binding.slot_address,
            image: image_interval,
        });
    };
    if binding.slot_address < reservation.base || slot_end > reservation.end {
        return Err(ModuleIatError::SlotOutsideReservation {
            consumer: slot.consumer.clone(),
            slot_rva: slot.slot_rva,
            slot_address: binding.slot_address,
            image: image_interval,
        });
    }
    if !image
        .pe
        .mapped_rva_range_is_backed(slot.slot_rva, IAT_SLOT_BYTES)
    {
        return Err(ModuleIatError::SlotImageBackingMissing {
            consumer: slot.consumer.clone(),
            slot_rva: slot.slot_rva,
            slot_address: binding.slot_address,
        });
    }
    Ok(())
}

fn slot_report(slot: &IatSlotPlan, outcome: IatSlotOutcome) -> IatSlotReport {
    IatSlotReport {
        consumer: slot.consumer.clone(),
        slot_rva: slot.slot_rva,
        requested_module: slot.requested_module.clone(),
        normalized_module: slot.normalized_module.clone(),
        requested_symbol: slot.requested_symbol.clone(),
        forwarder_chain: slot.forwarder_chain.clone(),
        outcome,
    }
}

fn write_slots(
    plan: &ModuleBindingPlan,
    reservations: &[ModuleReservation],
    module_images: &[ExplicitModuleImage<'_>],
    memory: &mut GuestMemory,
) -> Result<ModuleIatReport, ModuleIatError> {
    let mut image_reports: Vec<_> = reservations
        .iter()
        .map(|reservation| ImageIatReport {
            image_index: reservation.image_index,
            slots: Vec::new(),
        })
        .collect();

    for slot in plan.slots() {
        let outcome = match &slot.outcome {
            BindingOutcome::Resolved(binding) => {
                // The exact-plan check makes the consumer index valid, and the
                // exact-one reservation check makes its image index valid.
                let reservation = reservations[slot.consumer.index];
                let image = &module_images[reservation.image_index];
                validate_slot_location(slot, binding, &reservation, image)?;
                memory
                    .write_u64_bypass(binding.slot_address, binding.target_address)
                    .map_err(|source| ModuleIatError::WriteFailed {
                        consumer: slot.consumer.clone(),
                        slot_rva: slot.slot_rva,
                        slot_address: binding.slot_address,
                        source,
                    })?;
                IatSlotOutcome::Written(binding.clone())
            }
            BindingOutcome::Unresolved(reason) => IatSlotOutcome::Unresolved(reason.clone()),
        };
        image_reports[slot.consumer.index]
            .slots
            .push(slot_report(slot, outcome));
    }

    Ok(ModuleIatReport {
        images: image_reports,
        inspection_errors: plan.inspection_errors().to_vec(),
    })
}
