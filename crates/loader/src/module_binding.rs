// SPDX-License-Identifier: GPL-3.0-or-later

//! A bounded, non-executing import-address-table binding plan.
//!
//! This planner transforms the ordinary import resolutions already computed by
//! [`ModuleGraph`] into explicit write descriptions using caller-supplied module
//! base addresses. It does not parse another copy of the import or export
//! tables, search host files, resolve API-set schemas or runtime services, map
//! images, write guest memory, process delayed imports, relocate or initialize
//! modules, run TLS callbacks or execute guest instructions.
//!
//! Base addresses are indexed by the same package index as
//! [`ModuleGraph::modules`]. The planner checks the address arithmetic it
//! performs, but it does not validate base alignment, image extents, memory
//! permissions or overlap between modules: those are properties of a later
//! memory-mapping transaction, not of this arithmetic plan.
//!
//! Plans preserve the graph's deterministic input module and import-table
//! order. When import address tables do not overlap, there is one plan entry
//! for each observed imported-symbol PE32+ import address table slot. The
//! import parser does
//! not reject overlapping address tables; such input can describe overlapping
//! eight-byte slot writes. Every entry in one overlapping write cluster is
//! marked [`UnresolvedBindingReason::OverlappingIatSlot`] rather than letting
//! table order choose a winning write; all source entries remain visible.
//!
//! The plan is inventory data for a later transactional writer. A resolved
//! target address is only the checked sum of the final provider base and the
//! exported RVA. It is not evidence that the target is code, that the memory is
//! mapped or that executing it would be correct. Circular module dependencies
//! do not by themselves make a slot unresolved once bases are chosen; this
//! arithmetic layer says nothing about initialization order.

use std::collections::BTreeMap;
use std::fmt;

use crate::export_table::ExportTableError;
use crate::import_table::{ImportName, ImportTableError};
use crate::module_graph::{
    ExportKey, ForwarderStep, ImportOutcome, ImportResolution, ModuleGraph, ModuleReference,
    UnresolvedImportReason,
};

/// Width in bytes of one PE32+ import address table entry.
pub const IAT_SLOT_BYTES: u64 = 8;

/// Input shape errors that prevent a binding plan from being constructed.
/// Resolution and arithmetic failures are instead explicit per-slot outcomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleBindingError {
    /// The base-address slice must have one address for each package module,
    /// in package-index order.
    BaseAddressCount { expected: usize, actual: usize },
}

impl fmt::Display for ModuleBindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BaseAddressCount { expected, actual } => write!(
                f,
                "binding plan needs {expected} module base addresses in package order, got {actual}"
            ),
        }
    }
}

impl std::error::Error for ModuleBindingError {}

/// A structured import or export table inspection error retained from the
/// module graph. These errors mean the corresponding table was truncated or
/// malformed, not that any particular observed import failed to resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleInspectionError {
    Import {
        module: ModuleReference,
        error: ImportTableError,
    },
    Export {
        module: ModuleReference,
        error: ExportTableError,
    },
}

/// A fully checked resolved write description for one ordinary IAT slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedBinding {
    /// Final provider after every forwarded export edge. For a direct export
    /// this is the imported module; it is never the intermediate forwarder.
    pub provider: ModuleReference,
    /// Exact-case exported name or biased ordinal finally selected.
    pub export: ExportKey,
    /// Biased ordinal of the final export address-table slot.
    pub ordinal: u32,
    /// RVA supplied by the final provider's export table.
    pub target_rva: u32,
    /// Caller-supplied base address of the consuming module.
    pub consumer_base: u64,
    /// Checked consumer-side address of the eight-byte IAT slot. The planner
    /// also checks that the final byte of the slot is representable as a u64.
    pub slot_address: u64,
    /// Caller-supplied base address of the final provider module.
    pub provider_base: u64,
    /// Checked `provider_base + target_rva` value to write into the slot.
    pub target_address: u64,
}

/// Why one observed IAT slot cannot be bound. Graph resolution failures are
/// preserved without reinterpreting them; arithmetic and overlapping-slot
/// failures are added by this planner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnresolvedBindingReason {
    /// The dependency graph could not resolve the requested module or symbol.
    Import(UnresolvedImportReason),
    /// More than one ordinary import entry in this consumer belongs to the
    /// same connected cluster of overlapping eight-byte slot writes. Entries
    /// are counted in transitive overlap clusters, so overlapping writes cannot
    /// produce an order-dependent final byte pattern. This is a conservative
    /// structural diagnostic, even if the entries would coincidentally resolve
    /// to the same value. The graph's original outcome is preserved so an
    /// overlapping missing module or forwarder failure is not erased; binding
    /// arithmetic is not performed for the overlapped entry.
    OverlappingIatSlot {
        entries: usize,
        resolution: Box<ImportOutcome>,
    },
    /// `consumer_base + slot_rva` or the end of the eight-byte slot is not
    /// representable as a u64. This check is performed before target-address
    /// arithmetic, so it takes precedence when both would overflow.
    SlotAddressOverflow {
        consumer: ModuleReference,
        slot_rva: u32,
        consumer_base: u64,
    },
    /// `provider_base + target_rva` is not representable as a u64.
    TargetAddressOverflow {
        provider: ModuleReference,
        export: ExportKey,
        ordinal: u32,
        target_rva: u32,
        provider_base: u64,
    },
}

/// Binding outcome for one observed ordinary IAT slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingOutcome {
    Resolved(ResolvedBinding),
    Unresolved(UnresolvedBindingReason),
}

/// Deterministic binding data for one ordinary import-table entry. When slots
/// do not overlap, this is also one unique IAT slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IatSlotPlan {
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
    /// Forwarder edges traversed by the graph before the terminal outcome.
    /// The text is inventory data and is never treated as code.
    pub forwarder_chain: Vec<ForwarderStep>,
    /// Either the checked write description or the explicit reason no write is
    /// planned for this slot.
    pub outcome: BindingOutcome,
}

/// A deterministic, owned binding plan for an explicit package. Build it with
/// [`ModuleBindingPlan::build`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleBindingPlan {
    slots: Vec<IatSlotPlan>,
    inspection_errors: Vec<ModuleInspectionError>,
}

impl ModuleBindingPlan {
    /// Transform all ordinary import resolutions in `graph` into a binding
    /// plan. `base_addresses[index]` is the caller-supplied base of the module
    /// with package index `index`.
    ///
    /// The planner never writes the slots it describes. Unresolved graph
    /// resolutions remain visible per slot. An overlapping-slot diagnostic
    /// takes precedence because no write ordering is sound for that entry.
    /// Otherwise resolved graph outcomes are downgraded to arithmetic
    /// diagnostics if either the consumer slot span or final target address
    /// cannot be represented as a u64.
    pub fn build(graph: &ModuleGraph, base_addresses: &[u64]) -> Result<Self, ModuleBindingError> {
        let expected = graph.modules().len();
        if base_addresses.len() != expected {
            return Err(ModuleBindingError::BaseAddressCount {
                expected,
                actual: base_addresses.len(),
            });
        }

        let overlapping_slots = overlapping_slot_entries(graph.import_resolutions());
        let slots = graph
            .import_resolutions()
            .iter()
            .enumerate()
            .map(|(position, resolution)| IatSlotPlan {
                consumer: resolution.consumer.clone(),
                slot_rva: resolution.iat_rva,
                requested_module: resolution.requested_module.clone(),
                normalized_module: resolution.normalized_module.clone(),
                requested_symbol: resolution.symbol.clone(),
                forwarder_chain: resolution.forwarder_chain.clone(),
                outcome: plan_outcome(resolution, overlapping_slots[position], base_addresses),
            })
            .collect();

        Ok(Self {
            slots,
            inspection_errors: inspection_errors(graph),
        })
    }

    /// Slot plans in the dependency graph's deterministic input module and
    /// ordinary import-table order.
    pub fn slots(&self) -> &[IatSlotPlan] {
        &self.slots
    }

    /// Structured table errors retained from the graph, in package module
    /// order. Import errors precede export errors for the same module.
    pub fn inspection_errors(&self) -> &[ModuleInspectionError] {
        &self.inspection_errors
    }

    /// Whether all ordinary import and export tables parsed completely. This
    /// does not imply that referenced modules or exports exist or that slots
    /// resolved; inspect each slot outcome for those facts.
    pub fn inspection_is_complete(&self) -> bool {
        self.inspection_errors.is_empty()
    }
}

fn plan_outcome(
    resolution: &ImportResolution,
    overlapping_entries: Option<usize>,
    base_addresses: &[u64],
) -> BindingOutcome {
    if let Some(entries) = overlapping_entries {
        return BindingOutcome::Unresolved(UnresolvedBindingReason::OverlappingIatSlot {
            entries,
            resolution: Box::new(resolution.outcome.clone()),
        });
    }

    let (provider, export, ordinal, rva) = match &resolution.outcome {
        ImportOutcome::Resolved {
            provider,
            export,
            ordinal,
            rva,
        } => (provider, export, ordinal, rva),
        ImportOutcome::Unresolved(reason) => {
            return BindingOutcome::Unresolved(UnresolvedBindingReason::Import(reason.clone()));
        }
    };

    let consumer_base = base_addresses[resolution.consumer.index];
    let Some(slot_address) = checked_slot_address(consumer_base, resolution.iat_rva) else {
        return BindingOutcome::Unresolved(UnresolvedBindingReason::SlotAddressOverflow {
            consumer: resolution.consumer.clone(),
            slot_rva: resolution.iat_rva,
            consumer_base,
        });
    };

    let provider_base = base_addresses[provider.index];
    let target_rva = *rva;
    let Some(target_address) = provider_base.checked_add(u64::from(target_rva)) else {
        return BindingOutcome::Unresolved(UnresolvedBindingReason::TargetAddressOverflow {
            provider: provider.clone(),
            export: export.clone(),
            ordinal: *ordinal,
            target_rva,
            provider_base,
        });
    };

    BindingOutcome::Resolved(ResolvedBinding {
        provider: provider.clone(),
        export: export.clone(),
        ordinal: *ordinal,
        target_rva,
        consumer_base,
        slot_address,
        provider_base,
        target_address,
    })
}

/// Find import entries whose fixed-width IAT writes overlap within one
/// consumer. The result is aligned with `resolutions`; a value is the size of
/// the entry's transitive overlap cluster. Half-open intervals are used, so a
/// slot starting exactly where another ends does not overlap it.
fn overlapping_slot_entries(resolutions: &[ImportResolution]) -> Vec<Option<usize>> {
    let mut by_consumer = BTreeMap::<usize, Vec<usize>>::new();
    for (position, resolution) in resolutions.iter().enumerate() {
        by_consumer
            .entry(resolution.consumer.index)
            .or_default()
            .push(position);
    }

    let mut overlapping = vec![None; resolutions.len()];
    for positions in by_consumer.values() {
        let mut sorted = positions.clone();
        sorted.sort_by_key(|&position| (resolutions[position].iat_rva, position));
        let Some((&first, _)) = sorted.split_first() else {
            continue;
        };
        let mut cluster_start = 0;
        let mut cluster_end = slot_end(resolutions[first].iat_rva);
        for sorted_position in 1..sorted.len() {
            let position = sorted[sorted_position];
            let start = u64::from(resolutions[position].iat_rva);
            let end = slot_end(resolutions[position].iat_rva);
            if start < cluster_end {
                cluster_end = cluster_end.max(end);
            } else {
                record_overlap_cluster(&mut overlapping, &sorted[cluster_start..sorted_position]);
                cluster_start = sorted_position;
                cluster_end = end;
            }
        }
        record_overlap_cluster(&mut overlapping, &sorted[cluster_start..]);
    }
    overlapping
}

fn record_overlap_cluster(overlapping: &mut [Option<usize>], cluster: &[usize]) {
    if cluster.len() <= 1 {
        return;
    }
    for &position in cluster {
        overlapping[position] = Some(cluster.len());
    }
}

fn slot_end(rva: u32) -> u64 {
    u64::from(rva)
        .checked_add(IAT_SLOT_BYTES)
        .expect("u32 RVA plus the fixed IAT width always fits in u64")
}

fn checked_slot_address(base: u64, slot_rva: u32) -> Option<u64> {
    let start = base.checked_add(u64::from(slot_rva))?;
    let _last_byte = start.checked_add(IAT_SLOT_BYTES - 1)?;
    Some(start)
}

fn inspection_errors(graph: &ModuleGraph) -> Vec<ModuleInspectionError> {
    let mut errors = Vec::new();
    for (index, node) in graph.modules().iter().enumerate() {
        let module = || ModuleReference {
            index,
            file_name: node.file_name.clone(),
        };
        if let Some(error) = node.import_error {
            errors.push(ModuleInspectionError::Import {
                module: module(),
                error,
            });
        }
        if let Some(error) = node.export_error {
            errors.push(ModuleInspectionError::Export {
                module: module(),
                error,
            });
        }
    }
    errors
}
