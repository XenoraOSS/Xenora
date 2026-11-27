// SPDX-License-Identifier: GPL-3.0-or-later

//! Issue #934: the Itanium C++ ABI guard-variable services run end to end in
//! an original C++ program through the loader and ARM64 dynamic binary
//! translation.
//!
//! `tests/compat_fixtures/build-cxx-guard.sh` builds
//! `tests/compat_fixtures/cxx_guard.cpp` at four optimization levels with no C
//! or C++ runtime linked. The compiler emits the guard calls for a function-
//! local static; the program also calls each guard service directly with its
//! own aligned guard variables. Expected values below are literals derived
//! from the public guard contract and the program source, never from a run.
//!
//! The build script and the fixture are authored for Xenora. This test fails,
//! and does not skip, when the native ARM64 build tools are missing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, StartupResult, run_synthetic_startup};

/// The load address of the guest image. It differs from the preferred base,
/// so the image's relocations matter.
const IMAGE_BASE: u64 = 0x2_5000_0000;
const STACK_TOP: u64 = 0x2_0001_0000;
const TLS_BASE: u64 = 0x3_0000_0000;
/// The instruction budget for a complete run.
const BUDGET: usize = 5_000_000;

const MAGIC: u32 = 0x6378_6764;
const LAZY_VALUE: u32 = 41;
const GUARD_MINUS_ONE: u32 = u32::MAX;
const LIMIT: ExitReason = ExitReason::IllegalInstruction(0xFE);

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

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
        let script = root.join("tests/compat_fixtures/build-cxx-guard.sh");
        let build = Command::new("sh")
            .arg(&script)
            .current_dir(&root)
            .output()
            .expect("run tests/compat_fixtures/build-cxx-guard.sh");
        assert!(
            build.status.success(),
            "the guest C++ guard program did not build:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let primary = root.join("artifacts/guest-cxx-guard");
        if primary.join("guest-cxx-guard-O0.exe").is_file() {
            return primary;
        }
        let artifacts = root.join("artifacts");
        if let Ok(entries) = std::fs::read_dir(&artifacts) {
            for entry in entries.flatten() {
                let candidate = entry.path();
                if candidate.join("guest-cxx-guard-O0.exe").is_file() {
                    return candidate;
                }
            }
        }
        panic!(
            "the build script did not produce guest-cxx-guard-O0.exe in {}",
            artifacts.display()
        );
    })
}

// ---------------------------------------------------------------------------
// Running the guest
// ---------------------------------------------------------------------------

struct Outcome {
    result: StartupResult,
    state: Arc<ServicesState>,
}

fn run(name: &str, budget: usize) -> Result<Outcome, String> {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let bytes =
        std::fs::read(built().join(format!("{name}.exe"))).expect("read the built guest program");
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let layout = StartupLayout {
        image_base: IMAGE_BASE,
        stack_top: STACK_TOP,
        tls_base: TLS_BASE,
    };
    match run_synthetic_startup(&bytes, layout, registry, budget) {
        Ok(result) => Ok(Outcome { result, state }),
        Err(error) => {
            assert_eq!(state.exit_status(), None, "{name}");
            assert_eq!(state.open_handle_count(), 0, "{name}");
            Err(error.to_string())
        }
    }
}

/// What the program reports in its `.gate` section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ResultBlock {
    magic: u32,
    lazy_first: u32,
    lazy_second: u32,
    init_count: u32,
    direct_first: u32,
    direct_second: u32,
    abort_first: u32,
    abort_after: u32,
    unmapped: u32,
    unaligned: u32,
    reentrant: u32,
    direct_guard: u64,
    abort_guard: u64,
}

fn result_block(outcome: &Outcome) -> ResultBlock {
    let image = &outcome.result.image;
    let section = image
        .pe
        .sections
        .iter()
        .find(|section| section.name == ".gate")
        .expect("a .gate section");
    let at = IMAGE_BASE + u64::from(section.virtual_address);
    let memory = &image.memory;
    ResultBlock {
        magic: memory.read_u32(at).expect("result block"),
        lazy_first: memory.read_u32(at + 4).expect("result block"),
        lazy_second: memory.read_u32(at + 8).expect("result block"),
        init_count: memory.read_u32(at + 12).expect("result block"),
        direct_first: memory.read_u32(at + 16).expect("result block"),
        direct_second: memory.read_u32(at + 20).expect("result block"),
        abort_first: memory.read_u32(at + 24).expect("result block"),
        abort_after: memory.read_u32(at + 28).expect("result block"),
        unmapped: memory.read_u32(at + 32).expect("result block"),
        unaligned: memory.read_u32(at + 36).expect("result block"),
        reentrant: memory.read_u32(at + 40).expect("result block"),
        direct_guard: memory.read_u64(at + 48).expect("result block"),
        abort_guard: memory.read_u64(at + 56).expect("result block"),
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(
        bytes[at..at + 2]
            .try_into()
            .expect("two bytes in the program"),
    )
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(
        bytes[at..at + 4]
            .try_into()
            .expect("four bytes in the program"),
    )
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(
        bytes[at..at + 8]
            .try_into()
            .expect("eight bytes in the program"),
    )
}

/// A section of the program: virtual address, virtual size, raw pointer, raw
/// size.
type Section = (u32, u32, u32, u32);

fn sections(bytes: &[u8]) -> Vec<Section> {
    let pe = u32_at(bytes, 0x3C) as usize;
    let coff = pe + 4;
    let count = u16_at(bytes, coff + 2) as usize;
    let size_of_optional = u16_at(bytes, coff + 16) as usize;
    let headers = pe + 24 + size_of_optional;
    (0..count)
        .map(|index| {
            let at = headers + index * 40;
            (
                u32_at(bytes, at + 12),
                u32_at(bytes, at + 8),
                u32_at(bytes, at + 20),
                u32_at(bytes, at + 16),
            )
        })
        .collect()
}

fn directory(bytes: &[u8], index: usize) -> Option<(u32, u32)> {
    let pe = u32_at(bytes, 0x3C) as usize;
    let optional = pe + 24;
    if index >= u32_at(bytes, optional + 108) as usize {
        return None;
    }
    let at = optional + 112 + index * 8;
    let (rva, size) = (u32_at(bytes, at), u32_at(bytes, at + 4));
    (size != 0).then_some((rva, size))
}

fn file_offset(sections: &[Section], rva: u32) -> Option<usize> {
    for &(virtual_address, virtual_size, raw_pointer, raw_size) in sections {
        let span = virtual_size.max(raw_size);
        if rva >= virtual_address && rva < virtual_address + span {
            return Some((raw_pointer + (rva - virtual_address)) as usize);
        }
    }
    None
}

/// The number of imported functions, read from the program's own import table
/// rather than from a constant or from the loader's binding report.
fn declared_imports(bytes: &[u8]) -> usize {
    let sections = sections(bytes);
    let Some((rva, _)) = directory(bytes, 1) else {
        return 0;
    };
    let Some(at) = file_offset(&sections, rva) else {
        panic!("the import directory is inside the program");
    };
    let mut count = 0;
    let mut descriptor = at;
    loop {
        let (lookup, address, first_thunk) = (
            u32_at(bytes, descriptor),
            u32_at(bytes, descriptor + 12),
            u32_at(bytes, descriptor + 16),
        );
        if lookup == 0 && address == 0 && first_thunk == 0 {
            break;
        }
        let thunks = if lookup != 0 { lookup } else { first_thunk };
        let mut thunk = file_offset(&sections, thunks).expect("a thunk array in the program");
        while u64_at(bytes, thunk) != 0 {
            count += 1;
            thunk += 8;
        }
        descriptor += 20;
    }
    count
}

fn assert_common_expectations(name: &str, outcome: &Outcome) {
    let result = &outcome.result;
    let program =
        std::fs::read(built().join(format!("{name}.exe"))).expect("read the built guest program");
    let imports = declared_imports(&program);
    assert!(imports >= 4, "{name}: the program declares too few imports");
    assert_eq!(result.bound_imports, imports, "{name}: bound imports");

    let metrics = &result.dbt_metrics;
    assert!(metrics.blocks_compiled > 0, "{name}: no block compiled");
    assert!(
        metrics.native_bytes_emitted > 0,
        "{name}: no native code emitted"
    );
    assert!(metrics.service_calls > 0, "{name}: no service call");

    let block = result_block(outcome);
    assert_eq!(block.magic, MAGIC, "{name}: result magic");
    assert_eq!(block.lazy_first, LAZY_VALUE, "{name}: first lazy value");
    assert_eq!(block.lazy_second, LAZY_VALUE, "{name}: second lazy value");
    assert_eq!(
        block.init_count, 1,
        "{name}: the static was initialized once"
    );
    assert_eq!(block.direct_first, 1, "{name}: first direct acquire");
    assert_eq!(block.direct_second, 0, "{name}: second direct acquire");
    assert_eq!(block.abort_first, 1, "{name}: first abort-guard acquire");
    assert_eq!(block.abort_after, 1, "{name}: acquire after abort");
    assert_eq!(
        block.unmapped, GUARD_MINUS_ONE,
        "{name}: unmapped guard result"
    );
    assert_eq!(
        block.unaligned, GUARD_MINUS_ONE,
        "{name}: unaligned guard result"
    );
    assert_eq!(
        block.direct_guard & 0xff,
        1,
        "{name}: release set guard byte"
    );
    assert_eq!(
        block.abort_guard & 0xff,
        1,
        "{name}: final release set guard byte"
    );
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn cxx_guard_abi_runs_at_every_optimization_level() {
    for name in [
        "guest-cxx-guard-O0",
        "guest-cxx-guard-O1",
        "guest-cxx-guard-O2",
        "guest-cxx-guard-Os",
    ] {
        let outcome = run(name, BUDGET).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            outcome.result.execution.exit_reason,
            ExitReason::ProcessExit(0),
            "{name}"
        );
        assert_eq!(outcome.state.exit_status(), Some(0), "{name}");
        assert_common_expectations(name, &outcome);
        assert_eq!(
            result_block(&outcome).reentrant,
            0,
            "{name}: no reentrant control ran"
        );
    }
}

#[test]
fn reentrant_guard_acquire_is_refused_without_mutating() {
    let name = "guest-cxx-guard-reentrant";
    let outcome = run(name, BUDGET).unwrap_or_else(|error| panic!("{name}: {error}"));
    assert_eq!(
        outcome.result.execution.exit_reason,
        ExitReason::ProcessExit(42),
        "{name}: the second acquire was not refused"
    );
    assert_eq!(outcome.state.exit_status(), Some(42), "{name}");
    assert_common_expectations(name, &outcome);
    assert_eq!(
        result_block(&outcome).reentrant,
        GUARD_MINUS_ONE,
        "{name}: reentrant acquire result"
    );
}

#[test]
fn invalid_guard_release_and_abort_stop_the_guest() {
    for name in [
        "guest-cxx-guard-invalid-release",
        "guest-cxx-guard-invalid-abort",
    ] {
        let outcome = run(name, BUDGET).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            outcome.result.execution.exit_reason, LIMIT,
            "{name}: the unsupported release or abort was accepted"
        );
        assert_eq!(outcome.state.exit_status(), None, "{name}");
        assert_common_expectations(name, &outcome);
        // The control's ExitProcess call after the invalid service never ran.
        assert_eq!(result_block(&outcome).reentrant, 0, "{name}");
    }
}
