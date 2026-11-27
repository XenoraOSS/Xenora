// SPDX-License-Identifier: GPL-3.0-or-later

//! Transactional service-free execution of explicit module initializers.
//!
//! This is the narrow execution layer above the explicit-module mapping,
//! relocation, ordinary-IAT, TLS-preparation and initialization-planning seams.
//! It validates that those inputs describe the same package and that their
//! memory-visible preparation is present, then invokes TLS callbacks followed
//! by potential `DllMain` routines in the plan's deterministic
//! provider-before-consumer order. It deliberately does not execute a root
//! process entry point, attach host services, model loader-lock reentrancy,
//! create threads, send detach notifications, unload modules or count references.
//!
//! Rollback is deliberately conservative: after validation succeeds, the whole
//! guest memory and CPU state are cloned before the first initializer runs. Any
//! execution fault, budget exhaustion or zero `BOOL` from `DllMain` restores
//! those exact states, including memory writes made by earlier successful
//! initializers. This is correct for the bounded batch interface but not a
//! final long-term transaction representation.

use std::fmt;

use crate::cpu::{CpuState, ExitReason, Register};
use crate::dbt::DbtEngine;
use crate::memory::{GuestMemory, MemoryError};
use crate::module_graph::{ImportOutcome, ModuleGraph, PackageModule};
use crate::module_init::{
    ModuleInitializationComponent, ModuleInitializationEntry, ModuleInitializationError,
    ModuleInitializationPlan, plan_module_initialization,
};
use crate::module_memory::{ExplicitModuleImage, ModuleReservation, validate_reservations};
use crate::module_tls::ModuleTlsReport;
use crate::tls::TLS_CALLBACK_RETURN;

const CALLBACK_FRAME: u64 = 40;
const DLL_PROCESS_ATTACH: u64 = 1;
const TLS_COPY_CHUNK: usize = 4096;

/// Initialization work performed for one module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleInitializerReport {
    pub image_index: usize,
    pub file_name: String,
    pub tls_callbacks_executed: usize,
    pub dll_main_executed: bool,
    /// EAX after `DllMain` returned. TLS callbacks return no status.
    pub dll_main_result: Option<u32>,
}

/// Successful batch execution evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleInitializationExecutionReport {
    pub modules: Vec<ModuleInitializerReport>,
    pub tls_callbacks_executed: usize,
    pub dll_mains_executed: usize,
    pub instructions_executed: usize,
}

/// Routine kind identified in an execution failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitializerRoutine {
    TlsCallback { index: usize },
    DllMain,
}

/// Errors that prevent or end transactional initialization execution. Errors
/// after validation restore memory and CPU before being returned.
#[derive(Debug)]
pub enum ModuleInitializationExecutionError {
    /// Rebuilding the initialization plan rejected the supplied package shape.
    InvalidInputs(ModuleInitializationError),
    /// The supplied plan does not exactly match the package, graph, images,
    /// reservations and TLS reports supplied with it.
    StalePlan,
    /// A circular dependency component has no honest initialization order.
    CircularComponent { image_index: usize },
    /// Incomplete import/export inspection means hidden dependencies could
    /// change the correct order.
    IncompleteInspection { image_index: usize },
    /// An ordinary import has no resolved in-package provider.
    UnresolvedImport {
        consumer_image_index: usize,
        module: String,
        symbol: String,
    },
    /// A resolved ordinary-IAT slot does not contain the expected final guest
    /// provider address before initialization.
    IatSlotMismatch {
        consumer_image_index: usize,
        slot_address: u64,
        expected: u64,
        actual: u64,
    },
    /// A prepared TLS index, array slot or template copy is absent or different.
    TlsStateMismatch {
        image_index: usize,
        field: &'static str,
    },
    /// Memory needed for validation could not be read before execution.
    ValidationMemory { source: MemoryError },
    /// The callback frame requirements are not met. No initializer ran.
    InvalidInitializerStack { stack_top: u64 },
    /// The instruction budgets must be nonzero.
    InvalidInstructionBudgets,
    /// The service-free DBT engine could not be created.
    JitUnavailable(String),
    /// Guest execution faulted or exhausted its budget. Memory and CPU were restored.
    ExecutionFault {
        image_index: usize,
        routine: InitializerRoutine,
        source: ExitReason,
    },
    /// `DllMain` returned zero for DLL_PROCESS_ATTACH. Memory and CPU were restored.
    DllMainFailed { image_index: usize },
}

impl fmt::Display for ModuleInitializationExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInputs(source) => write!(f, "invalid initialization inputs: {source}"),
            Self::StalePlan => f.write_str("supplied initialization plan is stale or inconsistent"),
            Self::CircularComponent { image_index } => {
                write!(
                    f,
                    "image {image_index} belongs to a circular dependency component"
                )
            }
            Self::IncompleteInspection { image_index } => {
                write!(
                    f,
                    "image {image_index} has incomplete import/export inspection"
                )
            }
            Self::UnresolvedImport {
                consumer_image_index,
                module,
                symbol,
            } => write!(
                f,
                "image {consumer_image_index} has unresolved ordinary import {module}!{symbol}"
            ),
            Self::IatSlotMismatch {
                consumer_image_index,
                slot_address,
                expected,
                actual,
            } => write!(
                f,
                "IAT slot {slot_address:#x} in image {consumer_image_index} contains {actual:#x}, expected {expected:#x}"
            ),
            Self::TlsStateMismatch { image_index, field } => {
                write!(
                    f,
                    "prepared TLS state mismatch in image {image_index}: {field}"
                )
            }
            Self::ValidationMemory { source } => {
                write!(f, "validation memory access failed: {source}")
            }
            Self::InvalidInitializerStack { stack_top } => {
                write!(
                    f,
                    "initializer stack top {stack_top:#x} cannot hold an aligned Win64 frame"
                )
            }
            Self::InvalidInstructionBudgets => {
                f.write_str("initializer instruction budgets must be nonzero")
            }
            Self::JitUnavailable(source) => {
                write!(f, "cannot create initializer DBT engine: {source}")
            }
            Self::ExecutionFault {
                image_index,
                routine,
                source,
            } => write!(
                f,
                "initializer {routine:?} in image {image_index} faulted or exhausted its budget: {source}"
            ),
            Self::DllMainFailed { image_index } => {
                write!(
                    f,
                    "DllMain in image {image_index} returned zero for DLL_PROCESS_ATTACH"
                )
            }
        }
    }
}

impl std::error::Error for ModuleInitializationExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidInputs(source) => Some(source),
            Self::ValidationMemory { source } => Some(source),
            _ => None,
        }
    }
}

fn symbol_text(symbol: &crate::import_table::ImportName) -> String {
    match symbol {
        crate::import_table::ImportName::Name(name) => name.clone(),
        crate::import_table::ImportName::Ordinal(ordinal) => format!("#{ordinal}"),
    }
}

fn validate_iat_slots(
    graph: &ModuleGraph,
    reservations: &[ModuleReservation],
    memory: &GuestMemory,
) -> Result<(), ModuleInitializationExecutionError> {
    for resolution in graph.import_resolutions() {
        let ImportOutcome::Resolved { provider, rva, .. } = &resolution.outcome else {
            return Err(ModuleInitializationExecutionError::UnresolvedImport {
                consumer_image_index: resolution.consumer.index,
                module: resolution.normalized_module.clone(),
                symbol: symbol_text(&resolution.symbol),
            });
        };
        let consumer = reservations[resolution.consumer.index];
        let provider_reservation = reservations[provider.index];
        let slot_address = consumer
            .base
            .checked_add(u64::from(resolution.iat_rva))
            .ok_or(ModuleInitializationExecutionError::ValidationMemory {
                source: MemoryError::Overflow,
            })?;
        let expected = provider_reservation
            .base
            .checked_add(u64::from(*rva))
            .ok_or(ModuleInitializationExecutionError::ValidationMemory {
                source: MemoryError::Overflow,
            })?;
        let actual = memory
            .read_u64(slot_address)
            .map_err(|source| ModuleInitializationExecutionError::ValidationMemory { source })?;
        if actual != expected {
            return Err(ModuleInitializationExecutionError::IatSlotMismatch {
                consumer_image_index: resolution.consumer.index,
                slot_address,
                expected,
                actual,
            });
        }
    }
    Ok(())
}

fn validate_tls_state(
    tls_reports: &[ModuleTlsReport],
    thread_tls_base: u64,
    memory: &GuestMemory,
) -> Result<(), ModuleInitializationExecutionError> {
    for report in tls_reports {
        let Some(prepared) = &report.prepared else {
            continue;
        };
        let index = memory
            .read_u32(prepared.index_address)
            .map_err(|source| ModuleInitializationExecutionError::ValidationMemory { source })?;
        if index != prepared.tls_index {
            return Err(ModuleInitializationExecutionError::TlsStateMismatch {
                image_index: report.image_index,
                field: "index",
            });
        }
        let Some(storage) = prepared.storage else {
            continue;
        };
        let slot_address = thread_tls_base
            .checked_add(u64::from(prepared.tls_index) * 8)
            .ok_or(ModuleInitializationExecutionError::ValidationMemory {
                source: MemoryError::Overflow,
            })?;
        let slot = memory
            .read_u64(slot_address)
            .map_err(|source| ModuleInitializationExecutionError::ValidationMemory { source })?;
        if slot != storage.base {
            return Err(ModuleInitializationExecutionError::TlsStateMismatch {
                image_index: report.image_index,
                field: "thread array slot",
            });
        }
        let template_len = prepared
            .template_end
            .checked_sub(prepared.template_start)
            .ok_or(ModuleInitializationExecutionError::TlsStateMismatch {
                image_index: report.image_index,
                field: "template length",
            })?;
        let len = usize::try_from(template_len).map_err(|_| {
            ModuleInitializationExecutionError::TlsStateMismatch {
                image_index: report.image_index,
                field: "template length",
            }
        })?;
        let mut offset = 0usize;
        let mut expected = [0u8; TLS_COPY_CHUNK];
        let mut actual = [0u8; TLS_COPY_CHUNK];
        while offset < len {
            let count = (len - offset).min(TLS_COPY_CHUNK);
            memory
                .read_bytes(
                    prepared.template_start + offset as u64,
                    &mut expected[..count],
                )
                .map_err(
                    |source| ModuleInitializationExecutionError::ValidationMemory { source },
                )?;
            memory
                .read_bytes(storage.base + offset as u64, &mut actual[..count])
                .map_err(
                    |source| ModuleInitializationExecutionError::ValidationMemory { source },
                )?;
            if expected[..count] != actual[..count] {
                return Err(ModuleInitializationExecutionError::TlsStateMismatch {
                    image_index: report.image_index,
                    field: "template copy",
                });
            }
            offset += count;
        }
    }
    Ok(())
}

fn run_routine(
    address: u64,
    dll_handle: u64,
    stack_top: u64,
    budget: usize,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
    dbt: &mut DbtEngine,
) -> Result<(usize, u32), ExitReason> {
    let rsp = stack_top - CALLBACK_FRAME;
    memory
        .write_u64(rsp, TLS_CALLBACK_RETURN)
        .map_err(ExitReason::MemoryFault)?;
    cpu.rip = address;
    cpu.set(Register::Rsp, rsp);
    cpu.set(Register::Rcx, dll_handle);
    cpu.set(Register::Rdx, DLL_PROCESS_ATTACH);
    cpu.set(Register::R8, 0);

    let mut used = 0usize;
    while cpu.rip != TLS_CALLBACK_RETURN {
        let remaining = budget.saturating_sub(used);
        if remaining == 0 {
            return Err(ExitReason::StepLimitExceeded);
        }
        let result = dbt.run(cpu, memory, remaining);
        used = used.saturating_add(result.instructions_executed);
        if cpu.rip == TLS_CALLBACK_RETURN {
            break;
        }
        if result.exit_reason != ExitReason::StepLimitExceeded || result.instructions_executed == 0
        {
            return Err(result.exit_reason);
        }
        if used >= budget {
            return Err(ExitReason::StepLimitExceeded);
        }
    }
    Ok((used, cpu.get(Register::Rax) as u32))
}

/// Validate and execute process-attach initializers for one explicit package.
///
/// `tls_reports` and `thread_tls_base` must describe the same preparation that
/// produced `plan`. A root executable's process entry is ignored here by
/// design; only TLS callbacks and DLL entry points are initialization work.
#[allow(clippy::too_many_arguments)]
pub fn execute_module_initialization(
    package: &[PackageModule<'_>],
    images: &[ExplicitModuleImage<'_>],
    graph: &ModuleGraph,
    reservations: &[ModuleReservation],
    tls_reports: &[ModuleTlsReport],
    plan: &ModuleInitializationPlan,
    thread_tls_base: u64,
    stack_top: u64,
    per_routine_instruction_budget: usize,
    total_instruction_budget: usize,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<ModuleInitializationExecutionReport, ModuleInitializationExecutionError> {
    let rebuilt =
        plan_module_initialization(package, images, graph, reservations, Some(tls_reports))
            .map_err(ModuleInitializationExecutionError::InvalidInputs)?;
    if rebuilt != *plan {
        return Err(ModuleInitializationExecutionError::StalePlan);
    }
    if !plan.inspection_incomplete.is_empty() {
        return Err(ModuleInitializationExecutionError::IncompleteInspection {
            image_index: plan.inspection_incomplete[0].image_index,
        });
    }
    for component in &plan.components {
        if let ModuleInitializationComponent::Circular(members) = component {
            return Err(ModuleInitializationExecutionError::CircularComponent {
                image_index: members[0].image_index,
            });
        }
    }
    if stack_top < CALLBACK_FRAME || !stack_top.is_multiple_of(16) {
        return Err(ModuleInitializationExecutionError::InvalidInitializerStack { stack_top });
    }
    if per_routine_instruction_budget == 0 || total_instruction_budget == 0 {
        return Err(ModuleInitializationExecutionError::InvalidInstructionBudgets);
    }
    if !thread_tls_base.is_multiple_of(8) {
        return Err(ModuleInitializationExecutionError::TlsStateMismatch {
            image_index: 0,
            field: "thread array alignment",
        });
    }

    let ordered_reservations = validate_reservations(images, reservations)
        .map_err(|error| ModuleInitializationExecutionError::InvalidInputs(error.into()))?;
    validate_iat_slots(graph, &ordered_reservations, memory)?;
    validate_tls_state(tls_reports, thread_tls_base, memory)?;

    let before_memory = memory.clone();
    let before_cpu = cpu.clone();
    let mut dbt = DbtEngine::new(1024 * 1024)
        .map_err(|error| ModuleInitializationExecutionError::JitUnavailable(error.to_string()))?;
    let mut report = ModuleInitializationExecutionReport {
        modules: Vec::new(),
        tls_callbacks_executed: 0,
        dll_mains_executed: 0,
        instructions_executed: 0,
    };

    enum RoutineTermination {
        Fault {
            image_index: usize,
            routine: InitializerRoutine,
            source: ExitReason,
        },
        DllMainFailed {
            image_index: usize,
        },
    }

    let result = (|| {
        for component in &plan.components {
            let ModuleInitializationComponent::Acyclic(metadata) = component else {
                unreachable!("circular components are rejected before execution")
            };
            let reservation = ordered_reservations[metadata.image_index];
            let mut module_report = ModuleInitializerReport {
                image_index: metadata.image_index,
                file_name: metadata.file_name.clone(),
                tls_callbacks_executed: 0,
                dll_main_executed: false,
                dll_main_result: None,
            };
            for (callback_index, callback) in metadata.tls_callbacks.iter().enumerate() {
                let remaining =
                    total_instruction_budget.saturating_sub(report.instructions_executed);
                let budget = per_routine_instruction_budget.min(remaining);
                if budget == 0 {
                    return Err(RoutineTermination::Fault {
                        image_index: metadata.image_index,
                        routine: InitializerRoutine::TlsCallback {
                            index: callback_index,
                        },
                        source: ExitReason::StepLimitExceeded,
                    });
                }
                let (used, _) = run_routine(
                    *callback,
                    reservation.base,
                    stack_top,
                    budget,
                    cpu,
                    memory,
                    &mut dbt,
                )
                .map_err(|source| RoutineTermination::Fault {
                    image_index: metadata.image_index,
                    routine: InitializerRoutine::TlsCallback {
                        index: callback_index,
                    },
                    source,
                })?;
                report.instructions_executed += used;
                module_report.tls_callbacks_executed += 1;
                report.tls_callbacks_executed += 1;
            }
            if let ModuleInitializationEntry::DllMain { address } = metadata.entry {
                let remaining =
                    total_instruction_budget.saturating_sub(report.instructions_executed);
                let budget = per_routine_instruction_budget.min(remaining);
                if budget == 0 {
                    return Err(RoutineTermination::Fault {
                        image_index: metadata.image_index,
                        routine: InitializerRoutine::DllMain,
                        source: ExitReason::StepLimitExceeded,
                    });
                }
                let (used, eax) = run_routine(
                    address,
                    reservation.base,
                    stack_top,
                    budget,
                    cpu,
                    memory,
                    &mut dbt,
                )
                .map_err(|source| RoutineTermination::Fault {
                    image_index: metadata.image_index,
                    routine: InitializerRoutine::DllMain,
                    source,
                })?;
                report.instructions_executed += used;
                module_report.dll_main_executed = true;
                module_report.dll_main_result = Some(eax);
                report.dll_mains_executed += 1;
                if eax == 0 {
                    return Err(RoutineTermination::DllMainFailed {
                        image_index: metadata.image_index,
                    });
                }
            }
            report.modules.push(module_report);
        }
        Ok(())
    })();

    match result {
        Ok(()) => Ok(report),
        Err(RoutineTermination::DllMainFailed { image_index }) => {
            *memory = before_memory;
            *cpu = before_cpu;
            Err(ModuleInitializationExecutionError::DllMainFailed { image_index })
        }
        Err(RoutineTermination::Fault {
            image_index,
            routine,
            source,
        }) => {
            *memory = before_memory;
            *cpu = before_cpu;
            Err(ModuleInitializationExecutionError::ExecutionFault {
                image_index,
                routine,
                source,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryPermissions;
    use crate::module_tls::{ModuleTlsStorage, PreparedModuleTls};
    use crate::pe::ParsedPe;

    const BASE: u64 = 0x1_4000_0000;
    const OPTIONAL: usize = 0x98;
    const RVA_CODE: u32 = 0x1000;
    const RVA_CALLBACK: u32 = 0x1020;
    const RVA_TLS: u32 = 0x1100;
    const RVA_TEMPLATE: u32 = 0x1180;
    const RVA_INDEX: u32 = 0x1190;
    const RVA_CALLBACKS: u32 = 0x11a0;
    const RAW_TEXT: usize = 0x200;
    const THREAD: u64 = 0x3000_0000;
    const TLS_STORAGE: u64 = THREAD + 0x1000;
    const SCRATCH: u64 = THREAD + 0x2000;
    const STACK_TOP: u64 = THREAD + 0x5000;

    fn put(pe: &mut [u8], at: usize, bytes: &[u8]) {
        pe[at..at + bytes.len()].copy_from_slice(bytes);
    }
    fn put16(pe: &mut [u8], at: usize, value: u16) {
        put(pe, at, &value.to_le_bytes());
    }
    fn put32(pe: &mut [u8], at: usize, value: u32) {
        put(pe, at, &value.to_le_bytes());
    }
    fn put64(pe: &mut [u8], at: usize, value: u64) {
        put(pe, at, &value.to_le_bytes());
    }
    fn raw(rva: u32) -> usize {
        RAW_TEXT + (rva - RVA_CODE) as usize
    }

    fn store_routine(pe: &mut [u8], rva: u32, target: u64, value: u32, result: u32) {
        let at = raw(rva);
        pe[at] = 0x48;
        pe[at + 1] = 0xb8;
        put64(pe, at + 2, target);
        pe[at + 10] = 0xc7;
        pe[at + 11] = 0x00;
        put32(pe, at + 12, value);
        pe[at + 16] = 0xb8;
        put32(pe, at + 17, result);
        pe[at + 21] = 0xc3;
    }

    fn image(dll_result: u32) -> Vec<u8> {
        let mut pe = vec![0; RAW_TEXT + 0x1000];
        put(&mut pe, 0, b"MZ");
        put32(&mut pe, 0x3c, 0x80);
        put(&mut pe, 0x80, b"PE\0\0");
        put16(&mut pe, 0x84, 0x8664);
        put16(&mut pe, 0x86, 1);
        put16(&mut pe, 0x94, 240);
        put16(&mut pe, 0x96, 0x2000);
        put16(&mut pe, OPTIONAL, 0x20b);
        put32(&mut pe, OPTIONAL + 16, RVA_CODE);
        put64(&mut pe, OPTIONAL + 24, BASE);
        put32(&mut pe, OPTIONAL + 32, 0x1000);
        put32(&mut pe, OPTIONAL + 36, 0x200);
        put32(&mut pe, OPTIONAL + 56, 0x2000);
        put32(&mut pe, OPTIONAL + 60, 0x200);
        put32(&mut pe, OPTIONAL + 108, 16);
        let section = OPTIONAL + 240;
        put(&mut pe, section, b".text");
        put32(&mut pe, section + 8, 0x1000);
        put32(&mut pe, section + 12, RVA_CODE);
        put32(&mut pe, section + 16, 0x1000);
        put32(&mut pe, section + 20, RAW_TEXT as u32);
        put32(&mut pe, section + 36, 0xe000_0020);
        let tls_directory = OPTIONAL + 112 + 9 * 8;
        put32(&mut pe, tls_directory, RVA_TLS);
        put32(&mut pe, tls_directory + 4, 40);
        let tls = raw(RVA_TLS);
        put64(&mut pe, tls, BASE + u64::from(RVA_TEMPLATE));
        put64(&mut pe, tls + 8, BASE + u64::from(RVA_TEMPLATE) + 4);
        put64(&mut pe, tls + 16, BASE + u64::from(RVA_INDEX));
        put64(&mut pe, tls + 24, BASE + u64::from(RVA_CALLBACKS));
        put64(&mut pe, raw(RVA_CALLBACKS), BASE + u64::from(RVA_CALLBACK));
        put(&mut pe, raw(RVA_TEMPLATE), &[9, 8, 7, 6]);
        store_routine(&mut pe, RVA_CALLBACK, SCRATCH, 2, 0);
        store_routine(&mut pe, RVA_CODE, SCRATCH + 4, 1, dll_result);
        pe
    }

    struct Fixture {
        bytes: Vec<u8>,
        pe: ParsedPe,
        memory: GuestMemory,
        reservation: ModuleReservation,
        tls: ModuleTlsReport,
    }

    fn fixture(dll_result: u32) -> Fixture {
        let bytes = image(dll_result);
        let pe = ParsedPe::parse(&bytes).unwrap();
        let mut memory = GuestMemory::new();
        memory
            .map_region(BASE, 0x2000, MemoryPermissions::READ_WRITE_EXECUTE)
            .unwrap();
        memory
            .write_bytes(
                BASE + u64::from(RVA_CODE),
                &bytes[RAW_TEXT..RAW_TEXT + 0x1000],
            )
            .unwrap();
        memory
            .map_region(THREAD, 0x5000, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory.write_u32(BASE + u64::from(RVA_INDEX), 0).unwrap();
        memory.write_u64(THREAD, TLS_STORAGE).unwrap();
        memory.write_bytes(TLS_STORAGE, &[9, 8, 7, 6]).unwrap();
        let reservation = ModuleReservation {
            image_index: 0,
            base: BASE,
            end: BASE + 0x2000,
            used_preferred_base: true,
            entry_point: Some(BASE + u64::from(RVA_CODE)),
        };
        let tls = ModuleTlsReport {
            image_index: 0,
            prepared: Some(PreparedModuleTls {
                tls_index: 0,
                index_address: BASE + u64::from(RVA_INDEX),
                template_start: BASE + u64::from(RVA_TEMPLATE),
                template_end: BASE + u64::from(RVA_TEMPLATE) + 4,
                storage: Some(ModuleTlsStorage {
                    base: TLS_STORAGE,
                    end: TLS_STORAGE + 4,
                    template_bytes: 4,
                    zero_fill_bytes: 0,
                }),
                callbacks: vec![BASE + u64::from(RVA_CALLBACK)],
            }),
        };
        Fixture {
            bytes,
            pe,
            memory,
            reservation,
            tls,
        }
    }

    fn plan_parts<'a>(
        fixture: &'a Fixture,
    ) -> (
        Vec<PackageModule<'a>>,
        Vec<ExplicitModuleImage<'a>>,
        ModuleGraph,
        Vec<ModuleReservation>,
        Vec<ModuleTlsReport>,
        ModuleInitializationPlan,
    ) {
        let package = vec![PackageModule {
            file_name: "one.dll",
            bytes: &fixture.bytes,
        }];
        let images = vec![ExplicitModuleImage {
            pe: &fixture.pe,
            bytes: &fixture.bytes,
        }];
        let graph = ModuleGraph::build(&package).unwrap();
        let reservations = vec![fixture.reservation];
        let tls = vec![fixture.tls.clone()];
        let plan = plan_module_initialization(&package, &images, &graph, &reservations, Some(&tls))
            .unwrap();
        (package, images, graph, reservations, tls, plan)
    }

    #[test]
    fn tls_callback_and_dllmain_run_and_write_memory() {
        let fixture = fixture(1);
        let (package, images, graph, reservations, tls, plan) = plan_parts(&fixture);
        let mut cpu = CpuState::default();
        let mut memory = fixture.memory.clone();
        let report = execute_module_initialization(
            &package,
            &images,
            &graph,
            &reservations,
            &tls,
            &plan,
            THREAD,
            STACK_TOP,
            1000,
            2000,
            &mut cpu,
            &mut memory,
        )
        .unwrap();
        assert_eq!(report.tls_callbacks_executed, 1);
        assert_eq!(report.dll_mains_executed, 1);
        assert_eq!(report.modules[0].dll_main_result, Some(1));
        assert_eq!(memory.read_u32(SCRATCH), Ok(2));
        assert_eq!(memory.read_u32(SCRATCH + 4), Ok(1));
    }

    #[test]
    fn zero_dllmain_restores_memory_and_cpu_exactly() {
        let fixture = fixture(0);
        let (package, images, graph, reservations, tls, plan) = plan_parts(&fixture);
        let mut cpu = CpuState::default();
        let mut memory = fixture.memory.clone();
        let before_cpu = cpu.clone();
        let error = execute_module_initialization(
            &package,
            &images,
            &graph,
            &reservations,
            &tls,
            &plan,
            THREAD,
            STACK_TOP,
            1000,
            2000,
            &mut cpu,
            &mut memory,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ModuleInitializationExecutionError::DllMainFailed { image_index: 0 }
        ));
        assert_eq!(cpu, before_cpu);
        assert_eq!(memory.read_u32(SCRATCH), Ok(0));
        assert_eq!(memory.read_u32(SCRATCH + 4), Ok(0));
        assert_eq!(memory.read_u32(BASE + u64::from(RVA_INDEX)), Ok(0));
        assert_eq!(memory.read_u64(THREAD), Ok(TLS_STORAGE));
    }
}
