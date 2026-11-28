// SPDX-License-Identifier: GPL-3.0-or-later

//! Check Unicode maximal-subpart substitution through the public Win32 import.
//! https://www.unicode.org/versions/Unicode17.0.0/core-spec/chapter-3/

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{
    ExitReason, ImportRegistry, MockClock, ServicesState, register_standard_services,
};

#[test]
fn compiled_multibyte_to_wide_uses_utf8_maximal_subparts() {
    let output = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("utf8-maximal-subparts-{}", std::process::id()));
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pe/utf8-maximal-subparts/build.sh");
    let build = Command::new("sh")
        .arg(script)
        .arg(&output)
        .output()
        .expect("build the original UTF-8 conversion guest");
    assert!(build.status.success(), "{build:?}");

    let bytes = std::fs::read(output.join("guest.exe")).unwrap();
    let probe = probe_pe_bytes(&bytes, &ProbeConfig::default()).unwrap();
    assert_eq!(probe.import_error, None);
    assert!(probe.imports.iter().any(|entry| {
        entry
            .dll
            .eq_ignore_ascii_case("api-ms-win-core-string-l1-1-0.dll")
            && entry.symbol == "MultiByteToWideChar"
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
    .expect("run the original UTF-8 conversion guest");
    assert!(result.dbt_metrics.native_bytes_emitted > 0);
    assert_eq!(
        (result.execution.exit_reason, state.exit_status()),
        (ExitReason::ProcessExit(0), Some(0))
    );
}
