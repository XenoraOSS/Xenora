// SPDX-License-Identifier: GPL-3.0-or-later

//! Check narrow scanf destination widths through compiler-generated x64 varargs.
//! Build the four guests with `sh fixtures/pe/crt-scan-destination-width/build.sh
//! artifacts/crt-scan-destination-width`.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{
    ExitReason, ImportRegistry, MockClock, ServicesState, register_standard_services,
};

#[test]
fn narrow_scanf_honors_character_destination_widths() {
    let output = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("crt-scan-destination-width-{}", std::process::id()));
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pe/crt-scan-destination-width/build.sh");
    let build = Command::new("sh")
        .arg(script)
        .arg(&output)
        .output()
        .expect("build the original CRT guests");
    assert!(build.status.success(), "{build:?}");

    let results = ["ordinary", "secure", "high-byte", "short-wide-buffer"].map(|name| {
        let bytes = std::fs::read(output.join(format!("{name}.exe"))).unwrap();
        let probe = probe_pe_bytes(&bytes, &ProbeConfig::default()).unwrap();
        assert_eq!(probe.import_error, None);
        assert!(probe.imports.iter().any(|entry| {
            entry
                .dll
                .eq_ignore_ascii_case("api-ms-win-crt-stdio-l1-1-0.dll")
                && entry.symbol == "__stdio_common_vsscanf"
        }));
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, Arc::clone(&state));
        let result = run_synthetic_startup(
            &bytes,
            StartupLayout {
                image_base: 0x1_4000_0000,
                stack_top: 0x2_0001_0000,
                tls_base: 0x4_0000_0000,
            },
            registry,
            100_000,
        )
        .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(result.dbt_metrics.native_bytes_emitted > 0);
        assert!(state.standard_output().is_empty());
        assert!(state.standard_error().is_empty());
        (name, result.execution.exit_reason, state.exit_status())
    });
    assert_eq!(
        results,
        [
            ("ordinary", ExitReason::ProcessExit(0), Some(0)),
            ("secure", ExitReason::ProcessExit(0), Some(0)),
            ("high-byte", ExitReason::IllegalInstruction(0xfe), None),
            (
                "short-wide-buffer",
                ExitReason::IllegalInstruction(0xfe),
                None
            ),
        ]
    );
}
