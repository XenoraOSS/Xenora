// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{self, BufRead, Read, Write};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use xenora::host_output::check_connection;

use super::session;
use super::{
    CompletedSession, Direction, Frame, MAX_EVENTS, MAX_REPLY_BYTES, Options, RGBA_BYTES, RunError,
    WALL_LIMIT, check_result, decode, encode, expected_native_draws, kill_and_reap,
    parse_direction, print_frame, wait_for_worker, worker_command,
};

// Private messages sent to stdout by the supervised worker. Directions go
// to its stdin as single bytes, with EOF requesting orderly guest shutdown.
const FRAME: u8 = b'F';
const FINISHED: u8 = b'E';
const IDLE_CHECK: Duration = Duration::from_millis(25);

fn read_command(input: &mut impl Read) -> Result<Option<Direction>, String> {
    let mut command = [0; 1];
    match input.read_exact(&mut command) {
        Ok(()) => match command[0] {
            b'L' => Ok(Some(Direction::Left)),
            b'R' => Ok(Some(Direction::Right)),
            _ => Err("invalid direction received by the worker".to_owned()),
        },
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(format!("could not read worker command: {error}")),
    }
}

pub(super) fn worker(
    options: &Options,
    mut input: impl Read + Send + 'static,
    mut output: impl Write,
) -> Result<(), String> {
    let (sender, commands) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("xenora-interactive-commands".to_owned())
        .spawn(move || {
            loop {
                let command = read_command(&mut input);
                let done = !matches!(command, Ok(Some(_)));
                if sender.send(command).is_err() || done {
                    return;
                }
            }
        })
        .map_err(|error| format!("could not read worker commands: {error}"))?;
    let result = session::run_with_input(
        options.workload,
        options.backend,
        options.steps,
        |session| loop {
            match commands.recv_timeout(IDLE_CHECK) {
                Ok(command) => break command,
                Err(RecvTimeoutError::Timeout) => session.check_idle()?,
                Err(RecvTimeoutError::Disconnected) => {
                    break Err("worker command reader disconnected".to_owned());
                }
            }
        },
        |frame| {
            output
                .write_all(&[FRAME])
                .and_then(|()| output.write_all(&frame.sequence.to_le_bytes()))
                .and_then(|()| output.write_all(&frame.rgba))
                .and_then(|()| output.flush())
                .map_err(|error| format!("could not send worker frame: {error}"))
        },
    )?;
    output
        .write_all(&[FINISHED])
        .and_then(|()| encode(&result, &mut output))
        .map_err(|error| format!("could not send worker reply: {error}"))
}

enum Reply {
    Frame(Frame),
    Finished(Vec<u8>),
}

fn read_reply(input: &mut impl Read) -> io::Result<Reply> {
    let mut tag = [0; 1];
    input.read_exact(&mut tag)?;
    match tag[0] {
        FRAME => {
            let mut sequence = [0; 4];
            let mut rgba = vec![0; RGBA_BYTES];
            input.read_exact(&mut sequence)?;
            input.read_exact(&mut rgba)?;
            Ok(Reply::Frame(Frame {
                sequence: u32::from_le_bytes(sequence),
                rgba,
            }))
        }
        FINISHED => {
            let mut reply = Vec::new();
            input
                .take((MAX_REPLY_BYTES + 1) as u64)
                .read_to_end(&mut reply)?;
            Ok(Reply::Finished(reply))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown worker message",
        )),
    }
}

fn read_replies(mut input: impl Read, replies: Sender<io::Result<Reply>>) {
    // At most 32 frames and a terminal record can be queued, even if the
    // worker sends data while the supervisor is waiting on user input.
    for _ in 0..=MAX_EVENTS {
        let reply = read_reply(&mut input);
        let done = !matches!(reply, Ok(Reply::Frame(_)));
        if replies.send(reply).is_err() || done {
            return;
        }
    }
}

fn receive(replies: &Receiver<io::Result<Reply>>, start: Instant) -> Result<Reply, RunError> {
    loop {
        check_output()?;
        let remaining = WALL_LIMIT.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(RunError::Deadline);
        }
        match replies.recv_timeout(remaining.min(IDLE_CHECK)) {
            Ok(reply) => {
                return reply.map_err(|error| {
                    RunError::Runtime(format!("could not read the session worker: {error}"))
                });
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(RunError::Runtime(
                    "session worker disconnected without a reply".to_owned(),
                ));
            }
        }
    }
}

fn check_output() -> Result<(), RunError> {
    check_connection()
        .map_err(|error| RunError::Runtime(format!("could not write result: {error}")))
}

fn read_direction(input: &mut impl BufRead, line: usize) -> Result<Option<Direction>, RunError> {
    // The longest valid line is "right\r\n". Read at most one byte more
    // than that so a stream without newlines cannot accumulate unbounded data.
    let mut bytes = Vec::with_capacity(8);
    if input
        .take(8)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| {
            RunError::Input(format!("could not read directions from stdin: {error}"))
        })?
        == 0
    {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    std::str::from_utf8(&bytes)
        .ok()
        .and_then(parse_direction)
        .map(Some)
        .ok_or_else(|| RunError::Input(format!("stdin line {line}: expected left or right")))
}

fn next_direction(
    input: &Receiver<Result<Option<Direction>, RunError>>,
    replies: Option<&Receiver<io::Result<Reply>>>,
) -> Result<Option<Direction>, RunError> {
    loop {
        check_output()?;
        match input.recv_timeout(IDLE_CHECK) {
            Ok(direction) => return direction,
            Err(RecvTimeoutError::Timeout) => match replies.map(Receiver::try_recv) {
                None | Some(Err(TryRecvError::Empty)) => {}
                Some(Ok(Err(error))) => {
                    return Err(RunError::Runtime(format!(
                        "session worker stopped while waiting for input: {error}"
                    )));
                }
                Some(Ok(Ok(_)) | Err(TryRecvError::Disconnected)) => {
                    return Err(RunError::Runtime(
                        "session worker stopped while waiting for input".to_owned(),
                    ));
                }
            },
            Err(RecvTimeoutError::Disconnected) => {
                return Err(RunError::Input("stdin reader disconnected".to_owned()));
            }
        }
    }
}

fn forward(
    options: &Options,
    child: &mut Child,
    replies: &Receiver<io::Result<Reply>>,
    input: &Receiver<Result<Option<Direction>, RunError>>,
    mut direction: Option<Direction>,
    mut output: impl Write,
) -> Result<CompletedSession, RunError> {
    let mut commands = child.stdin.take().expect("piped worker stdin");
    let mut directions = Vec::new();
    let mut frames = Vec::new();
    while let Some(selected) = direction {
        if directions.len() == MAX_EVENTS {
            return Err(RunError::Input(format!(
                "specify at most {MAX_EVENTS} directions"
            )));
        }
        let start = Instant::now();
        let command = match selected {
            Direction::Left => b'L',
            Direction::Right => b'R',
        };
        commands.write_all(&[command]).map_err(|error| {
            RunError::Runtime(format!(
                "could not send direction to the session worker: {error}"
            ))
        })?;
        let Reply::Frame(frame) = receive(replies, start)? else {
            return Err(RunError::Runtime(
                "session worker finished before presenting a frame".to_owned(),
            ));
        };
        if frame.sequence as usize != frames.len() + 1 {
            return Err(RunError::Runtime(
                "worker returned an out-of-order live frame".to_owned(),
            ));
        }
        print_frame(options.backend, &frame, selected, &mut output)
            .and_then(|()| output.flush())
            .map_err(|error| RunError::Runtime(format!("could not write result: {error}")))?;
        directions.push(selected);
        frames.push(frame);
        direction = next_direction(input, Some(replies))?;
    }

    let start = Instant::now();
    drop(commands);
    let Reply::Finished(reply) = receive(replies, start)? else {
        return Err(RunError::Runtime(
            "session worker sent an unrequested frame".to_owned(),
        ));
    };
    let status = wait_for_worker(child, start)?;
    if !status.success() {
        return Err(RunError::Runtime(format!(
            "session worker exited with {status}"
        )));
    }
    let result = decode(&reply, directions.len()).map_err(RunError::Runtime)?;
    if result.frames != frames {
        return Err(RunError::Runtime(
            "final worker reply disagrees with the streamed frames".to_owned(),
        ));
    }
    let native_draws = expected_native_draws(&options.backend, directions.len());
    check_result(&result, options.workload, native_draws, &directions)
        .map_err(RunError::Runtime)?;
    Ok(result)
}

pub(super) fn supervise(
    options: &Options,
    mut input: impl BufRead + Send + 'static,
    output: impl Write,
) -> Result<CompletedSession, RunError> {
    let (directions, pending) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("xenora-interactive-stdin".to_owned())
        .spawn(move || {
            for line in 1..=MAX_EVENTS + 1 {
                let command = read_direction(&mut input, line);
                let done = !matches!(command, Ok(Some(_)));
                if directions.send(command).is_err() || done {
                    return;
                }
            }
        })
        .map_err(|error| RunError::Input(format!("could not read stdin: {error}")))?;
    let first_direction = next_direction(&pending, None)?;
    let mut child = worker_command(options)?
        .arg("--interactive")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| {
            RunError::Runtime(format!("could not start the session worker: {error}"))
        })?;
    let stdout = child.stdout.take().expect("piped worker stdout");
    let (sender, replies) = mpsc::channel();
    let reader = match thread::Builder::new()
        .name("xenora-interactive-replies".to_owned())
        .spawn(move || read_replies(stdout, sender))
    {
        Ok(reader) => reader,
        Err(error) => {
            let _ = kill_and_reap(&mut child);
            return Err(RunError::Runtime(format!(
                "could not read the session worker: {error}"
            )));
        }
    };
    let result = forward(
        options,
        &mut child,
        &replies,
        &pending,
        first_direction,
        output,
    );
    if result.is_err() {
        let _ = kill_and_reap(&mut child);
    }
    reader
        .join()
        .map_err(|_| RunError::Runtime("session reply reader stopped unexpectedly".to_owned()))?;
    result
}
