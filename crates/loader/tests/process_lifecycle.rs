// SPDX-License-Identifier: GPL-3.0-or-later

//! Process and thread lifecycle services: `TerminateProcess`,
//! `GetExitCodeProcess`, `GetCurrentThreadStackLimits`,
//! `GetThreadGroupAffinity` and `SetThreadGroupAffinity`.
//!
//! `tests/compat_fixtures/build-process-lifecycle.sh` compiles an original C
//! program with mingw-w64, so this test fails, not skips, without it. The
//! expected stack bounds are the layout this file gives the process.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::{ImportRegistry, ServiceStatus};
use xenora_loader::scheduler::DEFAULT_QUANTUM;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup_scheduled};

const STATUS: u32 = 0x7E57;
const STACK_TOP: u64 = 0x2_0001_0000;

fn built() -> &'static Path {
    static OUT: OnceLock<PathBuf> = OnceLock::new();
    OUT.get_or_init(|| {
        let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("process-lifecycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/compat_fixtures/build-process-lifecycle.sh"
        );
        let build = Command::new("sh").arg(script).arg(&out).output().unwrap();
        assert!(
            build.status.success(),
            "the lifecycle program did not build; it needs mingw-w64:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        out
    })
}

fn run(name: &str) -> (ExitReason, Arc<ServicesState>) {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let bytes = std::fs::read(built().join(format!("{name}.exe"))).unwrap();
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let layout = StartupLayout {
        image_base: 0x2_5000_0000,
        stack_top: STACK_TOP,
        tls_base: 0x3_0000_0000,
    };
    let result = run_synthetic_startup_scheduled(
        &bytes,
        layout,
        registry,
        Arc::clone(&state),
        5_000_000,
        DEFAULT_QUANTUM,
    )
    .unwrap();
    (result.execution.exit_reason, state)
}

#[test]
fn the_services_are_registered_as_partial() {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    for symbol in [
        "TerminateProcess",
        "GetExitCodeProcess",
        "GetCurrentThreadStackLimits",
        "GetThreadGroupAffinity",
        "SetThreadGroupAffinity",
    ] {
        let status = registry
            .iter_registered()
            .find(|(module, name, _)| {
                module.eq_ignore_ascii_case("kernel32.dll") && *name == symbol
            })
            .map(|(_, _, status)| status);
        assert_eq!(status, Some(ServiceStatus::Partial), "{symbol}");
    }
}

#[test]
fn a_c_program_uses_the_lifecycle_services_and_terminates_itself() {
    for level in ["O0", "O1", "O2", "Os"] {
        let (exit, state) = run(&format!("process-lifecycle-{level}"));
        assert_eq!(exit, ExitReason::ProcessExit(STATUS), "{level}");
        assert_eq!(state.exit_status(), Some(STATUS));
        let bytes = state.file_contents("C:\\stacks.bin").unwrap();
        let bound = |i: usize| u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap());
        assert_eq!(bound(1), STACK_TOP, "main StackBase");
        assert!(bound(0) < bound(1));
        // A worker stack lies in the thread area and holds its default 256 KiB.
        assert!(bound(2) >= 0x6_0000_0000, "{:#x}", bound(2));
        assert_eq!(bound(3) - bound(2), 256 * 1024);
    }
}

#[test]
fn the_control_fails_its_first_check() {
    let (exit, state) = run("process-lifecycle-break");
    assert_eq!(exit, ExitReason::ProcessExit(2));
    assert_eq!(state.file_contents("C:\\stacks.bin"), None);
}
