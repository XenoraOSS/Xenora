// SPDX-License-Identifier: GPL-3.0-or-later

//! Run `cargo test --locked --target aarch64-apple-darwin -p xenora-loader
//! --test guest_crt_records` on Apple Silicon with MinGW-w64 UCRT GCC.
//! This builds an original hosted C application. The expected records follow
//! the documented [CRT environment], [scanf], [modf] and [UTC calendar] rules.
//!
//! [CRT environment]: https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/getenv-s-wgetenv-s
//! [scanf]: https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/fscanf-fscanf-l-fwscanf-fwscanf-l
//! [modf]: https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/modf-modff-modfl
//! [UTC calendar]: https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/gmtime-gmtime32-gmtime64

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{
    ExitReason, ImportRegistry, MockClock, ServicesState, register_standard_services,
};

struct RecordCase {
    mode: &'static str,
    status: u32,
    stdout: &'static [u8],
    input: &'static [u8],
    report: Option<&'static [u8]>,
}

const CASES: [RecordCase; 3] = [
    RecordCase {
        mode: "initial",
        status: 0,
        stdout: b"records: alpha\r\n",
        input: b"1709164800 17.25\n",
        report: Some(b"alpha|2024-02-29 00:00:00|17.00|0.25\n"),
    },
    RecordCase {
        mode: "malformed",
        status: 23,
        stdout: b"records: rejected malformed input\r\n",
        input: b"invalid 17.25\n",
        report: None,
    },
    RecordCase {
        mode: "replay",
        status: 0,
        stdout: b"records: beta\r\n",
        input: b"1709164800 5.50\n",
        report: Some(b"beta|2024-02-29 00:00:00|5.00|0.50\n"),
    },
];

#[test]
fn hosted_c_records_combine_environment_scan_math_calendar_and_files() {
    let output =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("crt-records-{}", std::process::id()));
    let builder = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/compat_fixtures/build-guest-crt-records.sh");
    let build = Command::new("sh")
        .arg(&builder)
        .arg(&output)
        .output()
        .expect("build the original hosted C record processor");
    assert!(
        build.status.success(),
        "{} failed:\n{}\n{}",
        builder.display(),
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    for optimization in ["O1", "O2"] {
        let image = format!("crt-records-{optimization}.exe");
        let bytes = std::fs::read(output.join(&image)).expect("read the original hosted PE");
        let probe =
            probe_pe_bytes(&bytes, &ProbeConfig::default()).expect("inspect the PE imports");
        assert_eq!(probe.import_error, None);
        for (family, symbol) in [
            ("environment", "_putenv"),
            ("environment", "getenv_s"),
            ("stdio", "fopen"),
            ("stdio", "__stdio_common_vfscanf"),
            ("stdio", "__stdio_common_vfprintf"),
            ("math", "modf"),
            ("time", "_gmtime64"),
            ("time", "strftime"),
        ] {
            let dll = format!("api-ms-win-crt-{family}-l1-1-0.dll");
            assert!(
                probe.imports.iter().any(|import| {
                    import.dll.eq_ignore_ascii_case(&dll) && import.symbol == symbol
                }),
                "{image}: missing {dll}!{symbol}"
            );
        }

        let mut completed = Vec::new();
        for &RecordCase {
            mode,
            status,
            stdout,
            ..
        } in &CASES
        {
            let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
            state.set_program_arguments(&[&image, mode]).unwrap();
            let mut imports = ImportRegistry::new();
            register_standard_services(&mut imports, Arc::clone(&state));
            let result = run_synthetic_startup(
                &bytes,
                StartupLayout {
                    image_base: 0x2_5000_0000,
                    stack_top: 0x2_0001_0000,
                    tls_base: 0x3_0000_0000,
                },
                imports,
                20_000_000,
            )
            .unwrap_or_else(|error| panic!("{image} {mode}: {error}"));
            assert_eq!(
                result.execution.exit_reason,
                ExitReason::ProcessExit(status),
                "{image} {mode}, stdout={:?}, stderr={:?}",
                String::from_utf8_lossy(&state.standard_output()),
                String::from_utf8_lossy(&state.standard_error())
            );
            assert_eq!(state.exit_status(), Some(status));
            assert_eq!(state.standard_output(), stdout);
            assert!(state.standard_error().is_empty());
            assert!(result.dbt_metrics.native_bytes_emitted > 0);
            assert_eq!(state.open_handle_count(), 0);
            completed.push(state);
        }

        // Check all files with all states still held, after the replay wrote
        // the same paths. Neither the reject nor replay can change prior files.
        for (
            state,
            RecordCase {
                mode,
                input,
                report,
                ..
            },
        ) in completed.iter().zip(CASES)
        {
            assert_eq!(
                state.file_contents(r"C:\crt-record-input.txt").as_deref(),
                Some(input),
                "{image} {mode}: input bytes"
            );
            assert_eq!(
                state.file_contents(r"C:\crt-record-report.txt").as_deref(),
                report,
                "{image} {mode}: report bytes"
            );
        }
    }
}
