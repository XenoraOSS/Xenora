// SPDX-License-Identifier: GPL-3.0-or-later

//! Build the original EXE variants and provider DLL with
//! `sh fixtures/pe/package-resource-recovery/build.sh artifacts/package-resource-recovery`.
//! Run this test on native ARM64 with
//! `cargo test --locked -p xenora-loader --test guest_package_resource_recovery`.
//!
//! The provider creates an unnamed event during attach. One root closes the
//! event and exits 0. The other exits 37 with the handle live. Cleanup is
//! observed after package return, and the subsequent fresh run keeps both
//! prior service states alive. The provider's detach result is a fixture
//! marker recorded by Xenora.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use xenora_loader::module_graph::PackageModule;
use xenora_loader::module_layout::ModuleLayoutArena;
use xenora_loader::package::{PackageEntryEvent, run_package_startup_with_entry_observer};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::{
    ExitReason, ImportRegistry, InitializationOutcome, InitializationPhase, PackageStartupLayout,
};

const PROVIDER: &str = "resource_provider.dll";

#[test]
fn compiled_package_releases_event_and_recovers_in_a_fresh_run() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("package-resource-recovery-{}", std::process::id()));
    let build = Command::new("sh")
        .arg(root.join("fixtures/pe/package-resource-recovery/build.sh"))
        .arg(&out)
        .output()
        .expect("run native package fixture builder");
    assert!(
        build.status.success(),
        "package fixture build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let provider = fs::read(out.join(PROVIDER)).unwrap();
    let explicit_close = fs::read(out.join("explicit-close.exe")).unwrap();
    let open_at_exit = fs::read(out.join("open-at-exit.exe")).unwrap();
    fs::remove_dir_all(&out).unwrap();

    let mut previous_runs = Vec::new();
    for (app, expected_status) in [
        (&explicit_close, 0),
        (&open_at_exit, 37),
        (&explicit_close, 0),
    ] {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        assert_eq!(state.open_handle_count(), 0);
        assert_eq!(state.exit_status(), None);

        let package = [
            PackageModule {
                file_name: "resource-root.exe",
                bytes: app,
            },
            PackageModule {
                file_name: PROVIDER,
                bytes: &provider,
            },
        ];
        let mut imports = ImportRegistry::new();
        register_standard_services(&mut imports, Arc::clone(&state));
        let mut entries = Vec::new();
        let result = run_package_startup_with_entry_observer(
            &package,
            PackageStartupLayout {
                arena: ModuleLayoutArena {
                    base: 0x1_5000_0000,
                    size: 0x20_0000,
                },
                stack_top: 0x2_0001_0000,
                thread_tls_base: 0x3_0000_0000,
            },
            0,
            imports,
            20_000,
            Some((Arc::clone(&state), 64)),
            |event| match event {
                PackageEntryEvent::Entering { image_index, .. } => {
                    entries.push((image_index, state.open_handle_count()));
                }
                PackageEntryEvent::Leaving => {}
            },
        )
        .expect("start the explicit package");

        assert_eq!(entries, [(0, 1)], "one provider handle at root entry");
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(expected_status)
        );
        assert_eq!(state.exit_status(), Some(expected_status));
        assert_eq!(state.open_handle_count(), 0, "handles after package return");
        assert!(result.dbt_metrics.native_bytes_emitted > 0);

        let [attach, entry] = result.diagnostics.initialization.as_slice() else {
            panic!("expected exactly one attach and one entry");
        };
        assert_eq!(attach.file_name, PROVIDER);
        assert_eq!(attach.phase, InitializationPhase::DllMain);
        assert_eq!(attach.outcome, InitializationOutcome::Returned(1));
        assert_eq!(entry.image_index, 0);
        assert_eq!(entry.phase, InitializationPhase::ProcessEntry);
        assert_eq!(
            entry.outcome,
            InitializationOutcome::ProcessExit(expected_status)
        );

        let [detach] = result.diagnostics.shutdown.as_slice() else {
            panic!("expected exactly one shutdown callback");
        };
        assert_eq!(detach.image_index, attach.image_index);
        assert_eq!(detach.file_name, PROVIDER);
        assert_eq!(detach.phase, InitializationPhase::DllDetach);
        assert_eq!(detach.outcome, InitializationOutcome::Returned(77));

        previous_runs.push(state);
    }

    for (state, expected_status) in previous_runs.iter().zip([0, 37, 0]) {
        assert_eq!(state.exit_status(), Some(expected_status));
        assert_eq!(state.open_handle_count(), 0);
    }
}
