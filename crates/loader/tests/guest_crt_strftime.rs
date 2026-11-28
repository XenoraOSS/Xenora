// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup_scheduled};
use xenora_loader::{
    ExitReason, ImportRegistry, MockClock, ServicesState, register_standard_services,
};

#[test]
fn compiled_strftime_preserves_literal_bytes_and_expands_time_directives() {
    let directory =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("crt-strftime-{}", std::process::id()));
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pe/crt-strftime/build.sh");
    let build = Command::new("sh")
        .arg(script)
        .arg(&directory)
        .output()
        .unwrap();
    assert!(build.status.success(), "{build:?}");

    let results = ["literal", "directives"].map(|name| {
        let bytes = std::fs::read(directory.join(format!("{name}.exe"))).unwrap();
        let probe = probe_pe_bytes(&bytes, &ProbeConfig::default()).unwrap();
        assert_eq!(probe.import_error, None);
        assert!(probe.imports.iter().any(|entry| {
            entry
                .dll
                .eq_ignore_ascii_case("api-ms-win-crt-time-l1-1-0.dll")
                && entry.symbol == "strftime"
        }));
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, Arc::clone(&state));
        let result = run_synthetic_startup_scheduled(
            &bytes,
            StartupLayout {
                image_base: 0x1_4000_0000,
                stack_top: 0x2_0001_0000,
                tls_base: 0x4_0000_0000,
            },
            registry,
            Arc::clone(&state),
            100_000,
            64,
            |_| {},
        )
        .unwrap();
        (name, result.execution.exit_reason, state.exit_status())
    });
    assert_eq!(
        results,
        [
            ("literal", ExitReason::ProcessExit(0), Some(0)),
            ("directives", ExitReason::ProcessExit(0), Some(0)),
        ]
    );
}
