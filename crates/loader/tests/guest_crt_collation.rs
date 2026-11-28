// SPDX-License-Identifier: GPL-3.0-or-later

//! Original hosted C text index, built with MinGW-w64 UCRT GCC on Apple Silicon.
//! Run `cargo test --locked --target aarch64-apple-darwin -p xenora-loader
//! --test guest_crt_collation`, or build both guest images with `sh
//! tests/compat_fixtures/build-guest-crt-collation.sh artifacts/crt-collation`.
//! The expected ordering and buffer limits follow Microsoft's [strcoll],
//! [strxfrm], [qsort], and [bsearch] contracts in the C locale.
//!
//! [strcoll]: https://learn.microsoft.com/en-us/cpp/c-runtime-library/strcoll-functions?view=msvc-170
//! [strxfrm]: https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/strxfrm-wcsxfrm-strxfrm-l-wcsxfrm-l?view=msvc-170
//! [qsort]: https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/qsort?view=msvc-170
//! [bsearch]: https://learn.microsoft.com/en-us/cpp/c-runtime-library/reference/bsearch?view=msvc-170

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{
    ExitReason, ImportRegistry, MockClock, ServicesState, register_standard_services,
};

#[test]
fn hosted_c_index_collates_transforms_sorts_and_searches_records() {
    let output = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("crt-collation-{}", std::process::id()));
    let builder = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/compat_fixtures/build-guest-crt-collation.sh");
    let build = Command::new("sh")
        .arg(&builder)
        .arg(&output)
        .output()
        .expect("build the original hosted C text index");
    assert!(
        build.status.success(),
        "{} failed:\n{}\n{}",
        builder.display(),
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    for optimization in ["O1", "O2"] {
        let image = format!("crt-collation-{optimization}.exe");
        let bytes = std::fs::read(output.join(&image)).expect("read the original hosted PE");
        let probe = probe_pe_bytes(&bytes, &ProbeConfig::default()).expect("inspect PE imports");
        assert_eq!(probe.import_error, None);
        for (family, symbol) in [
            ("string", "strcoll"),
            ("string", "strxfrm"),
            ("utility", "qsort"),
            ("utility", "bsearch"),
        ] {
            let dll = format!("api-ms-win-crt-{family}-l1-1-0.dll");
            assert!(
                probe.imports.iter().any(|import| {
                    import.dll.eq_ignore_ascii_case(&dll) && import.symbol == symbol
                }),
                "{image}: missing {dll}!{symbol}"
            );
        }
        let mut finished = Vec::new();
        for (mode, input_order) in [("initial", "forward"), ("replay", "reverse")] {
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
                ExitReason::ProcessExit(0),
                "{image} {mode}, stdout={:?}, stderr={:?}",
                String::from_utf8_lossy(&state.standard_output()),
                String::from_utf8_lossy(&state.standard_error())
            );
            assert!(result.dbt_metrics.native_bytes_emitted > 0);
            finished.push((state, input_order));
        }

        for (state, input_order) in finished {
            let expected = format!(
                "collation: 1 2 4 3 5 6\r\nlookup: six positions, two misses\r\ninput: {input_order}\r\n"
            );
            assert_eq!(state.exit_status(), Some(0));
            assert_eq!(state.standard_output(), expected.as_bytes());
            assert!(state.standard_error().is_empty());
            assert_eq!(state.open_handle_count(), 0);
        }
    }
}
