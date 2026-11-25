// SPDX-License-Identifier: GPL-3.0-or-later

//! Process startup for trusted, original PE32+ fixtures.

use crate::dbt::DbtMetrics;
use crate::tls::TlsError;
use crate::{
    CpuState, DbtEngine, ExecutionResult, ExitReason, ImportRegistry, LoadedImage, MemoryError,
    MemoryPermissions, ParsedPe, PeError, RelocationReport, apply_relocations,
    execute_tls_callbacks, parse_tls_directory, setup_tls,
};

const STACK_SIZE: usize = 64 * 1024;
const JIT_CAPACITY: usize = 1024 * 1024;
const RETURN_SENTINEL_PAGE_SIZE: usize = 0x1000;
/// The TEB is placed this far above the TLS base and spans two 4 KiB pages.
const TEB_OFFSET: u64 = 0x10000;
const TEB_SIZE: usize = 0x2000;
// Win64 TEB fields (NT_TIB prefix plus the TLS slot) written by the harness.
const TEB_STACK_BASE: u64 = 0x08;
const TEB_STACK_LIMIT: u64 = 0x10;
const TEB_SELF: u64 = 0x30;
const TEB_TLS_POINTER: u64 = 0x58;

/// Guest addresses reserved by the fixture harness.
pub struct StartupLayout {
    pub image_base: u64,
    /// End of the fixture stack. Must be 16-byte aligned.
    pub stack_top: u64,
    pub tls_base: u64,
}

pub struct StartupResult {
    pub image: LoadedImage,
    pub relocations: RelocationReport,
    pub bound_imports: usize,
    pub tls_callbacks: usize,
    pub execution: ExecutionResult,
    pub dbt_metrics: DbtMetrics,
}

/// Completed startup phases and the transition to the guest entry.
pub enum StartupEvent {
    Parsed {
        sections: usize,
        entry_rva: u32,
    },
    Mapped {
        entry_va: u64,
    },
    Relocated {
        applied_dir64: usize,
    },
    ImportsBound {
        count: usize,
    },
    TlsCompleted {
        callbacks: usize,
        native_steps: usize,
    },
    Entering {
        step_limit: usize,
    },
}

/// Map, rebase and bind a trusted synthetic fixture, then run its TLS callbacks
/// and entry point. Candidate inspection uses the static probe separately.
pub fn run_synthetic_startup(
    bytes: &[u8],
    layout: StartupLayout,
    imports: ImportRegistry,
    entry_budget: usize,
) -> Result<StartupResult, Box<dyn std::error::Error>> {
    run_synthetic_startup_with_observer(bytes, layout, imports, entry_budget, |_| {})
}

/// Run a trusted synthetic fixture and report milestones in execution order.
pub fn run_synthetic_startup_with_observer(
    bytes: &[u8],
    layout: StartupLayout,
    imports: ImportRegistry,
    entry_budget: usize,
    observe: impl FnMut(StartupEvent),
) -> Result<StartupResult, Box<dyn std::error::Error>> {
    run_startup_with(
        bytes,
        layout,
        imports,
        entry_budget,
        observe,
        |engine, memory, entry_cpu, budget| {
            let mut entry_cpu = entry_cpu.clone();
            engine.run(&mut entry_cpu, memory, budget)
        },
    )
}

/// Run a trusted synthetic fixture with the entry point driven by the
/// deterministic cooperative scheduler.
///
/// The scheduler runs one guest thread at a time, so a guest that creates a
/// thread with `CreateThread` runs to completion with every thread taking
/// turns. The entry point and the threads it creates share one instruction
/// budget.
///
/// The state must be the one the import registry was built with: the
/// scheduler finds the guest's threads through it.
pub fn run_synthetic_startup_scheduled(
    bytes: &[u8],
    layout: StartupLayout,
    imports: ImportRegistry,
    state: std::sync::Arc<crate::services::ServicesState>,
    entry_budget: usize,
    quantum: usize,
) -> Result<StartupResult, Box<dyn std::error::Error>> {
    run_startup_with(
        bytes,
        layout,
        imports,
        entry_budget,
        |_| {},
        move |engine, memory, entry_cpu, budget| match crate::scheduler::Scheduler::new(
            std::sync::Arc::clone(&state),
            quantum,
        )
        .run(engine, memory, entry_cpu, budget)
        {
            Ok(report) => ExecutionResult {
                instructions_executed: report.instructions,
                final_state: report.main,
                exit_reason: scheduled_exit_reason(report.outcome),
            },
            Err(reason) => ExecutionResult {
                instructions_executed: 0,
                final_state: entry_cpu.clone(),
                exit_reason: reason,
            },
        },
    )
}

/// The guest's exit reason from the scheduler's report of how the run ended.
fn scheduled_exit_reason(outcome: crate::scheduler::Outcome) -> ExitReason {
    use crate::scheduler::Outcome;
    match outcome {
        Outcome::Halted => ExitReason::Halt,
        Outcome::AllThreadsExited => ExitReason::Halt,
        Outcome::BudgetExhausted => ExitReason::StepLimitExceeded,
        Outcome::Deadlock { .. } => ExitReason::GuestBlocked,
        Outcome::Fault { reason, .. } => reason,
    }
}

/// The shared startup, with the entry point left to the caller to run.
fn run_startup_with(
    bytes: &[u8],
    layout: StartupLayout,
    mut imports: ImportRegistry,
    entry_budget: usize,
    mut observe: impl FnMut(StartupEvent),
    run_entry: impl FnOnce(
        &mut DbtEngine,
        &mut crate::memory::GuestMemory,
        &CpuState,
        usize,
    ) -> ExecutionResult,
) -> Result<StartupResult, Box<dyn std::error::Error>> {
    if !layout.stack_top.is_multiple_of(16) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "fixture stack top must be 16-byte aligned",
        )
        .into());
    }
    // Reject an impossible layout before parsing or mapping anything.
    let (stack_base, teb_base) = layout_regions(&layout)?;
    let pe = ParsedPe::parse(bytes)?;
    observe(StartupEvent::Parsed {
        sections: pe.sections.len(),
        entry_rva: pe.entry_point_rva,
    });
    let mut image = pe.load_at_base(bytes, layout.image_base)?;
    let image_base_field = image.image_base_field;
    let memory = &mut image.memory;
    memory
        .fetch_instruction_byte(image.entry_point)
        .map_err(|_| PeError::InvalidEntryPoint)?;
    observe(StartupEvent::Mapped {
        entry_va: image.entry_point,
    });

    let relocations = apply_relocations(&pe, bytes, layout.image_base, memory)?;
    // The mapped optional header names the base the image is loaded at. This
    // is written after the relocations so that an entry aimed at the field
    // itself cannot leave it holding anything else.
    memory
        .write_u64_bypass(image_base_field, layout.image_base)
        .map_err(PeError::SectionMappingError)?;
    observe(StartupEvent::Relocated {
        applied_dir64: relocations.applied_dir64,
    });
    memory.main_image = Some(crate::memory::MainImage {
        base: layout.image_base,
        size: pe.size_of_image,
        entry_point: image.entry_point,
    });
    crate::exceptions::register_exception_directory(
        bytes,
        layout.image_base,
        pe.size_of_image,
        memory,
    )
    .map_err(|why| std::io::Error::new(std::io::ErrorKind::InvalidData, why))?;
    let bound_imports = imports.bind_imports(&pe, bytes, layout.image_base, memory)?;
    observe(StartupEvent::ImportsBound {
        count: bound_imports,
    });
    let tls = parse_tls_directory(bytes, &pe)?;
    if let Some(tls) = &tls {
        setup_tls(
            tls,
            layout.image_base,
            pe.image_base,
            memory,
            layout.tls_base,
        )?;
    }

    memory.map_region(stack_base, STACK_SIZE, MemoryPermissions::READ_WRITE)?;
    // Keep guest code and data out of the page that holds the return
    // addresses the engine intercepts. An image or layout that already covers
    // it is refused here, and a later guest allocation there fails.
    memory.map_region(
        crate::imports::RETURN_SENTINEL_PAGE,
        RETURN_SENTINEL_PAGE_SIZE,
        MemoryPermissions::NO_ACCESS,
    )?;

    map_teb(memory, teb_base, &layout, stack_base)?;

    let mut engine = DbtEngine::new(JIT_CAPACITY)?;
    engine.set_import_registry(imports);

    let mut tls_cpu = CpuState::new(image.entry_point, layout.stack_top);
    tls_cpu.gs_base = teb_base;
    let mut tls_callbacks = 0;
    let mut exited_in_tls = None;
    if let Some(tls) = &tls {
        // One callback at a time, so that a callback which ends the process
        // is seen as an exit and not as a fault, and the callbacks that
        // finished before it are counted.
        for callback in &tls.callbacks {
            match execute_tls_callbacks(
                std::slice::from_ref(callback),
                layout.image_base,
                pe.image_base,
                layout.stack_top,
                &mut tls_cpu,
                memory,
                &mut engine,
            ) {
                Ok(_) => tls_callbacks += 1,
                Err(TlsError::CallbackExecutionFault(ExitReason::ProcessExit(status))) => {
                    exited_in_tls = Some(status);
                    break;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    observe(StartupEvent::TlsCompleted {
        callbacks: tls_callbacks,
        native_steps: engine.metrics.instructions_executed,
    });
    if let Some(status) = exited_in_tls {
        // The process ended before its entry point: the remaining callbacks
        // and the entry point do not run, and this is an ordinary outcome.
        return Ok(StartupResult {
            image,
            relocations,
            bound_imports,
            tls_callbacks,
            execution: ExecutionResult {
                instructions_executed: 0,
                final_state: tls_cpu,
                exit_reason: ExitReason::ProcessExit(status),
            },
            dbt_metrics: engine.metrics,
        });
    }
    observe(StartupEvent::Entering {
        step_limit: entry_budget,
    });

    // Model function entry with the Win64 32-byte home area and return slot.
    // An entry point may end with HLT, call ExitProcess, or return: the
    // return slot holds the address that ends the process with EAX.
    memory.write_u64_bypass(layout.stack_top - 40, crate::imports::ENTRY_RETURN)?;
    let mut entry_cpu = CpuState::new(image.entry_point, layout.stack_top - 40);
    entry_cpu.gs_base = teb_base;
    let execution = run_entry(&mut engine, memory, &entry_cpu, entry_budget);
    Ok(StartupResult {
        image,
        relocations,
        bound_imports,
        tls_callbacks,
        execution,
        dbt_metrics: engine.metrics,
    })
}

/// Validate the harness layout and return `(stack_base, teb_base)`.
///
/// The stack occupies `[stack_top - STACK_SIZE, stack_top)` and the TEB
/// `[tls_base + TEB_OFFSET, + TEB_SIZE)`. Both ranges must fit in the address
/// space and must not overlap each other.
fn layout_regions(layout: &StartupLayout) -> Result<(u64, u64), MemoryError> {
    let stack_base = layout
        .stack_top
        .checked_sub(STACK_SIZE as u64)
        .ok_or(MemoryError::Overflow)?;
    let teb_base = layout
        .tls_base
        .checked_add(TEB_OFFSET)
        .ok_or(MemoryError::Overflow)?;
    let teb_end = teb_base
        .checked_add(TEB_SIZE as u64)
        .ok_or(MemoryError::Overflow)?;
    if teb_base < layout.stack_top && stack_base < teb_end {
        return Err(MemoryError::OverlappingRegion {
            base: teb_base,
            size: TEB_SIZE as u64,
        });
    }
    Ok((stack_base, teb_base))
}

/// Map the Win64 TEB and write the fields the harness provides.
fn map_teb(
    memory: &mut crate::GuestMemory,
    teb_base: u64,
    layout: &StartupLayout,
    stack_base: u64,
) -> Result<(), MemoryError> {
    memory.map_region(teb_base, TEB_SIZE, MemoryPermissions::READ_WRITE)?;
    memory.write_u64_bypass(teb_base + TEB_STACK_BASE, layout.stack_top)?;
    memory.write_u64_bypass(teb_base + TEB_STACK_LIMIT, stack_base)?;
    memory.write_u64_bypass(teb_base + TEB_SELF, teb_base)?;
    memory.write_u64_bypass(teb_base + TEB_TLS_POINTER, layout.tls_base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GuestMemory;

    fn layout(stack_top: u64, tls_base: u64) -> StartupLayout {
        StartupLayout {
            image_base: 0x1_4000_0000,
            stack_top,
            tls_base,
        }
    }

    #[test]
    fn valid_layout_places_stack_and_teb() {
        let regions = layout_regions(&layout(0x2_0001_0000, 0x3_0000_0000));
        assert_eq!(regions, Ok((0x2_0000_0000, 0x3_0001_0000)));
    }

    #[test]
    fn stack_below_zero_is_rejected() {
        assert_eq!(
            layout_regions(&layout(0x8000, 0x3_0000_0000)),
            Err(MemoryError::Overflow)
        );
    }

    #[test]
    fn teb_address_overflow_is_rejected_not_saturated() {
        for tls_base in [u64::MAX, u64::MAX - 0xFFFF, u64::MAX - TEB_OFFSET - 0x1FFF] {
            assert_eq!(
                layout_regions(&layout(0x2_0001_0000, tls_base)),
                Err(MemoryError::Overflow),
                "tls_base {tls_base:#x}"
            );
        }
        // The highest TEB that fits ends at u64::MAX: an end of 2^64 is not
        // representable, and GuestMemory could not map that range either.
        let top = u64::MAX - TEB_OFFSET - TEB_SIZE as u64;
        assert!(layout_regions(&layout(0x2_0001_0000, top)).is_ok());
    }

    #[test]
    fn teb_overlapping_stack_is_rejected() {
        // TEB at 0x2_0000_8000..0x2_0000_A000 lies inside the stack 0x2_0000_0000..0x2_0001_0000.
        let collide = layout(0x2_0001_0000, 0x1_FFFF_8000);
        assert!(matches!(
            layout_regions(&collide),
            Err(MemoryError::OverlappingRegion { .. })
        ));
        // Directly above the stack is fine.
        assert!(layout_regions(&layout(0x2_0001_0000, 0x2_0000_0000)).is_ok());
    }

    #[test]
    fn invalid_layout_fails_before_parsing() {
        // Empty bytes would fail PE parsing; the layout error must come first.
        let err = run_synthetic_startup(
            &[],
            layout(0x2_0001_0000, u64::MAX),
            ImportRegistry::new(),
            1,
        )
        .err()
        .expect("startup must fail");
        assert_eq!(
            err.downcast_ref::<MemoryError>(),
            Some(&MemoryError::Overflow)
        );
    }

    #[test]
    fn teb_fields_are_at_win64_offsets() {
        let l = layout(0x2_0001_0000, 0x3_0000_0000);
        let (stack_base, teb) = layout_regions(&l).unwrap();
        let mut memory = GuestMemory::new();
        map_teb(&mut memory, teb, &l, stack_base).unwrap();
        assert_eq!(memory.read_u64(teb + 0x08), Ok(0x2_0001_0000));
        assert_eq!(memory.read_u64(teb + 0x10), Ok(0x2_0000_0000));
        assert_eq!(memory.read_u64(teb + 0x30), Ok(teb));
        assert_eq!(memory.read_u64(teb + 0x58), Ok(0x3_0000_0000));
        assert!(memory.read_u8(teb + TEB_SIZE as u64).is_err());
    }
}
