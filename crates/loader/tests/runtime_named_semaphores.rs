// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};

#[test]
fn original_guest_shares_named_semaphore_counts_and_lifetime() {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    let layout = StartupLayout {
        image_base: 0x2_5000_0000,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let result = run_synthetic_startup(
        include_bytes!("../../../fixtures/pe/named-semaphore/named-semaphore.exe"),
        layout,
        registry,
        100_000,
    )
    .unwrap();
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::ProcessExit(0),
        "a nonzero exit code is the fixture source line"
    );
}
