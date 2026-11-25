// SPDX-License-Identifier: GPL-3.0-or-later

//! `betterxen imports coverage` is deterministic and agrees with the registry.

use std::process::Command;

fn report(name: &str) -> String {
    let path =
        std::env::temp_dir().join(format!("xenora-coverage-{}-{name}.md", std::process::id()));
    let status = Command::new(env!("CARGO_BIN_EXE_betterxen"))
        .args(["imports", "coverage", "--out"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    text
}

fn count(report: &str, measure: &str) -> usize {
    let prefix = format!("| {measure} | ");
    let line = report
        .lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no {measure} row"));
    line[prefix.len()..].trim_end_matches(" |").parse().unwrap()
}

#[test]
fn coverage_is_deterministic_and_consistent() {
    let first = report("a");
    assert_eq!(first, report("b"), "two runs differ");

    let services = count(&first, "Services");
    assert!(services > 100, "{services}");
    assert_eq!(
        services,
        count(&first, "Implemented") + count(&first, "Partial") + count(&first, "Stub")
    );
    // The import generator skips adapters for these, so none may be missing.
    assert_eq!(
        count(&first, "Metadata expects a service that is missing"),
        0
    );
    for row in [
        "| `xinput1_4.dll` |",
        "| `vcruntime140.dll` |",
        "| `ws2_32.dll` |",
    ] {
        assert!(first.contains(row), "{row}");
    }
}

#[test]
fn coverage_rejects_unknown_arguments() {
    let status = Command::new(env!("CARGO_BIN_EXE_betterxen"))
        .args(["imports", "coverage", "--bogus"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}
