// SPDX-License-Identifier: GPL-3.0-or-later

//! Original COM API fixture, compiled independently of the Rust handlers.

use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{
    StartupLayout, run_synthetic_startup, run_synthetic_startup_scheduled,
};

#[test]
fn guest_exercises_com_base_services() {
    assert_eq!(
        run(include_bytes!("../../../fixtures/pe/com-guid.exe"), false),
        ExitReason::ProcessExit(0)
    );
}

#[test]
fn guest_cpp_calls_com_vtable_methods() {
    assert_eq!(
        run(include_bytes!("../../../fixtures/pe/com-vtable.exe"), false),
        ExitReason::ProcessExit(0)
    );
}

#[test]
fn retired_guest_thread_does_not_leave_an_apartment_behind() {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let layout = StartupLayout {
        image_base: 0x2_5000_0000,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let result = run_synthetic_startup_scheduled(
        include_bytes!("../../../fixtures/pe/com-thread.exe"),
        layout,
        registry,
        state,
        2_000_000,
        1024,
        |_| {},
    )
    .unwrap();
    assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
}

#[test]
fn guest_calls_an_opt_in_host_factory_and_object() {
    assert_eq!(
        run(include_bytes!("../../../fixtures/pe/com-factory.exe"), true),
        ExitReason::ProcessExit(0)
    );
}

#[test]
fn released_host_object_is_unmapped() {
    assert!(matches!(
        run(
            include_bytes!("../../../fixtures/pe/com-factory-stale.exe"),
            true
        ),
        ExitReason::MemoryFault(_)
    ));
}

fn run(bytes: &[u8], fixture: bool) -> ExitReason {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    if fixture {
        xenora_loader::services::com_fixture::register(&mut registry, &state);
    }
    let layout = StartupLayout {
        image_base: 0x2_5000_0000,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let result = run_synthetic_startup(bytes, layout, registry, 2_000_000).unwrap();
    result.execution.exit_reason
}
