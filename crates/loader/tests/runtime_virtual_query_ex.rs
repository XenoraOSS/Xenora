// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::import_table::ImportName;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};

const PE: &[u8] = include_bytes!("../../../fixtures/pe/virtual-query-ex/virtual-query-ex.exe");

#[test]
fn original_guest_queries_regions_through_the_exact_memory_api_set() {
    let report = probe_pe_bytes(PE, &ProbeConfig::default()).unwrap();
    assert_eq!(report.import_error, None);
    let symbol = ImportName::Name("VirtualQueryEx".into());
    let mut imports = report
        .imports
        .iter()
        .filter(|entry| entry.import_name == symbol);
    assert_eq!(
        imports.next().unwrap().dll,
        "api-ms-win-core-memory-l1-1-0.dll"
    );
    assert!(imports.next().is_none());

    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    let layout = StartupLayout {
        image_base: 0x2_5000_0000,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let result = run_synthetic_startup(PE, layout, registry, 100_000).unwrap();
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::ProcessExit(0),
        "a nonzero exit code identifies the fixture source line"
    );
}
