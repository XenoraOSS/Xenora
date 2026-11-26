// SPDX-License-Identifier: GPL-3.0-or-later

mod support;

use std::sync::Arc;
use support::Guest;
use xenora_loader::cpu::ExitReason;
use xenora_loader::import_table::ImportName;
use xenora_loader::imports::{ImportRegistry, ServiceStatus};
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup_scheduled};

const GUEST: &[u8] = include_bytes!("../../../fixtures/pe/event-rights.exe");
const KERNEL32: &str = "kernel32.dll";
const SYNC_API_SETS: [&str; 3] = [
    "api-ms-win-core-synch-l1-1-0.dll",
    "api-ms-win-core-synch-l1-2-0.dll",
    "api-ms-win-core-synch-l1-2-1.dll",
];
const INSTRUCTION_LIMIT: usize = 100_000;

#[test]
fn extended_event_api_sets_resolve_to_the_canonical_partial_services() {
    let services = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut imports = ImportRegistry::new();
    register_standard_services(&mut imports, services);

    for symbol in ["CreateEventExA", "CreateEventExW"] {
        let name = ImportName::Name(symbol.to_owned());
        let canonical = imports
            .resolve(KERNEL32, &name)
            .unwrap_or_else(|| panic!("missing {KERNEL32}!{symbol}"));
        assert_eq!(canonical.status, ServiceStatus::Partial, "{symbol}");

        for module in SYNC_API_SETS {
            let alias = imports
                .resolve(module, &name)
                .unwrap_or_else(|| panic!("missing {module}!{symbol}"));
            assert!(std::ptr::eq(alias, canonical), "{module}!{symbol}");
        }
    }
}

#[test]
fn rejected_event_creation_leaves_the_live_handle_table_unchanged() {
    const IMPORTS: &[(&str, &str)] = &[
        (KERNEL32, "CreateEventExA"),
        (KERNEL32, "CreateEventExW"),
        (KERNEL32, "GetLastError"),
        (KERNEL32, "WaitForSingleObject"),
        (KERNEL32, "CloseHandle"),
    ];
    const SYNCHRONIZE: u64 = 0x0010_0000;
    let mut guest = Guest::new(IMPORTS, &[]);
    let live = guest.call("CreateEventExW", &[0, 0, 2, SYNCHRONIZE]);
    assert_ne!(live, 0);
    assert_eq!(guest.state.open_handle_count(), 1);

    for symbol in ["CreateEventExA", "CreateEventExW"] {
        for (args, error) in [
            ([1, 0, 0, SYNCHRONIZE], 50),
            ([0, 1, 0, SYNCHRONIZE], 50),
            ([0, 0, 0, 0x8000_0000], 50),
            ([0, 0, 0x8000_0000, SYNCHRONIZE], 87),
        ] {
            assert_eq!(guest.call(symbol, &args), 0, "{symbol} {args:?}");
            assert_eq!(guest.error(), error, "{symbol} {args:?}");
            assert_eq!(guest.state.open_handle_count(), 1, "{symbol} {args:?}");
        }
    }

    assert_eq!(guest.call("WaitForSingleObject", &[live, 0]), 0);
    assert_eq!(guest.call("CloseHandle", &[live]), 1);
    assert_eq!(guest.state.open_handle_count(), 0);
}

#[test]
fn original_event_rights_guest_runs_under_the_cooperative_scheduler() {
    let probe = probe_pe_bytes(GUEST, &ProbeConfig::default()).expect("inspect the original PE");
    assert_eq!(probe.import_error, None);
    for (module, symbol) in [
        ("api-ms-win-core-synch-l1-2-0.dll", "CreateEventExA"),
        (KERNEL32, "CreateEventExW"),
    ] {
        let name = ImportName::Name(symbol.to_owned());
        let import = probe
            .imports
            .iter()
            .find(|import| import.dll.eq_ignore_ascii_case(module) && import.import_name == name)
            .unwrap_or_else(|| panic!("fixture does not import {module}!{symbol}"));
        assert_eq!(import.service_status, Some(ServiceStatus::Partial));
        assert_eq!(import.canonical_module.as_deref(), Some(KERNEL32));
        assert_eq!(import.canonical_symbol.as_deref(), Some(symbol));
    }

    for quantum in [1, 64, 1024] {
        let services = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut imports = ImportRegistry::new();
        register_standard_services(&mut imports, Arc::clone(&services));
        let result = run_synthetic_startup_scheduled(
            GUEST,
            StartupLayout {
                image_base: 0x1_4000_0000,
                stack_top: 0x2_0001_0000,
                tls_base: 0x3_0000_0000,
            },
            imports,
            Arc::clone(&services),
            INSTRUCTION_LIMIT,
            quantum,
            |_| {},
        )
        .unwrap_or_else(|error| {
            panic!("start original event rights PE at quantum {quantum}: {error}")
        });

        assert_eq!(
            result.bound_imports,
            probe.imports.len(),
            "quantum {quantum}"
        );
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(0),
            "quantum {quantum}"
        );
        assert_eq!(services.exit_status(), Some(0), "quantum {quantum}");
        assert_eq!(services.open_handle_count(), 0, "quantum {quantum}");
        assert!(result.dbt_metrics.dbt_dispatches > 0, "quantum {quantum}");
    }
}
