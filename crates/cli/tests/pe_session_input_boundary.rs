// SPDX-License-Identifier: GPL-3.0-or-later

//! The CLI publishes only input boundaries that can accept a guest input.

use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(5);

fn fixture(name: &str) -> PathBuf {
    static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
    let directory = DIRECTORY.get_or_init(|| {
        let directory = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "authored-session-input-boundary-{}",
            std::process::id()
        ));
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/pe/authored-session/build-authored-session.sh");
        let output = Command::new("sh")
            .arg(script)
            .arg(&directory)
            .output()
            .expect("build authored PE fixtures");
        assert!(output.status.success(), "{output:?}");
        directory
    });
    directory.join(format!("{name}.exe"))
}

struct Session {
    child: Child,
    stdout: Receiver<String>,
    stderr: Receiver<String>,
}

impl Session {
    fn start(name: &str) -> Self {
        let path = fixture(name);
        let digest = format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap()));
        let mut child = Command::new(env!("CARGO_BIN_EXE_xenora-pe-session"))
            .arg("--pe")
            .arg(path)
            .args(["--sha256", &digest, "--experimental"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start authored PE session");
        let (lines, stdout) = mpsc::channel();
        let output = child.stdout.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if lines.send(line.expect("read CLI output")).is_err() {
                    return;
                }
            }
        });
        let (diagnostics, stderr) = mpsc::channel();
        let mut errors = child.stderr.take().unwrap();
        thread::spawn(move || {
            let mut text = String::new();
            errors
                .read_to_string(&mut text)
                .expect("read CLI diagnostics");
            let _ = diagnostics.send(text);
        });
        Self {
            child,
            stdout,
            stderr,
        }
    }

    fn expect_boundary_failure(&mut self) {
        // Keep stdin open throughout. An erroneous Ready would make the CLI
        // wait indefinitely for the next host request.
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll CLI exit") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "CLI stalled with stdin open; remaining stdout: {:?}",
                self.stdout.try_iter().collect::<Vec<_>>(),
            );
            thread::sleep(Duration::from_millis(10));
        };
        let stderr = self.stderr.recv_timeout(TIMEOUT).expect("CLI diagnostics");
        assert_eq!(status.code(), Some(125), "{stderr}");
        assert!(
            stderr.contains("invalid NextInput output address"),
            "{stderr}"
        );
        assert_eq!(
            self.stdout.recv_timeout(TIMEOUT),
            Err(RecvTimeoutError::Disconnected),
            "the CLI published an invalid ready boundary or an unconfirmed frame",
        );
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Give the supervisor EOF first so it can shut down its own worker.
        drop(self.child.stdin.take());
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn null_input_destination_fails_before_ready_without_waiting_for_eof() {
    let mut session = Session::start("authored-session-null-next-input");
    session.expect_boundary_failure();
}

#[test]
fn next_input_preflights_both_pages_before_ready() {
    let mut session = Session::start("authored-session-cross-page-next-input");
    session.expect_boundary_failure();
}

#[test]
fn readonly_next_input_destination_withholds_unconfirmed_frame() {
    let mut session = Session::start("authored-session-readonly-next-input");
    assert_eq!(session.stdout.recv_timeout(TIMEOUT).as_deref(), Ok("ready"));
    writeln!(session.child.stdin.as_mut().unwrap(), "10").expect("send first host input");
    session.expect_boundary_failure();
}
