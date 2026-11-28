// SPDX-License-Identifier: GPL-3.0-or-later

//! Authored PE programs exchange requests and frames with the shipped CLI.

use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const EXECUTABLE: &str = env!("CARGO_BIN_EXE_xenora-pe-session");
const TIMEOUT: Duration = Duration::from_secs(20);
const PIPE_TIMEOUT: Duration = Duration::from_secs(2);

fn fixture(name: &str) -> PathBuf {
    fixture_directory().join(format!("{name}.exe"))
}

fn fixture_directory() -> &'static Path {
    static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
    DIRECTORY.get_or_init(|| {
        let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("authored-session-{}", std::process::id()));
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/pe/authored-session/build-authored-session.sh");
        let build = Command::new("sh")
            .arg(script)
            .arg(&directory)
            .output()
            .expect("build the authored PE session fixtures");
        assert!(build.status.success(), "{build:?}");
        directory
    })
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(std::fs::read(path).unwrap()))
}

fn command(path: &Path, digest: Option<&str>) -> Command {
    let mut command = Command::new(EXECUTABLE);
    let digest = digest.map_or_else(|| sha256(path), str::to_owned);
    command
        .arg("--pe")
        .arg(path)
        .args(["--sha256", &digest, "--experimental"]);
    command
}

fn companion(variant: &str) -> PathBuf {
    fixture_directory()
        .join(variant)
        .join("xenora-authored-companion.dll")
}

fn add_companion(command: &mut Command, variant: &str) {
    let path = companion(variant);
    command.arg("--package").arg(&path).arg(sha256(&path));
}

struct Session {
    child: Child,
    input: Option<ChildStdin>,
    lines: Receiver<String>,
    diagnostics: Receiver<String>,
}

struct Completed {
    status: ExitStatus,
    lines: Vec<String>,
    diagnostics: String,
}

fn wait_for_exit(child: &mut Child, timeout: Duration, label: &str) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("wait for session") {
            return status;
        }
        assert!(Instant::now() < deadline, "session did not exit {label}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn collect_diagnostics(child: &mut Child) -> Receiver<String> {
    let (send, diagnostics) = mpsc::channel();
    let mut stderr = child.stderr.take().unwrap();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read session stderr");
        let _ = send.send(String::from_utf8_lossy(&bytes).into_owned());
    });
    diagnostics
}

impl Completed {
    fn assert_code(&self, expected: i32) {
        assert_eq!(
            self.status.code(),
            Some(expected),
            "stdout: {:?}\nstderr: {}",
            self.lines,
            self.diagnostics,
        );
    }
}

impl Session {
    fn start(command: &mut Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start xenora-pe-session");
        let (lines_tx, lines) = mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if lines_tx.send(line.expect("read session stdout")).is_err() {
                    break;
                }
            }
        });
        let diagnostics = collect_diagnostics(&mut child);
        Self {
            input: child.stdin.take(),
            child,
            lines,
            diagnostics,
        }
    }

    fn next_line(&self) -> String {
        self.lines.recv_timeout(TIMEOUT).unwrap_or_else(|error| {
            panic!(
                "waiting for session output: {error}; stderr: {}",
                self.diagnostics.try_recv().unwrap_or_default(),
            )
        })
    }

    fn send(&mut self, input: u32) {
        writeln!(self.input.as_mut().unwrap(), "{input}").expect("send a live session request");
    }

    fn finish(mut self) -> Completed {
        drop(self.input.take());
        let status = wait_for_exit(&mut self.child, TIMEOUT, "after EOF");
        let mut lines = Vec::new();
        loop {
            match self.lines.recv_timeout(TIMEOUT) {
                Ok(line) => lines.push(line),
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => panic!("session stdout did not close"),
            }
        }
        Completed {
            status,
            lines,
            diagnostics: self
                .diagnostics
                .recv_timeout(TIMEOUT)
                .expect("session stderr did not close"),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct RawPipeSession(Child);

impl RawPipeSession {
    // Return the read end intact and stop draining immediately after `ready`.
    // The child retains its writable stdin pipe so neither test sends EOF.
    fn start(command: &mut Command) -> (Self, BufReader<ChildStdout>) {
        let mut session = Self(
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("start xenora-pe-session"),
        );
        let mut output = BufReader::new(session.0.stdout.take().unwrap());
        let (send, ready) = mpsc::channel();
        thread::spawn(move || {
            let mut line = String::new();
            output.read_line(&mut line).expect("read initial ready");
            let _ = send.send((line, output));
        });
        let (line, output) = ready.recv_timeout(TIMEOUT).expect("wait for initial ready");
        assert_eq!(line, "ready\n");
        (session, output)
    }

    fn assert_exit(&mut self, code: i32, label: &str) -> String {
        let diagnostics = collect_diagnostics(&mut self.0);
        let status = wait_for_exit(&mut self.0, PIPE_TIMEOUT, label);
        let stderr = diagnostics
            .recv_timeout(PIPE_TIMEOUT)
            .expect("session stderr did not close");
        assert_eq!(status.code(), Some(code), "{stderr}");
        stderr
    }
}

impl Drop for RawPipeSession {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn expected_frame(sequence: u8, input: u32, pixels: bool) -> String {
    expected_frame_with_blue(sequence, input, 0x3c, pixels)
}

fn expected_frame_with_blue(sequence: u8, input: u32, blue: u8, pixels: bool) -> String {
    let red_column = if input.is_multiple_of(2) { 0 } else { 3 };
    let mut rgba = Vec::new();
    for _ in 0..3 {
        for column in 0..4 {
            let pixel = if column == red_column {
                [255, 0, 0, 255]
            } else {
                [input as u8, sequence, blue, 0xff]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    let hash = Sha256::digest(&rgba);
    let mut line = format!("frame {sequence} width=4 height=3 sha256={hash:x}");
    if pixels {
        line.push_str(" rgba=");
        for byte in rgba {
            write!(&mut line, "{byte:02x}").unwrap();
        }
    }
    line
}

fn run_package_session(backend: &str) -> Vec<String> {
    let mut command = command(&fixture("authored-session-package"), None);
    add_companion(&mut command, "");
    command.args(["--graphics-backend", backend, "--pixels"]);
    let mut session = Session::start(&mut command);
    assert_eq!(session.next_line(), "ready");
    let mut transcript = Vec::new();
    // Attach seeds blue to 0x40. Each frame adds its input to the same DLL state.
    for (sequence, input, blue) in [(1, 10, 0x4a), (2, 21, 0x5f), (3, 10, 0x69)] {
        session.send(input);
        let line = session.next_line();
        assert_eq!(line, expected_frame_with_blue(sequence, input, blue, true));
        transcript.push(line);
    }
    let completed = session.finish();
    completed.assert_code(0);
    assert_eq!(completed.lines, ["done frames=3"]);
    transcript
}

#[test]
fn packaged_session_keeps_companion_state_across_inputs() {
    assert_eq!(
        run_package_session("reference"),
        run_package_session("reference")
    );
}

#[test]
fn packaged_session_rejects_bad_admission_and_lifecycle() {
    let path = fixture("authored-session-package");
    let mut missing = command(&path, None);
    let mut wrong_pin = command(&path, None);
    wrong_pin
        .arg("--package")
        .arg(companion(""))
        .arg("0".repeat(64));
    let mut duplicate = command(&path, None);
    add_companion(&mut duplicate, "");
    add_companion(&mut duplicate, "refuse-attach");
    let mut reserved = command(&path, None);
    let reserved_name = fixture_directory().join("xenora-session-v1.dll");
    add_companion(&mut reserved, "");
    reserved
        .arg("--package")
        .arg(&reserved_name)
        .arg(sha256(&reserved_name));
    let mut refuse = command(&path, None);
    add_companion(&mut refuse, "refuse-attach");
    let mut attach_abi = command(&path, None);
    add_companion(&mut attach_abi, "abi-during-attach");

    let outside_entry = "session imports require the main guest thread after startup";
    for (case, command, reason) in [
        ("missing", &mut missing, "xenora-authored-companion.dll"),
        ("wrong pin", &mut wrong_pin, "SHA-256"),
        ("duplicate", &mut duplicate, "canonical identity"),
        (
            "reserved name",
            &mut reserved,
            "reserved for the session ABI",
        ),
        ("refuse attach", &mut refuse, "returned FALSE"),
        ("ABI during attach", &mut attach_abi, outside_entry),
    ] {
        let completed = Session::start(command).finish();
        completed.assert_code(125);
        assert!(completed.lines.is_empty(), "{case}: {:?}", completed.lines);
        assert!(
            completed.diagnostics.contains(reason),
            "{case}: {}",
            completed.diagnostics
        );
    }

    for (variant, reason) in [
        ("abi-during-detach", outside_entry),
        ("fault-during-detach", "guest package shutdown"),
    ] {
        let mut command = command(&path, None);
        add_companion(&mut command, variant);
        let session = Session::start(&mut command);
        assert_eq!(session.next_line(), "ready");
        let completed = session.finish();
        completed.assert_code(125);
        assert!(
            completed.lines.is_empty(),
            "{variant}: {:?}",
            completed.lines
        );
        assert!(
            completed.diagnostics.contains(reason),
            "{variant}: {}",
            completed.diagnostics
        );
    }
}

#[test]
fn packaged_session_limits_total_input_bytes_and_requires_dll_companions() {
    let path = fixture("authored-session-package");
    let oversized = fixture_directory().join("too-large-companion.dll");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(64 * 1024 * 1024)
        .unwrap();

    let mut command_with_file = command(&path, None);
    command_with_file
        .arg("--package")
        .arg(&oversized)
        .arg("0".repeat(64));
    let completed = Session::start(&mut command_with_file).finish();
    completed.assert_code(125);
    assert!(completed.lines.is_empty());
    assert!(completed.diagnostics.contains("combined 64 MiB limit"));

    let executable = fixture("authored-session");
    let mut command_with_exe = command(&path, None);
    command_with_exe
        .arg("--package")
        .arg(&executable)
        .arg(sha256(&executable));
    let completed = Session::start(&mut command_with_exe).finish();
    completed.assert_code(125);
    assert!(completed.lines.is_empty());
    assert!(completed.diagnostics.contains("must be a PE DLL"));

    let dll_as_root = fixture_directory().join("root-is-dll.dll");
    std::fs::copy(companion("abi-during-attach"), &dll_as_root).unwrap();
    let mut command_with_dll_root = command(&dll_as_root, None);
    add_companion(&mut command_with_dll_root, "");
    let completed = Session::start(&mut command_with_dll_root).finish();
    completed.assert_code(125);
    assert!(completed.lines.is_empty());
    assert!(completed.diagnostics.contains("must be a PE executable"));
}

#[test]
fn standalone_dll_is_refused_as_a_session_root() {
    let completed = Session::start(&mut command(&companion(""), None)).finish();
    completed.assert_code(125);
    assert!(completed.lines.is_empty(), "{:?}", completed.lines);
    assert!(
        completed.diagnostics.contains("must be a PE executable"),
        "{}",
        completed.diagnostics
    );
}

#[test]
#[ignore = "requires native Metal on Apple Silicon"]
fn packaged_session_matches_reference_on_native_metal() {
    assert_eq!(
        run_package_session("metal"),
        run_package_session("reference")
    );
}

#[test]
fn live_requests_keep_guest_state_until_clean_eof() {
    let path = fixture("authored-session");
    let mut session =
        Session::start(command(&path, None).args(["--pixels", "--phase-seconds", "1"]));
    assert_eq!(session.next_line(), "ready");

    session.send(10);
    assert_eq!(session.next_line(), expected_frame(1, 10, true));
    // The guest has finished processing. Keeping stdin open here must neither
    // trip the guest phase deadline nor produce a frame without a request.
    assert_eq!(
        session.lines.recv_timeout(Duration::from_millis(1200)),
        Err(RecvTimeoutError::Timeout),
    );

    session.send(21);
    assert_eq!(session.next_line(), expected_frame(2, 21, true));
    session.send(10);
    assert_eq!(session.next_line(), expected_frame(3, 10, true));

    let completed = session.finish();
    completed.assert_code(0);
    assert_eq!(completed.lines, ["done frames=3"]);
}

#[test]
fn deterministic_clock_advances_for_inputs_and_guest_sleep_but_not_host_idle() {
    let path = fixture("authored-session-deterministic-clock");
    for host_idle in [false, true] {
        let mut session =
            Session::start(command(&path, None).args(["--deterministic-clock", "--pixels"]));
        assert_eq!(session.next_line(), "ready");
        // The first input advances by 16 ms. Between frames the guest sleeps
        // for 5 ms, and each next input advances by another 16 ms.
        for (sequence, milliseconds) in [(1, 16), (2, 37), (3, 58)] {
            if host_idle {
                assert_eq!(
                    session.lines.recv_timeout(Duration::from_millis(125)),
                    Err(RecvTimeoutError::Timeout),
                );
            }
            session.send(10);
            assert_eq!(
                session.next_line(),
                expected_frame_with_blue(sequence, 10, milliseconds, true),
            );
        }
        let completed = session.finish();
        completed.assert_code(0);
        assert_eq!(completed.lines, ["done frames=3"]);
    }
}

#[test]
fn admission_and_instruction_budget_failures_publish_no_completion() {
    let path = fixture("authored-session");
    let wrong_digest = "0".repeat(64);
    let rejected = Session::start(&mut command(&path, Some(&wrong_digest))).finish();
    rejected.assert_code(125);
    assert!(rejected.lines.is_empty(), "{:?}", rejected.lines);
    assert!(
        rejected.diagnostics.contains("SHA-256"),
        "{}",
        rejected.diagnostics
    );

    let exhausted = Session::start(command(&path, None).args(["--max-steps", "1"])).finish();
    exhausted.assert_code(125);
    assert!(exhausted.lines.is_empty(), "{:?}", exhausted.lines);
    assert!(
        exhausted.diagnostics.contains("budget"),
        "{}",
        exhausted.diagnostics
    );
}

#[test]
fn guest_protocol_and_cleanup_failures_never_publish_done() {
    for variant in ["missing-present", "extra-present", "leak"] {
        let path = fixture(&format!("authored-session-{variant}"));
        let mut session = Session::start(&mut command(&path, None));
        assert_eq!(session.next_line(), "ready");
        session.send(10);
        if variant == "leak" {
            assert_eq!(session.next_line(), expected_frame(1, 10, false));
        }
        let completed = session.finish();
        completed.assert_code(125);
        assert!(
            completed
                .lines
                .iter()
                .all(|line| !line.starts_with("done ")),
            "{variant}: {:?}",
            completed.lines,
        );
        assert!(!completed.diagnostics.is_empty(), "{variant}");
    }
}

#[test]
fn a_frame_phase_deadline_terminates_the_worker() {
    let path = fixture("authored-session-stall-frame");
    let mut session = Session::start(command(&path, None).args(["--phase-seconds", "1"]));
    assert_eq!(session.next_line(), "ready");
    session.send(10);
    let completed = session.finish();
    completed.assert_code(124);
    assert!(completed.lines.is_empty(), "{:?}", completed.lines);
    assert!(
        completed
            .diagnostics
            .contains("frame exceeded the phase deadline"),
        "{}",
        completed.diagnostics
    );
}

#[test]
fn closing_stdout_while_stdin_is_open_terminates_an_idle_session() {
    let path = fixture("authored-session");
    let (mut session, output) = RawPipeSession::start(&mut command(&path, None));
    drop(output);
    let stderr = session.assert_exit(125, "after the output consumer disconnected");
    assert!(stderr.contains("host output disconnected"), "{stderr}");
}

#[test]
fn an_undrained_large_frame_hits_the_wall_deadline() {
    let path = fixture("authored-session-large-frame");
    let mut draining = Session::start(command(&path, None).arg("--pixels"));
    assert_eq!(draining.next_line(), "ready");
    draining.send(10);
    let frame = draining.next_line();
    assert!(frame.starts_with("frame 1 width=64 height=64 sha256="));
    assert_eq!(frame.split_once(" rgba=").unwrap().1.len(), 32_768);
    let completed = draining.finish();
    completed.assert_code(0);
    assert_eq!(completed.lines, ["done frames=1"]);

    let (mut stalled, mut output) =
        RawPipeSession::start(command(&path, None).args(["--pixels", "--phase-seconds", "1"]));
    // Four supported frames exceed the pipe capacity even if the host
    // accepts the first few reports before it stops draining.
    for _ in 0..4 {
        writeln!(stalled.0.stdin.as_mut().unwrap(), "10")
            .expect("send input without draining stdout");
    }
    let stderr = stalled.assert_exit(124, "while frame output was blocked");
    assert!(
        stderr.contains("frame exceeded the phase deadline"),
        "{stderr}"
    );
    // The supervisor has exited, so it is now safe to drain the partial
    // frame. Its prefix proves the guest rendered before output stalled.
    let mut partial = String::new();
    output.read_to_string(&mut partial).unwrap();
    assert!(partial.starts_with("frame 1 width=64 height=64 sha256="));
    assert!(
        partial.len() < 4 * (frame.len() + 1),
        "all four frames reached the pipe"
    );
}

#[test]
fn eof_without_input_and_full_sessions_both_finish() {
    let path = fixture("authored-session");
    for count in [0, 64] {
        let mut session = Session::start(&mut command(&path, None));
        assert_eq!(session.next_line(), "ready");
        for sequence in 1..=count {
            session.send(u32::MAX);
            assert_eq!(
                session.next_line(),
                expected_frame(sequence, u32::MAX, false)
            );
        }
        let completed = session.finish();
        completed.assert_code(0);
        assert_eq!(completed.lines, [format!("done frames={count}")]);
    }
}

#[test]
fn an_invalid_host_line_fails_without_publishing_done() {
    let path = fixture("authored-session");
    let mut session = Session::start(&mut command(&path, None));
    assert_eq!(session.next_line(), "ready");
    writeln!(session.input.as_mut().unwrap(), "4294967296").unwrap();
    let completed = session.finish();
    completed.assert_code(125);
    assert!(completed.lines.is_empty(), "{:?}", completed.lines);
    assert!(
        completed
            .diagnostics
            .contains("unsigned 32-bit decimal value")
    );
}

#[test]
fn required_options_and_invalid_limits_are_usage_errors() {
    let digest = "0".repeat(64);
    for flags in [
        vec!["--experimental", "--sha256", &digest, "--pe", "--pixels"],
        vec!["--experimental", "--pe", "some.exe"],
        vec!["--pe", "some.exe", "--sha256", &digest],
        vec!["--experimental", "--pe", "some.exe", "--sha256", "xyz"],
        vec![
            "--experimental",
            "--pe",
            "some.exe",
            "--sha256",
            &digest,
            "--phase-seconds",
            "0",
        ],
        vec![
            "--experimental",
            "--pe",
            "some.exe",
            "--sha256",
            &digest,
            "--max-steps",
            "10000001",
        ],
    ] {
        let invocation = Command::new(env!("CARGO_BIN_EXE_xenora-pe-session"))
            .args(flags)
            .output()
            .unwrap();
        assert_eq!(invocation.status.code(), Some(2));
        assert!(invocation.stdout.is_empty());
    }
}
