// SPDX-License-Identifier: GPL-3.0-or-later

//! Build the original MS x64 witnesses with
//! `sh fixtures/pe/msvc-exception-witness/build.sh artifacts/msvc-exception-witness`.
//! The typed throw compiles to COFF and retains its compiler-emitted reference
//! to runtime type data. The linked catch-all/rethrow image needs only function
//! imports and probes the loader's refusal. The normal-return image checks RAII
//! destructor order without invoking exception handling.
//! Run `cargo test --locked -p xenora-loader --test guest_msvc_exception_boundary`
//! on Apple Silicon. To inspect compiler evidence, pass `--symbols --relocations
//! --unwind` to llvm-readobj for typed-throw.obj and `--coff-imports --unwind`
//! for rethrow.exe.
//!
//! See [Microsoft x64 exception handling] and the [_CxxThrowException] contract.
//!
//! [Microsoft x64 exception handling]: https://learn.microsoft.com/en-us/cpp/build/exception-handling-x64
//! [_CxxThrowException]: https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/cxxthrowexception

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{
    StartupEvent, StartupLayout, run_synthetic_startup, run_synthetic_startup_with_observer,
};
use xenora_loader::{
    ExitReason, ImportError, ImportRegistry, ProbeConfig, ProbeReport, probe_pe_bytes,
};

const IMAGE_BASE: u64 = 0x1_4000_0000;
const RUNTIME: &str = "vcruntime140.dll";
const THROW: &str = "_CxxThrowException";
const HANDLER: &str = "__CxxFrameHandler3";

fn fixture(name: &str) -> Vec<u8> {
    static OUTPUT: OnceLock<PathBuf> = OnceLock::new();
    let directory = OUTPUT.get_or_init(|| {
        let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("msvc-exception-witness-{}", std::process::id()));
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/pe/msvc-exception-witness/build.sh");
        let build = Command::new("sh")
            .arg(&script)
            .arg(&directory)
            .output()
            .expect("run the Microsoft C++ exception witness builder");
        assert!(
            build.status.success(),
            "{} failed:\n{}{}",
            script.display(),
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        directory
    });
    fs::read(directory.join(name)).unwrap_or_else(|error| panic!("read {name}: {error}"))
}

fn layout() -> StartupLayout {
    StartupLayout {
        image_base: IMAGE_BASE,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    }
}

/// Read the public AMD64 RUNTIME_FUNCTION record covering the entry point.
/// Its third field is the RVA of UNWIND_INFO, whose first byte holds version
/// and flags. This fixture leaves language-specific handler data uninterpreted.
fn entry_unwind_flags(bytes: &[u8], probe: &ProbeReport) -> u8 {
    let pe = &probe.parsed;
    let pdata = pe
        .sections
        .iter()
        .find(|section| section.name == ".pdata")
        .expect("compiler-emitted .pdata section");
    let start = pdata.raw_data_ptr as usize;
    let end = start + pdata.virtual_size as usize;
    let (records, trailing) = bytes[start..end].as_chunks::<12>();
    assert!(trailing.is_empty(), "complete RUNTIME_FUNCTION records");
    let entry_record = records
        .iter()
        .find(|record| {
            let begin = u32::from_le_bytes(record[0..4].try_into().unwrap());
            let end = u32::from_le_bytes(record[4..8].try_into().unwrap());
            (begin..end).contains(&pe.entry_point_rva)
        })
        .expect("runtime function covering the entry point");
    let rva = u32::from_le_bytes(entry_record[8..12].try_into().unwrap());
    let offset = pe.rva_to_file_offset(rva).expect("mapped UNWIND_INFO");
    assert_eq!(bytes[offset] & 7, 1, "x64 unwind version");
    bytes[offset] >> 3
}

#[test]
fn normal_scope_exit_destroys_inner_before_outer() {
    let bytes = fixture("normal.exe");
    let probe = probe_pe_bytes(&bytes, &ProbeConfig::default()).unwrap();
    assert!(probe.import_error.is_none());
    assert_eq!(probe.imports.len(), 1);
    assert_eq!(probe.imports[0].dll, "kernel32.dll");
    assert_eq!(probe.imports[0].symbol, "ExitProcess");
    assert_eq!(entry_unwind_flags(&bytes, &probe), 0);

    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let result = run_synthetic_startup(&bytes, layout(), registry, 200_000).unwrap();
    assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
    assert_eq!(state.exit_status(), Some(0));
    assert_eq!(state.open_handle_count(), 0);
    assert!(result.dbt_metrics.native_bytes_emitted > 0);
    let data = result
        .image
        .pe
        .sections
        .iter()
        .find(|section| section.name == ".gate")
        .expect("destructor observation section");
    assert_eq!(
        result
            .image
            .memory
            .read_u32(IMAGE_BASE + u64::from(data.virtual_address))
            .unwrap(),
        12
    );
}

#[test]
fn compiler_emitted_cxx_exception_imports_require_unsupported_runtime_before_entry() {
    let bytes = fixture("rethrow.exe");
    let probe = probe_pe_bytes(&bytes, &ProbeConfig::default()).unwrap();
    assert!(probe.import_error.is_none());
    assert_eq!(
        probe
            .imports
            .iter()
            .map(|entry| (
                entry.dll.as_str(),
                entry.symbol.as_str(),
                entry.is_supported
            ))
            .collect::<Vec<_>>(),
        [
            ("kernel32.dll", "ExitProcess", true),
            (RUNTIME, THROW, false),
            (RUNTIME, HANDLER, false),
        ]
    );
    const UNW_FLAG_EHANDLER: u8 = 1;
    const UNW_FLAG_UHANDLER: u8 = 2;
    assert_eq!(
        entry_unwind_flags(&bytes, &probe),
        UNW_FLAG_EHANDLER | UNW_FLAG_UHANDLER
    );

    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let error =
        match run_synthetic_startup_with_observer(&bytes, layout(), registry, 200_000, |event| {
            assert!(
                !matches!(
                    event,
                    StartupEvent::ImportsBound { .. }
                        | StartupEvent::TlsCompleted { .. }
                        | StartupEvent::Entering { .. }
                ),
                "startup ran past the unresolved C++ exception import"
            )
        }) {
            Err(error) => error,
            Ok(_) => panic!("startup accepted unresolved C++ exception imports"),
        };
    assert_eq!(
        error.downcast_ref::<ImportError>(),
        Some(&ImportError::UnresolvedImport {
            module: RUNTIME.to_owned(),
            symbol: THROW.to_owned(),
        })
    );
    assert_eq!(state.exit_status(), None);
    assert_eq!(state.open_handle_count(), 0);
}
