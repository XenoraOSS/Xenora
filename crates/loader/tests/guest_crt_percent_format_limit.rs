// SPDX-License-Identifier: GPL-3.0-or-later

//! Check the local format limit after a documented literal-percent directive
//! in both CRT widths, through compiler-generated Windows x64 varargs.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{
    ExitReason, ImportRegistry, MockClock, ServicesState, register_standard_services,
};

#[test]
fn escaped_percent_counts_toward_the_narrow_and_wide_format_limits() {
    let output = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("crt-percent-format-limit-{}", std::process::id()));
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pe/crt-percent-format-limit/build.sh");
    let build = Command::new("sh")
        .arg(script)
        .arg(&output)
        .output()
        .expect("build the original CRT guests");
    assert!(build.status.success(), "{build:?}");

    let results = [
        ("exact", true, true),
        ("narrow-over", true, false),
        ("wide-over", false, true),
    ]
    .map(|(name, narrow, wide)| {
        let bytes = std::fs::read(output.join(format!("{name}.exe"))).unwrap();
        let probe = probe_pe_bytes(&bytes, &ProbeConfig::default()).unwrap();
        assert_eq!(probe.import_error, None);
        for (symbol, expected) in [
            ("__stdio_common_vsprintf", narrow),
            ("__stdio_common_vswprintf", wide),
        ] {
            assert_eq!(
                probe.imports.iter().any(|entry| {
                    entry
                        .dll
                        .eq_ignore_ascii_case("api-ms-win-crt-stdio-l1-1-0.dll")
                        && entry.symbol == symbol
                }),
                expected,
                "{name}: {symbol}"
            );
        }
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
            ("exact", ExitReason::ProcessExit(0), Some(0)),
            ("narrow-over", ExitReason::IllegalInstruction(0xfe), None),
            ("wide-over", ExitReason::IllegalInstruction(0xfe), None),
        ]
    );
}
