// SPDX-License-Identifier: GPL-3.0-or-later

mod replay;
mod session;

use session::{Backend, CompletedSession, Direction, Frame, HEIGHT, MAX_EVENTS, MAX_STEPS, WIDTH};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const USAGE: &str = "Usage: xenora-session [--backend metal|reference] [--steps N] [--frame-dir PATH] [--replay PATH | left|right ...]\n\
Runs the embedded guest in one bounded session. Defaults to Metal, 1000000 steps and left right left.\n\
Specify up to 32 directions, or replay a xenora-session-v1 file.\n\
--frame-dir creates a new directory of PPM frames after a successful session.";
const WORKER: &str = "--session-worker";
const WALL_LIMIT: Duration = Duration::from_secs(15);
const RGBA_BYTES: usize = WIDTH * HEIGHT * 4;
const REPLY_MAGIC: &[u8; 4] = b"XSP1";
const MAX_REPLY_BYTES: usize = REPLY_MAGIC.len() + 3 * 4 + MAX_EVENTS * (4 + RGBA_BYTES + 1);

struct Options {
    backend: Backend,
    steps: usize,
    directions: Vec<Direction>,
    frame_dir: Option<PathBuf>,
}

enum Request {
    Help,
    Run(Options),
}

fn backend_name(backend: &Backend) -> &'static str {
    match backend {
        Backend::Metal => "metal",
        Backend::Reference => "reference",
    }
}

fn direction_name(direction: &Direction) -> &'static str {
    match direction {
        Direction::Left => "left",
        Direction::Right => "right",
    }
}

fn direction_byte(direction: &Direction) -> u8 {
    match direction {
        Direction::Left => b'L',
        Direction::Right => b'R',
    }
}

fn parse_direction(value: &str) -> Option<Direction> {
    match value {
        "left" => Some(Direction::Left),
        "right" => Some(Direction::Right),
        _ => None,
    }
}

fn parse_path(value: Option<OsString>, option: &str) -> Result<PathBuf, String> {
    value
        .filter(|value| !value.is_empty() && !value.as_encoded_bytes().starts_with(b"--"))
        .map(PathBuf::from)
        .ok_or_else(|| format!("{option} requires a path"))
}

fn parse(args: impl IntoIterator<Item = OsString>, worker_mode: bool) -> Result<Request, String> {
    let mut args = args.into_iter();
    let mut backend = None;
    let mut steps = None;
    let mut frame_dir = None;
    let mut replay_path = None;
    let mut directions = Vec::new();
    let mut positional = false;
    while let Some(arg) = args.next() {
        match (arg.to_str(), positional) {
            (Some("--help"), false) => return Ok(Request::Help),
            (Some("--"), false) => positional = true,
            (Some("--backend"), false) if backend.is_none() => {
                backend = Some(
                    match args.next().as_deref().and_then(|value| value.to_str()) {
                        Some("metal") => Backend::Metal,
                        Some("reference") => Backend::Reference,
                        _ => return Err("--backend requires metal or reference".into()),
                    },
                );
            }
            (Some("--steps"), false) if steps.is_none() => {
                steps = Some(
                    args.next()
                        .as_deref()
                        .and_then(|value| value.to_str())
                        .and_then(|value| value.parse().ok())
                        .filter(|value| (1..=MAX_STEPS).contains(value))
                        .ok_or("--steps requires an integer in 1..=1000000")?,
                );
            }
            (Some("--frame-dir"), false) if frame_dir.is_none() => {
                frame_dir = Some(parse_path(args.next(), "--frame-dir")?);
            }
            (Some("--replay"), false) if worker_mode => {
                return Err("--replay is available only in the session supervisor".into());
            }
            (Some("--replay"), false) if replay_path.is_none() => {
                replay_path = Some(parse_path(args.next(), "--replay")?);
            }
            (Some("--backend" | "--steps" | "--frame-dir" | "--replay"), false) => {
                return Err(format!("duplicate option {:?}", arg));
            }
            (value, _) => directions.push(
                value
                    .and_then(parse_direction)
                    .ok_or_else(|| format!("unexpected argument {:?}", arg))?,
            ),
        }
        if directions.len() > MAX_EVENTS {
            return Err(format!("specify at most {MAX_EVENTS} directions"));
        }
    }
    if let Some(path) = replay_path {
        if !directions.is_empty() {
            return Err("--replay cannot be combined with positional directions".into());
        }
        directions = replay::read_directions(&path)?;
    } else if directions.is_empty() {
        directions.extend([Direction::Left, Direction::Right, Direction::Left]);
    }
    Ok(Request::Run(Options {
        backend: backend.unwrap_or(Backend::Metal),
        steps: steps.unwrap_or(MAX_STEPS),
        directions,
        frame_dir,
    }))
}

fn expected_native_draws(backend: &Backend, frames: usize) -> usize {
    match backend {
        Backend::Metal => frames,
        Backend::Reference => 0,
    }
}

fn check_result(
    result: &CompletedSession,
    native_draws: usize,
    directions: &[Direction],
) -> Result<(), String> {
    let count = directions.len();
    if result.exit_status != 0
        || result.open_handles != 0
        || result.native_draws != native_draws
        || result.frames.len() != count
        || !result
            .journal
            .iter()
            .copied()
            .eq(directions.iter().map(direction_byte))
    {
        return Err("worker returned inconsistent session counters or journal".into());
    }
    for (index, frame) in result.frames.iter().enumerate() {
        if frame.sequence as usize != index + 1 || frame.rgba.len() != RGBA_BYTES {
            return Err("worker returned an invalid frame record".into());
        }
    }
    Ok(())
}

// The worker's stdout is a private, bounded wire format. Text and frame files
// are emitted by the supervisor only after the worker has exited successfully.
fn encode(result: &CompletedSession, mut output: impl Write) -> io::Result<()> {
    output.write_all(REPLY_MAGIC)?;
    for value in [
        result.exit_status,
        result.open_handles as u32,
        result.native_draws as u32,
    ] {
        output.write_all(&value.to_le_bytes())?;
    }
    output.write_all(&result.journal)?;
    for frame in &result.frames {
        output.write_all(&frame.sequence.to_le_bytes())?;
        output.write_all(&frame.rgba)?;
    }
    output.flush()
}

fn decode(mut input: &[u8], count: usize) -> Result<CompletedSession, String> {
    fn read_u32(input: &mut &[u8]) -> io::Result<u32> {
        let mut data = [0; 4];
        input.read_exact(&mut data)?;
        Ok(u32::from_le_bytes(data))
    }

    let expected = REPLY_MAGIC.len() + 3 * 4 + count * (4 + RGBA_BYTES + 1);
    if input.len() != expected || !input.starts_with(REPLY_MAGIC) {
        return Err("worker sent a malformed or incomplete reply".into());
    }
    input = &input[REPLY_MAGIC.len()..];
    let mut read_reply = || -> io::Result<CompletedSession> {
        let exit_status = read_u32(&mut input)?;
        let open_handles = read_u32(&mut input)? as usize;
        let native_draws = read_u32(&mut input)? as usize;
        let mut journal = vec![0; count];
        input.read_exact(&mut journal)?;
        let mut frames = Vec::with_capacity(count);
        for _ in 0..count {
            let sequence = read_u32(&mut input)?;
            let mut rgba = vec![0; RGBA_BYTES];
            input.read_exact(&mut rgba)?;
            frames.push(Frame { sequence, rgba });
        }
        Ok(CompletedSession {
            frames,
            journal,
            exit_status,
            open_handles,
            native_draws,
        })
    };
    read_reply().map_err(|error| format!("could not decode worker reply: {error}"))
}

fn worker(options: Options) -> Result<(), String> {
    let native_draws = expected_native_draws(&options.backend, options.directions.len());
    let result = session::run(options.backend, options.steps, &options.directions)?;
    check_result(&result, native_draws, &options.directions)?;
    encode(&result, io::stdout().lock())
        .map_err(|error| format!("could not send worker reply: {error}"))
}

enum RunError {
    Deadline,
    Runtime(String),
}

fn kill_and_reap(child: &mut Child) -> io::Result<()> {
    // The process may already have exited between try_wait and kill.
    let _ = child.kill();
    child.wait().map(|_| ())
}

fn supervise(options: &Options) -> Result<CompletedSession, RunError> {
    let exe = std::env::current_exe().map_err(|error| {
        RunError::Runtime(format!("could not locate the session worker: {error}"))
    })?;
    let mut command = Command::new(exe);
    command
        .args([
            WORKER,
            "--backend",
            backend_name(&options.backend),
            "--steps",
        ])
        .arg(options.steps.to_string())
        .args(options.directions.iter().map(direction_name))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let start = Instant::now();
    let mut child = command.spawn().map_err(|error| {
        RunError::Runtime(format!("could not start the session worker: {error}"))
    })?;
    let stdout = child.stdout.take().expect("piped worker stdout");
    let reader = match thread::Builder::new().spawn(move || {
        let mut reply = Vec::with_capacity(MAX_REPLY_BYTES);
        stdout
            .take((MAX_REPLY_BYTES + 1) as u64)
            .read_to_end(&mut reply)?;
        Ok::<_, io::Error>(reply)
    }) {
        Ok(reader) => reader,
        Err(error) => {
            let _ = kill_and_reap(&mut child);
            return Err(RunError::Runtime(format!(
                "could not read the session worker: {error}"
            )));
        }
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < WALL_LIMIT => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                kill_and_reap(&mut child).map_err(|error| {
                    RunError::Runtime(format!("could not reap the timed-out worker: {error}"))
                })?;
                return Err(RunError::Deadline);
            }
            Err(error) => {
                let _ = kill_and_reap(&mut child);
                return Err(RunError::Runtime(format!(
                    "could not wait for the session worker: {error}"
                )));
            }
        }
    };
    if !status.success() {
        return Err(RunError::Runtime(format!(
            "session worker exited with {status}"
        )));
    }
    let reply = reader
        .join()
        .map_err(|_| RunError::Runtime("session reply reader stopped unexpectedly".into()))?
        .map_err(|error| RunError::Runtime(format!("could not read worker reply: {error}")))?;
    let result = decode(&reply, options.directions.len()).map_err(RunError::Runtime)?;
    let native_draws = expected_native_draws(&options.backend, options.directions.len());
    check_result(&result, native_draws, &options.directions).map_err(RunError::Runtime)?;
    Ok(result)
}

#[derive(Debug)]
struct PendingExport<'a> {
    dir: &'a Path,
    created: Vec<PathBuf>,
    committed: bool,
}

impl PendingExport<'_> {
    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for PendingExport<'_> {
    fn drop(&mut self) {
        if !self.committed {
            for path in &self.created {
                let _ = fs::remove_file(path);
            }
            let _ = fs::remove_dir(self.dir);
        }
    }
}

fn write_frames<'a>(dir: &'a Path, frames: &[Frame]) -> io::Result<PendingExport<'a>> {
    fs::create_dir(dir)?;
    let mut export = PendingExport {
        dir,
        created: Vec::with_capacity(frames.len()),
        committed: false,
    };
    for frame in frames {
        let path = dir.join(format!("frame-{:04}.ppm", frame.sequence));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        export.created.push(path);
        write!(file, "P6\n{WIDTH} {HEIGHT}\n255\n")?;
        for pixel in frame.rgba.as_chunks::<4>().0 {
            file.write_all(&pixel[..3])?;
        }
    }
    Ok(export)
}

fn print_result(
    options: &Options,
    result: &CompletedSession,
    mut output: impl Write,
) -> io::Result<()> {
    for (frame, direction) in result.frames.iter().zip(&options.directions) {
        let mut rgba = String::with_capacity(RGBA_BYTES * 2);
        for byte in &frame.rgba {
            write!(rgba, "{byte:02x}").expect("writing to a string");
        }
        writeln!(
            output,
            "frame sequence={} direction={} backend={} width={WIDTH} height={HEIGHT} draws=1 rgba={rgba}",
            frame.sequence,
            direction_name(direction),
            backend_name(&options.backend),
        )?;
    }
    writeln!(
        output,
        "exit status={} frames={} journal={} handles={} native_draws={}",
        result.exit_status,
        result.frames.len(),
        String::from_utf8_lossy(&result.journal),
        result.open_handles,
        result.native_draws,
    )?;
    output.flush()
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1).peekable();
    let worker_mode = args.peek().is_some_and(|arg| arg == WORKER);
    if worker_mode {
        args.next();
    }
    let options = match parse(args, worker_mode) {
        Ok(Request::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Request::Run(options)) => options,
        Err(error) => {
            eprintln!("xenora-session: {error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if worker_mode {
        return match worker(options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("xenora-session worker: {error}");
                ExitCode::from(125)
            }
        };
    }
    if let Some(dir) = &options.frame_dir {
        match fs::symlink_metadata(dir) {
            Ok(_) => {
                eprintln!(
                    "xenora-session: frame directory already exists: {}",
                    dir.display()
                );
                return ExitCode::from(2);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                eprintln!(
                    "xenora-session: could not check frame directory {}: {error}",
                    dir.display()
                );
                return ExitCode::from(2);
            }
        }
    }

    let result = match supervise(&options) {
        Ok(result) => result,
        Err(RunError::Deadline) => {
            eprintln!(
                "xenora-session: worker exceeded the {} second wall deadline",
                WALL_LIMIT.as_secs()
            );
            return ExitCode::from(124);
        }
        Err(RunError::Runtime(error)) => {
            eprintln!("xenora-session: {error}");
            return ExitCode::from(125);
        }
    };
    let export = if let Some(dir) = &options.frame_dir {
        match write_frames(dir, &result.frames) {
            Ok(export) => Some(export),
            Err(error) => {
                eprintln!(
                    "xenora-session: could not export frames to {}: {error}",
                    dir.display()
                );
                if error.kind() == io::ErrorKind::AlreadyExists {
                    return ExitCode::from(2);
                }
                return ExitCode::from(125);
            }
        }
    } else {
        None
    };
    if let Err(error) = print_result(&options, &result, io::stdout().lock()) {
        eprintln!("xenora-session: could not write result: {error}");
        return ExitCode::from(125);
    }
    if let Some(export) = export {
        export.commit();
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_frame(sequence: u32) -> Frame {
        Frame {
            sequence,
            rgba: vec![0; RGBA_BYTES],
        }
    }

    #[test]
    fn incomplete_or_oversized_worker_replies_are_refused() {
        let session = CompletedSession {
            frames: vec![example_frame(1)],
            journal: vec![b'L'],
            exit_status: 0,
            open_handles: 0,
            native_draws: 0,
        };
        let mut reply = Vec::new();
        encode(&session, &mut reply).unwrap();
        assert!(decode(&reply, 1).is_ok());
        assert!(decode(&reply[..reply.len() - 1], 1).is_err());
        reply.push(0);
        assert!(decode(&reply, 1).is_err());
    }

    #[test]
    fn a_failed_export_removes_the_files_it_created() {
        let dir =
            std::env::temp_dir().join(format!("xenora-session-rollback-{}", std::process::id()));
        let duplicate = [example_frame(1), example_frame(1)];
        assert_eq!(
            write_frames(&dir, &duplicate).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(!dir.exists());
    }
}
