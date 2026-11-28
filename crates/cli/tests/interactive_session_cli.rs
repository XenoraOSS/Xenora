// SPDX-License-Identifier: GPL-3.0-or-later

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const EXECUTABLE: &str = env!("CARGO_BIN_EXE_xenora-session");
const DEADLINE: Duration = Duration::from_secs(20);
const THREADED_EXIT: u32 = 0x73d0_ca11;
static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        loop {
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "xenora interactive session {} {sequence}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create test directory {}: {error}", path.display()),
            }
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Interactive {
    child: Child,
    stdout: Receiver<std::io::Result<String>>,
}

impl Interactive {
    fn start(command: &mut Command) -> Self {
        let mut child = command
            .arg("--interactive")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start interactive xenora-session");
        let output = child.stdout.take().unwrap();
        let (sender, stdout) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if sender.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });
        Self { child, stdout }
    }

    fn input(&mut self, bytes: &[u8]) {
        let input = self.child.stdin.as_mut().unwrap();
        input.write_all(bytes).expect("write interactive input");
        input.flush().expect("flush interactive input");
    }

    fn line(&self) -> String {
        self.stdout
            .recv_timeout(DEADLINE)
            .expect("expected a frame while interactive stdin is open")
            .expect("read interactive stdout")
    }

    fn finish(mut self, expected_exit: i32) -> (Vec<String>, String) {
        drop(self.child.stdin.take());
        self.await_exit(expected_exit)
    }

    fn await_exit(mut self, expected_exit: i32) -> (Vec<String>, String) {
        let deadline = Instant::now() + DEADLINE;
        let mut tail = Vec::new();
        loop {
            match self
                .stdout
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(line) => tail.push(line.expect("read interactive stdout")),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("interactive stdout stayed open"),
            }
        }
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll interactive session") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "interactive session did not exit"
            );
            thread::sleep(Duration::from_millis(10));
        };
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .expect("read interactive stderr");
        assert_eq!(status.code(), Some(expected_exit), "{tail:?}\n{stderr}");
        (tail, stderr)
    }
}

impl Drop for Interactive {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn command(backend: &str, frame_dir: &Path) -> Command {
    let mut command = Command::new(EXECUTABLE);
    command
        .args([
            "--backend",
            backend,
            "--workload",
            "threaded",
            "--frame-dir",
        ])
        .arg(frame_dir);
    command
}

fn expected_ppm(column: usize) -> Vec<u8> {
    let mut ppm = b"P6\n4 3\n255\n".to_vec();
    for pixel in 0..12 {
        let rgb = if pixel % 4 == column {
            [255, 0, 0]
        } else {
            [19, 43, 93]
        };
        ppm.extend_from_slice(&rgb);
    }
    ppm
}

fn verify_threaded_session(backend: &str, native_draws: usize) {
    let scratch = Scratch::new();
    let frame_dir = scratch.0.join("interactive frames");
    let mut guest = Interactive::start(&mut command(backend, &frame_dir));

    guest.input(b"right\r\n");
    let first = guest.line();
    assert!(first.starts_with("frame sequence=1 direction=right "));
    assert!(!frame_dir.exists(), "frames published before EOF");
    guest.input(b"right\n");
    let second = guest.line();
    assert!(second.starts_with("frame sequence=2 direction=right "));
    assert!(!frame_dir.exists(), "frames published before EOF");

    // The final line has no line terminator and is accepted when stdin closes.
    guest.input(b"left");
    let (tail, stderr) = guest.finish(0);
    assert!(
        stderr.lines().all(|line| backend == "metal"
            && (line.ends_with("Metal API Validation Enabled")
                || line.ends_with("Metal GPU Validation Enabled"))),
        "{stderr}"
    );
    assert_eq!(tail.len(), 2);
    assert!(tail[0].starts_with("frame sequence=3 direction=left "));
    assert_eq!(
        tail[1],
        format!(
            "exit status={THREADED_EXIT} frames=3 journal=121 handles=0 native_draws={native_draws} workload=threaded\n"
        )
    );

    let batch_dir = scratch.0.join("batch frames");
    let batch = command(backend, &batch_dir)
        .args(["right", "right", "left"])
        .output()
        .expect("run matching batch session");
    assert!(
        batch.status.success(),
        "{}",
        String::from_utf8_lossy(&batch.stderr)
    );
    assert_eq!(
        [first, second, tail.concat()].concat().as_bytes(),
        batch.stdout
    );
    assert_eq!(fs::read_dir(&frame_dir).unwrap().count(), 3);
    for (index, column) in [1, 2, 1].into_iter().enumerate() {
        let name = format!("frame-{:04}.ppm", index + 1);
        let expected = expected_ppm(column);
        assert_eq!(fs::read(batch_dir.join(&name)).unwrap(), expected);
        assert_eq!(fs::read(frame_dir.join(name)).unwrap(), expected);
    }
}

#[test]
fn threaded_reference_streams_frames_before_eof_and_exports_at_completion() {
    verify_threaded_session("reference", 0);
}

#[test]
#[ignore = "requires a native Metal device and validation enabled"]
fn threaded_metal_streams_native_frames_before_eof_and_exports_at_completion() {
    verify_threaded_session("metal", 3);
}

#[test]
fn interactive_eof_without_input_finishes_with_no_frames() {
    let scratch = Scratch::new();
    let frame_dir = scratch.0.join("empty frames");
    let guest = Interactive::start(&mut command("reference", &frame_dir));
    let (tail, _) = guest.finish(0);
    assert_eq!(
        tail,
        [format!(
            "exit status={THREADED_EXIT} frames=0 journal= handles=0 native_draws=0 workload=threaded\n"
        )]
    );
    assert_eq!(fs::read_dir(frame_dir).unwrap().count(), 0);
}

#[test]
fn interactive_rejects_replay_positional_and_option_like_frame_paths() {
    let scratch = Scratch::new();
    let frame_dir = scratch.0.join("must stay absent");
    let replay = scratch.0.join("replay.txt");
    fs::write(&replay, "xenora-session-v1\nright\n").unwrap();
    let replayed = command("reference", &frame_dir)
        .arg("--replay")
        .arg(&replay)
        .arg("--interactive")
        .output()
        .unwrap();
    let positional = command("reference", &frame_dir)
        .args(["--interactive", "left"])
        .output()
        .unwrap();
    let option_as_path = Command::new(EXECUTABLE)
        .args(["--backend", "reference", "--frame-dir", "--interactive"])
        .output()
        .unwrap();
    for output in [replayed, positional, option_as_path] {
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
    assert!(!frame_dir.exists());
}

#[test]
fn interactive_refuses_an_existing_export_directory_without_modifying_it() {
    let scratch = Scratch::new();
    let frame_dir = scratch.0.join("existing frames");
    fs::create_dir(&frame_dir).unwrap();
    let sentinel = frame_dir.join("frame-0001.ppm");
    fs::write(&sentinel, b"preserve this file").unwrap();
    let guest = Interactive::start(&mut command("reference", &frame_dir));
    let (tail, _) = guest.finish(2);
    assert!(tail.is_empty(), "{tail:?}");
    assert_eq!(fs::read(&sentinel).unwrap(), b"preserve this file");
    assert_eq!(fs::read_dir(&frame_dir).unwrap().count(), 1);
}

#[test]
fn interactive_invalid_or_oversized_directions_publish_no_completion() {
    let scratch = Scratch::new();
    let frame_dir = scratch.0.join("must stay absent");
    for invalid in [b"up\n".as_slice(), b"\xff\n", &[b'x'; 128]] {
        let mut guest = Interactive::start(&mut command("reference", &frame_dir));
        guest.input(b"right\n");
        assert!(
            guest
                .line()
                .starts_with("frame sequence=1 direction=right ")
        );
        guest.input(invalid);
        let (tail, _) = guest.finish(2);
        assert!(tail.is_empty(), "{tail:?}");
        assert!(!frame_dir.exists());
    }
}

#[test]
fn interactive_rejects_the_33rd_direction_without_exporting_frames() {
    let scratch = Scratch::new();
    let frame_dir = scratch.0.join("must stay absent");
    let mut guest = Interactive::start(&mut command("reference", &frame_dir));
    guest.input(b"left\n".repeat(32).as_slice());
    for sequence in 1..=32 {
        assert!(
            guest
                .line()
                .starts_with(&format!("frame sequence={sequence} direction=left "))
        );
    }
    assert!(!frame_dir.exists());
    guest.input(b"left\n");
    let (tail, _) = guest.finish(2);
    assert!(tail.is_empty(), "{tail:?}");
    assert!(!frame_dir.exists());
}

#[test]
fn interactive_worker_failure_has_no_completion_or_export() {
    let scratch = Scratch::new();
    let frame_dir = scratch.0.join("must stay absent");
    let guest = Interactive::start(command("reference", &frame_dir).args(["--steps", "1"]));
    let (tail, stderr) = guest.finish(125);
    assert!(tail.is_empty(), "{tail:?}");
    assert!(stderr.contains("instruction budget exhausted"), "{stderr}");
    assert!(!frame_dir.exists());
}

#[test]
fn interactive_reaps_a_worker_failure_while_stdin_stays_open() {
    let scratch = Scratch::new();
    let frame_dir = scratch.0.join("must stay absent");
    let mut command = Command::new(EXECUTABLE);
    // This budget lets the persistent guest present its first frame, then
    // expires before the guest can wait for a second command.
    command
        .args(["--backend", "reference", "--steps", "410", "--frame-dir"])
        .arg(&frame_dir);
    let mut guest = Interactive::start(&mut command);
    guest.input(b"right\n");
    assert!(
        guest
            .line()
            .starts_with("frame sequence=1 direction=right ")
    );
    let (tail, stderr) = guest.await_exit(125);
    assert!(tail.is_empty(), "{tail:?}");
    assert!(stderr.contains("instruction budget exhausted"), "{stderr}");
    assert!(!frame_dir.exists());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn interactive_exits_on_stdout_disconnect_before_input_and_between_frames() {
    let scratch = Scratch::new();
    for (send_direction, directory) in [(false, "before-input"), (true, "between-frames")] {
        let frame_dir = scratch.0.join(directory);
        let mut child = command("reference", &frame_dir)
            .arg("--interactive")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start interactive xenora-session");
        let output = child.stdout.take().unwrap();
        let (sender, stdout) = mpsc::channel();
        let mut session = Interactive { child, stdout };
        if send_direction {
            thread::spawn(move || {
                let mut reader = BufReader::new(output);
                let mut frame = String::new();
                let result = reader.read_line(&mut frame);
                drop(reader);
                let _ = sender.send(result.map(|_| frame));
            });
            session.input(b"left\n");
            assert!(
                session
                    .line()
                    .starts_with("frame sequence=1 direction=left "),
                "{directory}"
            );
        } else {
            drop(output);
            drop(sender);
        }
        let (tail, stderr) = session.await_exit(125);
        assert!(tail.is_empty(), "{directory}: {tail:?}");
        assert!(
            stderr.contains("host output disconnected"),
            "{directory}: {stderr}"
        );
        assert!(!frame_dir.exists(), "{directory}");
    }
}
