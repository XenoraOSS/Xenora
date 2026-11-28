// SPDX-License-Identifier: GPL-3.0-or-later

//! Exercise the exact CRT runtime API-set cleanup import through original
//! compiler-linked Win64 guest code.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup_scheduled};

#[test]
fn guest_c_exit_preserves_callbacks_and_open_descriptors() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("crt-c-exit-{}", std::process::id()));
    let build = Command::new("sh")
        .arg(root.join("fixtures/pe/crt-c-exit/build.sh"))
        .arg(&out)
        .output()
        .expect("run native CRT fixture builder");
    assert!(
        build.status.success(),
        "CRT fixture build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let bytes = fs::read(out.join("crt-c-exit.exe")).unwrap();
    fs::remove_dir_all(out).unwrap();

    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let result = run_synthetic_startup_scheduled(
        &bytes,
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
    .expect("start the original CRT cleanup fixture");
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::ProcessExit(0),
        "see the numbered guest checks in fixtures/pe/crt-c-exit/guest.c"
    );
    assert_eq!(state.exit_status(), Some(0));
    assert_eq!(state.open_handle_count(), 0);
    assert_eq!(state.file_contents("C:\\crt-cleanup.bin").unwrap(), b"OK");
}
