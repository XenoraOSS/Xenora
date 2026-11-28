// SPDX-License-Identifier: GPL-3.0-or-later
//! Original guest exercises ostream write and guest stream buffer callbacks.
use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};

#[test]
fn ostream_write_uses_guest_buffer_and_records_short_write() {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    let result = run_synthetic_startup(
        include_bytes!("../../../fixtures/pe/msvc-ostream-write.exe"),
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

fn run(bytes: &[u8]) -> ExitReason {
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
#[test]
fn write_faults_on_invalid_object() {
    assert!(matches!(
        run(include_bytes!(
            "../../../fixtures/pe/msvc-ostream-write-invalid.exe"
        )),
        ExitReason::MemoryFault(_)
    ));
}
#[test]
fn write_refuses_exception_mask_failure() {
    assert_eq!(
        run(include_bytes!(
            "../../../fixtures/pe/msvc-ostream-write-exception.exe"
        )),
        ExitReason::IllegalInstruction(0xFE)
    );
}
