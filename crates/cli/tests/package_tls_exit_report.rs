// SPDX-License-Identifier: GPL-3.0-or-later

//! The original Gate A1 TLS fixture exits before its process entry. An
//! unrelated original DLL selects package startup without preceding the first
//! callback with any module initialization evidence.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

#[test]
fn package_reports_exit_during_the_first_tls_callback() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("package-tls-exit-report-{}", std::process::id()));
    let build = Command::new("sh")
        .arg(root.join("tests/compat_fixtures/build-gate-a1.sh"))
        .arg(&out)
        .output()
        .expect("build the original Gate A1 fixture");
    assert!(
        build.status.success(),
        "Gate A1 fixture build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let executable = out.join("gate-a1-tls-exit.exe");
    let companion = root.join("fixtures/pe/package-scene/xenora_scene.dll");

    for (packaged, last_stage) in [(false, "tls_completed"), (true, "imports_bound")] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xenora"));
        command
            .args(["run", "--experimental", "--startup-report", "--pe"])
            .arg(&executable)
            .stdin(Stdio::null());
        if packaged {
            command.arg("--package").arg(&companion);
        }
        let output = command.output().expect("run xenora");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(73), "{stderr}");
        assert!(output.stdout.is_empty(), "{:?}", output.stdout);
        let mut reports = stderr
            .lines()
            .filter_map(|line| line.strip_prefix("xenora-startup "));
        let report = reports.next().expect("a startup report");
        assert!(reports.next().is_none(), "{stderr}");
        assert!(
            report.contains(r#""protocol":"complete","outcome":{"kind":"guest_exit","guest_status":73,"worker_code":73}"#),
            "packaged={packaged}: {report}"
        );
        assert!(
            report.contains(&format!(r#""last_completed_stage":"{last_stage}""#)),
            "packaged={packaged}: {report}"
        );
        assert_eq!(report.contains("tls_completed"), !packaged, "{report}");
        assert!(!report.contains("entry_started"), "{report}");
        assert!(!report.contains("entry_returned"), "{report}");
    }

    fs::remove_dir_all(out).unwrap();
}
