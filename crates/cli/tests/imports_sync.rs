// SPDX-License-Identifier: GPL-3.0-or-later

//! `betterxen imports sync` over a probe report that lists delay-loaded imports.

use std::process::Command;

#[test]
fn backlog_leaves_out_delay_loaded_imports() {
    // `fixtures/pe/delay-imports.py` writes two ordinary imports from
    // `fixture-ordinary.dll` and three delay-loaded ones from two other DLLs.
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/pe/delay-imports.exe"
    );
    let report = std::env::temp_dir().join(format!("xenora-sync-delayed-{}", std::process::id()));
    let betterxen = |arguments: &[&str]| {
        let status = Command::new(env!("CARGO_BIN_EXE_betterxen"))
            .args(arguments)
            .arg("--report")
            .arg(&report)
            .status()
            .unwrap();
        assert!(status.success(), "{arguments:?}");
    };
    betterxen(&["probe", "--pe", fixture]);
    betterxen(&["imports", "sync"]);
    let listed = std::fs::read_to_string(report.join("imports.csv")).unwrap();
    let backlog = std::fs::read_to_string(report.join("import_backlog.md")).unwrap();
    std::fs::remove_dir_all(report).unwrap();

    assert_eq!(listed.matches(",delayed,").count(), 3);
    assert!(backlog.contains("| **Total** | 2 |"), "{backlog}");
    assert!(backlog.contains("fixture-ordinary.dll"), "{backlog}");
    assert!(!backlog.contains("fixture-delay"), "{backlog}");
}
