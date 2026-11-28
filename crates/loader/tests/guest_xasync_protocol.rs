// SPDX-License-Identifier: GPL-3.0-or-later
//! Original guest exercise of the XAsync provider protocol end to end: the
//! fixture drives a custom provider through Begin, the scheduled DoWork, the
//! completion callback, the status query and the GetResult payload retrieval
//! ending in Cleanup, exiting nonzero at the exact step that failed.
use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};

#[test]
fn xasync_protocol_round_trips_through_a_guest_provider() {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    let result = run_synthetic_startup(
        include_bytes!("../../../fixtures/pe/xasync-protocol.exe"),
        StartupLayout {
            image_base: 0x1_4000_0000,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        },
        registry,
        2_000_000,
    )
    .unwrap();
    assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
}

fn run_variant(bytes: &[u8]) -> ExitReason {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    run_synthetic_startup(
        bytes,
        StartupLayout {
            image_base: 0x1_4000_0000,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        },
        registry,
        2_000_000,
    )
    .unwrap()
    .execution
    .exit_reason
}

/// The cancel variant: cancelling before the work is pumped invokes the
/// provider's Cancel synchronously, the status reads E_ABORT and the
/// retrieval answers it without ever invoking GetResult.
#[test]
fn xasync_cancel_aborts_before_the_work_runs() {
    assert_eq!(
        run_variant(include_bytes!(
            "../../../fixtures/pe/xasync-protocol-cancel.exe"
        )),
        ExitReason::ProcessExit(0)
    );
}

/// The process-queue variant: a block with no queue of its own schedules
/// through the process queue the caller set, and the whole protocol
/// round-trips identically.
#[test]
fn xasync_schedules_through_the_process_queue_fallback() {
    assert_eq!(
        run_variant(include_bytes!(
            "../../../fixtures/pe/xasync-protocol-process.exe"
        )),
        ExitReason::ProcessExit(0)
    );
}
