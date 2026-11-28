// SPDX-License-Identifier: GPL-3.0-or-later

//! Issue #941: compiler-emitted aligned allocation operators run end to end in
//! an original no-CRT C++ guest through the loader and ARM64 dynamic binary
//! translation. Expected statuses are literals derived from the fixture source,
//! never from a run.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, StartupResult, run_synthetic_startup};

const IMAGE_BASE: u64 = 0x2_5000_0000;
const STACK_TOP: u64 = 0x2_0001_0000;
const TLS_BASE: u64 = 0x3_0000_0000;
const BUDGET: usize = 5_000_000;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}

fn built() -> &'static Path {
    static OUT: OnceLock<PathBuf> = OnceLock::new();
    OUT.get_or_init(|| {
        let root = repo_root();
        let script = root.join("tests/compat_fixtures/build-cxx-aligned.sh");
        let build = Command::new("sh")
            .arg(&script)
            .current_dir(&root)
            .output()
            .expect("run tests/compat_fixtures/build-cxx-aligned.sh");
        assert!(
            build.status.success(),
            "the aligned-allocation guest did not build:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        root.join("artifacts/guest-cxx-aligned")
    })
}

struct Outcome {
    result: StartupResult,
    state: Arc<ServicesState>,
}

fn run(name: &str) -> Result<Outcome, String> {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let bytes = std::fs::read(built().join(format!("{name}.exe"))).expect("read built guest");
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let layout = StartupLayout {
        image_base: IMAGE_BASE,
        stack_top: STACK_TOP,
        tls_base: TLS_BASE,
    };
    match run_synthetic_startup(&bytes, layout, registry, BUDGET) {
        Ok(result) => Ok(Outcome { result, state }),
        Err(error) => Err(error.to_string()),
    }
}

fn assert_execution_evidence(name: &str, outcome: &Outcome) {
    let metrics = &outcome.result.dbt_metrics;
    assert!(metrics.blocks_compiled > 0, "{name}: no block compiled");
    assert!(
        metrics.native_bytes_emitted > 0,
        "{name}: no native code emitted"
    );
    assert!(metrics.service_calls > 0, "{name}: no service call");
    assert!(
        outcome.result.bound_imports >= 5,
        "{name}: too few bound imports"
    );
}

#[test]
fn aligned_operators_run_at_every_optimization_level() {
    for name in [
        "guest-cxx-aligned-O0",
        "guest-cxx-aligned-O1",
        "guest-cxx-aligned-O2",
        "guest-cxx-aligned-Os",
    ] {
        let outcome = run(name).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            outcome.result.execution.exit_reason,
            ExitReason::ProcessExit(0),
            "{name}"
        );
        assert_eq!(outcome.state.exit_status(), Some(0), "{name}");
        assert_execution_evidence(name, &outcome);
    }
}

#[test]
fn invalid_interior_aligned_delete_faults_before_the_control_exit() {
    let name = "guest-cxx-aligned-invalid-interior";
    let outcome = run(name).unwrap_or_else(|error| panic!("{name}: {error}"));
    assert!(
        matches!(
            outcome.result.execution.exit_reason,
            ExitReason::MemoryFault(_)
        ),
        "{name}: interior deletion did not fault: {:?}",
        outcome.result.execution.exit_reason
    );
    assert_eq!(outcome.state.exit_status(), None, "{name}");
    assert_execution_evidence(name, &outcome);
}
