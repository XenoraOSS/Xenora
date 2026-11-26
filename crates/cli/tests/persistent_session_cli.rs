// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

const EXECUTABLE: &str = env!("CARGO_BIN_EXE_xenora-session");
const CLEAR: [u8; 4] = [19, 43, 93, 251];
const RED: [u8; 4] = [255, 0, 0, 255];
static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        loop {
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "xenora persistent session {} {sequence}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create test directory {}: {error}", path.display()),
            }
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Output {
    Command::new(EXECUTABLE)
        .args(args)
        .output()
        .expect("run the xenora-session CLI")
}

fn text(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("CLI stdout is UTF-8")
}

fn status(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout:\n{}\nstderr:\n{}",
        text(output),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn pixels(direction: &str) -> [[u8; 4]; 12] {
    let column = match direction {
        "left" => 0,
        "right" => 3,
        _ => panic!("unexpected test direction {direction}"),
    };
    // Calculate all 12 texels independently in row-major order.
    std::array::from_fn(|pixel| if pixel % 4 == column { RED } else { CLEAR })
}

fn verify_run(output: Output, backend: &str, sequence: &[&str], journal: &str, native: usize) {
    status(&output, 0);
    let mut lines = text(&output).lines();
    for (index, direction) in sequence.iter().enumerate() {
        let mut hex = String::new();
        for byte in pixels(direction).into_iter().flatten() {
            write!(hex, "{byte:02x}").unwrap();
        }
        let line = format!(
            "frame sequence={} direction={direction} backend={backend} width=4 height=3 draws=1 rgba={hex}",
            index + 1
        );
        assert_eq!(lines.next(), Some(line.as_str()), "frame {}", index + 1);
    }
    let terminal = format!(
        "exit status=0 frames={} journal={journal} handles=0 native_draws={native}",
        sequence.len()
    );
    assert_eq!(lines.next(), Some(terminal.as_str()));
    assert_eq!(
        lines.next(),
        None,
        "unexpected stdout after the terminal record"
    );
}

fn verify_exports(directory: &Path, sequence: &[&str]) {
    let mut actual: Vec<_> = std::fs::read_dir(directory)
        .expect("read exported frame directory")
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    actual.sort_unstable();

    let expected: Vec<_> = (1..=sequence.len())
        .map(|index| format!("frame-{index:04}.ppm"))
        .collect();
    assert_eq!(actual, expected);

    for (filename, direction) in expected.iter().zip(sequence) {
        let frame = std::fs::read(directory.join(filename)).expect("read exported PPM");
        let rgb = frame
            .strip_prefix(b"P6\n4 3\n255\n")
            .expect("P6 header for 4 by 3 RGB");
        let expected: Vec<_> = pixels(direction)
            .into_iter()
            .flat_map(|[r, g, b, _]| [r, g, b])
            .collect();
        assert_eq!(rgb, expected, "{filename}");
    }
}

#[test]
fn reference_default_sequence_renders_exact_pixels() {
    verify_run(
        run(["--backend", "reference"]),
        "reference",
        &["left", "right", "left"],
        "LRL",
        0,
    );
}

#[test]
fn reference_repeated_directions_preserve_order_and_export_each_frame() {
    let scratch = Scratch::new();
    let frame_dir = scratch.0.join("requested frames");
    let sequence = ["right", "left", "left", "right", "right"];
    let result = Command::new(EXECUTABLE)
        .args([
            "--backend",
            "reference",
            "--steps",
            "1000000",
            "--frame-dir",
        ])
        .arg(&frame_dir)
        .args(sequence)
        .output()
        .expect("run the xenora-session CLI");
    verify_run(result, "reference", &sequence, "RLLRR", 0);
    verify_exports(&frame_dir, &sequence);
}

#[test]
fn instruction_exhaustion_does_not_publish_frames() {
    let scratch = Scratch::new();
    let frame_dir = scratch.0.join("must stay absent");
    let result = Command::new(EXECUTABLE)
        .args(["--backend", "reference", "--steps", "1", "--frame-dir"])
        .arg(&frame_dir)
        .output()
        .expect("run the xenora-session CLI");
    status(&result, 125);
    assert!(!text(&result).lines().any(|line| line.starts_with("frame ")));
    assert!(!frame_dir.exists(), "failed run published a directory");
}

#[test]
fn a_budget_failure_after_a_frame_does_not_publish_partial_results() {
    // This budget is enough to finish with just the first direction.
    verify_run(
        run(["--backend", "reference", "--steps", "512", "left"]),
        "reference",
        &["left"],
        "L",
        0,
    );
    let scratch = Scratch::new();
    let frame_dir = scratch.0.join("late failure");
    let result = Command::new(EXECUTABLE)
        .args(["--backend", "reference", "--steps", "512", "--frame-dir"])
        .arg(&frame_dir)
        .args(["left", "right", "left"])
        .output()
        .expect("run the xenora-session CLI");
    status(&result, 125);
    assert!(String::from_utf8_lossy(&result.stderr).contains("instruction budget exhausted"));
    assert!(
        result.stdout.is_empty(),
        "failed run exposed collected frames"
    );
    assert!(!frame_dir.exists(), "failed run published a directory");
}

#[test]
fn the_full_event_budget_preserves_every_frame_and_journal_byte() {
    let directions: [&str; 32] =
        std::array::from_fn(|index| if index % 2 == 0 { "left" } else { "right" });
    verify_run(
        run(["--backend", "reference"].into_iter().chain(directions)),
        "reference",
        &directions,
        &"LR".repeat(16),
        0,
    );
}

fn run_with_frames(backend: &str, directory: &Path, args: &[&OsStr]) -> Output {
    Command::new(EXECUTABLE)
        .args(["--backend", backend, "--frame-dir"])
        .arg(directory)
        .args(args)
        .output()
        .expect("run the xenora-session CLI")
}

fn verify_replay_matches_directions(
    backend: &str,
    direction_file: &[u8],
    directions: &[&str],
    journal: &str,
    native: usize,
) {
    let scratch = Scratch::new();
    let replay = scratch.0.join("recorded directions.txt");
    std::fs::write(&replay, direction_file).unwrap();
    let replay_frames = scratch.0.join("replayed frames");
    let replayed = run_with_frames(
        backend,
        &replay_frames,
        &[OsStr::new("--replay"), replay.as_os_str()],
    );
    let positional_frames = scratch.0.join("positional frames");
    let arguments: Vec<_> = directions.iter().map(OsStr::new).collect();
    let positional = run_with_frames(backend, &positional_frames, &arguments);
    status(&replayed, 0);
    status(&positional, 0);
    assert_eq!(replayed.stdout, positional.stdout);
    verify_run(replayed, backend, directions, journal, native);
    verify_exports(&replay_frames, directions);
    verify_exports(&positional_frames, directions);
    for index in 1..=directions.len() {
        let filename = format!("frame-{index:04}.ppm");
        assert_eq!(
            std::fs::read(replay_frames.join(&filename)).unwrap(),
            std::fs::read(positional_frames.join(&filename)).unwrap(),
            "{filename}"
        );
    }
}

#[test]
fn replay_reference_matches_positional_stdout_and_ppms() {
    let directions = ["right", "left", "left", "right", "right"];
    for file in [
        &b"xenora-session-v1\nright\nleft\nleft\nright\nright\n"[..],
        &b"xenora-session-v1\r\nright\r\nleft\r\nleft\r\nright\r\nright"[..],
    ] {
        verify_replay_matches_directions("reference", file, &directions, "RLLRR", 0);
    }
}

#[test]
fn replay_reference_preserves_all_32_events() {
    let directions: [&str; 32] =
        std::array::from_fn(|index| if index % 2 == 0 { "right" } else { "left" });
    let replay = format!("xenora-session-v1\r\n{}\r\n", directions.join("\r\n"));
    verify_replay_matches_directions(
        "reference",
        replay.as_bytes(),
        &directions,
        &"RL".repeat(16),
        0,
    );
}

#[test]
fn replay_invalid_inputs_never_publish_frames() {
    let scratch = Scratch::new();
    let replay = scratch.0.join("recording");
    let frame_dir = scratch.0.join("must stay absent");
    let replay_args = [OsStr::new("--replay"), replay.as_os_str()];
    let assert_refusal = |args: &[&OsStr]| {
        let result = run_with_frames("reference", &frame_dir, args);
        status(&result, 2);
        assert!(result.stdout.is_empty());
        assert!(!frame_dir.exists());
    };
    assert_refusal(&[OsStr::new("--replay")]);
    assert_refusal(&replay_args); // missing path
    assert_refusal(&[OsStr::new("--replay"), scratch.0.as_os_str()]);

    let too_many = format!("xenora-session-v1\n{}", "left\n".repeat(33));
    let oversize = [b"xenora-session-v1\nleft\n".as_slice(), &[b'x'; 256]].concat();
    for file in [
        &b""[..],
        b"xenora-session-v2\nleft\n",
        b"xenora-session-v1\n",
        b"xenora-session-v1\nleft\n\n",
        b"xenora-session-v1\nleft\nRight\n",
        b"xenora-session-v1\n left\n",
        b"xenora-session-v1\nleft \n",
        b"xenora-session-v1\rleft\r",
        b"xenora-session-v1\nleft\r",
        b"xenora-session-v1\n\xff\n",
        too_many.as_bytes(),
        &oversize,
    ] {
        std::fs::write(&replay, file).unwrap();
        assert_refusal(&replay_args);
    }

    std::fs::write(&replay, b"xenora-session-v1\nleft\n").unwrap();
    let [option, path] = replay_args;
    assert_refusal(&[option, path, option, path]);
    assert_refusal(&[option, path, OsStr::new("left")]);
    assert_refusal(&[OsStr::new("right"), option, path]);
    let worker = run([OsStr::new("--session-worker"), option, path]);
    status(&worker, 2);
    assert!(worker.stdout.is_empty());
    assert!(String::from_utf8_lossy(&worker.stderr).contains("only in the session supervisor"));
    assert_eq!(std::fs::read_dir(&scratch.0).unwrap().count(), 1);
}

#[test]
fn replay_fifo_is_rejected_without_waiting_for_a_writer() {
    use std::os::unix::fs::symlink;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let scratch = Scratch::new();
    let fifo = scratch.0.join("replay pipe");
    let alias = scratch.0.join("pipe link");
    let frame_dir = scratch.0.join("must stay absent");
    let made = Command::new("mkfifo").arg(&fifo).status().unwrap();
    assert!(made.success());
    symlink(&fifo, &alias).unwrap();
    for path in [&fifo, &alias] {
        let mut child = Command::new(EXECUTABLE)
            .args(["--backend", "reference", "--frame-dir"])
            .arg(&frame_dir)
            .arg("--replay")
            .arg(path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                result => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("replay must refuse {path:?} before it has a writer: {result:?}");
                }
            }
        }
        let output = child.wait_with_output().unwrap();
        status(&output, 2);
        assert!(output.stdout.is_empty());
        assert!(!frame_dir.exists());
    }
}

#[test]
fn invalid_arguments_are_refused() {
    for args in [
        &["--backend", "reference", "--steps", "0"][..],
        &["--backend", "reference", "--steps", "1000001"],
        &["--backend", "reference", "--steps", "NaN"],
        &["--backend", "unknown"],
        &["--backend", "reference", "up"],
        &["--backend", "reference", "--frame-dir"],
    ] {
        let result = run(args);
        status(&result, 2);
        assert!(
            result.stdout.is_empty(),
            "invalid command emitted a frame record"
        );
    }

    let result = run(["--backend", "reference"]
        .into_iter()
        .chain(std::iter::repeat_n("left", 33)));
    status(&result, 2);
    assert!(result.stdout.is_empty());
}

#[test]
fn existing_export_destinations_are_not_modified() {
    let scratch = Scratch::new();
    let marker = scratch.0.join("frame-0001.ppm");
    std::fs::write(&marker, b"keep this file").expect("write destination marker");
    for destination in [&scratch.0, &marker] {
        let result = Command::new(EXECUTABLE)
            .args(["--backend", "reference", "--frame-dir"])
            .arg(destination)
            .output()
            .expect("run the xenora-session CLI");
        status(&result, 2);
        assert!(result.stdout.is_empty());
        assert_eq!(
            std::fs::read(&marker).expect("previous destination file is intact"),
            b"keep this file"
        );
    }
    assert_eq!(std::fs::read_dir(&scratch.0).unwrap().count(), 1);
}

#[test]
fn a_closed_stdout_rolls_back_exports_and_allows_retry() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::Stdio;

    let scratch = Scratch::new();
    let frame_dir = scratch.0.join("retry frames");
    let sequence = ["right", "left"];
    let (reader, writer) = UnixStream::pair().expect("open a stream pair");
    drop(reader);
    let failed = Command::new(EXECUTABLE)
        .args(["--backend", "reference", "--frame-dir"])
        .arg(&frame_dir)
        .args(sequence)
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .output()
        .expect("run the CLI with a closed output stream");

    status(&failed, 125);
    assert!(String::from_utf8_lossy(&failed.stderr).contains("could not write result"));
    assert!(!frame_dir.exists(), "failed stdout left a frame directory");

    let retry = Command::new(EXECUTABLE)
        .args(["--backend", "reference", "--frame-dir"])
        .arg(&frame_dir)
        .args(sequence)
        .output()
        .expect("retry the CLI with working stdout");
    verify_run(retry, "reference", &sequence, "RL", 0);
    verify_exports(&frame_dir, &sequence);
}

#[test]
#[ignore = "requires a native Metal device and validation enabled"]
fn default_metal_backend_draws_every_frame_natively() {
    verify_run(
        run(["right", "left", "right", "right"]),
        "metal",
        &["right", "left", "right", "right"],
        "RLRR",
        4,
    );
}

#[test]
#[ignore = "requires a native Metal device and validation enabled"]
fn replay_metal_matches_positional_stdout_and_ppms() {
    verify_replay_matches_directions(
        "metal",
        b"xenora-session-v1\nright\nleft\nright\nright",
        &["right", "left", "right", "right"],
        "RLRR",
        4,
    );
}
