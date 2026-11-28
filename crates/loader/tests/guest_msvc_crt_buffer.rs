// SPDX-License-Identifier: GPL-3.0-or-later
//! Original guest exercises the unbuffered UCRT buffer-field interface.
use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
fn run(bytes: &[u8]) -> (ExitReason, Vec<u8>) {
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
    (result.execution.exit_reason, state.standard_output())
}
#[test]
fn standard_stream_fields_are_live_and_output_stays_unbuffered() {
    let (exit, output) = run(include_bytes!("../../../fixtures/pe/msvc-crt-buffer.exe"));
    assert_eq!(exit, ExitReason::ProcessExit(0));
    assert_eq!(output, b"X");
}
#[test]
fn invalid_stream_stops_instead_of_exposing_memory() {
    let (exit, _) = run(include_bytes!(
        "../../../fixtures/pe/msvc-crt-buffer-invalid.exe"
    ));
    assert_eq!(exit, ExitReason::IllegalInstruction(0xFE));
}

#[test]
fn invalid_output_pointer_faults_guest() {
    let (exit, _) = run(include_bytes!(
        "../../../fixtures/pe/msvc-crt-buffer-bad_output.exe"
    ));
    assert!(matches!(exit, ExitReason::MemoryFault(_)));
}
