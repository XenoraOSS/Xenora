// SPDX-License-Identifier: GPL-3.0-or-later

//! Issue #453: original C programs use MSVC-style structured exception
//! handling around CPU faults and raised exceptions, through the loader,
//! `__C_specific_handler`, the exception dispatcher and the ARM64 dynamic
//! binary translation.
//!
//! `tests/compat_fixtures/build-guest-c-seh.sh` builds
//! `tests/compat_fixtures/guest_c_seh.c` once per case at -O0, -O1 and -O2
//! with clang `-fasync-exceptions`. Each case calls `xenora_note`, which this
//! test registers, to report what ran, and exits with its own status.
//!
//! The expected notes and statuses are literals written from the program's
//! source, where each case states them, and from the documented exception
//! codes and access-violation parameters. Nothing here is taken from a run.
//! The test fails, and does not skip, when the toolchain is missing.

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, StartupResult, run_synthetic_startup};

const BUDGET: usize = 1_000_000;
const IMAGE_BASE: u64 = 0x2_5000_0000;
const LEVELS: [&str; 3] = ["O0", "O1", "O2"];

/// Stands for "the address of the faulting instruction", which the program
/// notes and the test checks lies inside the image.
const IN_IMAGE: u64 = u64::MAX;

fn built() -> &'static PathBuf {
    static OUT: OnceLock<PathBuf> = OnceLock::new();
    OUT.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let build = Command::new("sh")
            .arg(root.join("tests/compat_fixtures/build-guest-c-seh.sh"))
            .current_dir(&root)
            .output()
            .expect("run tests/compat_fixtures/build-guest-c-seh.sh");
        assert!(
            build.status.success(),
            "the guest C SEH programs did not build:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        root.join("artifacts/guest-c-seh")
    })
}

fn run(name: &str) -> (StartupResult, Vec<u64>) {
    let bytes = std::fs::read(built().join(format!("{name}.exe"))).expect("read the guest program");
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    let notes = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&notes);
    registry.register("xenora_seh.dll", "xenora_note", move |args, _, _| {
        sink.lock().unwrap().push(args.rcx);
        Ok(0)
    });
    // Loaded away from its preferred base, so relocated handler and scope
    // addresses matter.
    let layout = StartupLayout {
        image_base: IMAGE_BASE,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let result = run_synthetic_startup(&bytes, layout, registry, BUDGET)
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    let notes = notes.lock().unwrap().clone();
    (result, notes)
}

fn in_image(result: &StartupResult, address: u64) -> bool {
    let size = u64::from(result.image.pe.size_of_image);
    (IMAGE_BASE..IMAGE_BASE + size).contains(&address)
}

/// Run `case` at every level and check its exit and its notes.
fn check(case: &str, exit: ExitReason, expected: &[u64]) {
    for level in LEVELS {
        let name = format!("guest-c-seh-{case}-{level}");
        let (result, notes) = run(&name);
        assert_eq!(
            result.execution.exit_reason, exit,
            "{name}: notes so far {notes:x?}"
        );
        assert_eq!(notes.len(), expected.len(), "{name}: notes {notes:x?}");
        for (index, (&note, &want)) in notes.iter().zip(expected).enumerate() {
            if want == IN_IMAGE {
                assert!(in_image(&result, note), "{name}: note {index} {note:#x}");
            } else {
                assert_eq!(note, want, "{name}: note {index}, notes {notes:x?}");
            }
        }
        assert!(result.dbt_metrics.blocks_compiled > 0, "{name}: no block");
    }
}

#[test]
fn a_divide_by_zero_in_try_except_is_caught() {
    check("divide", ExitReason::ProcessExit(42), &[0xC000_0094]);
}

#[test]
fn a_declining_filter_passes_the_fault_to_the_caller() {
    check("search", ExitReason::ProcessExit(43), &[2, 3, 4]);
}

/// Xenora's policy: a fault no filter accepts stops the guest with the
/// original fault at the faulting instruction, after every filter ran.
#[test]
fn a_fault_no_filter_accepts_stops_the_guest_at_the_fault() {
    for level in LEVELS {
        let name = format!("guest-c-seh-unhandled-{level}");
        let (result, notes) = run(&name);
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::DivideError,
            "{name}"
        );
        assert_eq!(notes, [5], "{name}");
        assert!(
            in_image(&result, result.execution.final_state.rip),
            "{name}: stopped at {:#x}",
            result.execution.final_state.rip
        );
    }
}

#[test]
fn an_access_violation_reports_write_read_and_execute() {
    check(
        "write_fault",
        ExitReason::ProcessExit(44),
        &[0xC000_0005, 2, 1, 0x1230, IN_IMAGE],
    );
    check(
        "read_fault",
        ExitReason::ProcessExit(45),
        &[0xC000_0005, 2, 0, 0x1238, IN_IMAGE],
    );
    // An instruction fetch: the faulting address is the instruction's own.
    check(
        "execute_fault",
        ExitReason::ProcessExit(46),
        &[0xC000_0005, 2, 8, 0x1240, 0x1240],
    );
}

#[test]
fn ud2_under_a_constant_filter_is_an_illegal_instruction() {
    check("illegal", ExitReason::ProcessExit(47), &[0xC000_001D]);
}

#[test]
fn a_termination_handler_runs_after_the_filter_and_before_the_handler() {
    check(
        "finally_across_frames",
        ExitReason::ProcessExit(48),
        &[10, 13, 11, 14],
    );
    check(
        "finally_same_frame",
        ExitReason::ProcessExit(49),
        &[30, 32, 20, 23, 21, 24],
    );
}

#[test]
fn a_filter_can_continue_execution_after_a_raise() {
    check("continue_execution", ExitReason::ProcessExit(50), &[41, 40]);
}
