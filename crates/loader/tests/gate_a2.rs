// SPDX-License-Identifier: GPL-3.0-or-later

//! Gate A2 (issue #419): a hosted C program, built from source by this test
//! with the mingw-w64 C runtime and run through the loader, the standard
//! services and the ARM64 DBT.
//!
//! "Hosted" means the C runtime's own startup code runs before `main`, and
//! the program uses the standard C library as an ordinary program does:
//! command-line arguments, heap memory, `qsort` with a callback, `printf`, a
//! file written with `fprintf` and read back with `fgets`, `atexit`, and a
//! return from `main`.
//!
//! `tests/compat_fixtures/build-gate-a2.sh` compiles
//! `tests/compat_fixtures/gate_a2.c` with `x86_64-w64-mingw32-gcc`, so the
//! test needs mingw-w64 on the host. It fails, and does not skip, when the
//! compiler is missing.
//!
//! Every expected value is recomputed here, in Rust, from the description of
//! the program: nothing is read back from a run and asserted. The host
//! arithmetic and the guest arithmetic share no code. The program also
//! checks its own results and exits with the number of the first check that
//! fails, and three control variants end in three other ways, so a runtime
//! that reported success for everything would fail this test.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, StartupResult, run_synthetic_startup};

const REPORT: &str = "C:\\gate-a2-report.txt";
const COUNT: usize = 48;
const SEED: u32 = 2026;
const LABEL: &str = "xenora";
const ARGUMENTS: [&str; 3] = ["gate-a2.exe", "2026", LABEL];
const BUDGET: usize = 2_000_000;

/// Build every variant once and return the output directory.
fn built() -> &'static Path {
    static OUT: OnceLock<PathBuf> = OnceLock::new();
    OUT.get_or_init(|| {
        // One directory per test process: two `cargo test` runs that share a
        // target directory must not write and read the same files.
        let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("gate-a2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/compat_fixtures/build-gate-a2.sh"
        );
        let build = Command::new("sh")
            .arg(script)
            .arg(&out)
            .output()
            .expect("run tests/compat_fixtures/build-gate-a2.sh");
        assert!(
            build.status.success(),
            "the Gate A2 program did not build; it needs mingw-w64 \
             (x86_64-w64-mingw32-gcc):\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let toolchain = std::fs::read_to_string(out.join("toolchain.txt")).unwrap();
        println!("Gate A2 built with:\n{toolchain}");
        out
    })
}

struct Outcome {
    result: StartupResult,
    state: Arc<ServicesState>,
}

impl Outcome {
    fn output(&self) -> String {
        String::from_utf8(self.state.standard_output()).expect("ASCII output")
    }

    fn errors(&self) -> String {
        String::from_utf8(self.state.standard_error()).expect("ASCII output")
    }
}

/// Run one built variant through the real startup path with the standard
/// services and the given command line, loaded away from its preferred base
/// so its relocations matter.
fn run_with(name: &str, arguments: Option<&[&str]>, budget: usize) -> Outcome {
    let bytes = std::fs::read(built().join(format!("{name}.exe"))).unwrap();
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    if let Some(arguments) = arguments {
        state.set_program_arguments(arguments).unwrap();
    }
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let layout = StartupLayout {
        image_base: 0x2_5000_0000,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let result = run_synthetic_startup(&bytes, layout, registry, budget)
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    Outcome { result, state }
}

fn run(name: &str) -> Outcome {
    run_with(name, Some(&ARGUMENTS), BUDGET)
}

// ---------------------------------------------------------------------------
// What the program computes, restated independently
// ---------------------------------------------------------------------------

/// The program's generator: a linear congruential step, then bits 8 to 23.
fn values(seed: u32) -> Vec<u32> {
    let mut state = seed;
    (0..COUNT)
        .map(|_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 8) & 0xFFFF
        })
        .collect()
}

/// 64-bit FNV-1a over each value's four bytes, low byte first.
fn checksum(values: &[u32]) -> u64 {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .fold(14_695_981_039_346_656_037, |sum, byte| {
            (sum ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
        })
}

struct Expected {
    /// Standard output of a complete run, with the line ends a text-mode
    /// stream produces.
    output: String,
    /// The report file as it is stored on the drive.
    report: String,
    /// What the program prints before its first self-check on the data.
    arguments: String,
    sorted_line: String,
}

/// How many checks the program counts on a complete run: five on its
/// arguments, one for the allocation, 47 on the sort order, one on the total,
/// the first `fopen`, five on writing the report, fifteen on reading it back
/// and one for the missing file.
const CHECKS: u32 = 5 + 1 + 47 + 1 + 1 + 5 + 15 + 1;

fn expected() -> Expected {
    let unsorted = values(SEED);
    let mut sorted = unsorted.clone();
    sorted.sort_unstable();
    let total: u64 = unsorted.iter().map(|value| u64::from(*value)).sum();
    let (min, max) = (sorted[0], sorted[COUNT - 1]);
    // A text-mode stream stores each line feed as a carriage return and a
    // line feed.
    let arguments = format!(
        "gate a2: 3 arguments\r\n  argv[0] = \"gate-a2.exe\"\r\n  argv[1] = \"{SEED}\"\r\n  \
         argv[2] = \"{LABEL}\"\r\n"
    );
    let sorted_line = format!("sorted {COUNT} values: min {min}, max {max}, total {total}\r\n");
    let output = format!(
        "{arguments}{sorted_line}checksum before {:016x}, after {:016x}\r\nreport \
         gate-a2-report.txt read back and verified\r\natexit: {CHECKS} checks passed\r\n",
        checksum(&unsorted),
        checksum(&sorted)
    );
    let report = format!("label {LABEL}\r\ntotal {total}\r\nrange {min} {max}\r\nend\r\n");
    Expected {
        output,
        report,
        arguments,
        sorted_line,
    }
}

#[test]
fn the_restated_program_is_not_trivial() {
    // The generator really produces varied data, the sort really changes it,
    // and the two checksums really differ: the expected text is not built
    // from degenerate values.
    let unsorted = values(SEED);
    let mut sorted = unsorted.clone();
    sorted.sort_unstable();
    assert_ne!(unsorted, sorted);
    assert_ne!(checksum(&unsorted), checksum(&sorted));
    let mut distinct = sorted.clone();
    distinct.dedup();
    assert!(distinct.len() > COUNT / 2);
    assert_eq!(CHECKS, 76);
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn the_program_runs_to_its_exit_at_every_optimization_level() {
    let wanted = expected();
    for name in ["gate-a2-O0", "gate-a2-O1", "gate-a2-O2", "gate-a2-Os"] {
        let outcome = run(name);
        let result = &outcome.result;
        let state = &outcome.state;

        // It returned from main, and the C runtime turned that into exit(0).
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(0),
            "{name}: stdout {:?}, stderr {:?}",
            outcome.output(),
            outcome.errors()
        );
        assert_eq!(state.exit_status(), Some(0), "{name}");

        // Standard output, byte for byte, including the line the function
        // registered with atexit printed after main returned.
        assert_eq!(outcome.output(), wanted.output, "{name}");
        assert_eq!(outcome.errors(), "", "{name}");

        // The report file is on the drive as the program wrote it, and the
        // file it looked for was not created.
        let report = state.file_contents(REPORT).expect("a report");
        assert_eq!(String::from_utf8(report).unwrap(), wanted.report, "{name}");
        assert_eq!(
            state.file_contents("C:\\gate-a2-missing.txt"),
            None,
            "{name}"
        );
        assert_eq!(state.open_handle_count(), 0, "{name}");

        // It was loaded, relocated and bound by the real startup path, and
        // ran as translated ARM64 code. The C runtime's startup code is most
        // of what runs.
        assert!(
            result.bound_imports >= 40,
            "{name}: {}",
            result.bound_imports
        );
        assert!(result.relocations.applied_dir64 > 0, "{name}");
        let metrics = &result.dbt_metrics;
        assert!(
            metrics.blocks_compiled > 0 && metrics.native_bytes_emitted > 0,
            "{name}"
        );
        assert!(metrics.instructions_executed > 5_000, "{name}");
        assert!(metrics.service_calls > 60, "{name}");
        println!(
            "{name}: {} imports bound, {} guest instructions, {} of them through the shared \
             semantics, {} service calls",
            result.bound_imports,
            metrics.instructions_executed,
            metrics.interpreter_fallbacks,
            metrics.service_calls
        );
    }
}

#[test]
fn other_arguments_give_other_results() {
    // A different seed and label change the data, the checksums and the
    // report, so the expected text above is not a constant the runtime could
    // echo.
    let outcome = run_with("gate-a2-O1", Some(&["a2", "7", "seven"]), BUDGET);
    assert_eq!(
        outcome.result.execution.exit_reason,
        ExitReason::ProcessExit(0)
    );
    let unsorted = values(7);
    let mut sorted = unsorted.clone();
    sorted.sort_unstable();
    let total: u64 = unsorted.iter().map(|value| u64::from(*value)).sum();
    let output = outcome.output();
    assert!(
        output.starts_with(
            "gate a2: 3 arguments\r\n  argv[0] = \"a2\"\r\n  argv[1] = \"7\"\r\n  argv[2] = \
             \"seven\"\r\n"
        ),
        "{output:?}"
    );
    assert!(
        output.contains(&format!(
            "sorted {COUNT} values: min {}, max {}, total {total}\r\n",
            sorted[0],
            sorted[COUNT - 1]
        )),
        "{output:?}"
    );
    assert!(
        output.contains(&format!(
            "checksum before {:016x}, after {:016x}\r\n",
            checksum(&unsorted),
            checksum(&sorted)
        )),
        "{output:?}"
    );
    let report = outcome.state.file_contents(REPORT).unwrap();
    assert_eq!(
        String::from_utf8(report).unwrap(),
        format!(
            "label seven\r\ntotal {total}\r\nrange {} {}\r\nend\r\n",
            sorted[0],
            sorted[COUNT - 1]
        )
    );
}

#[test]
fn wrong_arguments_fail_the_check_that_looks_at_them() {
    // (arguments, exit status, the lines printed before the failure)
    let cases: [(Option<&[&str]>, u32, &str); 4] = [
        // No host arguments: one empty argument, so the count is wrong.
        (None, 11, "gate a2: 1 arguments\r\n  argv[0] = \"\"\r\n"),
        (
            Some(&["a2", "5"]),
            11,
            "gate a2: 2 arguments\r\n  argv[0] = \"a2\"\r\n  argv[1] = \"5\"\r\n",
        ),
        // A seed that is not a number.
        (
            Some(&["a2", "5x", "label"]),
            13,
            "gate a2: 3 arguments\r\n  argv[0] = \"a2\"\r\n  argv[1] = \"5x\"\r\n  argv[2] = \
             \"label\"\r\n",
        ),
        // An empty label.
        (
            Some(&["a2", "5", ""]),
            14,
            "gate a2: 3 arguments\r\n  argv[0] = \"a2\"\r\n  argv[1] = \"5\"\r\n  argv[2] = \
             \"\"\r\n",
        ),
    ];
    for (arguments, status, printed) in cases {
        let outcome = run_with("gate-a2-O1", arguments, BUDGET);
        let what = format!("{arguments:?}");
        assert_eq!(
            outcome.result.execution.exit_reason,
            ExitReason::ProcessExit(status),
            "{what}"
        );
        // The failed check says so on standard error, and the function
        // registered with atexit still runs, because main returned. It
        // reports the checks that passed before the failure: registering
        // itself, and then one for each argument check before this one.
        assert_eq!(
            outcome.errors(),
            format!("gate a2: check {status} failed\r\n"),
            "{what}"
        );
        let passed = status - 10;
        assert_eq!(
            outcome.output(),
            format!("{printed}atexit: {passed} checks passed\r\n"),
            "{what}"
        );
        assert_eq!(outcome.state.file_contents(REPORT), None, "{what}");
    }
}

#[test]
fn a_wrong_expectation_exits_with_the_number_of_the_failed_check() {
    // The variant expects the sorted total to be one more than it is.
    // Everything before that check passes, and nothing after it runs.
    let wanted = expected();
    let outcome = run("gate-a2-break");
    assert_eq!(
        outcome.result.execution.exit_reason,
        ExitReason::ProcessExit(22)
    );
    assert_eq!(outcome.state.exit_status(), Some(22));
    assert_eq!(outcome.errors(), "gate a2: check 22 failed\r\n");
    // Five argument checks, the allocation and 47 comparisons passed.
    assert_eq!(
        outcome.output(),
        format!("{}atexit: 53 checks passed\r\n", wanted.arguments)
    );
    assert_eq!(outcome.state.file_contents(REPORT), None);
}

#[test]
fn exit_half_way_runs_the_registered_function_and_nothing_after() {
    // The variant calls exit(42) after the sort. The status is exit's, the
    // function registered with atexit runs, and the report is never written.
    let wanted = expected();
    let outcome = run("gate-a2-exit");
    assert_eq!(
        outcome.result.execution.exit_reason,
        ExitReason::ProcessExit(42)
    );
    assert_eq!(outcome.state.exit_status(), Some(42));
    assert_eq!(outcome.errors(), "");
    let output = outcome.output();
    assert!(
        output.starts_with(&format!("{}{}", wanted.arguments, wanted.sorted_line)),
        "{output:?}"
    );
    // Through check 22: 5 + 1 + 47 + 1.
    assert!(
        output.ends_with("atexit: 54 checks passed\r\n"),
        "{output:?}"
    );
    assert!(!output.contains("report "), "{output:?}");
    assert_eq!(outcome.state.file_contents(REPORT), None);
}

#[test]
fn abort_half_way_runs_no_registered_function() {
    // The variant calls abort() after the sort: exit code 3, and the function
    // registered with atexit does not run.
    let wanted = expected();
    let outcome = run("gate-a2-abort");
    assert_eq!(
        outcome.result.execution.exit_reason,
        ExitReason::ProcessExit(3)
    );
    assert_eq!(outcome.state.exit_status(), Some(3));
    let output = outcome.output();
    assert!(
        output.starts_with(&format!("{}{}", wanted.arguments, wanted.sorted_line)),
        "{output:?}"
    );
    assert!(!output.contains("atexit:"), "{output:?}");
    assert_eq!(outcome.state.file_contents(REPORT), None);
}

#[test]
fn a_step_budget_that_is_too_small_is_not_an_exit() {
    let outcome = run_with("gate-a2-O1", Some(&ARGUMENTS), 200);
    assert_eq!(
        outcome.result.execution.exit_reason,
        ExitReason::StepLimitExceeded
    );
    assert_eq!(outcome.state.exit_status(), None);
    assert_eq!(outcome.output(), "");
    assert_eq!(outcome.state.file_contents(REPORT), None);
}
