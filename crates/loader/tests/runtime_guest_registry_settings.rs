// SPDX-License-Identifier: GPL-3.0-or-later

//! Original compiler-linked guests exercising the synthetic HKCU store.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup_scheduled};

fn run_in_fresh_guest(name: &str, bytes: &[u8]) {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let result = run_synthetic_startup_scheduled(
        bytes,
        StartupLayout {
            image_base: 0x1_5000_0000,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        },
        registry,
        Arc::clone(&state),
        100_000,
        64,
        |_| {},
    )
    .unwrap_or_else(|error| panic!("start {name}: {error}"));
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::ProcessExit(0),
        "{name}: see numbered guest checks in fixtures/pe/guest-registry-settings/settings.c"
    );
    assert_eq!(state.exit_status(), Some(0), "{name}");
}

#[test]
fn guest_registry_settings_round_trip_and_fresh_sessions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("guest-registry-settings-{}", std::process::id()));
    let build = Command::new("sh")
        .arg(root.join("fixtures/pe/guest-registry-settings/build.sh"))
        .arg(&out)
        .output()
        .expect("run native registry fixture builder");
    assert!(
        build.status.success(),
        "registry fixture build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let settings = fs::read(out.join("guest-registry-settings.exe")).unwrap();
    let cold = fs::read(out.join("guest-registry-settings-cold.exe")).unwrap();
    fs::remove_dir_all(out).unwrap();

    run_in_fresh_guest("guest-registry-settings", &settings);
    run_in_fresh_guest("guest-registry-settings-cold", &cold);
}
