// SPDX-License-Identifier: GPL-3.0-or-later

//! Run `cargo test --locked --target aarch64-apple-darwin -p xenora
//! --test runtime_load_library_boundary` on Apple Silicon. The test builds
//! the provider and both original callers with native Clang and LLD.
//!
//! The provisioning path leaves the provider unavailable. Explicit package
//! startup uses the same DLL and executes process attach before the export.
//! The missing-provider run checks the exact unresolved import before entry.
//! A fresh package run reproduces the exact attach, export and result markers.
//!
//! Build the standalone images with `sh fixtures/pe/runtime-load-library-boundary/build.sh
//! artifacts/runtime-load-library-boundary`. The fixture supplies a baseline
//! for dynamic loading in [issue #923]. The Windows contracts describe
//! [LoadLibraryA], [GetModuleHandleA] and [DllMain].
//!
//! [issue #923]: https://github.com/ranveersoni98/Xenora/issues/923
//! [LoadLibraryA]: https://learn.microsoft.com/en-us/windows/win32/api/libloaderapi/nf-libloaderapi-loadlibrarya
//! [GetModuleHandleA]: https://learn.microsoft.com/en-us/windows/win32/api/libloaderapi/nf-libloaderapi-getmodulehandlea
//! [DllMain]: https://learn.microsoft.com/en-us/windows/win32/dlls/dllmain

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVIDER: &str = "xenora_runtime_boundary.dll";
const PACKAGED_OUTPUT: &[u8] =
    b"provider: attach\nprovider: export\nconsumer: package export returned 42\n";

fn run(executable: &Path, options: &[OsString]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xenora"))
        .args([
            "run",
            "--experimental",
            "--trace",
            "--startup-report",
            "--max-steps",
            "200000",
            "--wall-seconds",
            "5",
            "--pe",
        ])
        .arg(executable)
        .args(options)
        .output()
        .expect("run the original guest through the Xenora CLI")
}

fn stderr(result: &Output) -> &str {
    std::str::from_utf8(&result.stderr).expect("the CLI emits UTF-8 diagnostics")
}

fn assert_packaged_success(result: &Output) {
    let trace = stderr(result);
    assert_eq!(result.status.code(), Some(0), "{trace}");
    assert_eq!(result.stdout, PACKAGED_OUTPUT);
    assert!(
        trace.contains("trace package modules=2 entry=packaged.exe"),
        "{trace}"
    );
    assert!(
        trace.contains(r#""last_completed_stage":"entry_returned""#),
        "{trace}"
    );
}

#[test]
fn provisioning_does_not_publish_a_dll_that_explicit_packaging_attaches() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pe/runtime-load-library-boundary/build.sh");
    let output = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "runtime-load-library-boundary-{}",
        std::process::id()
    ));
    let build = Command::new("sh")
        .arg(&fixture)
        .arg(&output)
        .output()
        .expect("build the original CLI fixture with native tools");
    assert!(build.status.success(), "{}", stderr(&build));

    let provider = output.join(PROVIDER);
    let provider_bytes = std::fs::metadata(&provider).unwrap().len();
    let mut provisioning = OsString::from(format!("{PROVIDER}="));
    provisioning.push(&provider);
    let provisioned = run(
        &output.join("provisioned.exe"),
        &["--module".into(), provisioning],
    );
    let trace = stderr(&provisioned);
    assert_eq!(provisioned.status.code(), Some(0), "{trace}");
    assert_eq!(
        provisioned.stdout,
        b"consumer: provisioned DLL unavailable\n"
    );
    assert!(
        trace.lines().any(|line| {
            line == format!("trace provisioned-modules=1 store-bytes={provider_bytes}")
        }),
        "{trace}"
    );
    assert!(
        trace.contains(r#""last_completed_stage":"entry_returned""#),
        "{trace}"
    );

    let packaged = output.join("packaged.exe");
    let package = ["--package".into(), provider.into_os_string()];
    assert_packaged_success(&run(&packaged, &package));

    let missing = run(&packaged, &[]);
    let trace = stderr(&missing);
    assert_eq!(missing.status.code(), Some(125), "{trace}");
    assert!(missing.stdout.is_empty(), "{:?}", missing.stdout);
    assert!(
        trace.contains("unresolved import dependency: xenora_runtime_boundary.dll!BoundaryValue"),
        "{trace}"
    );
    assert!(
        trace.contains(r#""last_completed_stage":"relocated""#),
        "{trace}"
    );
    assert!(
        trace.contains(r#""kind":"known_failure","phase":"startup""#),
        "{trace}"
    );
    assert!(!trace.contains(r#""stage":"entry_started""#), "{trace}");

    assert_packaged_success(&run(&packaged, &package));
}
