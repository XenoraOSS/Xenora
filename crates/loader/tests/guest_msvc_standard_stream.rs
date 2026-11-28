// SPDX-License-Identifier: GPL-3.0-or-later
//! Original freestanding MSVC-ABI guest: cout initialization, output and exit.
use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};

const LIMIT: ExitReason = ExitReason::IllegalInstruction(0xFE);

fn run(bytes: &[u8]) -> (Arc<ServicesState>, ExitReason) {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let result = run_synthetic_startup(
        bytes,
        StartupLayout {
            image_base: 0x1_4000_0000,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        },
        registry,
        2_000_000,
    )
    .unwrap();
    (state, result.execution.exit_reason)
}

#[test]
fn cout_initialization_writes_and_terminates_cleanly() {
    let (state, reason) = run(include_bytes!(
        "../../../fixtures/pe/msvc-standard-stream.exe"
    ));
    assert_eq!(reason, ExitReason::ProcessExit(0));
    assert_eq!(state.standard_output(), b"hello from std::cout\r\n");
    assert!(state.standard_error().is_empty());
}

#[test]
fn cout_write_rejects_a_forged_object() {
    let (state, reason) = run(include_bytes!(
        "../../../fixtures/pe/msvc-standard-stream-invalid.exe"
    ));
    assert!(matches!(reason, ExitReason::MemoryFault(_)));
    assert!(state.standard_output().is_empty());
}

#[test]
fn cout_write_stops_when_the_exception_mask_would_fire() {
    let (state, reason) = run(include_bytes!(
        "../../../fixtures/pe/msvc-standard-stream-exception.exe"
    ));
    assert_eq!(reason, LIMIT);
    assert!(state.standard_output().is_empty());
}

#[test]
fn cout_write_refuses_an_oversized_transfer() {
    let (state, reason) = run(include_bytes!(
        "../../../fixtures/pe/msvc-standard-stream-oversize.exe"
    ));
    assert_eq!(reason, LIMIT);
    assert!(state.standard_output().is_empty());
}

#[test]
fn cout_state_does_not_leak_between_processes() {
    let bytes = include_bytes!("../../../fixtures/pe/msvc-standard-stream.exe");
    for _ in 0..2 {
        let (state, reason) = run(bytes);
        assert_eq!(reason, ExitReason::ProcessExit(0));
        assert_eq!(state.standard_output(), b"hello from std::cout\r\n");
        assert!(state.standard_error().is_empty());
    }
}
