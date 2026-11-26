// SPDX-License-Identifier: GPL-3.0-or-later

use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const NOT_REGULAR: &str = "execution denied: a program must be a regular file";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "xenora-run-admission-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }

    fn run(&self, path: &Path, arguments: &[&str]) -> Output {
        let stdout = self.0.join("stdout");
        let stderr = self.0.join("stderr");
        // Files let the watchdog collect diagnostics even if a child worker
        // still holds its inherited descriptors when the supervisor is killed.
        let mut child = Command::new(env!("CARGO_BIN_EXE_xenora"))
            .args(["run", "--experimental", "--pe"])
            .arg(path)
            .args(["--startup-report", "--wall-seconds", "2"])
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(File::create(&stdout).unwrap())
            .stderr(File::create(&stderr).unwrap())
            .spawn()
            .expect("launch xenora run");
        let deadline = Instant::now() + Duration::from_secs(8);
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll xenora run") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                child.wait().expect("reap stalled xenora run");
                panic!("xenora run stalled on {}", path.display());
            }
            thread::sleep(Duration::from_millis(10));
        };
        Output {
            status,
            stdout: fs::read(stdout).unwrap(),
            stderr: fs::read(stderr).unwrap(),
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn failure_report(output: &Output) -> &str {
    let stderr = std::str::from_utf8(&output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(125), "{stderr}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let mut reports = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("xenora-startup "));
    let report = reports.next().expect("expected one startup report");
    assert!(reports.next().is_none(), "{stderr}");
    assert!(report.contains(r#""protocol":"complete""#), "{report}");
    report
}

#[test]
fn nonregular_programs_fail_admission_without_waiting_for_a_writer() {
    let scratch = Scratch::new();
    let fifo = scratch.0.join("program.fifo");
    let fifo_link = scratch.0.join("program.fifo.link");
    assert!(
        Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    symlink(&fifo, &fifo_link).unwrap();

    for (path, args) in [
        (fifo.as_path(), &[][..]),
        (fifo_link.as_path(), &["--sha256", &"0".repeat(64)][..]),
        (scratch.0.as_path(), &[][..]),
    ] {
        let output = scratch.run(path, args);
        let report = failure_report(&output);
        assert!(
            report.contains(r#""events":[],"last_completed_stage":null"#),
            "{report}"
        );
        assert!(
            report.contains(r#""kind":"known_failure","phase":"admission""#),
            "{report}"
        );
        assert!(report.contains(NOT_REGULAR), "{report}");
        assert!(
            std::str::from_utf8(&output.stderr)
                .unwrap()
                .lines()
                .any(|line| line == format!("run failed: {NOT_REGULAR}")),
            "{output:?}"
        );
    }
}

#[test]
fn regular_symlink_preserves_startup_and_digest_pinning() {
    let scratch = Scratch::new();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pe/delay-imports.exe");
    let link = scratch.0.join("program.exe");
    symlink(&fixture, &link).unwrap();

    let plain = scratch.run(&fixture, &[]);
    let expected = failure_report(&plain);
    assert!(
        expected.contains(r#""last_completed_stage":"relocated""#),
        "{expected}"
    );
    assert!(
        expected.contains(r#""kind":"known_failure","phase":"startup""#),
        "{expected}"
    );
    assert!(
        expected.contains("fixture-ordinary.dll!OrdinaryByName"),
        "{expected}"
    );
    assert_eq!(failure_report(&scratch.run(&link, &[])), expected);

    let digest = format!("{:x}", Sha256::digest(fs::read(&fixture).unwrap()));
    assert_eq!(
        failure_report(&scratch.run(&link, &["--sha256", &digest])),
        expected
    );
    let wrong_digest = scratch.run(&link, &["--sha256", &"0".repeat(64)]);
    let report = failure_report(&wrong_digest);
    assert!(
        report.contains(r#""kind":"known_failure","phase":"admission""#),
        "{report}"
    );
    assert!(report.contains("SHA-256 is not the one given"), "{report}");
}
