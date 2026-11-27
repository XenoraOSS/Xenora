// SPDX-License-Identifier: GPL-3.0-or-later

//! Closed-world provisioning and non-executing runtime load preparation.
//!
//! The store holds authorized module images under bounded logical names. It is
//! populated by the host before execution; a prepared load can only name one
//! of those images and never turns a guest string into a host path. Registering
//! a module whose canonical name is already a service-registry key is refused,
//! so the merged resolver has one authoritative owner for each module identity.
//!
//! `prepare_runtime_load` collects a root's transitive package dependencies,
//! lays the selected images out in a caller-supplied arena and composes the
//! existing transactional map, relocate, exception, IAT and TLS seams. Service
//! registry keys are resolved first, then in-package exports; every import is
//! inspected before a guest byte is written. Any later failure restores the
//! caller's whole `GuestMemory` snapshot. No guest code runs here: TLS callbacks
//! are discovered but not invoked, and there is no `LoadLibrary` wiring yet.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use crate::imports::ImportRegistry;
use crate::memory::GuestMemory;
use crate::module_binding::{BindingOutcome, ModuleBindingError, ModuleBindingPlan};
use crate::module_exceptions::{
    ModuleExceptionError, ModuleExceptionReport, register_module_exception_directories,
};
use crate::module_graph::{
    DependencyComponent, FileNameError, ModuleGraph, ModuleGraphError, PackageModule,
    validate_file_name,
};
use crate::module_iat::{ModuleIatError, ModuleIatReport, bind_module_iat};
use crate::module_layout::{
    AddressInterval, ImageLayoutFailure, ModuleImageDescriptor, ModuleLayoutArena,
    ModuleLayoutError, ModuleLayoutPlan,
};
use crate::module_memory::{
    ExplicitModuleImage, ModuleMemoryError, ModuleRelocationError, ModuleReservation,
    map_module_batch, relocate_module_batch,
};
use crate::module_tls::{ModuleTlsError, ModuleTlsReport, prepare_module_tls};
use crate::package::{
    ImportDisposition, StartupDiagnostics, classify_unresolved, module_diagnostics, resolve_service,
};
use crate::pe::{ParsedPe, PeError};
use crate::reloc::RelocationReport;

/// Most modules accepted by one provisioned store. This reuses the package
/// graph's Xenora policy bound.
pub const MAX_PROVISIONED_MODULES: usize = crate::module_graph::MAX_PACKAGE_MODULES;
/// Aggregate bytes held by one store. This is a Xenora policy bound and is
/// checked before a new snapshot is inserted.
pub const MAX_PROVISIONED_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// One authorized module snapshot. The logical name is the closed-world key;
/// the exported DLL name inside the image is only PE metadata.
#[derive(Clone, Debug)]
pub struct ProvisionedModule {
    name: String,
    canonical_name: String,
    bytes: Arc<[u8]>,
}

impl ProvisionedModule {
    /// Caller-supplied logical name, preserved for diagnostics.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// ASCII-lowercase lookup identity.
    pub fn canonical_name(&self) -> &str {
        &self.canonical_name
    }

    /// Immutable image bytes admitted for this module.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Why a module snapshot was not admitted to the store.
#[derive(Debug)]
pub enum ModuleStoreError {
    TooManyModules {
        count: usize,
        max: usize,
    },
    InvalidName {
        name: String,
        reason: FileNameError,
    },
    DuplicateModule {
        name: String,
        canonical_name: String,
    },
    ServiceNameCollision {
        name: String,
        canonical_name: String,
    },
    AggregateBudgetExceeded {
        bytes: u64,
        max: u64,
        remaining: u64,
    },
    InvalidImage {
        name: String,
        source: ModuleGraphError,
    },
}

impl fmt::Display for ModuleStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyModules { count, max } => {
                write!(
                    f,
                    "the provisioned store already holds {count} of {max} modules"
                )
            }
            Self::InvalidName { name, reason } => {
                write!(f, "module name {name:?} is invalid: {reason}")
            }
            Self::DuplicateModule {
                name,
                canonical_name,
            } => write!(
                f,
                "module name {name:?} duplicates provisioned identity {canonical_name:?}"
            ),
            Self::ServiceNameCollision {
                name,
                canonical_name,
            } => write!(
                f,
                "module name {name:?} collides with service module {canonical_name:?}"
            ),
            Self::AggregateBudgetExceeded {
                bytes,
                max,
                remaining,
            } => write!(
                f,
                "module needs {bytes} bytes but only {remaining} remain in the {max}-byte store budget"
            ),
            Self::InvalidImage { name, source } => {
                write!(f, "module image {name:?} is invalid: {source}")
            }
        }
    }
}

impl std::error::Error for ModuleStoreError {}

/// A bounded map from logical module names to authorized image snapshots.
#[derive(Clone, Debug, Default)]
pub struct ProvisionedModuleStore {
    modules: Vec<ProvisionedModule>,
    index: BTreeMap<String, usize>,
    total_bytes: u64,
}

impl ProvisionedModuleStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.modules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Remaining aggregate budget, for callers admitting files incrementally.
    pub fn remaining_budget(&self) -> u64 {
        MAX_PROVISIONED_TOTAL_BYTES.saturating_sub(self.total_bytes)
    }

    pub fn modules(&self) -> impl Iterator<Item = &ProvisionedModule> {
        self.modules.iter()
    }

    pub fn get(&self, canonical_name: &str) -> Option<&ProvisionedModule> {
        self.index
            .get(&canonical_name.to_ascii_lowercase())
            .map(|index| &self.modules[*index])
    }

    /// Admit one immutable image snapshot under a bounded logical name.
    ///
    /// The registry is consulted only for collision detection. A service name
    /// may never also name a provisioned module; that keeps resolution
    /// deterministic when services are checked before package exports.
    pub fn insert(
        &mut self,
        name: impl Into<String>,
        bytes: impl Into<Arc<[u8]>>,
        services: &ImportRegistry,
    ) -> Result<(), ModuleStoreError> {
        let name = name.into();
        if let Err(reason) = validate_file_name(&name) {
            return Err(ModuleStoreError::InvalidName { name, reason });
        }
        let canonical_name = name.to_ascii_lowercase();
        if self.index.contains_key(&canonical_name) {
            return Err(ModuleStoreError::DuplicateModule {
                name,
                canonical_name,
            });
        }
        if services
            .iter_resolution_keys()
            .any(|(module, _, _)| module.eq_ignore_ascii_case(&canonical_name))
        {
            return Err(ModuleStoreError::ServiceNameCollision {
                name,
                canonical_name,
            });
        }
        if self.modules.len() >= MAX_PROVISIONED_MODULES {
            return Err(ModuleStoreError::TooManyModules {
                count: self.modules.len(),
                max: MAX_PROVISIONED_MODULES,
            });
        }

        let bytes = bytes.into();
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let remaining = self.remaining_budget();
        if size > remaining {
            return Err(ModuleStoreError::AggregateBudgetExceeded {
                bytes: size,
                max: MAX_PROVISIONED_TOTAL_BYTES,
                remaining,
            });
        }

        // A singleton graph validates the PE headers and both data-directory
        // shapes before the image becomes available to a prepared load.
        let package = [PackageModule {
            file_name: name.as_str(),
            bytes: &bytes,
        }];
        ModuleGraph::build(&package).map_err(|source| ModuleStoreError::InvalidImage {
            name: name.clone(),
            source,
        })?;

        let index = self.modules.len();
        self.index.insert(canonical_name.clone(), index);
        self.modules.push(ProvisionedModule {
            name,
            canonical_name,
            bytes,
        });
        self.total_bytes = self.total_bytes.saturating_add(size);
        Ok(())
    }
}

/// Why a runtime load could not be prepared.
#[derive(Debug)]
pub enum RuntimeLoadPreparationError {
    EmptyRoots,
    UnknownRoot {
        root: String,
    },
    Graph(ModuleGraphError),
    CircularDependency {
        modules: Vec<String>,
    },
    Pe(PeError),
    Layout(ModuleLayoutError),
    ImageNotPlaced {
        image_index: usize,
        reason: ImageLayoutFailure,
    },
    BindingPlan(ModuleBindingError),
    Map(ModuleMemoryError),
    Relocate(ModuleRelocationError),
    Exceptions(ModuleExceptionError),
    Bind(ModuleIatError),
    Tls(ModuleTlsError),
    /// All imports were inspected and at least one has no honest binding.
    /// The caller's memory is unchanged.
    UnresolvedImports(Box<StartupDiagnostics>),
    ImportBinding {
        detail: String,
        diagnostics: Box<StartupDiagnostics>,
    },
}

impl fmt::Display for RuntimeLoadPreparationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRoots => f.write_str("a runtime load needs at least one root module"),
            Self::UnknownRoot { root } => write!(f, "no provisioned module is named {root:?}"),
            Self::Graph(error) => write!(f, "module graph rejected: {error}"),
            Self::CircularDependency { modules } => {
                write!(f, "circular module dependency: {}", modules.join(", "))
            }
            Self::Pe(error) => write!(f, "image parse failed: {error}"),
            Self::Layout(error) => write!(f, "module layout rejected: {error}"),
            Self::ImageNotPlaced {
                image_index,
                reason,
            } => write!(f, "image {image_index} was not placed: {reason}"),
            Self::BindingPlan(error) => write!(f, "binding plan rejected: {error}"),
            Self::Map(error) => write!(f, "module mapping failed: {error}"),
            Self::Relocate(error) => write!(f, "module relocation failed: {error}"),
            Self::Exceptions(error) => write!(f, "exception registration failed: {error}"),
            Self::Bind(error) => write!(f, "import binding failed: {error}"),
            Self::Tls(error) => write!(f, "TLS preparation failed: {error}"),
            Self::UnresolvedImports(diagnostics) => write!(
                f,
                "{} required imports are unresolved",
                diagnostics.unresolved_imports().count()
            ),
            Self::ImportBinding { detail, .. } => write!(f, "import binding failed: {detail}"),
        }
    }
}

impl std::error::Error for RuntimeLoadPreparationError {}

impl RuntimeLoadPreparationError {
    pub fn diagnostics(&self) -> Option<&StartupDiagnostics> {
        match self {
            Self::UnresolvedImports(diagnostics) | Self::ImportBinding { diagnostics, .. } => {
                Some(diagnostics)
            }
            _ => None,
        }
    }
}

/// Successfully prepared modules. The images are mapped into the caller's
/// memory and ready for a later lifecycle layer to initialize; nothing here
/// has executed guest code.
#[derive(Debug)]
pub struct PreparedModuleLoad {
    pub roots: Vec<String>,
    pub graph: ModuleGraph,
    pub layout: ModuleLayoutPlan,
    pub reservations: Vec<ModuleReservation>,
    pub relocations: Vec<RelocationReport>,
    pub exceptions: Vec<ModuleExceptionReport>,
    pub tls: Vec<ModuleTlsReport>,
    pub imports: ModuleIatReport,
    /// Number of IAT slots bound to service trampolines or service data cells.
    pub service_bindings: usize,
    pub diagnostics: StartupDiagnostics,
}

/// Collect the roots' transitive in-package dependencies and prepare all
/// selected images as one transaction.
///
/// `occupied` must describe every region in the arena that must not be reused,
/// including the thread TLS array. Service keys are resolved before package
/// exports, while a store insertion has already refused a same-named module.
/// Any error after the first guest write restores `memory` to its entry state.
#[allow(clippy::too_many_arguments)]
pub fn prepare_runtime_load(
    store: &ProvisionedModuleStore,
    roots: &[&str],
    layout: ModuleLayoutArena,
    occupied: &[AddressInterval],
    services: &mut ImportRegistry,
    thread_tls_base: u64,
    memory: &mut GuestMemory,
) -> Result<PreparedModuleLoad, RuntimeLoadPreparationError> {
    if roots.is_empty() {
        return Err(RuntimeLoadPreparationError::EmptyRoots);
    }

    let all_modules: Vec<&ProvisionedModule> = store.modules().collect();
    let all_package: Vec<PackageModule<'_>> = all_modules
        .iter()
        .map(|module| PackageModule {
            file_name: module.name(),
            bytes: module.bytes(),
        })
        .collect();
    let all_graph = ModuleGraph::build(&all_package).map_err(RuntimeLoadPreparationError::Graph)?;

    let mut selected = BTreeSet::new();
    let mut queue = VecDeque::new();
    for root in roots {
        let canonical = root.to_ascii_lowercase();
        let index = store
            .get(&canonical)
            .and_then(|module| {
                all_graph
                    .modules()
                    .iter()
                    .position(|node| node.canonical_file_name == module.canonical_name())
            })
            .ok_or_else(|| RuntimeLoadPreparationError::UnknownRoot {
                root: (*root).to_owned(),
            })?;
        if selected.insert(index) {
            queue.push_back(index);
        }
    }
    while let Some(consumer) = queue.pop_front() {
        for edge in all_graph
            .dependency_edges()
            .iter()
            .filter(|edge| edge.consumer == consumer)
        {
            if selected.insert(edge.provider) {
                queue.push_back(edge.provider);
            }
        }
    }

    let selected_package: Vec<PackageModule<'_>> = selected
        .iter()
        .map(|&index| PackageModule {
            file_name: all_modules[index].name(),
            bytes: all_modules[index].bytes(),
        })
        .collect();
    let graph =
        ModuleGraph::build(&selected_package).map_err(RuntimeLoadPreparationError::Graph)?;
    for component in graph.dependency_order() {
        if let DependencyComponent::Circular(indexes) = component {
            return Err(RuntimeLoadPreparationError::CircularDependency {
                modules: indexes
                    .iter()
                    .map(|&index| graph.modules()[index].file_name.clone())
                    .collect(),
            });
        }
    }

    let parsed: Vec<ParsedPe> = selected_package
        .iter()
        .map(|module| ParsedPe::parse(module.bytes))
        .collect::<Result<_, PeError>>()
        .map_err(RuntimeLoadPreparationError::Pe)?;
    let images: Vec<ExplicitModuleImage<'_>> = parsed
        .iter()
        .zip(selected_package.iter())
        .map(|(pe, module)| ExplicitModuleImage {
            pe,
            bytes: module.bytes,
        })
        .collect();
    let descriptors: Vec<ModuleImageDescriptor> = parsed
        .iter()
        .map(ModuleImageDescriptor::from_parsed_pe)
        .collect();
    let plan = ModuleLayoutPlan::build(layout, occupied, &descriptors)
        .map_err(RuntimeLoadPreparationError::Layout)?;
    for image in plan.images() {
        if let crate::module_layout::ImageLayoutOutcome::Failed(reason) = image.outcome {
            return Err(RuntimeLoadPreparationError::ImageNotPlaced {
                image_index: image.index,
                reason,
            });
        }
    }
    let bases = plan
        .base_addresses()
        .ok_or(RuntimeLoadPreparationError::ImageNotPlaced {
            image_index: 0,
            reason: ImageLayoutFailure::ZeroImageSize,
        })?;
    let binding_plan = ModuleBindingPlan::build(&graph, &bases)
        .map_err(RuntimeLoadPreparationError::BindingPlan)?;

    // Resolve every import before mapping. Services win over package exports;
    // the store has already refused a module identity owned by a service.
    let known_service_modules: BTreeSet<String> = services
        .iter_resolution_keys()
        .map(|(module, _, _)| module.to_ascii_lowercase())
        .collect();
    let mut diagnostics = StartupDiagnostics::default();
    let mut service_bindings = Vec::new();
    let mut unresolved = 0usize;
    for resolution in binding_plan.slots() {
        let image_index = resolution.consumer.index;
        let slot_address = bases[image_index]
            .checked_add(u64::from(resolution.slot_rva))
            .ok_or_else(|| RuntimeLoadPreparationError::ImportBinding {
                detail: "IAT slot address overflow".to_owned(),
                diagnostics: Box::new(diagnostics.clone()),
            })?;
        let disposition = match resolve_service(
            services,
            &resolution.requested_module,
            &resolution.requested_symbol,
            &resolution.forwarder_chain,
            slot_address,
            resolution.slot_rva,
        ) {
            Ok(Some(mut binding)) => {
                binding.image_index = image_index;
                service_bindings.push(binding.clone());
                ImportDisposition::Service {
                    module: binding.module.clone(),
                    symbol: binding.symbol.clone(),
                    status: binding.status,
                    data: binding.data,
                    address: 0,
                }
            }
            Ok(None) => match &resolution.outcome {
                BindingOutcome::Resolved(resolved) => ImportDisposition::Package {
                    provider_index: resolved.provider.index,
                    provider: resolved.provider.file_name.clone(),
                    export: resolved.export.clone(),
                    target_address: resolved.target_address,
                },
                BindingOutcome::Unresolved(reason) => {
                    unresolved += 1;
                    ImportDisposition::Unresolved(classify_unresolved(
                        &graph,
                        image_index,
                        &resolution.requested_module,
                        &resolution.requested_symbol,
                        reason,
                        &known_service_modules,
                    ))
                }
            },
            Err(failure) => {
                unresolved += 1;
                ImportDisposition::Unresolved(failure)
            }
        };
        diagnostics.imports.push(crate::package::ImportDiagnostic {
            image_index,
            file_name: graph.modules()[image_index].file_name.clone(),
            requested_module: resolution.requested_module.clone(),
            symbol: resolution.requested_symbol.clone(),
            iat_rva: resolution.slot_rva,
            disposition,
        });
    }
    diagnostics.modules =
        module_diagnostics(&graph, &bases, &plan, &diagnostics.imports, None, None);
    if unresolved != 0 {
        return Err(RuntimeLoadPreparationError::UnresolvedImports(Box::new(
            diagnostics,
        )));
    }

    let before = memory.clone();
    let prepared = (|| -> Result<PreparedModuleLoad, RuntimeLoadPreparationError> {
        let reservations =
            map_module_batch(&images, &plan, memory).map_err(RuntimeLoadPreparationError::Map)?;
        let relocations = relocate_module_batch(&images, &reservations, memory)
            .map_err(RuntimeLoadPreparationError::Relocate)?;
        let exceptions = register_module_exception_directories(&images, &reservations, memory)
            .map_err(RuntimeLoadPreparationError::Exceptions)?;
        let imports = bind_module_iat(&images, &reservations, &graph, &binding_plan, memory)
            .map_err(RuntimeLoadPreparationError::Bind)?;

        for binding in &service_bindings {
            let slot = services
                .bind_slot(
                    &binding.module,
                    &binding.import_name,
                    binding.slot_address,
                    memory,
                )
                .map_err(|error| RuntimeLoadPreparationError::ImportBinding {
                    detail: error.to_string(),
                    diagnostics: Box::new(diagnostics.clone()),
                })?;
            if let Some(import) = diagnostics.imports.iter_mut().find(|import| {
                import.image_index == binding.image_index && import.iat_rva == binding.slot_rva
            }) && let ImportDisposition::Service { address, .. } = &mut import.disposition
            {
                *address = slot.address();
            }
        }
        let written = imports.written_slot_count() + service_bindings.len();
        if written != binding_plan.slots().len() {
            return Err(RuntimeLoadPreparationError::ImportBinding {
                detail: format!(
                    "{written} of {} import slots were written",
                    binding_plan.slots().len()
                ),
                diagnostics: Box::new(diagnostics.clone()),
            });
        }

        let tls = prepare_module_tls(&images, &reservations, memory, thread_tls_base)
            .map_err(RuntimeLoadPreparationError::Tls)?;
        diagnostics.modules = module_diagnostics(
            &graph,
            &bases,
            &plan,
            &diagnostics.imports,
            Some(&reservations),
            Some(&tls),
        );

        Ok(PreparedModuleLoad {
            roots: roots.iter().map(|root| (*root).to_owned()).collect(),
            graph,
            layout: plan,
            reservations,
            relocations,
            exceptions,
            tls,
            imports,
            service_bindings: service_bindings.len(),
            diagnostics,
        })
    })();

    if prepared.is_err() {
        *memory = before;
    }
    prepared
}
