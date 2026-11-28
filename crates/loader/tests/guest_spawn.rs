// SPDX-License-Identifier: GPL-3.0-or-later

//! Issue #1040: a hosted C program re-executes itself through `_spawnve` in
//! `_P_WAIT` mode. The parent writes a file, the child reads it back through
//! the shared machine-wide filesystem, prints through the inherited output
//! capture and exits 42; the spawn returns that status to the parent.
//!
//! `tests/compat_fixtures/build-guest-spawn.sh` builds
//! `tests/compat_fixtures/guest_spawn.c` with the mingw-w64 C runtime.
//!
//! The expected output is a literal written from the program's source: each
//! line is stated in the fixture next to the printf that produces it.
//! Nothing here is taken from a run. The test fails, and does not skip, when
//! the toolchain is missing.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, StartupResult, run_synthetic_startup};

const BUDGET: usize = 20_000_000;

const EXPECTED_OUTPUT: &str = concat!(
    "guest spawn: child ran\r\n",
    "guest spawn: child status 42\r\n",
);

fn built() -> &'static PathBuf {
    static OUT: OnceLock<PathBuf> = OnceLock::new();
    OUT.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let build = Command::new("sh")
            .arg(root.join("tests/compat_fixtures/build-guest-spawn.sh"))
            .current_dir(&root)
            .output()
            .expect("run tests/compat_fixtures/build-guest-spawn.sh");
        assert!(
            build.status.success(),
            "the guest spawn program did not build:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        root.join("artifacts/guest-spawn")
    })
}

fn run(args: &[&str]) -> (StartupResult, Arc<ServicesState>) {
    let bytes = std::fs::read(built().join("guest-spawn.exe")).expect("read the guest program");
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    state.set_program_arguments(args).unwrap();
    state.set_spawn_image("guest-spawn.exe", &bytes);
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let layout = StartupLayout {
        image_base: 0x2_5000_0000,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let result = run_synthetic_startup(&bytes, layout, registry, BUDGET)
        .unwrap_or_else(|error| panic!("guest-spawn: {error}"));
    (result, state)
}

fn output(state: &ServicesState) -> String {
    String::from_utf8(state.standard_output()).expect("ASCII output")
}

#[test]
fn a_spawned_child_runs_shares_the_drive_and_returns_its_status() {
    let (result, state) = run(&["guest-spawn.exe"]);
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::ProcessExit(0),
        "output so far:\n{}",
        output(&state)
    );
    assert_eq!(output(&state), EXPECTED_OUTPUT);
}

/// The child is a separate process: its CRT argument vector is its own, so
/// `argv[1]` is exactly what the parent passed, and the parent's own
/// arguments are undisturbed after the spawn. Both directions are covered by
/// the fixture's branching on `argv[1]`: the child prints "child ran" only
/// because its vector carries "child", and the parent exits 0 only because
/// its own flow continues past the spawn with its original vector.
#[test]
fn the_parent_process_state_survives_the_child() {
    let (result, state) = run(&["guest-spawn.exe"]);
    assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
    // The parent's open handles from its own file work are closed at exit;
    // the shared drive keeps the file the child read.
    assert_eq!(
        state.file_contents("spawn-shared.txt"),
        Some(b"parent data".to_vec())
    );
}

#[test]
fn inherited_wrapped_file_survives_child_close_and_preserves_position() {
    let (result, state) = run(&["guest-spawn.exe", "wrapped-parent"]);
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::ProcessExit(0),
        "output so far:\n{}",
        output(&state)
    );
    assert_eq!(
        state.standard_output(),
        concat!(
            "wrapped child: read B and closed descriptor\r\n",
            "wrapped parent: read C after child close\r\n",
            "wrapped child: read D and left descriptor open\r\n",
            "wrapped parent: read E through surviving duplicate\r\n",
            "wrapped parent: final close released the handle\r\n",
        )
        .as_bytes()
    );
    assert_eq!(state.open_handle_count(), 0);
}
