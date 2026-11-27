// SPDX-License-Identifier: GPL-3.0-or-later

//! Non-executing initialization-order planning for explicit module batches.
//!
//! The planner deliberately reuses [`ModuleGraph`]'s dependency condensation
//! instead of inventing initialization order. It validates that the supplied
//! graph, raw package bytes, positional parsed images and reservations describe
//! the same batch, then annotates each ordered acyclic component with entry
//! metadata. When TLS preparation reports are supplied, they are validated and
//! their discovered callbacks are carried too. Circular strongly
//! connected components remain explicit diagnostics and receive no invented
//! order. No TLS callback, `DllMain`, process entry point or other guest code
//! is executed.
//!
//! Rebuilding the supplied graph from the supplied package is a bounded but
//! deliberately conservative validation cost. It proves that the graph was not
//! stale or constructed for different bytes; positional byte equality then ties
//! those package inputs to the explicit parsed images. Host file search,
//! loader-lock semantics, thread policy, unloading and reference counting are
//! outside this layer.

use std::fmt;

use crate::export_table::ExportTableError;
use crate::import_table::ImportTableError;
use crate::module_graph::{DependencyComponent, ModuleGraph, ModuleGraphError, PackageModule};
use crate::module_memory::{
    ExplicitModuleImage, ModuleReservation, ModuleReservationError, ReservationSetError,
    validate_reservations,
};
use crate::module_tls::ModuleTlsReport;

/// Entry work associated with one module. `DllMain` and `ProcessEntry` are
/// distinguished honestly; neither has been executed by the planner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleInitializationEntry {
    /// The image has no entry point. A DLL entry RVA of zero is valid and
    /// means there is no `DllMain` to initialize later.
    NoEntryPoint,
    /// A DLL entry point at its actual reserved base, commonly `DllMain`.
    DllMain { address: u64 },
    /// A non-DLL image entry point at its actual reserved base. This is not
    /// `DllMain` and is not module initialization work.
    ProcessEntry { address: u64 },
}

/// Initialization metadata for one module. Package and image indexes are equal
/// in this first positional interface. TLS callbacks are rebased guest addresses
/// discovered by the TLS preparation layer; they are never executed here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleInitializationMetadata {
    pub package_index: usize,
    pub image_index: usize,
    pub file_name: String,
    pub entry: ModuleInitializationEntry,
    pub tls_callbacks: Vec<u64>,
}

/// One dependency-condensation component annotated for a later initialization
/// executor. `Acyclic` means only that this module is not part of a dependency
/// cycle; it does not mean initialization is safe when inspection is incomplete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleInitializationComponent {
    Acyclic(ModuleInitializationMetadata),
    /// Members of a circular strongly connected component, sorted by package
    /// index for identity only. Their relative initialization order is
    /// deliberately unresolved.
    Circular(Vec<ModuleInitializationMetadata>),
}

/// A module whose import or export inspection did not complete. Since hidden
/// dependencies can change initialization ordering, a plan containing any of
/// these records is static metadata rather than execution-ready ordering proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModuleInspectionUncertainty {
    pub package_index: usize,
    pub image_index: usize,
    pub import_error: Option<ImportTableError>,
    pub export_error: Option<ExportTableError>,
}

/// Deterministic non-executing initialization-order plan. Component order is
/// provider-before-consumer at component granularity. Circular members remain
/// grouped and unordered, and incomplete table inspection is separately
/// explicit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleInitializationPlan {
    pub components: Vec<ModuleInitializationComponent>,
    pub inspection_incomplete: Vec<ModuleInspectionUncertainty>,
}

impl ModuleInitializationPlan {
    /// Whether every component is acyclic and all import/export inspection
    /// completed. Even then this only means the static ordering proof is
    /// complete; no loader-lock, thread or execution semantics are implied.
    pub fn ordering_proof_is_complete(&self) -> bool {
        self.inspection_incomplete.is_empty()
            && self
                .components
                .iter()
                .all(|component| matches!(component, ModuleInitializationComponent::Acyclic(_)))
    }
}

/// Errors that prevent construction of an initialization-order plan.
#[derive(Debug, PartialEq, Eq)]
pub enum ModuleInitializationError {
    /// Package and positional image slices must have the same length.
    InputLengthMismatch { package: usize, images: usize },
    /// A supplied TLS-report slice must have exactly one report per input image.
    TlsReportInputMismatch {
        tls_reports: usize,
        module_images: usize,
    },
    /// The supplied package itself cannot produce a valid bounded graph.
    GraphBuildFailed { source: ModuleGraphError },
    /// A graph rebuilt from the supplied package differs from the caller's
    /// graph, so the graph is stale or describes different input.
    GraphMismatch,
    /// Positional package bytes and explicit-module bytes differ.
    PackageImageMismatch { index: usize },
    /// A TLS report index is outside the input batch or repeats an earlier one.
    InvalidTlsReportIndex { position: usize, image_index: usize },
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
}

impl From<ReservationSetError> for ModuleInitializationError {
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

impl fmt::Display for ModuleInitializationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputLengthMismatch { package, images } => write!(
                f,
                "package and image input lengths differ: {package} and {images}"
            ),
            Self::TlsReportInputMismatch {
                tls_reports,
                module_images,
            } => write!(
                f,
                "{tls_reports} TLS reports do not describe {module_images} module images exactly once"
            ),
            Self::GraphBuildFailed { source } => {
                write!(f, "package graph cannot be rebuilt: {source}")
            }
            Self::GraphMismatch => {
                f.write_str("supplied graph does not match the supplied package bytes")
            }
            Self::PackageImageMismatch { index } => write!(
                f,
                "package bytes and explicit image bytes differ at positional index {index}"
            ),
            Self::InvalidTlsReportIndex {
                position,
                image_index,
            } => write!(
                f,
                "TLS report {position} has missing, repeated or out-of-batch image index {image_index}"
            ),
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
        }
    }
}

impl std::error::Error for ModuleInitializationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::GraphBuildFailed { source } => Some(source),
            Self::InvalidReservation { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Build deterministic non-executing initialization metadata for one explicit
/// positional module batch.
///
/// The positional contract is strict: `package[i]`, `images[i]` and the
/// reservation for `image_index == i` describe the same bytes. When TLS reports
/// are supplied, exactly one report for `image_index == i` is also required.
/// Passing `None` allows the order to be planned before TLS preparation; the
/// same image indexes later let a caller combine both outputs. `images` input
/// order is not required to be dependency order; the graph's component order
/// determines the returned plan.
/// Circular components and incomplete inspection remain explicit, so callers
/// cannot mistake them for executable initialization order.
pub fn plan_module_initialization(
    package: &[PackageModule<'_>],
    images: &[ExplicitModuleImage<'_>],
    graph: &ModuleGraph,
    reservations: &[ModuleReservation],
    tls_reports: Option<&[ModuleTlsReport]>,
) -> Result<ModuleInitializationPlan, ModuleInitializationError> {
    if package.len() != images.len() {
        return Err(ModuleInitializationError::InputLengthMismatch {
            package: package.len(),
            images: images.len(),
        });
    }
    let tls_reports = match tls_reports {
        Some(reports) => {
            if reports.len() != images.len() {
                return Err(ModuleInitializationError::TlsReportInputMismatch {
                    tls_reports: reports.len(),
                    module_images: images.len(),
                });
            }
            reports
        }
        None => &[],
    };
    let ordered_reservations =
        validate_reservations(images, reservations).map_err(ModuleInitializationError::from)?;

    let rebuilt = ModuleGraph::build(package)
        .map_err(|source| ModuleInitializationError::GraphBuildFailed { source })?;
    if rebuilt != *graph {
        return Err(ModuleInitializationError::GraphMismatch);
    }

    let mut tls_by_image = vec![None; images.len()];
    for (position, report) in tls_reports.iter().enumerate() {
        if report.image_index >= images.len() || tls_by_image[report.image_index].is_some() {
            return Err(ModuleInitializationError::InvalidTlsReportIndex {
                position,
                image_index: report.image_index,
            });
        }
        tls_by_image[report.image_index] = Some(report);
    }

    let mut metadata = Vec::with_capacity(images.len());
    for index in 0..images.len() {
        if package[index].bytes != images[index].bytes {
            return Err(ModuleInitializationError::PackageImageMismatch { index });
        }
        let reservation = ordered_reservations[index];
        let entry = match reservation.entry_point {
            None => ModuleInitializationEntry::NoEntryPoint,
            Some(address) if graph.modules()[index].is_dll => {
                ModuleInitializationEntry::DllMain { address }
            }
            Some(address) => ModuleInitializationEntry::ProcessEntry { address },
        };
        let tls_callbacks = tls_by_image[index]
            .and_then(|report| report.prepared.as_ref())
            .map(|prepared| prepared.callbacks.clone())
            .unwrap_or_default();
        metadata.push(ModuleInitializationMetadata {
            package_index: index,
            image_index: index,
            file_name: package[index].file_name.to_owned(),
            entry,
            tls_callbacks,
        });
    }

    let components = graph
        .dependency_order()
        .iter()
        .map(|component| match component {
            DependencyComponent::Acyclic(index) => {
                ModuleInitializationComponent::Acyclic(metadata[*index].clone())
            }
            DependencyComponent::Circular(indexes) => ModuleInitializationComponent::Circular(
                indexes
                    .iter()
                    .map(|index| metadata[*index].clone())
                    .collect(),
            ),
        })
        .collect();
    let inspection_incomplete = graph
        .modules()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.import_error.is_some() || node.export_error.is_some())
        .map(|(index, node)| ModuleInspectionUncertainty {
            package_index: index,
            image_index: index,
            import_error: node.import_error,
            export_error: node.export_error,
        })
        .collect();

    Ok(ModuleInitializationPlan {
        components,
        inspection_incomplete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module_tls::PreparedModuleTls;
    use crate::pe::ParsedPe;

    const BASE: u64 = 0x1_4000_0000;
    const OPTIONAL: usize = 0x98;
    const CODE_RVA: u32 = 0x1000;
    const DATA_RVA: u32 = 0x2000;
    const DATA_RAW: usize = 0x400;
    const DATA_LEN: usize = 0x4000;
    const DATA_LEN_U32: u32 = DATA_LEN as u32;
    const EXPORT_RVA: u32 = DATA_RVA;
    const EXPORT_LEN: u32 = 0x1000;
    const EXPORT_END: u32 = EXPORT_RVA + EXPORT_LEN;
    const EAT_RVA: u32 = EXPORT_RVA + 0x100;
    const NPT_RVA: u32 = EXPORT_RVA + 0x200;
    const EOT_RVA: u32 = EXPORT_RVA + 0x280;
    const EXPORT_STRINGS: u32 = EXPORT_RVA + 0x300;
    const IMPORT_RVA: u32 = EXPORT_END;
    const IMPORT_LEN: u32 = 0x1000;
    const IMPORT_END: u32 = IMPORT_RVA + IMPORT_LEN;

    enum ImportSpec {
        Named { module: String, symbol: String },
        BrokenDescriptor,
    }

    fn named(module: &str, symbol: &str) -> ImportSpec {
        ImportSpec::Named {
            module: module.to_owned(),
            symbol: symbol.to_owned(),
        }
    }

    struct ExportSpec {
        module_name: String,
        exported_names: Vec<String>,
    }

    fn export(module_name: &str, exported_names: &[&str]) -> ExportSpec {
        ExportSpec {
            module_name: module_name.to_owned(),
            exported_names: exported_names
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
        }
    }

    fn put(pe: &mut [u8], at: usize, bytes: &[u8]) {
        pe[at..at + bytes.len()].copy_from_slice(bytes);
    }

    fn put16(pe: &mut [u8], at: usize, value: u16) {
        put(pe, at, &value.to_le_bytes());
    }

    fn put32(pe: &mut [u8], at: usize, value: u32) {
        put(pe, at, &value.to_le_bytes());
    }

    fn data_offset(rva: u32) -> usize {
        DATA_RAW + (rva - DATA_RVA) as usize
    }

    fn put32_rva(pe: &mut [u8], rva: u32, value: u32) {
        put32(pe, data_offset(rva), value);
    }

    fn allocate(cursor: &mut u32, len: usize, end: u32) -> u32 {
        let start = *cursor;
        let len = u32::try_from(len).unwrap();
        let next = start.checked_add(len).unwrap();
        *cursor = next.next_multiple_of(8);
        assert!(*cursor <= end, "fixture data does not fit in its region");
        start
    }

    fn write_string(pe: &mut [u8], cursor: &mut u32, end: u32, text: &str) -> u32 {
        let rva = allocate(cursor, text.len() + 1, end);
        put(pe, data_offset(rva), text.as_bytes());
        rva
    }

    fn write_exports(pe: &mut [u8], exports: &ExportSpec) {
        put32(pe, OPTIONAL + 112, EXPORT_RVA);
        put32(pe, OPTIONAL + 116, EXPORT_LEN);
        let mut cursor = EXPORT_STRINGS;
        let module = write_string(pe, &mut cursor, EXPORT_END, &exports.module_name);
        for (position, name) in exports.exported_names.iter().enumerate() {
            let name_rva = write_string(pe, &mut cursor, EXPORT_END, name);
            put32_rva(pe, NPT_RVA + 4 * position as u32, name_rva);
            put16(pe, data_offset(EOT_RVA) + 2 * position, 0);
        }
        put32_rva(pe, EAT_RVA, CODE_RVA);
        let name_count = exports.exported_names.len() as u32;
        put32_rva(pe, EXPORT_RVA + 12, module);
        put32_rva(pe, EXPORT_RVA + 16, 1);
        put32_rva(pe, EXPORT_RVA + 20, 1);
        put32_rva(pe, EXPORT_RVA + 24, name_count);
        put32_rva(pe, EXPORT_RVA + 28, EAT_RVA);
        put32_rva(
            pe,
            EXPORT_RVA + 32,
            if name_count == 0 { 0 } else { NPT_RVA },
        );
        put32_rva(
            pe,
            EXPORT_RVA + 36,
            if name_count == 0 { 0 } else { EOT_RVA },
        );
    }

    fn write_imports(pe: &mut [u8], imports: &[ImportSpec]) {
        put32(pe, OPTIONAL + 120, IMPORT_RVA);
        put32(pe, OPTIONAL + 124, IMPORT_LEN);
        let descriptor_len = 20 * (imports.len() as u32 + 1);
        let mut cursor = (IMPORT_RVA + descriptor_len).next_multiple_of(8);
        for (index, import) in imports.iter().enumerate() {
            let descriptor = data_offset(IMPORT_RVA) + 20 * index;
            if matches!(import, ImportSpec::BrokenDescriptor) {
                let lookup = allocate(&mut cursor, 16, IMPORT_END);
                let slot = allocate(&mut cursor, 16, IMPORT_END);
                put32(pe, descriptor, lookup);
                put32(pe, descriptor + 12, 0);
                put32(pe, descriptor + 16, slot);
                continue;
            }
            let ImportSpec::Named { module, symbol } = import else {
                unreachable!()
            };
            let hint = allocate(&mut cursor, symbol.len() + 3, IMPORT_END);
            put(pe, data_offset(hint) + 2, symbol.as_bytes());
            let module_rva = write_string(pe, &mut cursor, IMPORT_END, module);
            let lookup = allocate(&mut cursor, 16, IMPORT_END);
            let slot = allocate(&mut cursor, 16, IMPORT_END);
            put(pe, data_offset(lookup), &(u64::from(hint)).to_le_bytes());
            put(pe, data_offset(slot), &(u64::from(hint)).to_le_bytes());
            put32(pe, descriptor, lookup);
            put32(pe, descriptor + 12, module_rva);
            put32(pe, descriptor + 16, slot);
        }
    }

    fn build_image(dll: bool, imports: &[ImportSpec], exports: Option<&ExportSpec>) -> Vec<u8> {
        let mut pe = vec![0; DATA_RAW + DATA_LEN];
        put(&mut pe, 0, b"MZ");
        put32(&mut pe, 0x3c, 0x80);
        put(&mut pe, 0x80, b"PE\0\0");
        put16(&mut pe, 0x84, 0x8664);
        put16(&mut pe, 0x86, 2);
        put16(&mut pe, 0x94, 240);
        put16(&mut pe, 0x96, if dll { 0x2000 } else { 0 });
        put16(&mut pe, OPTIONAL, 0x20b);
        put32(&mut pe, OPTIONAL + 16, CODE_RVA);
        put(&mut pe, OPTIONAL + 24, &BASE.to_le_bytes());
        put32(&mut pe, OPTIONAL + 32, 0x1000);
        put32(&mut pe, OPTIONAL + 36, 0x200);
        put32(&mut pe, OPTIONAL + 56, DATA_RVA + DATA_LEN_U32);
        put32(&mut pe, OPTIONAL + 60, 0x200);
        put32(&mut pe, OPTIONAL + 108, 16);
        for (index, (name, rva, raw, len, flags)) in [
            (b".text", CODE_RVA, 0x200usize, 0x200usize, 0x6000_0020u32),
            (b".data", DATA_RVA, DATA_RAW, DATA_LEN, 0xc000_0040u32),
        ]
        .into_iter()
        .enumerate()
        {
            let section = OPTIONAL + 240 + 40 * index;
            put(&mut pe, section, name);
            put32(&mut pe, section + 8, len as u32);
            put32(&mut pe, section + 12, rva);
            put32(&mut pe, section + 16, len as u32);
            put32(&mut pe, section + 20, raw as u32);
            put32(&mut pe, section + 36, flags);
        }
        pe[0x200] = 0xc3;
        if !imports.is_empty() {
            write_imports(&mut pe, imports);
        }
        if let Some(exports) = exports {
            write_exports(&mut pe, exports);
        }
        pe
    }

    struct Batch {
        items: Vec<(String, Vec<u8>)>,
        pes: Vec<ParsedPe>,
    }

    impl Batch {
        fn new(items: Vec<(&str, Vec<u8>)>) -> Self {
            let items: Vec<_> = items
                .into_iter()
                .map(|(name, bytes)| (name.to_owned(), bytes))
                .collect();
            let pes = items
                .iter()
                .map(|(_, bytes)| ParsedPe::parse(bytes).unwrap())
                .collect();
            Self { items, pes }
        }

        fn package(&self) -> Vec<PackageModule<'_>> {
            self.items
                .iter()
                .map(|(name, bytes)| PackageModule {
                    file_name: name,
                    bytes,
                })
                .collect()
        }

        fn images(&self) -> Vec<ExplicitModuleImage<'_>> {
            self.pes
                .iter()
                .zip(&self.items)
                .map(|(pe, (_, bytes))| ExplicitModuleImage { pe, bytes })
                .collect()
        }

        fn reservations(&self) -> Vec<ModuleReservation> {
            self.pes
                .iter()
                .enumerate()
                .map(|(index, pe)| {
                    let base = 0x2_0000_0000 + index as u64 * 0x1_0000;
                    ModuleReservation {
                        image_index: index,
                        base,
                        end: base + u64::from(pe.size_of_image),
                        used_preferred_base: false,
                        entry_point: Some(base + u64::from(pe.entry_point_rva)),
                    }
                })
                .collect()
        }
    }

    fn tls_reports(count: usize, callbacks: &[(usize, u64)]) -> Vec<ModuleTlsReport> {
        (0..count)
            .map(|index| {
                let callback = callbacks
                    .iter()
                    .find(|(callback_index, _)| *callback_index == index)
                    .map(|(_, address)| *address);
                ModuleTlsReport {
                    image_index: index,
                    prepared: callback.map(|address| PreparedModuleTls {
                        tls_index: index as u32,
                        index_address: 0,
                        template_start: 0,
                        template_end: 0,
                        storage: None,
                        callbacks: vec![address],
                    }),
                }
            })
            .collect()
    }

    fn metadata(component: &ModuleInitializationComponent) -> &[ModuleInitializationMetadata] {
        match component {
            ModuleInitializationComponent::Acyclic(metadata) => std::slice::from_ref(metadata),
            ModuleInitializationComponent::Circular(metadata) => metadata,
        }
    }

    #[test]
    fn dependency_order_differs_from_input_order_and_carries_entry_and_tls_metadata() {
        let shared = build_image(true, &[], Some(&export("shared.dll", &["Value"])));
        let middle = build_image(
            true,
            &[named("shared.dll", "Value")],
            Some(&export("middle.dll", &["Middle"])),
        );
        let other = build_image(
            true,
            &[named("shared.dll", "Value")],
            Some(&export("other.dll", &["Other"])),
        );
        let root = build_image(
            false,
            &[named("middle.dll", "Middle"), named("other.dll", "Other")],
            None,
        );
        let batch = Batch::new(vec![
            ("root.exe", root),
            ("middle.dll", middle),
            ("shared.dll", shared),
            ("other.dll", other),
        ]);
        let package = batch.package();
        let images = batch.images();
        let graph = ModuleGraph::build(&package).unwrap();
        let reservations = batch.reservations();
        let tls = tls_reports(4, &[(1, 0xaaaa), (2, 0xbbbb)]);

        let plan = plan_module_initialization(&package, &images, &graph, &reservations, Some(&tls))
            .unwrap();
        let order: Vec<_> = plan
            .components
            .iter()
            .map(|component| metadata(component)[0].image_index)
            .collect();
        assert_eq!(order, [2, 1, 3, 0]);
        assert!(plan.ordering_proof_is_complete());
        assert_eq!(
            plan,
            plan_module_initialization(&package, &images, &graph, &reservations, Some(&tls))
                .unwrap()
        );
        let early =
            plan_module_initialization(&package, &images, &graph, &reservations, None).unwrap();
        let early_order: Vec<_> = early
            .components
            .iter()
            .map(|component| metadata(component)[0].image_index)
            .collect();
        assert_eq!(early_order, order);
        assert!(
            early
                .components
                .iter()
                .flat_map(metadata)
                .all(|module| module.tls_callbacks.is_empty())
        );

        let middle = metadata(&plan.components[1]);
        assert_eq!(middle[0].file_name, "middle.dll");
        assert_eq!(middle[0].tls_callbacks, [0xaaaa]);
        assert!(matches!(
            middle[0].entry,
            ModuleInitializationEntry::DllMain { .. }
        ));
        let root = metadata(&plan.components[3]);
        assert!(matches!(
            root[0].entry,
            ModuleInitializationEntry::ProcessEntry { .. }
        ));
        assert!(root[0].tls_callbacks.is_empty());
    }

    #[test]
    fn circular_dependencies_are_grouped_without_an_invented_order() {
        let first = build_image(
            true,
            &[named("second.dll", "Second")],
            Some(&export("first.dll", &["First"])),
        );
        let second = build_image(
            true,
            &[named("first.dll", "First")],
            Some(&export("second.dll", &["Second"])),
        );
        let batch = Batch::new(vec![("first.dll", first), ("second.dll", second)]);
        let package = batch.package();
        let images = batch.images();
        let graph = ModuleGraph::build(&package).unwrap();
        let reservations = batch.reservations();
        let tls = tls_reports(2, &[]);

        let plan = plan_module_initialization(&package, &images, &graph, &reservations, Some(&tls))
            .unwrap();
        assert_eq!(plan.components.len(), 1);
        let ModuleInitializationComponent::Circular(members) = &plan.components[0] else {
            panic!("cycle was not preserved: {:?}", plan.components);
        };
        assert_eq!(
            members
                .iter()
                .map(|member| member.image_index)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert!(!plan.ordering_proof_is_complete());
    }

    #[test]
    fn incomplete_import_inspection_is_reported_as_ordering_uncertainty() {
        let provider = build_image(true, &[], Some(&export("provider.dll", &["Value"])));
        let consumer = build_image(
            true,
            &[named("provider.dll", "Value"), ImportSpec::BrokenDescriptor],
            None,
        );
        let batch = Batch::new(vec![("consumer.dll", consumer), ("provider.dll", provider)]);
        let package = batch.package();
        let images = batch.images();
        let graph = ModuleGraph::build(&package).unwrap();
        let reservations = batch.reservations();
        let tls = tls_reports(2, &[]);

        let plan = plan_module_initialization(&package, &images, &graph, &reservations, Some(&tls))
            .unwrap();
        assert_eq!(plan.inspection_incomplete.len(), 1);
        assert_eq!(plan.inspection_incomplete[0].image_index, 0);
        assert!(plan.inspection_incomplete[0].import_error.is_some());
        assert!(!plan.ordering_proof_is_complete());
    }

    #[test]
    fn stale_graphs_positional_bytes_and_tls_report_indexes_are_rejected() {
        let first = build_image(true, &[], Some(&export("first.dll", &["First"])));
        let second = build_image(true, &[], Some(&export("second.dll", &["Second"])));
        let batch = Batch::new(vec![("first.dll", first), ("second.dll", second)]);
        let package = batch.package();
        let images = batch.images();
        let graph = ModuleGraph::build(&package).unwrap();
        let reservations = batch.reservations();
        let tls = tls_reports(2, &[]);

        let wrong_graph_batch = Batch::new(vec![
            (
                "first.dll",
                build_image(true, &[], Some(&export("first.dll", &["First"]))),
            ),
            (
                "other.dll",
                build_image(true, &[], Some(&export("second.dll", &["Second"]))),
            ),
        ]);
        let wrong_package = wrong_graph_batch.package();
        let wrong_graph = ModuleGraph::build(&wrong_package).unwrap();
        assert_eq!(
            plan_module_initialization(&package, &images, &wrong_graph, &reservations, Some(&tls)),
            Err(ModuleInitializationError::GraphMismatch)
        );

        let mut altered_images = images.clone();
        let mut altered_bytes = batch.items[0].1.clone();
        altered_bytes[0x400] ^= 1;
        altered_images[0].bytes = &altered_bytes;
        assert_eq!(
            plan_module_initialization(
                &package,
                &altered_images,
                &graph,
                &reservations,
                Some(&tls)
            ),
            Err(ModuleInitializationError::PackageImageMismatch { index: 0 })
        );

        assert_eq!(
            plan_module_initialization(&package, &images, &graph, &reservations, Some(&tls[..1])),
            Err(ModuleInitializationError::TlsReportInputMismatch {
                tls_reports: 1,
                module_images: 2,
            })
        );

        let duplicate_tls = vec![tls[0].clone(), tls[0].clone()];
        assert_eq!(
            plan_module_initialization(
                &package,
                &images,
                &graph,
                &reservations,
                Some(&duplicate_tls)
            ),
            Err(ModuleInitializationError::InvalidTlsReportIndex {
                position: 1,
                image_index: 0,
            })
        );
    }
}
