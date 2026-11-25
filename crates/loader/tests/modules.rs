// SPDX-License-Identifier: GPL-3.0-or-later

//! Which module a program is, and the handles, references and address lookup of
//! the built-in modules.
//!
//! `tests/compat_fixtures/build-modules.sh` compiles an original C program with
//! no C library and an original hosted C++ program with mingw-w64, so this test
//! needs it on the host and fails, not skips, when it is missing. The programs
//! compare the answers with what they can see themselves: the linker's
//! `__ImageBase`, their own entry point and their mapped header.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use support::{Guest, SCRATCH, SCRATCH_LEN};
use xenora_loader::cpu::ExitReason;
use xenora_loader::import_table::ImportName;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};

/// The status of the C program's run in which every check passed.
const STATUS: u32 = 0x4D0D;

fn built() -> &'static Path {
    static OUT: OnceLock<PathBuf> = OnceLock::new();
    OUT.get_or_init(|| {
        let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("modules-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/compat_fixtures/build-modules.sh"
        );
        let build = Command::new("sh").arg(script).arg(&out).output().unwrap();
        assert!(
            build.status.success(),
            "the module programs did not build; they need mingw-w64:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        out
    })
}

fn state() -> Arc<ServicesState> {
    Arc::new(ServicesState::new(Arc::new(MockClock::new(0))))
}

/// Run a program at `image_base`, away from the linker's choice.
fn run_at(name: &str, image_base: u64) -> (ExitReason, String) {
    let bytes = std::fs::read(built().join(format!("{name}.exe"))).unwrap();
    let state = state();
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let layout = StartupLayout {
        image_base,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let result = run_synthetic_startup(&bytes, layout, registry, 5_000_000)
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    let output = String::from_utf8(state.standard_output()).unwrap();
    (result.execution.exit_reason, output)
}

const LEVELS: [&str; 4] = ["O0", "O1", "O2", "Os"];

#[test]
fn a_c_program_finds_itself_and_the_built_in_modules() {
    for level in LEVELS {
        for base in [0x2_5000_0000, 0x1_7FFF_0000] {
            let (exit, _) = run_at(&format!("modules-{level}"), base);
            assert_eq!(
                exit,
                ExitReason::ProcessExit(STATUS),
                "{level} at {base:#x}"
            );
        }
    }
}

#[test]
fn a_pinned_module_outlives_its_references() {
    let (exit, _) = run_at("modules-pin", 0x2_5000_0000);
    assert_eq!(exit, ExitReason::ProcessExit(STATUS));
}

#[test]
fn the_api_sets_and_kernelbase_reach_the_same_services() {
    let (exit, _) = run_at("modules-apiset", 0x2_5000_0000);
    assert_eq!(exit, ExitReason::ProcessExit(STATUS));
}

#[test]
fn a_failed_check_ends_the_program_with_its_number() {
    let (exit, _) = run_at("modules-break", 0x2_5000_0000);
    assert_eq!(exit, ExitReason::ProcessExit(5));
}

#[test]
fn a_hosted_cpp_program_holds_and_releases_references() {
    for level in LEVELS {
        let (exit, output) = run_at(&format!("modules-cpp-{level}"), 0x2_5000_0000);
        assert_eq!(exit, ExitReason::ProcessExit(0), "{level}");
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines, ["self 1", "held 1", "freed 1", "info 1"], "{level}");
    }
}

const IMPORTS: &[(&str, &str)] = &[
    ("KERNEL32.dll", "GetModuleHandleA"),
    ("KERNEL32.dll", "GetModuleHandleW"),
    ("KERNEL32.dll", "GetModuleHandleExA"),
    ("KERNEL32.dll", "GetModuleHandleExW"),
    ("KERNEL32.dll", "LoadLibraryA"),
    ("KERNEL32.dll", "FreeLibrary"),
    ("KERNEL32.dll", "GetProcAddress"),
    ("KERNEL32.dll", "K32GetModuleInformation"),
    ("PSAPI.dll", "GetModuleInformation"),
    ("KERNEL32.dll", "GetLastError"),
    ("KERNEL32.dll", "SetLastError"),
];
const PROCESS: u64 = u64::MAX;
const NAME: u64 = SCRATCH + 0x100;

fn guest(name: &[u8]) -> Guest {
    let mut guest = Guest::new(IMPORTS, &[]);
    guest.memory.write_bytes(NAME, name).unwrap();
    guest
}

fn error(guest: &mut Guest) -> u64 {
    guest.call("GetLastError", &[]) & 0xFFFF_FFFF
}

/// No image was started, so there is no program to name.
#[test]
fn without_a_program_image_a_null_name_is_not_found() {
    let mut guest = guest(b"kernel32\0");
    for symbol in ["GetModuleHandleA", "GetModuleHandleW"] {
        assert_eq!(guest.call(symbol, &[0]), 0);
        assert_eq!(error(&mut guest), 126);
    }
    assert_eq!(guest.call("GetModuleHandleExA", &[0, 0, SCRATCH]), 0);
    assert_eq!(error(&mut guest), 126);
    assert_eq!(guest.call("GetModuleHandleExA", &[4, 0x1234, SCRATCH]), 0);
    assert_eq!(error(&mut guest), 126);
}

#[test]
fn get_module_handle_ex_clears_the_handle_on_failure_and_needs_a_place_for_it() {
    for (flags, name, wanted) in [
        (0u64, b"missing\0\0\0", 126),
        (3, b"kernel32\0\0", 87),
        (7, b"kernel32\0\0", 87),
        (8, b"kernel32\0\0", 87),
        (0x8000_0000, b"kernel32\0\0", 87),
        (0, b"c:\\a.dll\0\0", 126),
    ] {
        let mut guest = guest(name);
        guest.memory.write_u64(SCRATCH, u64::MAX).unwrap();
        // The flags are a DWORD: bits above it are not read.
        let flags = flags | 0xDEAD_0000_0000_0000;
        assert_eq!(guest.call("GetModuleHandleExA", &[flags, NAME, SCRATCH]), 0);
        assert_eq!(error(&mut guest), wanted, "{flags:#x}");
        assert_eq!(guest.memory.read_u64(SCRATCH).unwrap(), 0);
    }
    let mut guest = guest(b"kernel32\0");
    let edge = SCRATCH + SCRATCH_LEN - 7;
    for (output, wanted) in [(0, 87), (0x9_9000, 998), (edge, 998)] {
        assert_eq!(guest.call("GetModuleHandleExA", &[0, NAME, output]), 0);
        assert_eq!(error(&mut guest), wanted, "{output:#x}");
    }
    let edge = SCRATCH + SCRATCH_LEN - 8;
    assert_eq!(guest.call("GetModuleHandleExA", &[0, NAME, edge]), 1);
    assert_ne!(guest.memory.read_u64(edge).unwrap(), 0);
    // A name that cannot be read stops the guest.
    for symbol in ["GetModuleHandleA", "GetModuleHandleW", "LoadLibraryA"] {
        let reason = guest.fails(symbol, &[0x9_9000]);
        assert!(matches!(reason, ExitReason::MemoryFault(_)), "{reason:?}");
    }
}

#[test]
fn references_are_counted_per_call_and_flag() {
    let mut guest = guest(b"UcrtBase.DLL\0");
    let handle = guest.call("GetModuleHandleA", &[NAME]);
    assert_ne!(handle, 0);
    // One reference is the startup binding's. Neither of these adds one.
    for _ in 0..5 {
        guest.call("GetModuleHandleExA", &[2, NAME, SCRATCH]);
        guest.call("GetModuleHandleExA", &[2 | 4, handle, SCRATCH]);
    }
    assert_eq!(guest.memory.read_u64(SCRATCH).unwrap(), handle);
    // These add one each.
    guest.call("GetModuleHandleExA", &[0, NAME, SCRATCH]);
    guest.call("GetModuleHandleExA", &[4, handle, SCRATCH]);
    guest.call("LoadLibraryA", &[NAME]);
    for _ in 0..4 {
        assert_eq!(guest.call("GetModuleHandleA", &[NAME]), handle);
        assert_eq!(guest.call("FreeLibrary", &[handle]), 1);
    }
    assert_eq!(guest.call("GetModuleHandleA", &[NAME]), 0);
    assert_eq!(guest.call("GetModuleHandleExA", &[4, handle, SCRATCH]), 0);
    assert_eq!(guest.call("FreeLibrary", &[handle]), 0);
    assert_eq!(error(&mut guest), 6);
    assert_eq!(guest.call("LoadLibraryA", &[NAME]), handle);
    // Pinning adds no reference to release.
    guest.call("GetModuleHandleExA", &[1, NAME, SCRATCH]);
    assert_eq!(guest.call("FreeLibrary", &[handle]), 1);
    assert_eq!(guest.call("FreeLibrary", &[handle]), 0);
    assert_eq!(guest.call("GetModuleHandleA", &[NAME]), handle);
}

#[test]
fn module_information_is_refused_for_anything_but_the_program() {
    for symbol in ["K32GetModuleInformation", "GetModuleInformation"] {
        let mut guest = guest(b"kernel32\0");
        let kernel = guest.call("GetModuleHandleA", &[NAME]);
        for (process, module, wanted) in [
            (PROCESS, kernel, 50),
            (PROCESS, 0, 6),
            (PROCESS, 0x1234_0000, 6),
            (0, kernel, 6),
            (0x44, kernel, 6),
        ] {
            guest.memory.write_u64(SCRATCH, 0xAAAA).unwrap();
            assert_eq!(guest.call(symbol, &[process, module, SCRATCH, 24]), 0);
            assert_eq!(
                error(&mut guest),
                wanted,
                "{symbol} {process:#x} {module:#x}"
            );
            assert_eq!(guest.memory.read_u64(SCRATCH).unwrap(), 0xAAAA);
        }
    }
}

/// Services registered after the catalog used to be missing from it.
#[test]
fn every_kernel32_service_that_is_not_a_stub_can_be_looked_up() {
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state());
    let mut guest = guest(b"kernel32\0");
    let kernel = guest.call("GetModuleHandleA", &[NAME]);
    let mut count = 0;
    for (module, symbol, status) in registry.iter_registered() {
        if module != "kernel32.dll" {
            continue;
        }
        let name = [symbol.as_bytes(), b"\0"].concat();
        guest.memory.write_bytes(NAME, &name).unwrap();
        let found = guest.call("GetProcAddress", &[kernel, NAME]) != 0;
        let stub = matches!(status, xenora_loader::imports::ServiceStatus::Stub);
        assert_eq!(found, !stub, "{symbol}");
        count += usize::from(found);
    }
    assert!(count > 100, "{count}");
    for symbol in [&b"ExitProcess\0"[..], b"TerminateProcess\0"] {
        guest.memory.write_bytes(NAME, symbol).unwrap();
        assert_ne!(guest.call("GetProcAddress", &[kernel, NAME]), 0);
    }
}

/// The locations Microsoft's pages list, and no similar ones.
#[test]
fn documented_locations_resolve_and_similar_ones_do_not() {
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state());
    let resolves = |module: &str, symbol: &str| {
        registry
            .resolve(module, &ImportName::Name(symbol.into()))
            .is_some()
    };
    let loader = |version: &str| format!("api-ms-win-core-libraryloader-l1-{version}.dll");
    for symbol in [
        "GetModuleHandleA",
        "GetModuleHandleW",
        "GetModuleHandleExA",
        "GetModuleHandleExW",
        "GetProcAddress",
        "FreeLibrary",
    ] {
        for version in ["1-0", "1-1", "2-0", "2-1", "2-2", "2-3"] {
            assert!(resolves(&loader(version), symbol), "{version} {symbol}");
        }
        for module in [
            "KernelBase.dll",
            "kernel32.dll",
            "api-ms-win-downlevel-kernel32-l1-1-0.dll",
        ] {
            assert!(resolves(module, symbol), "{module} {symbol}");
        }
        for module in [&loader("2-4"), &loader("1-2"), "vertdll.dll", "psapi.dll"] {
            assert!(!resolves(module, symbol), "{module} {symbol}");
        }
    }
    for symbol in ["LoadLibraryA", "LoadLibraryW"] {
        for version in ["2-1", "2-2", "2-3"] {
            assert!(resolves(&loader(version), symbol), "{version} {symbol}");
        }
        for version in 0..=6 {
            let module = format!("api-ms-win-core-kernel32-legacy-l1-1-{version}.dll");
            assert!(resolves(&module, symbol), "{module} {symbol}");
        }
        for module in ["KernelBase.dll", "api-ms-win-downlevel-kernel32-l2-1-0.dll"] {
            assert!(resolves(module, symbol), "{module} {symbol}");
        }
        for module in [
            &loader("1-0"),
            "api-ms-win-downlevel-kernel32-l1-1-0.dll",
            "api-ms-win-core-kernel32-legacy-l1-1-7.dll",
        ] {
            assert!(!resolves(module, symbol), "{module} {symbol}");
        }
    }
    for module in [
        "api-ms-win-core-psapi-l1-1-0.dll",
        "api-ms-win-core-psapi-obsolete-l1-1-0.dll",
    ] {
        assert!(resolves(module, "K32GetModuleInformation"));
        assert!(resolves(module, "GetModuleInformation"));
    }
    assert!(resolves("kernelbase.dll", "K32GetModuleInformation"));
    assert!(resolves("psapi.dll", "GetModuleInformation"));
    assert!(!resolves("psapi.dll", "K32GetModuleInformation"));
    assert!(!resolves("kernel32.dll", "GetModuleInformation"));
}
