// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end startup for an explicit, independently supplied PE32+ package.
//!
//! This layer composes the existing module primitives; it does not replace
//! them. One call parses the package dependency graph, plans and maps every
//! image, applies base relocations, registers exception directories, binds
//! both in-package and host-service imports, prepares TLS, and runs module
//! initialization in dependency order before the entry point.
//!
//! The package is explicit: every supplied image is loaded. Module references
//! are never turned into host paths, and no host library is loaded. An import
//! that neither the package graph nor the service registry can resolve is a
//! startup failure; it is never skipped or bound to a fake address.
//!
//! Initialization follows the static provider-before-consumer order. Circular
//! components and incomplete import/export inspection are refused because
//! neither gives a defensible initialization order. TLS callbacks and
//! `DllMain(DLL_PROCESS_ATTACH)` run per module, then the entry image's process
//! entry. After the process ends, `DLL_PROCESS_DETACH` and detach TLS callbacks
//! run in reverse order and are reported separately from the process outcome.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use crate::cpu::{CpuState, ExecutionResult, ExitReason, Register};
use crate::dbt::{DbtEngine, DbtMetrics};
use crate::import_table::ImportName;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError, MemoryPermissions};
use crate::module_binding::{BindingOutcome, ModuleBindingPlan, UnresolvedBindingReason};
use crate::module_exceptions::{
    ModuleExceptionError, ModuleExceptionReport, register_module_exception_directories,
};
use crate::module_graph::{
    ExportKey, ForwarderStep, ModuleGraph, ModuleGraphError, PackageModule, UnresolvedImportReason,
};
use crate::module_iat::{ModuleIatError, ModuleIatReport, bind_module_iat};
use crate::module_init::{
    ModuleInitializationComponent, ModuleInitializationEntry, ModuleInitializationError,
    ModuleInitializationMetadata, ModuleInitializationPlan, plan_module_initialization,
};
use crate::module_layout::{
    AddressInterval, ImageLayoutFailure, ModuleImageDescriptor, ModuleLayoutArena,
    ModuleLayoutError, ModuleLayoutPlan,
};
use crate::module_memory::{
    ExplicitModuleImage, ModuleMemoryError, ModuleRelocationError, ModuleReservation,
    map_module_batch, relocate_module_batch,
};
use crate::module_tls::{ModuleTlsError, ModuleTlsReport, prepare_module_tls};
use crate::pe::{ParsedPe, PeError};
use crate::reloc::RelocationReport;
use crate::services::ServicesState;
use crate::startup::{STACK_SIZE, layout_regions, map_teb};
use crate::tls::{TlsError, execute_tls_callbacks_with_reason};

/// Bytes reserved below the initial stack pointer for the return address and
/// the 32-byte Win64 home area.
const CALL_FRAME: u64 = 40;
/// Guest page that holds the loader's return sentinels.
const RETURN_SENTINEL_PAGE_SIZE: usize = 0x1000;
/// Per-module initialization work is bounded so a hostile `DllMain` cannot run
/// forever. The entry point keeps the caller's own budget.
const INITIALIZATION_STEP_BUDGET: usize = 1_000_000;
/// Most initialization steps run between budget checks.
const INITIALIZATION_SLICE: usize = 100_000;
/// `DLL_PROCESS_ATTACH`.
const DLL_PROCESS_ATTACH: u64 = 1;
/// `DLL_PROCESS_DETACH`.
const DLL_PROCESS_DETACH: u64 = 0;

/// Guest address ranges reserved by the package harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackageStartupLayout {
    /// Address range in which the loader may place images.
    pub arena: ModuleLayoutArena,
    /// End of the initial thread stack. Must be 16-byte aligned.
    pub stack_top: u64,
    /// Base of the thread's TLS slot array. The TEB is placed above it.
    pub thread_tls_base: u64,
}

/// Boundary of the root executable's entry attempt in a package run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageEntryEvent {
    /// Initialization succeeded and the loader is about to enter the root EXE.
    Entering {
        image_index: usize,
        address: u64,
        main_teb: u64,
    },
    /// The attempt ended. Package detach callbacks have yet to run.
    Leaving,
}

/// What happened to one imported symbol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportDisposition {
    /// An export of another image in this explicit package.
    Package {
        provider_index: usize,
        provider: String,
        export: ExportKey,
        target_address: u64,
    },
    /// A host service registered in the import registry.
    Service {
        module: String,
        symbol: String,
        status: ServiceStatus,
        data: bool,
        address: u64,
    },
    /// No honest binding exists. Startup refuses the package.
    Unresolved(ImportFailure),
}

/// Why an import has no binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportFailure {
    /// No package image and no service module has this identity.
    MissingModule { module: String },
    /// The module is known, but it exports no such name or ordinal.
    MissingExport { module: String, symbol: String },
    /// The reference is an API-set contract with no registered alias.
    ApiSetUnresolved { contract: String },
    /// The reference or its export data uses a feature this loader refuses.
    UnsupportedAbi { detail: String },
    /// The dependency graph is cyclic, too deep, or otherwise invalid.
    InvalidDependencyGraph { detail: String },
    /// Import or export inspection did not complete.
    InspectionIncomplete { detail: String },
    /// The only binding is an explicit stub.
    StubService { module: String, symbol: String },
}

impl fmt::Display for ImportFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingModule { module } => write!(f, "module {module:?} is not present"),
            Self::MissingExport { module, symbol } => {
                write!(f, "module {module:?} has no export {symbol:?}")
            }
            Self::ApiSetUnresolved { contract } => {
                write!(f, "API-set contract {contract:?} has no registered alias")
            }
            Self::UnsupportedAbi { detail } => write!(f, "unsupported ABI: {detail}"),
            Self::InvalidDependencyGraph { detail } => {
                write!(f, "invalid dependency graph: {detail}")
            }
            Self::InspectionIncomplete { detail } => {
                write!(f, "import inspection incomplete: {detail}")
            }
            Self::StubService { module, symbol } => {
                write!(f, "service {module}!{symbol} is an explicit stub")
            }
        }
    }
}

/// One imported symbol and its disposition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportDiagnostic {
    pub image_index: usize,
    pub file_name: String,
    pub requested_module: String,
    pub symbol: ImportName,
    pub iat_rva: u32,
    pub disposition: ImportDisposition,
}

impl ImportDiagnostic {
    /// Whether the import was bound to a real package export or service.
    pub fn is_resolved(&self) -> bool {
        !matches!(self.disposition, ImportDisposition::Unresolved(_))
    }
}

/// Static and runtime facts about one package image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleDiagnostic {
    pub index: usize,
    pub file_name: String,
    pub base: u64,
    pub size_of_image: u32,
    pub is_dll: bool,
    pub entry: ModuleInitializationEntry,
    pub tls_index: Option<u32>,
    pub dependencies: Vec<usize>,
    pub imports: usize,
    pub resolved_imports: usize,
}

/// One executed initialization or shutdown callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitializationPhase {
    TlsCallback { callback_index: usize },
    DllMain,
    ProcessEntry,
    DllDetach,
    TlsDetach { callback_index: usize },
}

/// How one callback ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitializationOutcome {
    /// The callback returned; the value is its RAX.
    Returned(u64),
    /// The callback completed and its return value is not meaningful here.
    Completed,
    /// `DllMain` returned zero, which is a failed attach.
    ReturnedZero,
    /// The guest ended its process during the callback.
    ProcessExit(u32),
    /// The callback stopped for a reason the loader cannot continue through.
    Fault(ExitReason),
}

/// One initialization record, in execution order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitializationRecord {
    pub image_index: usize,
    pub file_name: String,
    pub phase: InitializationPhase,
    pub address: u64,
    pub steps: usize,
    pub outcome: InitializationOutcome,
}

/// Structured startup evidence. It is present on success and on every
/// diagnostic startup failure, so a caller can report more than the first bad
/// import.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StartupDiagnostics {
    pub modules: Vec<ModuleDiagnostic>,
    pub imports: Vec<ImportDiagnostic>,
    pub initialization: Vec<InitializationRecord>,
    pub shutdown: Vec<InitializationRecord>,
}

impl StartupDiagnostics {
    /// Imports with no honest binding.
    pub fn unresolved_imports(&self) -> impl Iterator<Item = &ImportDiagnostic> {
        self.imports.iter().filter(|import| !import.is_resolved())
    }

    /// Count of imports by failure category, for a compact report.
    pub fn failure_counts(&self) -> Vec<(&'static str, usize)> {
        let mut missing_modules = 0;
        let mut missing_exports = 0;
        let mut api_sets = 0;
        let mut unsupported = 0;
        let mut graphs = 0;
        let mut incomplete = 0;
        let mut stubs = 0;
        for import in self.unresolved_imports() {
            match import.disposition {
                ImportDisposition::Unresolved(ImportFailure::MissingModule { .. }) => {
                    missing_modules += 1
                }
                ImportDisposition::Unresolved(ImportFailure::MissingExport { .. }) => {
                    missing_exports += 1
                }
                ImportDisposition::Unresolved(ImportFailure::ApiSetUnresolved { .. }) => {
                    api_sets += 1
                }
                ImportDisposition::Unresolved(ImportFailure::UnsupportedAbi { .. }) => {
                    unsupported += 1
                }
                ImportDisposition::Unresolved(ImportFailure::InvalidDependencyGraph { .. }) => {
                    graphs += 1
                }
                ImportDisposition::Unresolved(ImportFailure::InspectionIncomplete { .. }) => {
                    incomplete += 1
                }
                ImportDisposition::Unresolved(ImportFailure::StubService { .. }) => stubs += 1,
                ImportDisposition::Package { .. } | ImportDisposition::Service { .. } => {}
            }
        }
        vec![
            ("missing-module", missing_modules),
            ("missing-export", missing_exports),
            ("api-set", api_sets),
            ("unsupported-abi", unsupported),
            ("invalid-graph", graphs),
            ("inspection-incomplete", incomplete),
            ("stub", stubs),
        ]
    }
}

/// Successful package startup.
#[derive(Debug)]
pub struct PackageStartupResult {
    pub graph: ModuleGraph,
    pub reservations: Vec<ModuleReservation>,
    pub relocations: Vec<RelocationReport>,
    pub exceptions: Vec<ModuleExceptionReport>,
    pub tls: Vec<ModuleTlsReport>,
    pub imports: ModuleIatReport,
    pub diagnostics: StartupDiagnostics,
    pub execution: ExecutionResult,
    pub dbt_metrics: DbtMetrics,
}

/// Why package startup stopped.
#[derive(Debug)]
pub enum PackageStartupError {
    InvalidLayout(String),
    Graph(ModuleGraphError),
    Pe(PeError),
    Layout(ModuleLayoutError),
    ImageNotPlaced {
        image_index: usize,
        reason: ImageLayoutFailure,
    },
    Map(ModuleMemoryError),
    Relocate(ModuleRelocationError),
    Exceptions(ModuleExceptionError),
    Bind(ModuleIatError),
    Tls(ModuleTlsError),
    InitializationPlan(ModuleInitializationError),
    InvalidDependencyGraph(String),
    /// Every import was inspected. At least one has no binding.
    UnresolvedImports(Box<StartupDiagnostics>),
    ImportBinding {
        detail: String,
        diagnostics: Box<StartupDiagnostics>,
    },
    Initialization {
        detail: String,
        diagnostics: Box<StartupDiagnostics>,
    },
    Entry(String),
    Jit(crate::jit::JitError),
    Memory(MemoryError),
}

impl fmt::Display for PackageStartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLayout(detail) => write!(f, "invalid package layout: {detail}"),
            Self::Graph(error) => write!(f, "module graph rejected: {error}"),
            Self::Pe(error) => write!(f, "image parse failed: {error}"),
            Self::Layout(error) => write!(f, "module layout rejected: {error}"),
            Self::ImageNotPlaced {
                image_index,
                reason,
            } => write!(f, "image {image_index} was not placed: {reason}"),
            Self::Map(error) => write!(f, "module mapping failed: {error}"),
            Self::Relocate(error) => write!(f, "module relocation failed: {error}"),
            Self::Exceptions(error) => write!(f, "exception registration failed: {error}"),
            Self::Bind(error) => write!(f, "import binding failed: {error}"),
            Self::Tls(error) => write!(f, "TLS preparation failed: {error}"),
            Self::InitializationPlan(error) => {
                write!(f, "initialization planning failed: {error}")
            }
            Self::InvalidDependencyGraph(detail) => {
                write!(f, "package cannot be initialized: {detail}")
            }
            Self::UnresolvedImports(diagnostics) => {
                let counts = diagnostics
                    .failure_counts()
                    .into_iter()
                    .filter(|(_, count)| *count != 0)
                    .map(|(name, count)| format!("{name}={count}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "{} required imports are unresolved ({counts})",
                    diagnostics.unresolved_imports().count()
                )
            }
            Self::ImportBinding { detail, .. } => write!(f, "import binding failed: {detail}"),
            Self::Initialization { detail, .. } => {
                write!(f, "module initialization failed: {detail}")
            }
            Self::Entry(detail) => write!(f, "entry point failed: {detail}"),
            Self::Jit(error) => write!(f, "JIT allocation failed: {error}"),
            Self::Memory(error) => write!(f, "guest memory error: {error}"),
        }
    }
}

impl std::error::Error for PackageStartupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Graph(source) => Some(source),
            Self::Pe(source) => Some(source),
            Self::Layout(source) => Some(source),
            Self::ImageNotPlaced { reason, .. } => Some(reason),
            Self::Map(source) => Some(source),
            Self::Relocate(source) => Some(source),
            Self::Exceptions(source) => Some(source),
            Self::Bind(source) => Some(source),
            Self::Tls(source) => Some(source),
            Self::InitializationPlan(source) => Some(source),
            Self::Jit(source) => Some(source),
            Self::Memory(source) => Some(source),
            _ => None,
        }
    }
}

impl PackageStartupError {
    /// Structured evidence when the failure happened after import inspection.
    pub fn diagnostics(&self) -> Option<&StartupDiagnostics> {
        match self {
            Self::UnresolvedImports(diagnostics)
            | Self::ImportBinding { diagnostics, .. }
            | Self::Initialization { diagnostics, .. } => Some(diagnostics),
            _ => None,
        }
    }
}

/// Load and start an explicit package, then run `entry_image`'s entry point.
///
/// `scheduler` optionally attaches the deterministic cooperative scheduler to
/// the entry point. TLS callbacks and `DllMain` always run on the initial
/// thread before the scheduler attaches.
#[allow(clippy::too_many_arguments)]
pub fn run_package_startup(
    package: &[PackageModule<'_>],
    layout: PackageStartupLayout,
    entry_image: usize,
    imports: ImportRegistry,
    entry_budget: usize,
    scheduler: Option<(Arc<ServicesState>, usize)>,
) -> Result<PackageStartupResult, PackageStartupError> {
    run_package_startup_with_entry_observer(
        package,
        layout,
        entry_image,
        imports,
        entry_budget,
        scheduler,
        |_| {},
    )
}

/// Run a package while observing only the root executable's entry phase.
///
/// TLS callbacks and DLL attach complete before `Entering`. Refusal or exit
/// during initialization produces no entry events. Every entry attempt emits
/// `Leaving`, including a scheduler refusal, before any normal detach
/// callbacks. The actual main TEB is available in `Entering` because package
/// initializers and detach callbacks can use the same TEB as the executable.
pub fn run_package_startup_with_entry_observer(
    package: &[PackageModule<'_>],
    layout: PackageStartupLayout,
    entry_image: usize,
    mut imports: ImportRegistry,
    entry_budget: usize,
    scheduler: Option<(Arc<ServicesState>, usize)>,
    mut observe: impl FnMut(PackageEntryEvent),
) -> Result<PackageStartupResult, PackageStartupError> {
    if package.is_empty() {
        return Err(PackageStartupError::InvalidLayout(
            "the package has no images".to_owned(),
        ));
    }
    if entry_image >= package.len() {
        return Err(PackageStartupError::InvalidLayout(format!(
            "entry image index {entry_image} is outside the {}-image package",
            package.len()
        )));
    }
    if !layout.stack_top.is_multiple_of(16) {
        return Err(PackageStartupError::InvalidLayout(
            "the initial stack top must be 16-byte aligned".to_owned(),
        ));
    }
    let (stack_base, teb_base) = layout_regions(layout.stack_top, layout.thread_tls_base)
        .map_err(|error| PackageStartupError::InvalidLayout(error.to_string()))?;
    let stack_size = layout
        .stack_top
        .checked_sub(stack_base)
        .ok_or_else(|| PackageStartupError::InvalidLayout("stack underflow".to_owned()))?;
    if stack_size != STACK_SIZE as u64 {
        return Err(PackageStartupError::InvalidLayout(
            "stack size changed unexpectedly".to_owned(),
        ));
    }
    let teb_end = teb_base
        .checked_add(0x2000)
        .ok_or_else(|| PackageStartupError::InvalidLayout("TEB end overflow".to_owned()))?;
    let tls_array_end = layout
        .thread_tls_base
        .checked_add(crate::tls::TLS_ARRAY_SIZE as u64)
        .ok_or_else(|| PackageStartupError::InvalidLayout("TLS array end overflow".to_owned()))?;

    // Parse and connect the package before any address is planned.
    let graph = ModuleGraph::build(package).map_err(PackageStartupError::Graph)?;
    let parsed: Vec<ParsedPe> = package
        .iter()
        .map(|module| ParsedPe::parse(module.bytes))
        .collect::<Result<_, PeError>>()
        .map_err(PackageStartupError::Pe)?;
    let images: Vec<ExplicitModuleImage<'_>> = parsed
        .iter()
        .zip(package)
        .map(|(pe, module)| ExplicitModuleImage {
            pe,
            bytes: module.bytes,
        })
        .collect();

    let descriptors: Vec<ModuleImageDescriptor> = parsed
        .iter()
        .map(ModuleImageDescriptor::from_parsed_pe)
        .collect();
    let occupied = [
        AddressInterval::new(stack_base, layout.stack_top),
        AddressInterval::new(teb_base, teb_end),
        AddressInterval::new(layout.thread_tls_base, tls_array_end),
    ];
    let plan = ModuleLayoutPlan::build(layout.arena, &occupied, &descriptors)
        .map_err(PackageStartupError::Layout)?;
    for image in plan.images() {
        if let crate::module_layout::ImageLayoutOutcome::Failed(reason) = image.outcome {
            return Err(PackageStartupError::ImageNotPlaced {
                image_index: image.index,
                reason,
            });
        }
    }
    let bases = plan
        .base_addresses()
        .ok_or_else(|| PackageStartupError::InvalidLayout("a planned image has no base".into()))?;

    // Inspect every import before any guest byte is written.
    let binding_plan = ModuleBindingPlan::build(&graph, &bases).map_err(|error| {
        PackageStartupError::InvalidLayout(format!("binding plan rejected: {error:?}"))
    })?;
    let known_service_modules: BTreeSet<String> = imports
        .iter_resolution_keys()
        .map(|(module, _, _)| module.to_ascii_lowercase())
        .collect();
    let mut diagnostics = StartupDiagnostics::default();
    let mut service_bindings: Vec<ServiceBinding> = Vec::new();
    let mut unresolved = 0usize;
    for resolution in binding_plan.slots() {
        let image_index = resolution.consumer.index;
        let slot_address = bases[image_index]
            .checked_add(u64::from(resolution.slot_rva))
            .ok_or_else(|| {
                PackageStartupError::InvalidLayout("IAT slot address overflow".to_owned())
            })?;
        let disposition = match &resolution.outcome {
            BindingOutcome::Resolved(resolved) => ImportDisposition::Package {
                provider_index: resolved.provider.index,
                provider: resolved.provider.file_name.clone(),
                export: resolved.export.clone(),
                target_address: resolved.target_address,
            },
            BindingOutcome::Unresolved(reason) => {
                // A package image that is present but lacks the export is a
                // packaging error, not a reason to bind a same-named host
                // service. Only a reference the package cannot provide at all
                // may fall back to the registry.
                let fallback = may_fall_back_to_service(reason);
                let resolved_service = if fallback {
                    resolve_service(
                        &imports,
                        resolution.requested_module.as_str(),
                        &resolution.requested_symbol,
                        &resolution.forwarder_chain,
                        slot_address,
                        resolution.slot_rva,
                    )
                } else {
                    Ok(None)
                };
                match resolved_service {
                    Ok(Some(mut service)) => {
                        service.image_index = image_index;
                        service_bindings.push(service.clone());
                        ImportDisposition::Service {
                            module: service.module.clone(),
                            symbol: service.symbol.clone(),
                            status: service.status,
                            data: service.data,
                            address: 0,
                        }
                    }
                    Ok(None) => {
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
                    Err(failure) => {
                        unresolved += 1;
                        ImportDisposition::Unresolved(failure)
                    }
                }
            }
        };
        diagnostics.imports.push(ImportDiagnostic {
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
        return Err(PackageStartupError::UnresolvedImports(Box::new(
            diagnostics,
        )));
    }

    let mut memory = GuestMemory::new();
    let reservations =
        map_module_batch(&images, &plan, &mut memory).map_err(PackageStartupError::Map)?;
    let relocations = relocate_module_batch(&images, &reservations, &mut memory)
        .map_err(PackageStartupError::Relocate)?;
    let exceptions = register_module_exception_directories(&images, &reservations, &mut memory)
        .map_err(PackageStartupError::Exceptions)?;

    // Package slots and service slots share the image. The address space is
    // local to this call, so a failure drops every partial write.
    let iat = match bind_module_iat(&images, &reservations, &graph, &binding_plan, &mut memory) {
        Ok(report) => report,
        Err(error) => return Err(PackageStartupError::Bind(error)),
    };
    for service in &service_bindings {
        match imports.bind_slot(
            &service.module,
            &service.import_name,
            service.slot_address,
            &mut memory,
        ) {
            Ok(binding) => {
                if let Some(import) = diagnostics.imports.iter_mut().find(|import| {
                    import.image_index == service.image_index && import.iat_rva == service.slot_rva
                }) && let ImportDisposition::Service { address, .. } = &mut import.disposition
                {
                    *address = binding.address();
                }
            }
            Err(error) => {
                // The address space is local to this call, so returning drops
                // every partial write with it.
                return Err(PackageStartupError::ImportBinding {
                    detail: error.to_string(),
                    diagnostics: Box::new(diagnostics),
                });
            }
        }
    }
    // Every plan slot must now be written exactly once, either by the package
    // binder or by a service binding.
    let written = iat.written_slot_count() + service_bindings.len();
    if written != binding_plan.slots().len() {
        return Err(PackageStartupError::ImportBinding {
            detail: format!(
                "{written} of {} import slots were written",
                binding_plan.slots().len()
            ),
            diagnostics: Box::new(diagnostics),
        });
    }

    // Reserve the harness regions before TLS preparation allocates module
    // storage, so the dynamic allocator cannot choose an address that the
    // stack, TEB, TLS array or return-sentinel page already owns.
    memory
        .map_region(stack_base, STACK_SIZE, MemoryPermissions::READ_WRITE)
        .map_err(PackageStartupError::Memory)?;
    memory
        .map_region(
            crate::imports::RETURN_SENTINEL_PAGE,
            RETURN_SENTINEL_PAGE_SIZE,
            MemoryPermissions::NO_ACCESS,
        )
        .map_err(PackageStartupError::Memory)?;
    map_teb(
        &mut memory,
        teb_base,
        layout.stack_top,
        layout.thread_tls_base,
        stack_base,
    )
    .map_err(PackageStartupError::Memory)?;
    memory
        .map_region(
            layout.thread_tls_base,
            crate::tls::TLS_ARRAY_SIZE,
            MemoryPermissions::READ_WRITE,
        )
        .map_err(PackageStartupError::Memory)?;

    let tls = prepare_module_tls(&images, &reservations, &mut memory, layout.thread_tls_base)
        .map_err(PackageStartupError::Tls)?;
    diagnostics.modules = module_diagnostics(
        &graph,
        &bases,
        &plan,
        &diagnostics.imports,
        Some(&reservations),
        Some(&tls),
    );

    let init_plan = plan_module_initialization(package, &images, &graph, &reservations, Some(&tls))
        .map_err(PackageStartupError::InitializationPlan)?;
    let order = initialization_order(&init_plan)?;

    let mut engine = DbtEngine::new(1024 * 1024).map_err(PackageStartupError::Jit)?;
    engine.set_import_registry(imports);
    let mut cpu = CpuState::new(0, layout.stack_top);
    cpu.gs_base = teb_base;

    let mut process_exit = None;
    let mut attached: Vec<usize> = Vec::new();
    for (order_index, metadata) in order.iter().enumerate() {
        if process_exit.is_some() {
            break;
        }
        if !metadata.tls_callbacks.is_empty() {
            // The prepared callbacks are already rebased; passing the same
            // base as preferred makes the executor's delta zero.
            match execute_tls_callbacks_with_reason(
                &metadata.tls_callbacks,
                metadata_base(metadata, &reservations),
                metadata_base(metadata, &reservations),
                layout.stack_top,
                DLL_PROCESS_ATTACH,
                &mut cpu,
                &mut memory,
                &mut engine,
            ) {
                Ok(count) => {
                    for index in 0..count {
                        diagnostics.initialization.push(InitializationRecord {
                            image_index: metadata.package_index,
                            file_name: metadata.file_name.clone(),
                            phase: InitializationPhase::TlsCallback {
                                callback_index: index,
                            },
                            address: metadata.tls_callbacks[index],
                            steps: 0,
                            outcome: InitializationOutcome::Completed,
                        });
                    }
                }
                Err(TlsError::CallbackExecutionFault(ExitReason::ProcessExit(status))) => {
                    process_exit = Some(status);
                    break;
                }
                Err(TlsError::CallbackExecutionFault(reason)) => {
                    diagnostics.initialization.push(InitializationRecord {
                        image_index: metadata.package_index,
                        file_name: metadata.file_name.clone(),
                        phase: InitializationPhase::TlsCallback { callback_index: 0 },
                        address: metadata.tls_callbacks.first().copied().unwrap_or(0),
                        steps: 0,
                        outcome: InitializationOutcome::Fault(reason),
                    });
                    return Err(PackageStartupError::Initialization {
                        detail: format!(
                            "TLS callback for {} stopped: {reason:?}",
                            metadata.file_name
                        ),
                        diagnostics: Box::new(diagnostics),
                    });
                }
                Err(error) => {
                    return Err(PackageStartupError::Initialization {
                        detail: format!("TLS callback for {} failed: {error}", metadata.file_name),
                        diagnostics: Box::new(diagnostics),
                    });
                }
            }
        }

        let ModuleInitializationEntry::DllMain { address } = metadata.entry else {
            // The entry image has a process entry, not a DllMain. Its attach
            // phase is the process entry itself and it never detaches.
            attached.push(order_index);
            continue;
        };
        let call = call_guest_callback(
            &mut engine,
            &mut memory,
            address,
            layout.stack_top,
            teb_base,
            [
                metadata_base(metadata, &reservations),
                DLL_PROCESS_ATTACH,
                0,
            ],
            &mut cpu,
            INITIALIZATION_STEP_BUDGET,
        );
        match call {
            Ok(CallbackResult {
                steps,
                outcome: CallbackOutcome::Returned(value),
            }) => {
                diagnostics.initialization.push(InitializationRecord {
                    image_index: metadata.package_index,
                    file_name: metadata.file_name.clone(),
                    phase: InitializationPhase::DllMain,
                    address,
                    steps,
                    outcome: if value == 0 {
                        InitializationOutcome::ReturnedZero
                    } else {
                        InitializationOutcome::Returned(value)
                    },
                });
                if value == 0 {
                    return Err(PackageStartupError::Initialization {
                        detail: format!("DllMain of {} returned FALSE", metadata.file_name),
                        diagnostics: Box::new(diagnostics),
                    });
                }
                attached.push(order_index);
            }
            Ok(CallbackResult {
                steps,
                outcome: CallbackOutcome::ProcessExit(status),
            }) => {
                diagnostics.initialization.push(InitializationRecord {
                    image_index: metadata.package_index,
                    file_name: metadata.file_name.clone(),
                    phase: InitializationPhase::DllMain,
                    address,
                    steps,
                    outcome: InitializationOutcome::ProcessExit(status),
                });
                process_exit = Some(status);
            }
            Ok(CallbackResult {
                steps,
                outcome: CallbackOutcome::Fault(reason),
            }) => {
                diagnostics.initialization.push(InitializationRecord {
                    image_index: metadata.package_index,
                    file_name: metadata.file_name.clone(),
                    phase: InitializationPhase::DllMain,
                    address,
                    steps,
                    outcome: InitializationOutcome::Fault(reason),
                });
                return Err(PackageStartupError::Initialization {
                    detail: format!("DllMain of {} stopped: {reason:?}", metadata.file_name),
                    diagnostics: Box::new(diagnostics),
                });
            }
            Err(error) => {
                return Err(PackageStartupError::Initialization {
                    detail: format!("DllMain of {} could not run: {error}", metadata.file_name),
                    diagnostics: Box::new(diagnostics),
                });
            }
        }
    }

    let execution = if let Some(status) = process_exit {
        ExecutionResult {
            instructions_executed: 0,
            final_state: cpu.clone(),
            exit_reason: ExitReason::ProcessExit(status),
        }
    } else {
        let entry = order
            .iter()
            .find(|metadata| metadata.package_index == entry_image)
            .copied()
            .ok_or_else(|| {
                PackageStartupError::InvalidLayout(
                    "the entry image is absent from the initialization plan".to_owned(),
                )
            })?;
        let ModuleInitializationEntry::ProcessEntry { address } = entry.entry else {
            return Err(PackageStartupError::InvalidLayout(format!(
                "entry image {} is a DLL or has no entry point",
                entry.file_name
            )));
        };
        memory
            .write_u64_bypass(layout.stack_top - CALL_FRAME, crate::imports::ENTRY_RETURN)
            .map_err(PackageStartupError::Memory)?;
        let mut entry_cpu = CpuState::new(address, layout.stack_top - CALL_FRAME);
        entry_cpu.gs_base = teb_base;
        observe(PackageEntryEvent::Entering {
            image_index: entry.package_index,
            address,
            main_teb: entry_cpu.gs_base,
        });
        let result = if let Some((state, quantum)) = scheduler {
            crate::scheduler::Scheduler::new(state, quantum)
                .run(&mut engine, &mut memory, &entry_cpu, entry_budget)
                .map(|report| ExecutionResult {
                    instructions_executed: report.instructions,
                    final_state: report.main,
                    exit_reason: scheduler_exit_reason(report.outcome),
                })
                .map_err(|reason| PackageStartupError::Entry(format!("{reason:?}")))
        } else {
            Ok(engine.run(&mut entry_cpu, &mut memory, entry_budget))
        };
        observe(PackageEntryEvent::Leaving);
        let result = result?;
        diagnostics.initialization.push(InitializationRecord {
            image_index: entry.package_index,
            file_name: entry.file_name.clone(),
            phase: InitializationPhase::ProcessEntry,
            address,
            steps: result.instructions_executed,
            outcome: match result.exit_reason {
                ExitReason::ProcessExit(status) => InitializationOutcome::ProcessExit(status),
                ExitReason::Halt => InitializationOutcome::Returned(0),
                other => InitializationOutcome::Fault(other),
            },
        });
        result
    };

    // Cleanup runs after the process outcome is fixed. It cannot change the
    // exit status; faults are recorded in the shutdown evidence.
    run_detach(
        &mut engine,
        &mut memory,
        &order,
        &attached,
        &reservations,
        layout.stack_top,
        teb_base,
        &mut diagnostics.shutdown,
    );

    Ok(PackageStartupResult {
        graph,
        reservations,
        relocations,
        exceptions,
        tls,
        imports: iat,
        diagnostics,
        execution,
        dbt_metrics: engine.metrics,
    })
}

fn scheduler_exit_reason(outcome: crate::scheduler::Outcome) -> ExitReason {
    use crate::scheduler::Outcome;
    match outcome {
        Outcome::Halted | Outcome::AllThreadsExited => ExitReason::Halt,
        Outcome::BudgetExhausted => ExitReason::StepLimitExceeded,
        Outcome::Deadlock { .. } => ExitReason::GuestBlocked,
        Outcome::Fault { reason, .. } => reason,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ServiceBinding {
    pub(crate) image_index: usize,
    pub(crate) module: String,
    pub(crate) symbol: String,
    pub(crate) import_name: ImportName,
    pub(crate) status: ServiceStatus,
    pub(crate) data: bool,
    pub(crate) slot_address: u64,
    pub(crate) slot_rva: u32,
}

/// Resolve an import that the package graph could not bind to a registered
/// service. The last forwarder target is tried before the direct key so a
/// package DLL that forwards to a host module works.
pub(crate) fn resolve_service(
    imports: &ImportRegistry,
    requested_module: &str,
    requested_name: &ImportName,
    forwarders: &[ForwarderStep],
    slot_address: u64,
    slot_rva: u32,
) -> Result<Option<ServiceBinding>, ImportFailure> {
    let mut candidates: Vec<(String, ImportName)> = Vec::new();
    if let Some(step) = forwarders.last() {
        candidates.push((
            step.target_module.clone(),
            export_key_to_import_name(&step.target_export),
        ));
    }
    candidates.push((requested_module.to_owned(), requested_name.clone()));
    for (module, name) in candidates {
        let Some(descriptor) = imports.binding_key(&module, &name) else {
            continue;
        };
        if descriptor.status == ServiceStatus::Stub {
            return Err(ImportFailure::StubService {
                module: descriptor.module.clone(),
                symbol: descriptor.symbol.clone(),
            });
        }
        return Ok(Some(ServiceBinding {
            image_index: 0,
            module,
            symbol: name.to_string(),
            import_name: name,
            status: descriptor.status,
            data: descriptor.data_import,
            slot_address,
            slot_rva,
        }));
    }
    Ok(None)
}

pub(crate) fn classify_unresolved(
    graph: &ModuleGraph,
    image_index: usize,
    requested_module: &str,
    requested_name: &ImportName,
    reason: &UnresolvedBindingReason,
    known_service_modules: &BTreeSet<String>,
) -> ImportFailure {
    let node = &graph.modules()[image_index];
    if node.import_error.is_some() || node.export_error.is_some() {
        return ImportFailure::InspectionIncomplete {
            detail: format!("{} has a malformed import or export table", node.file_name),
        };
    }
    match reason {
        UnresolvedBindingReason::OverlappingIatSlot { .. } => ImportFailure::UnsupportedAbi {
            detail: format!(
                "{}!{} shares an overlapping IAT slot with another import",
                requested_module, requested_name
            ),
        },
        UnresolvedBindingReason::SlotAddressOverflow { .. }
        | UnresolvedBindingReason::TargetAddressOverflow { .. } => ImportFailure::UnsupportedAbi {
            detail: format!(
                "{}!{} needs an address outside the 64-bit guest space",
                requested_module, requested_name
            ),
        },
        UnresolvedBindingReason::Import(reason) => match reason {
            UnresolvedImportReason::MissingModule { module } => {
                if known_service_modules.contains(&module.to_ascii_lowercase()) {
                    ImportFailure::MissingExport {
                        module: module.clone(),
                        symbol: requested_name.to_string(),
                    }
                } else {
                    ImportFailure::MissingModule {
                        module: module.clone(),
                    }
                }
            }
            UnresolvedImportReason::UnsupportedModulePath { module } => {
                ImportFailure::UnsupportedAbi {
                    detail: format!("module reference {module:?} is a path, not a bare name"),
                }
            }
            UnresolvedImportReason::ApiSetContract { contract } => {
                ImportFailure::ApiSetUnresolved {
                    contract: contract.clone(),
                }
            }
            UnresolvedImportReason::ForwarderApiSetContract { step } => {
                ImportFailure::ApiSetUnresolved {
                    contract: step.target.clone(),
                }
            }
            UnresolvedImportReason::ForwarderMissingModule { step } => {
                ImportFailure::MissingModule {
                    module: step.target_module.clone(),
                }
            }
            UnresolvedImportReason::ProviderExportsMalformed { provider } => {
                ImportFailure::UnsupportedAbi {
                    detail: format!(
                        "provider {} has a malformed export table",
                        provider.file_name
                    ),
                }
            }
            UnresolvedImportReason::MissingExport { provider, export } => {
                ImportFailure::MissingExport {
                    module: provider.file_name.clone(),
                    symbol: export.to_string(),
                }
            }
            UnresolvedImportReason::EmptyExportSlot {
                provider,
                export,
                ordinal,
            } => ImportFailure::MissingExport {
                module: provider.file_name.clone(),
                symbol: format!("{export} (empty slot at ordinal {ordinal})"),
            },
            UnresolvedImportReason::MalformedForwarder {
                provider,
                export,
                target,
                error,
            } => ImportFailure::UnsupportedAbi {
                detail: format!(
                    "{}!{export} forwards to {target:?}: {error}",
                    provider.file_name
                ),
            },
            UnresolvedImportReason::ForwarderCycle { step } => {
                ImportFailure::InvalidDependencyGraph {
                    detail: format!(
                        "forwarder cycle at {}!{}",
                        step.provider.file_name, step.export
                    ),
                }
            }
            UnresolvedImportReason::ForwarderHopLimit { step } => {
                ImportFailure::InvalidDependencyGraph {
                    detail: format!(
                        "forwarder hop limit at {}!{}",
                        step.provider.file_name, step.export
                    ),
                }
            }
        },
    }
}

/// Whether an unresolved package binding may be satisfied by a host service.
///
/// The package is authoritative for a module it contains. A missing export or
/// a malformed provider is therefore a package error, while an absent module,
/// an API-set contract, or a forwarder whose target is outside the package can
/// legitimately name a registered service.
fn may_fall_back_to_service(reason: &UnresolvedBindingReason) -> bool {
    match reason {
        UnresolvedBindingReason::Import(reason) => matches!(
            reason,
            UnresolvedImportReason::MissingModule { .. }
                | UnresolvedImportReason::ApiSetContract { .. }
                | UnresolvedImportReason::ForwarderApiSetContract { .. }
                | UnresolvedImportReason::ForwarderMissingModule { .. }
        ),
        UnresolvedBindingReason::OverlappingIatSlot { .. }
        | UnresolvedBindingReason::SlotAddressOverflow { .. }
        | UnresolvedBindingReason::TargetAddressOverflow { .. } => false,
    }
}

fn export_key_to_import_name(key: &ExportKey) -> ImportName {
    match key {
        ExportKey::Name(name) => ImportName::Name(name.clone()),
        ExportKey::Ordinal(ordinal) => match u16::try_from(*ordinal) {
            Ok(ordinal) => ImportName::Ordinal(ordinal),
            // Export ordinals above the import ordinal range cannot be named
            // by an import; the registry lookup will simply miss.
            Err(_) => ImportName::Name(format!("Ordinal#{ordinal}")),
        },
    }
}

pub(crate) fn module_diagnostics(
    graph: &ModuleGraph,
    bases: &[u64],
    plan: &ModuleLayoutPlan,
    imports: &[ImportDiagnostic],
    reservations: Option<&[ModuleReservation]>,
    tls: Option<&[ModuleTlsReport]>,
) -> Vec<ModuleDiagnostic> {
    let mut result = Vec::with_capacity(graph.modules().len());
    for (index, node) in graph.modules().iter().enumerate() {
        let entry = match reservations
            .and_then(|reservations| reservations.get(index))
            .and_then(|reservation| reservation.entry_point)
        {
            None => ModuleInitializationEntry::NoEntryPoint,
            Some(address) if node.is_dll => ModuleInitializationEntry::DllMain { address },
            Some(address) => ModuleInitializationEntry::ProcessEntry { address },
        };
        let dependencies = graph
            .dependency_edges()
            .iter()
            .filter(|edge| edge.consumer == index)
            .map(|edge| edge.provider)
            .collect();
        let import_count = imports
            .iter()
            .filter(|import| import.image_index == index)
            .count();
        let resolved = imports
            .iter()
            .filter(|import| import.image_index == index && import.is_resolved())
            .count();
        let tls_index = tls.and_then(|reports| {
            reports
                .iter()
                .find(|report| report.image_index == index)
                .and_then(|report| report.prepared.as_ref())
                .map(|prepared| prepared.tls_index)
        });
        result.push(ModuleDiagnostic {
            index,
            file_name: node.file_name.clone(),
            base: bases[index],
            size_of_image: plan.images()[index].descriptor.size_of_image,
            is_dll: node.is_dll,
            entry,
            tls_index,
            dependencies,
            imports: import_count,
            resolved_imports: resolved,
        });
    }
    result
}

fn initialization_order(
    plan: &ModuleInitializationPlan,
) -> Result<Vec<&ModuleInitializationMetadata>, PackageStartupError> {
    if !plan.ordering_proof_is_complete() {
        let mut circular = Vec::new();
        for component in &plan.components {
            if let ModuleInitializationComponent::Circular(members) = component {
                circular.extend(members.iter().map(|member| member.file_name.clone()));
            }
        }
        let detail = if !circular.is_empty() {
            format!(
                "circular dependency component among {}",
                circular.join(", ")
            )
        } else {
            format!(
                "{} modules have incomplete import or export inspection",
                plan.inspection_incomplete.len()
            )
        };
        return Err(PackageStartupError::InvalidDependencyGraph(detail));
    }
    Ok(plan
        .components
        .iter()
        .filter_map(|component| match component {
            ModuleInitializationComponent::Acyclic(metadata) => Some(metadata),
            ModuleInitializationComponent::Circular(_) => None,
        })
        .collect())
}

fn metadata_base(
    metadata: &ModuleInitializationMetadata,
    reservations: &[ModuleReservation],
) -> u64 {
    reservations[metadata.image_index].base
}

struct CallbackResult {
    steps: usize,
    outcome: CallbackOutcome,
}

enum CallbackOutcome {
    Returned(u64),
    ProcessExit(u32),
    Fault(ExitReason),
}

#[allow(clippy::too_many_arguments)]
fn call_guest_callback(
    engine: &mut DbtEngine,
    memory: &mut GuestMemory,
    address: u64,
    stack_top: u64,
    teb_base: u64,
    args: [u64; 3],
    cpu: &mut CpuState,
    budget: usize,
) -> Result<CallbackResult, String> {
    memory
        .fetch_instruction_byte(address)
        .map_err(|error| format!("callback address {address:#x} is not executable: {error}"))?;
    let rsp = stack_top
        .checked_sub(CALL_FRAME)
        .ok_or_else(|| "callback stack underflow".to_owned())?;
    memory
        .write_u64_bypass(rsp, crate::tls::TLS_CALLBACK_RETURN)
        .map_err(|error| error.to_string())?;
    *cpu = CpuState::new(address, rsp);
    cpu.gs_base = teb_base;
    cpu.set(Register::Rcx, args[0]);
    cpu.set(Register::Rdx, args[1]);
    cpu.set(Register::R8, args[2]);
    let mut steps = 0usize;
    while steps < budget {
        let slice = (budget - steps).min(INITIALIZATION_SLICE);
        let result = engine.run(cpu, memory, slice);
        steps += result.instructions_executed;
        if result.exit_reason == ExitReason::CallbackReturned
            && cpu.rip == crate::tls::TLS_CALLBACK_RETURN
        {
            return Ok(CallbackResult {
                steps,
                outcome: CallbackOutcome::Returned(cpu.get(Register::Rax)),
            });
        }
        match result.exit_reason {
            ExitReason::ProcessExit(status) => {
                return Ok(CallbackResult {
                    steps,
                    outcome: CallbackOutcome::ProcessExit(status),
                });
            }
            ExitReason::StepLimitExceeded if result.instructions_executed != 0 => {}
            other => {
                return Ok(CallbackResult {
                    steps,
                    outcome: CallbackOutcome::Fault(other),
                });
            }
        }
    }
    Ok(CallbackResult {
        steps,
        outcome: CallbackOutcome::Fault(ExitReason::StepLimitExceeded),
    })
}

#[allow(clippy::too_many_arguments)]
fn run_detach(
    engine: &mut DbtEngine,
    memory: &mut GuestMemory,
    order: &[&ModuleInitializationMetadata],
    attached: &[usize],
    reservations: &[ModuleReservation],
    stack_top: u64,
    teb_base: u64,
    records: &mut Vec<InitializationRecord>,
) {
    // Detach runs after the guest requested process exit, which latches the
    // engine. A fresh engine continues cleanup with the same registry and
    // address space.
    let Some(registry) = engine.take_import_registry() else {
        return;
    };
    let mut cleanup = match DbtEngine::new(1024 * 1024) {
        Ok(engine) => engine,
        Err(_) => return,
    };
    cleanup.set_import_registry(registry);
    let mut cpu = CpuState::new(0, stack_top);
    cpu.gs_base = teb_base;
    for &order_index in attached.iter().rev() {
        let metadata = order[order_index];
        let base = metadata_base(metadata, reservations);
        if !metadata.tls_callbacks.is_empty()
            && let Ok(count) = execute_tls_callbacks_with_reason(
                &metadata.tls_callbacks,
                base,
                base,
                stack_top,
                DLL_PROCESS_DETACH,
                &mut cpu,
                memory,
                &mut cleanup,
            )
        {
            for index in 0..count {
                records.push(InitializationRecord {
                    image_index: metadata.package_index,
                    file_name: metadata.file_name.clone(),
                    phase: InitializationPhase::TlsDetach {
                        callback_index: index,
                    },
                    address: metadata.tls_callbacks[index],
                    steps: 0,
                    outcome: InitializationOutcome::Completed,
                });
            }
        }
        if let ModuleInitializationEntry::DllMain { address } = metadata.entry {
            let result = call_guest_callback(
                &mut cleanup,
                memory,
                address,
                stack_top,
                teb_base,
                [base, DLL_PROCESS_DETACH, 1],
                &mut cpu,
                INITIALIZATION_STEP_BUDGET,
            );
            let (steps, outcome) = match result {
                Ok(CallbackResult { steps, outcome }) => (
                    steps,
                    match outcome {
                        CallbackOutcome::Returned(value) => InitializationOutcome::Returned(value),
                        CallbackOutcome::ProcessExit(status) => {
                            InitializationOutcome::ProcessExit(status)
                        }
                        CallbackOutcome::Fault(reason) => InitializationOutcome::Fault(reason),
                    },
                ),
                Err(_) => (
                    0,
                    InitializationOutcome::Fault(ExitReason::MemoryFault(
                        MemoryError::UnmappedAddress(address),
                    )),
                ),
            };
            records.push(InitializationRecord {
                image_index: metadata.package_index,
                file_name: metadata.file_name.clone(),
                phase: InitializationPhase::DllDetach,
                address,
                steps,
                outcome,
            });
        }
    }
}
