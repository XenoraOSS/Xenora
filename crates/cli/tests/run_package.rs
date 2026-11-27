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
