// SPDX-License-Identifier: GPL-3.0-or-later
//! Original guest checking bounded Microsoft ios_base state accessors.
use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};

#[test]
fn ios_state_accessors_report_the_guest_bits() {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    let result = run_synthetic_startup(
        include_bytes!("../../../fixtures/pe/msvc-ios-state.exe"),
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

#[test]
fn ios_state_accessor_faults_on_overflowing_pointer() {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    let result = run_synthetic_startup(
        include_bytes!("../../../fixtures/pe/msvc-ios-state-fault.exe"),
        StartupLayout {
            image_base: 0x1_4000_0000,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        },
        registry,
        2_000_000,
    )
    .unwrap();
    assert!(matches!(
        result.execution.exit_reason,
        ExitReason::MemoryFault(_)
    ));
}
