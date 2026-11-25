// SPDX-License-Identifier: GPL-3.0-or-later

//! Gate A1 (issue #329): an original C program with no C library linked,
//! built from source by this test and run through the loader, the standard
//! services and the ARM64 DBT.
//!
//! `tests/compat_fixtures/build-gate-a1.sh` compiles
//! `tests/compat_fixtures/gate_a1.c` with clang and links it with an LLD, so
//! the test needs both on the host. It fails, and does not skip, when they
//! are missing.
//!
//! Every expected value is recomputed here, in Rust, from the description of
//! the program: nothing is read back from a run and asserted. The host
//! arithmetic and the guest arithmetic share no code.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, StartupResult, run_synthetic_startup};

/// The status of a run in which every check passed.
const STATUS: u32 = 0x00A1_A1A1;
const RECORDS: u32 = 64;
/// Checks the program counts on a complete run, and the one that the
/// `GATE_BREAK` variant fails: three for allocation, 63 for the sort order,
/// then the Fibonacci check.
const CHECKS: u32 = 92;
const BROKEN_CHECK: u32 = 67;
/// Twelve from ucrtbase.dll and eight from kernel32.dll.
const IMPORTS: usize = 20;
/// The six entries of the program's table of function pointers.
const RELOCATIONS: usize = 6;

/// Build every variant once and return the output directory.
fn built() -> &'static Path {
    static OUT: OnceLock<PathBuf> = OnceLock::new();
    OUT.get_or_init(|| {
        // One directory per test process: two `cargo test` runs that share a
        // target directory must not write and read the same files.
        let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("gate-a1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/compat_fixtures/build-gate-a1.sh"
        );
        let build = Command::new("sh")
            .arg(script)
            .arg(&out)
            .output()
            .expect("run tests/compat_fixtures/build-gate-a1.sh");
        assert!(
            build.status.success(),
            "the Gate A1 program did not build; it needs an ARM64 clang and an LLD, for example 'brew install llvm lld', or set LLD_LINK:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let toolchain = std::fs::read_to_string(out.join("toolchain.txt")).unwrap();
        println!("Gate A1 built with:\n{toolchain}");
        out
    })
}

struct Outcome {
    result: StartupResult,
    state: Arc<ServicesState>,
}

/// Run one built variant through the real startup path with the standard
/// services, loaded away from its preferred base so its relocations matter.
fn run(name: &str, budget: usize) -> Result<Outcome, String> {
    let bytes = std::fs::read(built().join(format!("{name}.exe"))).unwrap();
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let layout = StartupLayout {
        image_base: 0x2_5000_0000,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    match run_synthetic_startup(&bytes, layout, registry, budget) {
        Ok(result) => Ok(Outcome { result, state }),
        Err(error) => {
            // Nothing may have run or been left behind.
            assert_eq!(state.exit_status(), None, "{name}");
            assert_eq!(state.open_handle_count(), 0, "{name}");
            assert_eq!(state.file_contents("C:\\gate-a1.txt"), None, "{name}");
            Err(error.to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// What the program computes, restated independently
// ---------------------------------------------------------------------------

/// One of six steps chosen by the round number.
fn mix(value: u64, round: u32) -> u64 {
    match round % 6 {
        0 => value
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407),
        1 => value.rotate_left(13),
        2 => value / 7 + value % 1_000_003,
        3 => value ^ (value >> 29),
        // C division of a signed 64-bit value truncates toward zero.
        4 => (value as i64 / -3) as u64,
        _ => value.wrapping_sub(u64::from(round)),
    }
}

struct Expected {
    report: String,
    /// The sorted records as the program stores them: key, weight, ratio.
    data: Vec<u8>,
    key_checksum: u64,
}

fn expected() -> Expected {
    let mut state = 0x243F_6A88_85A3_08D3_u64;
    let mut records = Vec::new();
    let mut weighted = 0u64;
    for index in 0..RECORDS {
        state = mix(state, index);
        let fraction = state & 0xFFFF;
        // The program sums fraction / 65536 * index in doubles. Every term
        // and the sum are exact, so the scaled total is this integer.
        weighted += fraction * u64::from(index);
        // The index in the low six bits makes every key different.
        let key = (((state >> 20) as u32) << 6) | index;
        records.push((key, index, fraction as f64 / 65536.0));
    }
    records.sort_by_key(|record| record.0);
    assert!(records.windows(2).all(|pair| pair[0].0 != pair[1].0));

    // FNV-1a over each key, least significant byte first.
    let mut key_checksum = 0xCBF2_9CE4_8422_2325_u64;
    let mut data = Vec::new();
    for (key, weight, ratio) in &records {
        for byte in key.to_le_bytes() {
            key_checksum = (key_checksum ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3);
        }
        data.extend(key.to_le_bytes());
        data.extend(weight.to_le_bytes());
        data.extend(ratio.to_le_bytes());
    }
    let wide = (state % 1_000_000_007) as i64 * -977 / 13;
    // Fibonacci(12) is 144. The text is 16 characters; its hexadecimal and
    // decimal fields are 0x7b and -42. A missing file is error 2
    // (ERROR_FILE_NOT_FOUND) and an impossible allocation sets errno to 12
    // (ENOMEM). The data file holds 64 records of 16 bytes.
    let report = format!(
        "gate a1\nrecords {RECORDS}\nkeys {key_checksum}\nweighted {weighted}\nfibonacci 144\n\
         wide {wide}\nstrings 16 123 -42\nerrors 2 12\nfile {}\n",
        data.len()
    );
    Expected {
        report,
        data,
        key_checksum,
    }
}

/// The program's result block, which it keeps in a section named `.gate`:
/// magic, check count, key checksum, kept pointer, freed pointer.
fn result_block(outcome: &Outcome) -> (u32, u32, u64, u64, u64) {
    let image = &outcome.result.image;
    let section = image
        .pe
        .sections
        .iter()
        .find(|section| section.name == ".gate")
        .expect("a .gate section");
    let at = 0x2_5000_0000 + u64::from(section.virtual_address);
    let memory = &image.memory;
    (
        memory.read_u32(at).unwrap(),
        memory.read_u32(at + 4).unwrap(),
        memory.read_u64(at + 8).unwrap(),
        memory.read_u64(at + 16).unwrap(),
        memory.read_u64(at + 24).unwrap(),
    )
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn the_program_runs_to_its_exit_at_every_optimization_level() {
    let wanted = expected();
    for name in ["gate-a1-O0", "gate-a1-O1", "gate-a1-O2", "gate-a1-Os"] {
        let outcome = run(name, 2_000_000).unwrap_or_else(|error| panic!("{name}: {error}"));
        let result = &outcome.result;
        let state = &outcome.state;

        // It was loaded, relocated and bound by the real startup path.
        assert_eq!(result.bound_imports, IMPORTS, "{name}");
        assert_eq!(result.relocations.applied_dir64, RELOCATIONS, "{name}");

        // It ended itself, with all 32 bits of its status.
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(STATUS),
            "{name}"
        );
        assert_eq!(state.exit_status(), Some(STATUS), "{name}");

        // It ran as translated ARM64 code.
        let metrics = &result.dbt_metrics;
        assert!(
            metrics.blocks_compiled > 0 && metrics.native_bytes_emitted > 0,
            "{name}"
        );
        assert!(metrics.instructions_executed > 1_000, "{name}");
        assert!(metrics.service_calls > 20, "{name}");
        println!(
            "{name}: {} guest instructions, {} of them through the shared semantics, {} service \
             calls",
            metrics.instructions_executed, metrics.interpreter_fallbacks, metrics.service_calls
        );

        // Output: the report it wrote.
        let report = state.file_contents("C:\\gate-a1.txt").expect("a report");
        assert_eq!(String::from_utf8(report).unwrap(), wanted.report, "{name}");

        // Filesystem: the data file holds the sorted records, the file it
        // looked for was not created, and the report handle it left open was
        // closed by the exit.
        assert_eq!(
            state.file_contents("C:\\gate-a1.bin").as_deref(),
            Some(wanted.data.as_slice()),
            "{name}"
        );
        assert_eq!(state.file_contents("C:\\none.bin"), None, "{name}");
        assert_eq!(state.open_handle_count(), 0, "{name}");

        // Memory: the result block, a block it kept and a block it freed.
        let (magic, checks, key_checksum, kept, freed) = result_block(&outcome);
        assert_eq!((magic, checks), (0x3141_5926, CHECKS), "{name}");
        assert_eq!(key_checksum, wanted.key_checksum, "{name}");
        let memory = &result.image.memory;
        let mut text = [0u8; 16];
        memory.read_bytes(kept, &mut text).unwrap();
        assert_eq!(&text, b"kept by gate a1\0", "{name}");
        assert!(
            memory.read_u8(freed).is_err(),
            "{name}: freed block is still mapped"
        );
    }
}

#[test]
fn a_returning_entry_point_gives_the_same_result() {
    let outcome = run("gate-a1-return", 2_000_000).unwrap();
    assert_eq!(
        outcome.result.execution.exit_reason,
        ExitReason::ProcessExit(STATUS)
    );
    assert_eq!(outcome.state.exit_status(), Some(STATUS));
    assert_eq!(outcome.state.open_handle_count(), 0);
    let report = outcome.state.file_contents("C:\\gate-a1.txt").unwrap();
    assert_eq!(String::from_utf8(report).unwrap(), expected().report);
}

#[test]
fn a_wrong_expectation_exits_with_the_number_of_the_failed_check() {
    // The variant expects Fibonacci(12) to be 145. Everything before that
    // check passes, and nothing after it runs.
    let outcome = run("gate-a1-break", 2_000_000).unwrap();
    assert_eq!(
        outcome.result.execution.exit_reason,
        ExitReason::ProcessExit(BROKEN_CHECK)
    );
    assert_eq!(outcome.state.exit_status(), Some(BROKEN_CHECK));
    assert_eq!(result_block(&outcome).1, BROKEN_CHECK);
    assert_eq!(outcome.state.file_contents("C:\\gate-a1.bin"), None);
    assert_eq!(outcome.state.file_contents("C:\\gate-a1.txt"), None);
}

#[test]
fn an_import_nobody_provides_is_refused_before_entry() {
    let error = run("gate-a1-missing", 2_000_000).err().expect("refusal");
    assert_eq!(
        error,
        "unresolved import dependency: kernel32.dll!GateMissingService"
    );
}

#[test]
fn a_step_budget_that_is_too_small_is_not_an_exit() {
    let outcome = run("gate-a1-O1", 500).unwrap();
    assert_eq!(
        outcome.result.execution.exit_reason,
        ExitReason::StepLimitExceeded
    );
    assert_eq!(outcome.state.exit_status(), None);
    assert_eq!(outcome.state.file_contents("C:\\gate-a1.txt"), None);
}
