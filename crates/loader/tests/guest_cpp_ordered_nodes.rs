// SPDX-License-Identifier: GPL-3.0-or-later

//! Run `cargo test --locked --target aarch64-apple-darwin -p xenora-loader
//! --test guest_cpp_ordered_nodes` on Apple Silicon with MinGW-w64 GCC.
//! The test builds the original hosted guest with its GNU C++ runtime linked
//! into the EXE. See the C++ rules for [node ownership] and [insertion].
//!
//! [node ownership]: https://eel.is/c++draft/container.node.overview
//! [insertion]: https://eel.is/c++draft/associative.reqmts

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{
    ExitReason, ImportRegistry, MockClock, ServicesState, register_standard_services,
};

#[test]
fn compiled_gnu_map_nodes_transfer_ownership_and_release_records() {
    let output = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("ordered-nodes-{}", std::process::id()));
    let builder = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/compat_fixtures/build-guest-cpp-ordered-nodes.sh");
    let build = Command::new("sh")
        .arg(&builder)
        .arg(&output)
        .output()
        .expect("build the original GNU C++ node fixtures");
    assert!(
        build.status.success(),
        "{} failed:\n{}\n{}",
        builder.display(),
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    for optimization in ["O1", "O2"] {
        let name = format!("ordered-nodes-{optimization}.exe");
        let bytes = std::fs::read(output.join(&name)).expect("read original node fixture");
        let probe =
            probe_pe_bytes(&bytes, &ProbeConfig::default()).expect("inspect the fixture PE");
        assert_eq!(probe.import_error, None);
        assert!(
            probe.imports.iter().all(|entry| {
                let dll = entry.dll.to_ascii_lowercase();
                !dll.starts_with("libstdc++") && !dll.starts_with("libgcc")
            }),
            "{name} must carry its GNU C++ runtime"
        );

        let mut previous_runs = Vec::new();
        for (reject_only, status, output) in [
            (
                false,
                0,
                b"ordered nodes: reinserted 123 132 0\r\n".as_slice(),
            ),
            (
                true,
                23,
                b"ordered nodes: rejected 123 132 0\r\n".as_slice(),
            ),
            (
                false,
                0,
                b"ordered nodes: reinserted 123 132 0\r\n".as_slice(),
            ),
        ] {
            let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
            let arguments = [name.as_str(), "reject"];
            state
                .set_program_arguments(&arguments[..if reject_only { 2 } else { 1 }])
                .unwrap();
            let mut registry = ImportRegistry::new();
            register_standard_services(&mut registry, Arc::clone(&state));
            let result = run_synthetic_startup(
                &bytes,
                StartupLayout {
                    image_base: 0x2_5000_0000,
                    stack_top: 0x2_0001_0000,
                    tls_base: 0x3_0000_0000,
                },
                registry,
                20_000_000,
            )
            .unwrap_or_else(|error| panic!("{name} reject={reject_only}: {error}"));
            assert_eq!(
                result.execution.exit_reason,
                ExitReason::ProcessExit(status),
                "{name} reject={reject_only}, stdout={:?}, stderr={:?}",
                String::from_utf8_lossy(&state.standard_output()),
                String::from_utf8_lossy(&state.standard_error())
            );
            assert_eq!(state.exit_status(), Some(status));
            assert_eq!(state.standard_output(), output);
            assert!(state.standard_error().is_empty());
            assert_eq!(state.open_handle_count(), 0);
            assert!(result.dbt_metrics.native_bytes_emitted > 0);
            previous_runs.push(state);
        }
        for (state, status) in previous_runs.iter().zip([0, 23, 0]) {
            assert_eq!(state.exit_status(), Some(status));
        }
    }
}
