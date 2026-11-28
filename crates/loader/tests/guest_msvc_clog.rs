// SPDX-License-Identifier: GPL-3.0-or-later

//! Source-built Microsoft x64 ABI probe for the synthetic narrow log stream.
//! The expected associations and last-Init flush come from [narrow.stream.objects]
//! and [ios.init]. The object probes use the existing Xenora state cell.
//!
//! [narrow.stream.objects]: https://eel.is/c++draft/narrow.stream.objects
//! [ios.init]: https://eel.is/c++draft/ios.init

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::memory::MemoryError;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, StartupResult, run_synthetic_startup};

const IMAGE_BASE: u64 = 0x1_4000_0000;
const STATE_OFFSET: u64 = 16;
const BADBIT: u32 = 4;

fn built() -> &'static Path {
    static OUTPUT: OnceLock<PathBuf> = OnceLock::new();
    OUTPUT.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let output = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("msvc-clog-{}", std::process::id()));
        let build = Command::new("sh")
            .arg(root.join("fixtures/pe/build-msvc-clog.sh"))
            .arg(&output)
            .output()
            .expect("run the native Clang and LLD fixture builder");
        assert!(
            build.status.success(),
            "msvc-clog fixture build failed:\n{}\n{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        output
    })
}

fn run(name: &str) -> (Arc<ServicesState>, StartupResult) {
    let bytes = fs::read(built().join(format!("{name}.exe"))).expect("read the compiled guest");
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let result = run_synthetic_startup(
        &bytes,
        StartupLayout {
            image_base: IMAGE_BASE,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        },
        registry,
        2_000_000,
    )
    .unwrap_or_else(|error| panic!("{name}: {error}"));
    assert!(result.dbt_metrics.native_bytes_emitted > 0, "{name}");
    (state, result)
}

fn report(result: &StartupResult) -> (u64, u64, u32) {
    let image = &result.image;
    let section = image
        .pe
        .sections
        .iter()
        .find(|section| section.name == ".gate")
        .expect("fixture report section");
    let address = IMAGE_BASE + u64::from(section.virtual_address);
    let out = image.memory.read_u64(address).unwrap();
    let log = image.memory.read_u64(address + 8).unwrap();
    let phase = image.memory.read_u32(address + 16).unwrap();
    assert_ne!(out, log);
    (out, log, phase)
}

#[test]
fn clog_uses_stderr_and_has_state_separate_from_cout() {
    let (state, result) = run("msvc-clog");
    assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
    assert_eq!(state.standard_output(), b"out\r\nout still good\r\n");
    assert_eq!(state.standard_error(), b"log\r\n");
    let (out, log, phase) = report(&result);
    assert_eq!(phase, 2);
    assert_eq!(result.image.memory.read_u32(out + STATE_OFFSET).unwrap(), 0);
    assert_eq!(
        result.image.memory.read_u32(log + STATE_OFFSET).unwrap(),
        BADBIT
    );
}

#[test]
fn oversized_clog_write_preserves_both_outputs_and_state_words() {
    let (state, result) = run("msvc-clog-oversize");
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::IllegalInstruction(0xFE)
    );
    assert_eq!(state.standard_output(), b"out\r\n");
    assert_eq!(state.standard_error(), b"log\r\n");
    let (out, log, phase) = report(&result);
    assert_eq!(phase, 0);
    assert_eq!(result.image.memory.read_u32(out + STATE_OFFSET).unwrap(), 0);
    assert_eq!(result.image.memory.read_u32(log + STATE_OFFSET).unwrap(), 0);
}

#[test]
fn the_last_init_destructor_flushes_clog_before_cout() {
    let (state, result) = run("msvc-clog-final-flush");
    let (out, log, phase) = report(&result);
    assert_eq!(phase, 1, "the inner destructor must complete");
    assert!(result.image.memory.read_u32(out + STATE_OFFSET).is_err());
    assert!(result.image.memory.read_u32(log + STATE_OFFSET).is_err());
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::MemoryFault(MemoryError::UnmappedAddress(log + STATE_OFFSET))
    );
    assert_eq!(state.standard_output(), b"out\r\n");
    assert_eq!(state.standard_error(), b"log\r\n");
    assert_eq!(state.exit_status(), None);
}
