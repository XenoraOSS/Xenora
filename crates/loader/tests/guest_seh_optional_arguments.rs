// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{
    ExitReason, ImportRegistry, MockClock, ServicesState, register_standard_services,
};

#[test]
fn raise_exception_ignores_the_argument_count_when_the_array_is_null() {
    let output = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("seh-optional-arguments-{}", std::process::id()));
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pe/seh-optional-arguments/build.sh");
    let build = Command::new("sh")
        .arg(script)
        .arg(&output)
        .output()
        .expect("build the original SEH guests");
    assert!(build.status.success(), "{build:?}");

    let results = ["populated", "null-ordinary", "null-oversized"].map(|name| {
        let bytes = std::fs::read(output.join(format!("{name}.exe"))).unwrap();
        let probe = probe_pe_bytes(&bytes, &ProbeConfig::default()).unwrap();
        assert_eq!(probe.import_error, None);
        for (dll, symbol) in [
            ("kernel32.dll", "RaiseException"),
            ("ucrtbase.dll", "__C_specific_handler"),
        ] {
            assert!(
                probe
                    .imports
                    .iter()
                    .any(|entry| { entry.dll.eq_ignore_ascii_case(dll) && entry.symbol == symbol }),
                "{name}: expected {dll}!{symbol}"
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
        (name, result.execution.exit_reason, state.exit_status())
    });
    assert_eq!(
        results,
        [
            ("populated", ExitReason::ProcessExit(0), Some(0)),
            ("null-ordinary", ExitReason::ProcessExit(0), Some(0)),
            ("null-oversized", ExitReason::ProcessExit(0), Some(0)),
        ]
    );
}
