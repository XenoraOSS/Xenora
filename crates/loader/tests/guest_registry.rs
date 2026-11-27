// SPDX-License-Identifier: GPL-3.0-or-later

//! Original compiler-linked registry fixture, built independently of the Rust
//! handlers.

use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};

#[test]
fn guest_exercises_reg_open_key_w() {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let layout = StartupLayout {
        image_base: 0x2_5000_0000,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let result = run_synthetic_startup(
        include_bytes!("../../../fixtures/pe/registry-open.exe"),
        layout,
        registry,
        2_000_000,
    )
    .unwrap();
    assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
}
