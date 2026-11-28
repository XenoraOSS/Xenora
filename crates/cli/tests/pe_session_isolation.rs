// SPDX-License-Identifier: GPL-3.0-or-later

//! Run `cargo test --locked --target aarch64-apple-darwin -p xenora
//! --test pe_session_isolation` on Apple Silicon. The existing authored PE
//! fixtures share the same guest journal path and verify their own journal
//! before the session completes.

use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, Read, Write as _};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(15);

struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Receiver<io::Result<String>>,
    stderr: Receiver<io::Result<String>>,
    reaped: bool,
}

impl Session {
    fn start(path: &Path) -> Self {
        let digest = format!("{:x}", Sha256::digest(std::fs::read(path).unwrap()));
        let mut child = Command::new(env!("CARGO_BIN_EXE_xenora-pe-session"))
            .arg("--pe")
            .arg(path)
            .args([
                "--sha256",
                &digest,
                "--experimental",
                "--graphics-backend",
                "reference",
                "--deterministic-clock",
                "--pixels",
                "--phase-seconds",
                "5",
            ])
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start authored PE session");
        let (send_stdout, stdout) = mpsc::channel();
        let output = child.stdout.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if send_stdout.send(line).is_err() {
                    break;
                }
            }
        });
        let (send_stderr, stderr) = mpsc::channel();
        let mut errors = child.stderr.take().unwrap();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = errors
                .read_to_end(&mut bytes)
                .map(|_| String::from_utf8_lossy(&bytes).into_owned());
            let _ = send_stderr.send(result);
        });
        let session = Self {
            stdin: child.stdin.take(),
            child,
            stdout,
            stderr,
            reaped: false,
        };
        assert_eq!(session.line(), "ready");
        session
    }

    fn line(&self) -> String {
        self.stdout
            .recv_timeout(TIMEOUT)
            .unwrap_or_else(|error| {
                panic!(
                    "session output unavailable: {error}; stderr: {:?}",
                    self.stderr.try_recv()
                )
            })
            .expect("read session output")
    }

    fn input(&mut self, value: u32, index: u8, clock_or_marker: u8) {
        writeln!(self.stdin.as_mut().unwrap(), "{value}").expect("send frame input");
        assert_eq!(self.line(), expected_frame(index, value, clock_or_marker));
    }

    fn finish(mut self) -> (ExitStatus, Vec<String>, String) {
        drop(self.stdin.take());
        let deadline = Instant::now() + TIMEOUT;
        let mut tail = Vec::new();
        loop {
            match self
                .stdout
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(line) => {
                    tail.push(line.expect("read final session output"));
                    assert!(tail.len() <= 1, "unexpected final session output: {tail:?}");
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("session stdout stayed open"),
            }
        }
        let errors = self
            .stderr
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("session stderr stayed open")
            .expect("read session diagnostics");
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("wait for session") {
                self.reaped = true;
                break status;
            }
            assert!(Instant::now() < deadline, "session did not exit after EOF");
            thread::sleep(Duration::from_millis(10));
        };
        (status, tail, errors)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.reaped {
            // The session supervisor and its worker inherit this private
            // process group, so neither can survive a test panic or deadline.
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{}", self.child.id())])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn expected_frame(index: u8, input: u32, blue: u8) -> String {
    let red_column = if input.is_multiple_of(2) { 0 } else { 3 };
    let mut rgba = Vec::new();
    for _ in 0..3 {
        for column in 0..4 {
            rgba.extend_from_slice(&if column == red_column {
                [255, 0, 0, 255]
            } else {
                [input as u8, index, blue, 255]
            });
        }
    }
    let digest = Sha256::digest(&rgba);
    let mut frame = format!("frame {index} width=4 height=3 sha256={digest:x} rgba=");
    for byte in rgba {
        write!(frame, "{byte:02x}").unwrap();
    }
    frame
}

#[test]
fn a_failed_concurrent_session_leaves_the_other_journal_and_clock_intact() {
    let fixtures = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pe-session-isolation-{}", std::process::id()));
    let builder = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pe/authored-session/build-authored-session.sh");
    let build = Command::new("sh")
        .arg(builder)
        .arg(&fixtures)
        .output()
        .expect("build the original session fixtures");
    assert!(build.status.success(), "{build:?}");

    let clock_guest = fixtures.join("authored-session-deterministic-clock.exe");
    let mut active = Session::start(&clock_guest);
    // Each input advances the guest clock by 16 ms, and the fixture sleeps
    // another 5 ms between inputs. Host scheduling adds no guest time.
    active.input(10, 1, 16);

    let mut failing = Session::start(&fixtures.join("authored-session-leak.exe"));
    failing.input(33, 1, 0x3c);
    active.input(21, 2, 37);
    let (status, tail, errors) = failing.finish();
    assert_eq!(status.code(), Some(125), "{errors}");
    assert!(tail.is_empty(), "{tail:?}");
    assert!(
        errors.contains("guest retained 1 open runtime handles"),
        "{errors}"
    );

    active.input(12, 3, 58);
    let (status, tail, errors) = active.finish();
    assert_eq!(status.code(), Some(0), "{errors}");
    assert_eq!(tail, ["done frames=3"]);
    assert!(errors.is_empty(), "{errors}");

    let mut fresh = Session::start(&clock_guest);
    fresh.input(10, 1, 16);
    let (status, tail, errors) = fresh.finish();
    assert_eq!(status.code(), Some(0), "{errors}");
    assert_eq!(tail, ["done frames=1"]);
    assert!(errors.is_empty(), "{errors}");
}
