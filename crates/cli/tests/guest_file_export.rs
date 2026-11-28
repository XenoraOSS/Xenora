// SPDX-License-Identifier: GPL-3.0-or-later

//! Export only completed, explicitly selected files from a synthetic guest run.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const BINARY: &str = r"C:\guest-export.bin";
const EMPTY: &str = r"C:\empty-export.bin";
const MISSING: &str = r"C:\absent-export.bin";
const PAYLOAD: &[u8] = &[0, 0x41, 0x0a, 0x0d, 0, 0xff, 0x80];
const READY: &[u8] = b"guest files ready\r\n";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "guest-export-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }

    fn assert_clean(&self) {
        assert!(
            !has_stage(&self.0),
            "export left private staging data in {}",
            self.0.display()
        );
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn has_stage(parent: &Path) -> bool {
    fs::read_dir(parent).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .as_encoded_bytes()
            .starts_with(b".xenora-export-")
    })
}

fn fixture() -> &'static Path {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("guest-export-fixture-{}", std::process::id()));
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/compat_fixtures/build-guest-file-export.sh");
        let build = Command::new("sh").arg(&script).arg(&dir).output().unwrap();
        assert_status(&build, 0);
        dir.join("guest-file-export.exe")
    })
}

fn command(
    scratch: &Scratch,
    export: Option<(&str, &Path)>,
    options: &[&str],
    guest: &[&str],
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xenora"));
    command
        .current_dir(&scratch.0)
        .args(["run", "--experimental", "--pe"])
        .arg(fixture())
        .args(options);
    if let Some((source, destination)) = export {
        command
            .arg("--export-guest-file")
            .arg(source)
            .arg(destination);
    }
    command.arg("--").args(guest);
    command
}

fn assert_status(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn report(output: &Output) -> &str {
    let stderr = std::str::from_utf8(&output.stderr).unwrap();
    let mut reports = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("xenora-startup "));
    let json = reports
        .next()
        .unwrap_or_else(|| panic!("no report: {stderr}"));
    assert!(reports.next().is_none(), "multiple reports: {stderr}");
    json
}

#[test]
fn exports_exact_binary_and_empty_files() {
    for (source, bytes) in [
        (BINARY, PAYLOAD),
        ("c:/GUEST-EXPORT.BIN", PAYLOAD),
        ("GUEST-EXPORT.BIN", PAYLOAD),
        (r"\GUEST-EXPORT.BIN", PAYLOAD),
        ("/GUEST-EXPORT.BIN", PAYLOAD),
        (r"C:\.\guest-export.bin", PAYLOAD),
        (r"C:\unused\..\guest-export.bin", PAYLOAD),
        (r"unused\..\guest-export.bin", PAYLOAD),
        (r"\.\guest-export.bin", PAYLOAD),
        (r"c:/unused\.././GUEST-EXPORT.BIN", PAYLOAD),
        (r"C:\unused\..\..\guest-export.bin", PAYLOAD),
        (EMPTY, &[][..]),
    ] {
        let scratch = Scratch::new();
        let destination = Path::new("export result.bin");
        let output = command(
            &scratch,
            Some((source, destination)),
            &["--startup-report"],
            &[],
        )
        .output()
        .unwrap();
        assert_status(&output, 0);
        assert_eq!(output.stdout, READY);
        let json = report(&output);
        assert!(
            json.contains(
                r#""protocol":"complete","outcome":{"kind":"guest_exit","guest_status":0"#
            ),
            "{json}"
        );
        let destination = scratch.0.join(destination);
        assert!(fs::metadata(&destination).unwrap().is_file());
        assert_eq!(fs::read(destination).unwrap(), bytes, "{source}");
        scratch.assert_clean();
    }
}

#[test]
fn no_option_leaves_guest_files_on_the_synthetic_drive() {
    let scratch = Scratch::new();
    let output = command(&scratch, None, &[], &[]).output().unwrap();
    assert_status(&output, 0);
    assert_eq!(output.stdout, READY);
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
}

#[test]
fn missing_guest_file_reports_a_verification_failure_without_an_export() {
    for source in [
        MISSING,
        r"\\guest-export.bin",
        "//guest-export.bin",
        r"\/guest-export.bin",
        r"/\guest-export.bin",
        r"\C:\guest-export.bin",
        r"\\unused\..\guest-export.bin",
        r"\/unused\..\guest-export.bin",
        r"C:\\unused\..\guest-export.bin",
        r"D:\..\guest-export.bin",
        r"C:unused\..\guest-export.bin",
        r"C:\unused:stream\..\guest-export.bin",
        r".\C:\guest-export.bin",
        r"C:\unused\\..\guest-export.bin",
        r"C:\guest-export.bin\",
    ] {
        let scratch = Scratch::new();
        let destination = scratch.0.join("missing.bin");
        let output = command(
            &scratch,
            Some((source, &destination)),
            &["--startup-report"],
            &[],
        )
        .output()
        .unwrap();
        assert_status(&output, 125);
        assert_eq!(output.stdout, READY);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("guest file {source:?} does not exist")),
            "{stderr}"
        );
        let json = report(&output);
        assert!(json.contains(r#""protocol":"complete""#), "{json}");
        assert!(
            json.contains(r#""kind":"known_failure","phase":"verification""#),
            "{json}"
        );
        assert!(!destination.exists());
        scratch.assert_clean();
    }
}

#[test]
fn an_unsuccessful_guest_never_exports_files_it_already_created() {
    for (options, mode, status, expected_outcome) in [
        (
            &["--startup-report"][..],
            "fail",
            1,
            r#""kind":"guest_exit","guest_status":256"#,
        ),
        (
            &["--startup-report", "--wall-seconds", "1"][..],
            "spin",
            124,
            r#""kind":"wall_timeout""#,
        ),
        (
            &["--startup-report", "--max-steps", "100000"][..],
            "spin",
            125,
            r#""kind":"known_failure","phase":"execution""#,
        ),
    ] {
        let scratch = Scratch::new();
        let destination = scratch.0.join("must not exist");
        let output = command(&scratch, Some((BINARY, &destination)), options, &[mode])
            .output()
            .unwrap();
        assert_status(&output, status);
        assert_eq!(
            output.stdout, READY,
            "the guest must create the files before failing"
        );
        let json = report(&output);
        assert!(json.contains(expected_outcome), "{json}");
        assert!(!destination.exists());
        scratch.assert_clean();
    }
}

#[test]
fn preexisting_regular_files_and_symlinks_are_left_intact() {
    for is_symlink in [false, true] {
        let scratch = Scratch::new();
        let regular = scratch.0.join("original");
        let sentinel = b"prior host bytes";
        fs::write(&regular, sentinel).unwrap();
        let destination = if is_symlink {
            let link = scratch.0.join("link");
            symlink(&regular, &link).unwrap();
            link
        } else {
            regular.clone()
        };

        let output = command(
            &scratch,
            Some((BINARY, &destination)),
            &["--startup-report"],
            &[],
        )
        .output()
        .unwrap();
        assert_status(&output, 125);
        assert!(output.stdout.is_empty());
        let json = report(&output);
        assert!(json.contains(r#""kind":"supervisor_error""#), "{json}");
        assert_eq!(fs::read(&regular).unwrap(), sentinel);
        if is_symlink {
            assert!(fs::symlink_metadata(&destination).unwrap().is_symlink());
            assert_eq!(fs::read_link(&destination).unwrap(), regular);
        }
        scratch.assert_clean();
    }
}

#[test]
fn export_respects_the_checkout_policy_before_startup() {
    let scratch = Scratch::new();
    fs::create_dir(scratch.0.join(".git")).unwrap();
    let destination = scratch.0.join("refused.bin");
    let output = command(&scratch, Some((BINARY, &destination)), &[], &[])
        .output()
        .unwrap();
    assert_status(&output, 125);
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("checkout exports are limited"));
    assert!(!destination.exists());
    scratch.assert_clean();
}

#[test]
fn the_export_option_does_not_change_the_embedded_fixture_path() {
    let scratch = Scratch::new();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pe/trusted-runner.exe");
    let destination = scratch.0.join("fixture.bin");
    let output = Command::new(env!("CARGO_BIN_EXE_xenora"))
        .args(["run", "--experimental", "--pe"])
        .arg(fixture)
        .args(["--export-guest-file", BINARY])
        .arg(&destination)
        .output()
        .unwrap();
    assert_status(&output, 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("applies to a hosted program"));
    assert!(!destination.exists());
    scratch.assert_clean();
}

#[test]
fn a_destination_created_while_the_worker_waits_wins() {
    let scratch = Scratch::new();
    let destination = scratch.0.join("concurrent destination");
    let mut child = command(
        &scratch,
        Some((BINARY, &destination)),
        &["--startup-report", "--wall-seconds", "10"],
        &[],
    )
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    let input = child.stdin.take().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !has_stage(&scratch.0) {
        if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            drop(input);
            let output = child.wait_with_output().unwrap();
            panic!("supervisor did not establish its staging directory: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let sentinel = b"concurrent writer won";
    let write = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .and_then(|mut file| file.write_all(sentinel));
    drop(input);
    let output = child.wait_with_output().unwrap();
    write.unwrap();
    assert_status(&output, 125);
    assert_eq!(output.stdout, READY);
    let json = report(&output);
    assert!(json.contains(r#""kind":"supervisor_error""#), "{json}");
    assert_eq!(fs::read(destination).unwrap(), sentinel);
    scratch.assert_clean();
}
