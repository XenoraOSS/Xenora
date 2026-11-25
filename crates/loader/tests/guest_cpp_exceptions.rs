// SPDX-License-Identifier: GPL-3.0-or-later

//! Issue #447: an original C++ program throws, catches and unwinds through
//! the loader, the guest services and the ARM64 dynamic binary translation.
//!
//! `tests/compat_fixtures/build-guest-cpp.sh` builds
//! `tests/compat_fixtures/guest_cpp_exceptions.cpp` at four optimisation
//! levels with `-static-libstdc++ -static-libgcc`, so GCC's own unwinder and
//! C++ personality run as guest code and call the exception services.
//!
//! The expected output is a literal written from the program's source: each
//! check's value is stated in the program next to the code that produces it.
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
    "guest cpp exceptions: int 7\r\n",
    "guest cpp exceptions: deep 1\r\n",
    "guest cpp exceptions: destroyed 6\r\n",
    "guest cpp exceptions: rethrow 1\r\n",
    "guest cpp exceptions: any 1\r\n",
    "guest cpp exceptions: registers 19036\r\n",
    "guest cpp exceptions: nested 21\r\n",
    "guest cpp exceptions: failures 0\r\n",
);

fn built() -> &'static PathBuf {
    static OUT: OnceLock<PathBuf> = OnceLock::new();
    OUT.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let build = Command::new("sh")
            .arg(root.join("tests/compat_fixtures/build-guest-cpp.sh"))
            .current_dir(&root)
            .output()
            .expect("run tests/compat_fixtures/build-guest-cpp.sh");
        assert!(
            build.status.success(),
            "the guest C++ programs did not build:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        root.join("artifacts/guest-cpp")
    })
}

fn run(name: &str, arguments: &[&str]) -> (StartupResult, Arc<ServicesState>) {
    let bytes = std::fs::read(built().join(format!("{name}.exe"))).expect("read the guest program");
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    state.set_program_arguments(arguments).unwrap();
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    // Loaded away from its preferred base, so relocated unwind data and
    // handler addresses matter.
    let layout = StartupLayout {
        image_base: 0x2_5000_0000,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let result = run_synthetic_startup(&bytes, layout, registry, BUDGET)
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    (result, state)
}

fn output(state: &ServicesState) -> String {
    String::from_utf8(state.standard_output()).expect("ASCII output")
}

#[test]
fn a_cpp_program_throws_catches_and_unwinds_at_every_optimisation_level() {
    for level in ["O0", "O1", "O2", "Os"] {
        let name = format!("guest-cpp-exceptions-{level}");
        let (result, state) = run(&name, &["guest-cpp-exceptions.exe"]);
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(0),
            "{name}: output so far:\n{}",
            output(&state)
        );
        assert_eq!(output(&state), EXPECTED_OUTPUT, "{name}: standard output");
        let metrics = &result.dbt_metrics;
        assert!(metrics.blocks_compiled > 0, "{name}: no block compiled");
        assert!(metrics.service_calls > 0, "{name}: no service call");
    }
}

/// Nothing catches the exception, so the search reaches the C runtime's
/// startup frame. Its handler is `__C_specific_handler`, whose scope there
/// has the filter `_gnu_exception_handler`. Read from the mingw-w64 14.0.0
/// startup code this fixture links (#453), that filter continues execution
/// for a continuable GCC exception (code `0x20474343` under the mask
/// `0x20ffffff`), so `RaiseException` returns to GCC's unwinder, which
/// reports the end of the stack. The C++ runtime then calls `std::terminate`,
/// whose default handler in libstdc++ prints the exception's type and
/// `what()` to `stderr` (the libstdc++ manual, "Verbose Terminate Handler")
/// and calls `abort`, which ends the process with exit code 3 (Microsoft
/// Learn, `abort`).
#[test]
fn an_uncaught_exception_terminates_through_the_startup_frame_filter() {
    let (result, state) = run(
        "guest-cpp-exceptions-O1",
        &["guest-cpp-exceptions.exe", "uncaught"],
    );
    assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(3));
    assert_eq!(output(&state), "guest cpp exceptions: throwing\r\n");
    assert_eq!(
        String::from_utf8(state.standard_error()).expect("ASCII output"),
        concat!(
            "terminate called after throwing an instance of 'std::runtime_error'\r\n",
            "  what():  nobody catches this\r\n",
        )
    );
}
