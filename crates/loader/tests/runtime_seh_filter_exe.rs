// SPDX-License-Identifier: GPL-3.0-or-later

//! A compiler-linked CRT exception filter runs inside original Win64 SEH scopes.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{
    StartupLayout, run_synthetic_startup, run_synthetic_startup_scheduled,
};

#[test]
fn guest_seh_filter_exe_maps_resumes_and_resets_signals() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("seh-filter-exe-{}", std::process::id()));
    let build = Command::new("sh")
        .arg(root.join("fixtures/pe/seh-filter-exe/build.sh"))
        .arg(&out)
        .output()
        .expect("run native SEH fixture builder");
    assert!(
        build.status.success(),
        "SEH filter fixture build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let fixtures = [
        ("seh-filter-exe.exe", ExitReason::ProcessExit(0)),
        ("seh-filter-exe-fpe.exe", ExitReason::InvalidUnwind),
    ]
    .map(|(name, expected)| (name, fs::read(out.join(name)).unwrap(), expected));
    fs::remove_dir_all(out).unwrap();

    for (name, bytes, expected) in fixtures {
        for scheduled in [false, true] {
            let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
            let mut registry = ImportRegistry::new();
            register_standard_services(&mut registry, Arc::clone(&state));
            let layout = StartupLayout {
                image_base: 0x1_5000_0000,
                stack_top: 0x2_0001_0000,
                tls_base: 0x3_0000_0000,
            };
            let result = if scheduled {
                run_synthetic_startup_scheduled(
                    &bytes,
                    layout,
                    registry,
                    state,
                    100_000,
                    64,
                    |_| {},
                )
            } else {
                run_synthetic_startup(&bytes, layout, registry, 100_000)
            }
            .unwrap_or_else(|error| panic!("start {name} (scheduled={scheduled}): {error}"));
            assert_eq!(
                result.execution.exit_reason, expected,
                "{name}, scheduled={scheduled}: see checks in fixtures/pe/seh-filter-exe/guest.c"
            );
        }
    }
}
