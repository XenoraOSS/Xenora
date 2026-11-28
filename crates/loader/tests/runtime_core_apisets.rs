// SPDX-License-Identifier: GPL-3.0-or-later

//! Exercise exactly the three SDK-declared API-set names in original PE guests.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use xenora_loader::imports::{ImportRegistry, ServiceStatus};
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::scheduler::DEFAULT_QUANTUM;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup_scheduled};
use xenora_loader::{ExitReason, pe::IMAGE_FILE_MACHINE_AMD64};

const KERNEL32: &str = "kernel32.dll";

fn fixtures() -> &'static Path {
    static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
    DIRECTORY.get_or_init(|| {
        let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("core-apiset-trio-{}", std::process::id()));
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/compat_fixtures/build-core-apiset-trio.sh"
        );
        let build = Command::new("sh")
            .arg(script)
            .arg(&directory)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "native PE fixture build failed: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        directory
    })
}

fn run_pair(fixture: &str, api_set: &str, symbol: &str) {
    for (variant, module) in [("canonical", KERNEL32), ("exact", api_set)] {
        let image_name = format!("{fixture}-{variant}.exe");
        let bytes = std::fs::read(fixtures().join(&image_name)).unwrap();
        let report = probe_pe_bytes(&bytes, &ProbeConfig::default()).unwrap();
        assert_eq!(report.parsed.machine, IMAGE_FILE_MACHINE_AMD64);
        assert!(report.import_error.is_none(), "{image_name}");
        let entry = report
            .imports
            .iter()
            .find(|entry| entry.symbol == symbol)
            .unwrap_or_else(|| panic!("{image_name} does not import {symbol}"));
        assert_eq!(entry.dll, module, "{image_name}");
        assert_eq!(
            entry.canonical_module.as_deref(),
            Some(KERNEL32),
            "{image_name}"
        );
        assert_eq!(
            entry.canonical_symbol.as_deref(),
            Some(symbol),
            "{image_name}"
        );
        assert_eq!(
            entry.service_status,
            Some(ServiceStatus::Partial),
            "{image_name}"
        );

        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, Arc::clone(&state));
        let result = run_synthetic_startup_scheduled(
            &bytes,
            StartupLayout {
                image_base: report.parsed.image_base,
                stack_top: 0x2_0001_0000,
                tls_base: 0x3_0000_0000,
            },
            registry,
            Arc::clone(&state),
            100_000,
            DEFAULT_QUANTUM,
            |_| {},
        )
        .unwrap_or_else(|error| panic!("{image_name}: {error}"));
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(0),
            "{image_name}"
        );
        assert_eq!(state.exit_status(), Some(0), "{image_name}");
    }
}

#[test]
fn localization_ansi_message_includes_exact_text_and_allocated_output() {
    run_pair(
        "format-message",
        "api-ms-win-core-localization-l1-2-0.dll",
        "FormatMessageA",
    );
}

#[test]
fn heap_local_free_releases_a_kernel32_local_allocation() {
    run_pair("local-free", "api-ms-win-core-heap-l2-1-0.dll", "LocalFree");
}

#[test]
fn processthreads_stack_bounds_contain_the_actual_stack_pointer() {
    run_pair(
        "stack-limits",
        "api-ms-win-core-processthreads-l1-1-1.dll",
        "GetCurrentThreadStackLimits",
    );
}
