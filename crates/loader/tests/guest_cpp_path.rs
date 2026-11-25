// SPDX-License-Identifier: GPL-3.0-or-later

//! Issue #437: an original C++ PE32+ program runs end to end through the
//! loader, the guest services and the ARM64 dynamic binary translation.
//!
//! `tests/compat_fixtures/build-guest-cpp.sh` compiles
//! `tests/compat_fixtures/guest_cpp.cpp` into four optimisation levels and a
//! negative control. The program is a hosted C++ program: it has namespace
//! scope objects with constructors and destructors, a function-local static
//! that is initialised on first use, a class with a virtual function, a
//! thread, a file it writes and reads, and it returns from `main`.
//!
//! The expected standard output is a literal in this file. It is not computed
//! from the guest program, from a previous run or from any function under
//! test: the ten lines below are what the program prints, in that order.
//!
//! The build script, the guest program and the C++ runtime services are built
//! by other workers. This test fails, and does not skip, when they are
//! missing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::scheduler::DEFAULT_QUANTUM;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{
    StartupLayout, StartupResult, run_synthetic_startup, run_synthetic_startup_scheduled,
};

/// The load address of the guest image. It differs from the preferred base,
/// so the image's relocations matter.
const IMAGE_BASE: u64 = 0x2_5000_0000;
const STACK_TOP: u64 = 0x2_0001_0000;
const TLS_BASE: u64 = 0x3_0000_0000;
/// The instruction budget for a complete run.
const BUDGET: usize = 20_000_000;

/// The file the program writes.
const FILE: &str = "C:\\guest-cpp.txt";

/// The ten lines the program prints, in order, as they appear on a text-mode
/// standard output: each line feed is a carriage return and a line feed.
///
/// This is a literal. Nothing in this file derives it from the guest program
/// or from a run.
const EXPECTED_OUTPUT: &str = concat!(
    "guest cpp: ctor 1\r\n",
    "guest cpp: lazy 1\r\n",
    "guest cpp: areas 9 25\r\n",
    "guest cpp: sorted 1 2 3 4 5\r\n",
    "guest cpp: len 5 cmp 0\r\n",
    "guest cpp: fn 7 8\r\n",
    "guest cpp: tls 7\r\n",
    "guest cpp: worker 7\r\n",
    "guest cpp: file alpha\r\n",
    "guest cpp: dtor 1\r\n",
);

/// The two lines the program writes into its file.
///
/// The guest's file stream is not translated by the loader, so the file holds
/// the line feeds the program wrote. A program that wrote a carriage return
/// and a line feed would produce the same two lines, so the comparison
/// accepts either line ending and checks the two lines themselves.
const EXPECTED_FILE: &str = "alpha\nbeta\n";

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// The repository root.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}

/// Build every variant once and return the directory that holds them.
///
/// The build script writes into `artifacts/guest-cpp`.
fn built() -> &'static Path {
    static OUT: OnceLock<PathBuf> = OnceLock::new();
    OUT.get_or_init(|| {
        let root = repo_root();
        let script = root.join("tests/compat_fixtures/build-guest-cpp.sh");
        let build = Command::new("sh")
            .arg(&script)
            .current_dir(&root)
            .output()
            .expect("run tests/compat_fixtures/build-guest-cpp.sh");
        assert!(
            build.status.success(),
            "the guest C++ program did not build:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        find_build_dir(&root)
    })
}

/// The directory the build script wrote the program into.
fn find_build_dir(root: &Path) -> PathBuf {
    let wanted = "guest-cpp-O0.exe";
    let primary = root.join("artifacts/guest-cpp");
    if primary.join(wanted).is_file() {
        return primary;
    }
    // The script may name its output directory differently; look for the
    // program one level below the artifacts directory.
    let artifacts = root.join("artifacts");
    if let Ok(entries) = std::fs::read_dir(&artifacts) {
        for entry in entries.flatten() {
            let candidate = entry.path();
            if candidate.join(wanted).is_file() {
                return candidate;
            }
        }
    }
    panic!(
        "the build script did not produce {wanted} in {} or in a directory below it",
        artifacts.display()
    );
}

// ---------------------------------------------------------------------------
// Running the guest
// ---------------------------------------------------------------------------

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
/// services, loaded away from its preferred base so its relocations matter.
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
    // The guest creates a thread, so the entry point runs under the
    // deterministic cooperative scheduler.
    match run_synthetic_startup_scheduled(
        &bytes,
        layout,
        registry,
        Arc::clone(&state),
        budget,
        DEFAULT_QUANTUM,
        |_| {},
    ) {
        Ok(result) => Ok(Outcome { result, state }),
        Err(error) => Err(error.to_string()),
    }
}

// ---------------------------------------------------------------------------
// What the built program declares, read from the program itself
// ---------------------------------------------------------------------------

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

/// A section of the program: virtual address, virtual size, raw pointer and
/// raw size.
type Section = (u32, u32, u32, u32);

/// The sections of a PE32+ program.
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

/// The offset of a directory entry in the optional header, if it is there.
fn directory(bytes: &[u8], index: usize) -> Option<(u32, u32)> {
    let pe = u32_at(bytes, 0x3C) as usize;
    let opt = pe + 24;
    let count = u32_at(bytes, opt + 108) as usize;
    if index >= count {
        return None;
    }
    let at = opt + 112 + index * 8;
    let rva = u32_at(bytes, at);
    let size = u32_at(bytes, at + 4);
    if size == 0 {
        return None;
    }
    Some((rva, size))
}

/// The file offset of an address in the program.
fn file_offset(sections: &[Section], rva: u32) -> Option<usize> {
    for &(virtual_address, virtual_size, raw_pointer, raw_size) in sections {
        let span = virtual_size.max(raw_size);
        if rva >= virtual_address && rva < virtual_address + span {
            let offset = raw_pointer + (rva - virtual_address);
            return Some(offset as usize);
        }
    }
    None
}

/// The number of imported functions the program declares.
///
/// This is read from the program's own import directory, so the loader's
/// report is checked against the program and not against a constant.
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
        let lookup = u32_at(bytes, descriptor);
        let address = u32_at(bytes, descriptor + 12);
        let first_thunk = u32_at(bytes, descriptor + 16);
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

/// The number of DIR64 relocations the program declares.
///
/// This is read from the program's own base relocation directory, so the
/// loader's report is checked against the program and not against a constant.
fn declared_dir64(bytes: &[u8]) -> usize {
    let sections = sections(bytes);
    let Some((rva, size)) = directory(bytes, 5) else {
        return 0;
    };
    let Some(at) = file_offset(&sections, rva) else {
        panic!("the relocation directory is inside the program");
    };
    let end = at + size as usize;
    let mut count = 0;
    let mut block = at;
    while block + 8 <= end {
        let block_size = u32_at(bytes, block + 4) as usize;
        if block_size < 8 {
            break;
        }
        // A base relocation entry is a 16-bit word: the type is in its high
        // four bits and the offset within the page in its low twelve.
        let mut entry = block + 8;
        while entry + 2 <= block + block_size {
            let kind = u16_at(bytes, entry) >> 12;
            if kind == 10 {
                count += 1;
            }
            entry += 2;
        }
        block += block_size;
    }
    count
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// The expected output is ten lines, and the last of them is the
/// destructor's.
#[test]
fn the_expected_output_is_ten_lines_and_ends_with_the_destructor() {
    assert_eq!(EXPECTED_OUTPUT.matches("\r\n").count(), 10);
    assert!(EXPECTED_OUTPUT.starts_with("guest cpp: ctor 1\r\n"));
    assert!(EXPECTED_OUTPUT.ends_with("guest cpp: dtor 1\r\n"));
}

/// The C++ program runs, prints its ten lines, writes its file and returns
/// zero, at every optimisation level.
#[test]
fn the_guest_cpp_path_runs_prints_writes_and_exits_at_every_optimization_level() {
    for name in [
        "guest-cpp-O0",
        "guest-cpp-O1",
        "guest-cpp-O2",
        "guest-cpp-Os",
    ] {
        let outcome = run(name, BUDGET).unwrap_or_else(|error| panic!("{name}: {error}"));
        let result = &outcome.result;
        let state = &outcome.state;

        // It was loaded, relocated and bound by the real startup path. The
        // expected numbers are read from the built program itself.
        let program = std::fs::read(built().join(format!("{name}.exe")))
            .expect("read the built guest program");
        let imports = declared_imports(&program);
        assert!(imports > 0, "{name}: the program declares no import");
        assert_eq!(result.bound_imports, imports, "{name}: bound imports");
        let dir64 = declared_dir64(&program);
        assert_eq!(
            result.relocations.applied_dir64, dir64,
            "{name}: applied DIR64 relocations"
        );
        println!("{name}: {imports} bound imports, {dir64} applied DIR64 relocations",);

        // It ended itself, with status zero: every check inside it passed.
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(0),
            "{name}: stdout {:?}, stderr {:?}",
            outcome.output(),
            outcome.errors()
        );
        assert_eq!(state.exit_status(), Some(0), "{name}");

        // It ran as translated ARM64 code and called the guest services.
        let metrics = &result.dbt_metrics;
        assert!(metrics.blocks_compiled > 0, "{name}: no block compiled");
        assert!(
            metrics.native_bytes_emitted > 0,
            "{name}: no native code emitted"
        );
        assert!(metrics.service_calls > 0, "{name}: no service call");

        // Its standard output, byte for byte, in order.
        assert_eq!(outcome.output(), EXPECTED_OUTPUT, "{name}: standard output");
        assert_eq!(outcome.errors(), "", "{name}: standard error");

        // The namespace scope object was destroyed at shutdown, so the last
        // line is the destructor's.
        assert!(
            outcome.output().ends_with("guest cpp: dtor 1\r\n"),
            "{name}: the destructor did not run"
        );

        // The file it wrote holds the two lines it wrote.
        let file = state
            .file_contents(FILE)
            .unwrap_or_else(|| panic!("{name}: no file"));
        let text = String::from_utf8(file).expect("ASCII file");
        assert_eq!(
            text.replace("\r\n", "\n"),
            EXPECTED_FILE,
            "{name}: the file the program wrote"
        );
        // The program closed the file, and left no handle behind.
        assert_eq!(state.open_handle_count(), 0, "{name}");
    }
}

/// A program that imports something nobody provides is refused before its
/// entry point runs.
#[test]
fn an_import_nobody_provides_is_refused_before_entry() {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let bytes =
        std::fs::read(built().join("guest-cpp-missing.exe")).expect("read the built guest program");
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let layout = StartupLayout {
        image_base: IMAGE_BASE,
        stack_top: STACK_TOP,
        tls_base: TLS_BASE,
    };
    let error = run_synthetic_startup(&bytes, layout, registry, BUDGET)
        .err()
        .expect("the image is refused")
        .to_string();
    println!("{error}");
    // The refusal names the symbol the control imports from a module no
    // service provides, so it is refused for that reason and not for another.
    assert!(
        error.to_lowercase().contains("missingservice"),
        "unexpected refusal: {error}"
    );
    // The guest never ran: it never exited, and it left no handle behind.
    assert_eq!(state.exit_status(), None, "the guest exited");
    assert_eq!(state.open_handle_count(), 0, "a handle was left open");
}
