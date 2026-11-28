// SPDX-License-Identifier: GPL-3.0-or-later

//! Check the documented FormatMessage escapes and insert options in both widths.
//! https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-formatmessage#remarks

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{
    ExitReason, ImportRegistry, MockClock, ServicesState, register_standard_services,
};

#[test]
fn compiled_format_message_expands_escapes_and_preserves_numbered_inserts() {
    let output = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("format-message-escapes-{}", std::process::id()));
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pe/format-message-escapes/build.sh");
    let build = Command::new("sh")
        .arg(script)
        .arg(&output)
        .output()
        .expect("build the original FormatMessage guests");
    assert!(build.status.success(), "{build:?}");

    let results = [("ansi", "FormatMessageA"), ("wide", "FormatMessageW")].map(|(name, symbol)| {
        let bytes = std::fs::read(output.join(format!("{name}.exe"))).unwrap();
        let probe = probe_pe_bytes(&bytes, &ProbeConfig::default()).unwrap();
        assert_eq!(probe.import_error, None);
        assert!(probe.imports.iter().any(|entry| {
            entry.dll.eq_ignore_ascii_case("kernel32.dll") && entry.symbol == symbol
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
        (name, result.execution.exit_reason, state.exit_status())
    });
    assert_eq!(
        results,
        [
            ("ansi", ExitReason::ProcessExit(0), Some(0)),
            ("wide", ExitReason::ProcessExit(0), Some(0)),
        ]
    );
}
