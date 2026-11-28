// SPDX-License-Identifier: GPL-3.0-or-later

//! The original multi-DLL package through the real `xenora run` command.
//!
//! The images are the independently authored fixture in
//! `fixtures/pe/multidll/`. The loader test derives the expected guest status
//! from the fixture's source; this test checks that the ordinary runner takes
//! `--package`, binds the package and returns that status.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}/../../fixtures/pe/multidll/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
}

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xenora"))
        .arg("run")
        .args(arguments)
        .output()
        .expect("run xenora")
}

fn name(path: &Path) -> &str {
    path.to_str().expect("UTF-8 fixture path")
}

fn startup_report(stderr: &[u8]) -> String {
    String::from_utf8(stderr.to_vec())
        .expect("startup diagnostics are UTF-8")
        .lines()
        .find(|line| line.starts_with("xenora-startup "))
        .expect("startup report line")
        .to_owned()
}

#[test]
fn the_original_multi_dll_package_runs_through_the_normal_runner() {
    let output = run(&[
        "--experimental",
        "--pe",
        name(&fixture("multidll-consumer.exe")),
        "--package",
        name(&fixture("xenora_chain.dll")),
        "--package",
        name(&fixture("xenora_math.dll")),
    ]);
    assert_eq!(
        output.status.code(),
        Some(70),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"");
}

#[test]
fn a_package_missing_a_provider_is_refused_by_the_runner() {
    let output = run(&[
        "--experimental",
        "--pe",
        name(&fixture("multidll-consumer.exe")),
        "--package",
        name(&fixture("xenora_math.dll")),
    ]);
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(output.stdout, b"");
    let stderr = String::from_utf8(output.stderr).expect("diagnostics");
    assert!(stderr.contains("missing-module=2"), "{stderr}");
}

#[test]
fn the_mapped_image_budget_still_bounds_a_package() {
    let output = run(&[
        "--experimental",
        "--max-mapped-image-bytes",
        "1024",
        "--pe",
        name(&fixture("multidll-consumer.exe")),
        "--package",
        name(&fixture("xenora_chain.dll")),
        "--package",
        name(&fixture("xenora_math.dll")),
    ]);
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(output.stdout, b"");
    let stderr = String::from_utf8(output.stderr).expect("diagnostics");
    assert!(
        stderr.starts_with("run failed: image 0 was not placed:"),
        "{stderr}"
    );
}

#[test]
fn a_package_module_must_be_a_regular_file() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pe/multidll");
    let output = run(&[
        "--experimental",
        "--pe",
        name(&fixture("multidll-consumer.exe")),
        "--package",
        name(&directory),
    ]);
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(output.stdout, b"");
    let stderr = String::from_utf8(output.stderr).expect("diagnostics");
    assert!(
        stderr.contains("a program must be a regular file"),
        "{stderr}"
    );
}

#[test]
fn a_normal_package_exit_reports_completed_startup_phases() {
    let output = run(&[
        "--experimental",
        "--startup-report",
        "--pe",
        name(&fixture("multidll-consumer.exe")),
        "--package",
        name(&fixture("xenora_chain.dll")),
        "--package",
        name(&fixture("xenora_math.dll")),
    ]);
    assert_eq!(output.status.code(), Some(70));
    let report = startup_report(&output.stderr);
    assert!(
        report.contains(
            r#""events":[{"stage":"parsed","completed":true},{"stage":"mapped","completed":true},{"stage":"relocated","completed":true},{"stage":"imports_bound","completed":true},{"stage":"tls_completed","completed":true},{"stage":"entry_started","completed":false},{"stage":"entry_returned","completed":true}]"#
        ),
        "{report}"
    );
    assert!(report.contains(r#""protocol":"complete""#), "{report}");
    assert!(
        report.contains(r#""kind":"guest_exit","guest_status":70,"worker_code":70"#),
        "{report}"
    );
}

#[test]
fn a_dll_refusing_attach_reports_failure_before_tls_completion_or_entry() {
    let output = run(&[
        "--experimental",
        "--startup-report",
        "--pe",
        name(&fixture("multidll-consumer.exe")),
        "--package",
        name(&fixture("refusing/xenora_chain.dll")),
        "--package",
        name(&fixture("xenora_math.dll")),
    ]);
    assert_eq!(output.status.code(), Some(125));
    let report = startup_report(&output.stderr);
    assert!(
        report.contains(r#""last_completed_stage":"imports_bound""#),
        "{report}"
    );
    assert!(!report.contains("tls_completed"), "{report}");
    assert!(!report.contains("entry_started"), "{report}");
    assert!(
        report.contains(r#""kind":"known_failure","phase":"startup""#)
            && report.contains("DllMain of xenora_chain.dll returned FALSE"),
        "{report}"
    );
}

#[test]
fn a_dll_exiting_before_root_entry_is_a_guest_exit_not_a_protocol_error() {
    let output = run(&[
        "--experimental",
        "--startup-report",
        "--pe",
        name(&fixture("multidll-consumer.exe")),
        "--package",
        name(&fixture("exiting/xenora_chain.dll")),
        "--package",
        name(&fixture("xenora_math.dll")),
    ]);
    assert_eq!(output.status.code(), Some(33));
    let report = startup_report(&output.stderr);
    assert!(
        report.contains(r#""last_completed_stage":"imports_bound""#),
        "{report}"
    );
    assert!(!report.contains("tls_completed"), "{report}");
    assert!(!report.contains("entry_started"), "{report}");
    assert!(
        report.contains(r#""protocol":"complete""#)
            && report.contains(r#""kind":"guest_exit","guest_status":33,"worker_code":33"#),
        "{report}"
    );
}

#[test]
fn dll_root_is_rejected_before_package_initializers() {
    // Used as a valid companion above, this DLL exits 33 during its attach.
    // If selected as the program root, it must never have that opportunity.
    let output = run(&[
        "--experimental",
        "--startup-report",
        "--pe",
        name(&fixture("exiting/xenora_chain.dll")),
        "--package",
        name(&fixture("xenora_math.dll")),
    ]);
    assert_eq!(
        output.status.code(),
        Some(125),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(
        startup_report(&output.stderr),
        concat!(
            r#"xenora-startup {"schema_version":1,"events":[],"last_completed_stage":null,"protocol":"complete","outcome":"#,
            r#"{"kind":"known_failure","phase":"admission","message":"execution denied: program root must be a PE executable","message_truncated":false,"worker_code":125}}"#,
        )
    );
}

#[test]
fn a_post_entry_stop_reports_the_entry_boundary_before_execution_failure() {
    let output = run(&[
        "--experimental",
        "--startup-report",
        "--max-steps",
        "1",
        "--pe",
        name(&fixture("multidll-consumer.exe")),
        "--package",
        name(&fixture("xenora_chain.dll")),
        "--package",
        name(&fixture("xenora_math.dll")),
    ]);
    assert_eq!(output.status.code(), Some(125));
    let report = startup_report(&output.stderr);
    assert!(
        report.contains(r#""stage":"entry_started","completed":false"#),
        "{report}"
    );
    assert!(
        report.contains(r#""stage":"entry_returned","completed":true"#),
        "{report}"
    );
    assert!(
        report.contains(r#""kind":"known_failure","phase":"execution""#)
            && report.contains("StepLimitExceeded"),
        "{report}"
    );
}
