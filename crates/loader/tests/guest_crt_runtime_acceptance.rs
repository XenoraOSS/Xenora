// SPDX-License-Identifier: GPL-3.0-or-later

//! Original hosted C program built with native ARM64 MinGW-w64 targeting UCRT.
//! Reproduce with `cargo test --locked -p xenora-loader --test guest_crt_runtime_acceptance`.
//! Conversion expectations follow the Microsoft wcstombs contract and C11
//! draft N1570 §7.22.8.2. This fixture exercises Xenora's single-byte locale.
//!
//! [Microsoft wcstombs contract](https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/wcstombs-wcstombs-l)
//! [C11 draft](https://www.open-std.org/jtc1/sc22/wg14/www/docs/n1570.pdf)

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::scheduler::DEFAULT_QUANTUM;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup_scheduled};

const CONVERSION_OUTPUT: &str = "conversion: limits, terminator and EILSEQ\r\n";
const FAMILY_OUTPUT: &str = concat!(
    "runtime: heap, wide classes, locale and thread errno\r\n",
    "files: ENOENT, exact bytes and EOF\r\n",
);
const EXIT_OUTPUT: &str = "exit: second registration\r\nexit: first registration\r\n";

fn built() -> &'static Path {
    static OUTPUT: OnceLock<PathBuf> = OnceLock::new();
    OUTPUT.get_or_init(|| {
        let out = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("crt-runtime-acceptance-{}", std::process::id()));
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/compat_fixtures/build-crt-runtime-acceptance.sh"
        );
        let build = Command::new("sh")
            .arg(script)
            .arg(&out)
            .output()
            .expect("run the original C fixture builder");
        assert!(
            build.status.success(),
            "MinGW-w64 UCRT fixture build failed:\n{}\n{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        out
    })
}

fn run(name: &str, mode: Option<&str>, status: u32, output: &str, file: Option<&[u8]>) {
    let bytes = fs::read(built().join(format!("{name}.exe"))).expect("read original guest image");
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut arguments = vec![name];
    arguments.extend(mode);
    state.set_program_arguments(&arguments).unwrap();
    let mut imports = ImportRegistry::new();
    register_standard_services(&mut imports, Arc::clone(&state));
    let result = run_synthetic_startup_scheduled(
        &bytes,
        StartupLayout {
            image_base: 0x2_5000_0000,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        },
        imports,
        Arc::clone(&state),
        20_000_000,
        DEFAULT_QUANTUM,
        |_| {},
    )
    .unwrap_or_else(|error| panic!("{name} {mode:?}: {error}"));
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::ProcessExit(status),
        "{name} {mode:?}: stdout {:?}, stderr {:?}",
        String::from_utf8_lossy(&state.standard_output()),
        String::from_utf8_lossy(&state.standard_error())
    );
    assert_eq!(state.exit_status(), Some(status));
    assert_eq!(state.standard_output(), output.as_bytes());
    assert!(state.standard_error().is_empty());
    assert_eq!(
        state.file_contents("runtime-acceptance.txt").as_deref(),
        file
    );
    assert!(
        state
            .file_contents("missing-runtime-acceptance.txt")
            .is_none()
    );
    assert_eq!(state.open_handle_count(), 0);
    assert!(result.dbt_metrics.native_bytes_emitted > 0);
}

#[test]
fn wcstombs_stops_before_an_invalid_unit_after_capacity() {
    run(
        "crt-runtime-acceptance-O2",
        Some("conversion"),
        0,
        CONVERSION_OUTPUT,
        None,
    );
}

#[test]
fn compiled_c_exercises_supported_families_together() {
    let output = format!("{FAMILY_OUTPUT}{CONVERSION_OUTPUT}{EXIT_OUTPUT}");
    for executable in ["crt-runtime-acceptance-O0", "crt-runtime-acceptance-O2"] {
        run(executable, None, 0, &output, Some(b"A=17\n"));
    }
}

#[test]
fn incorrect_expectation_triggers_failure_and_reverse_exit_callbacks() {
    run(
        "crt-runtime-acceptance-control",
        None,
        34,
        EXIT_OUTPUT,
        None,
    );
}
