// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{self, BufRead, BufReader, Read};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError, sync_channel};
use std::time::{Duration, Instant};

use super::Options;
use super::host_output::{self, HostOutput, Report};
use super::wire::{self, MAX_FRAMES, Reply, Request};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_INPUT_LINE: u64 = 12;

#[derive(Debug)]
pub(super) enum Failure {
    Deadline(&'static str),
    Session(String),
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::Session(error.to_string())
    }
}

struct WorkerProcess {
    child: Child,
    requests: ChildStdin,
    replies: Receiver<io::Result<Option<Reply>>>,
}

impl WorkerProcess {
    fn start(options: &Options) -> io::Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--experimental", "--worker", "--pe"])
            .arg(&options.executable.path)
            .arg("--sha256")
            .arg(options.executable.digest_hex())
            .args(["--graphics-backend", options.backend.name(), "--max-steps"])
            .arg(options.steps.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        for companion in &options.companions {
            command
                .arg("--package")
                .arg(&companion.path)
                .arg(companion.digest_hex());
        }
        if options.deterministic_clock {
            command.arg("--deterministic-clock");
        }
        let mut child = command.spawn()?;
        let requests = child.stdin.take().expect("worker stdin was piped");
        let stdout = child.stdout.take().expect("worker stdout was piped");
        let (send, replies) = sync_channel(1);
        let worker = Self {
            child,
            requests,
            replies,
        };
        std::thread::Builder::new().spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let reply = wire::read_reply(&mut stdout);
                let more = matches!(reply, Ok(Some(_)));
                if send.send(reply).is_err() || !more {
                    break;
                }
            }
        })?;
        Ok(worker)
    }

    fn receive(&self, deadline: Instant, phase: &'static str) -> Result<Option<Reply>, Failure> {
        loop {
            // The host can stop reading while the guest is still executing.
            // Keep the phase deadline while polling for that disconnection.
            host_output::check_connection()?;
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(Failure::Deadline(phase))?;
            match self.replies.recv_timeout(remaining.min(POLL_INTERVAL)) {
                Ok(reply) => return Ok(reply?),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Failure::Session("worker reply channel closed".into()));
                }
            }
        }
    }

    fn wait_for_input(
        &mut self,
        inputs: &Receiver<io::Result<Request>>,
    ) -> Result<Request, Failure> {
        loop {
            host_output::check_connection()?;
            if let Some(status) = self.child.try_wait()? {
                return Err(Failure::Session(format!(
                    "worker exited while waiting for input ({status})"
                )));
            }
            if !matches!(self.replies.try_recv(), Err(TryRecvError::Empty)) {
                return Err(Failure::Session(
                    "worker left its input boundary before a request".into(),
                ));
            }
            match inputs.recv_timeout(POLL_INTERVAL) {
                Ok(request) => return Ok(request?),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Failure::Session("host input channel closed".into()));
                }
            }
        }
    }

    fn finish(&mut self, deadline: Instant) -> Result<(), Failure> {
        if !matches!(self.receive(deadline, "shutdown")?, Some(Reply::Done)) {
            return Err(Failure::Session("worker did not finish the session".into()));
        }
        if self.receive(deadline, "shutdown")?.is_some() {
            return Err(Failure::Session("worker replied after Done".into()));
        }
        loop {
            host_output::check_connection()?;
            if let Some(status) = self.child.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(Failure::Session(format!("worker exited with {status}")))
                };
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(Failure::Deadline("shutdown"))?;
            std::thread::sleep(remaining.min(POLL_INTERVAL));
        }
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn read_input(reader: &mut impl BufRead) -> io::Result<Request> {
    let mut line = Vec::new();
    reader
        .take(MAX_INPUT_LINE + 1)
        .read_until(b'\n', &mut line)?;
    if line.is_empty() {
        return Ok(Request::Stop);
    }
    let value = line
        .strip_suffix(b"\n")
        .and_then(|line| {
            let digits = line.strip_suffix(b"\r").unwrap_or(line);
            if !(1..=10).contains(&digits.len()) || !digits.iter().all(u8::is_ascii_digit) {
                return None;
            }
            std::str::from_utf8(digits).ok()?.parse().ok()
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected one unsigned 32-bit decimal value followed by a newline",
            )
        })?;
    Ok(Request::Input(value))
}

fn read_host_requests() -> io::Result<Receiver<io::Result<Request>>> {
    let (send, inputs) = sync_channel(1);
    std::thread::Builder::new().spawn(move || {
        let mut input = io::stdin().lock();
        loop {
            let request = read_input(&mut input);
            let more = matches!(request, Ok(Request::Input(_)));
            if send.send(request).is_err() || !more {
                break;
            }
        }
    })?;
    Ok(inputs)
}

pub(super) fn run(options: &Options) -> Result<(), Failure> {
    let deadline = Instant::now() + options.phase_limit;
    let mut worker = WorkerProcess::start(options)?;
    if !matches!(worker.receive(deadline, "startup")?, Some(Reply::Ready)) {
        return Err(Failure::Session(
            "worker did not reach the initial input boundary".into(),
        ));
    }

    let inputs = read_host_requests()?;
    let output = HostOutput::start()?;
    output.publish(Report::Ready, deadline, "startup")?;
    let mut frames = 0;
    loop {
        let request = worker.wait_for_input(&inputs)?;
        let deadline = Instant::now() + options.phase_limit;
        if matches!(request, Request::Stop) {
            wire::write_request(&mut worker.requests, request)?;
            worker.finish(deadline)?;
            output.publish(Report::Done(frames), deadline, "shutdown")?;
            return Ok(());
        }
        if frames == MAX_FRAMES {
            return Err(Failure::Session(format!(
                "session accepts at most {MAX_FRAMES} inputs"
            )));
        }
        wire::write_request(&mut worker.requests, request)?;
        let Some(Reply::Frame {
            width,
            height,
            rgba,
        }) = worker.receive(deadline, "frame")?
        else {
            return Err(Failure::Session("worker did not deliver a frame".into()));
        };
        if !matches!(worker.receive(deadline, "frame")?, Some(Reply::Ready)) {
            return Err(Failure::Session(
                "worker did not return to its input boundary after a frame".into(),
            ));
        }
        frames += 1;
        output.publish(
            Report::Frame {
                sequence: frames,
                width,
                height,
                rgba,
                pixels: options.pixels,
            },
            deadline,
            "frame",
        )?;
    }
}
