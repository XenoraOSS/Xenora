// SPDX-License-Identifier: GPL-3.0-or-later

//! Keep potentially blocking writes to a host pipe off the supervisor thread.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::os::fd::AsFd;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::Instant;

use sha2::{Digest, Sha256};

use super::supervisor::Failure;

pub(super) enum Report {
    Ready,
    Frame {
        sequence: usize,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        pixels: bool,
    },
    Done(usize),
}

pub(super) struct HostOutput {
    reports: SyncSender<Report>,
    written: Receiver<io::Result<()>>,
}

impl HostOutput {
    pub(super) fn start() -> io::Result<Self> {
        // Use an owned duplicate so a stalled write holds no global stdout lock.
        let output = File::from(io::stdout().as_fd().try_clone_to_owned()?);
        let (reports, pending) = sync_channel(1);
        let (completed, written) = sync_channel(1);
        std::thread::Builder::new().spawn(move || {
            let mut output = BufWriter::new(output);
            for report in pending {
                let result = write_report(&mut output, report).and_then(|()| output.flush());
                let failed = result.is_err();
                if completed.send(result).is_err() || failed {
                    break;
                }
            }
        })?;
        Ok(Self { reports, written })
    }

    pub(super) fn publish(
        &self,
        report: Report,
        deadline: Instant,
        phase: &'static str,
    ) -> Result<(), Failure> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(Failure::Deadline(phase))?;
        self.reports
            .try_send(report)
            .map_err(|_| Failure::Session("host output channel closed".into()))?;
        match self.written.recv_timeout(remaining) {
            Ok(result) => Ok(result?),
            Err(RecvTimeoutError::Timeout) => Err(Failure::Deadline(phase)),
            Err(RecvTimeoutError::Disconnected) => {
                Err(Failure::Session("host output channel closed".into()))
            }
        }
    }
}

fn write_report(output: &mut impl Write, report: Report) -> io::Result<()> {
    match report {
        Report::Ready => writeln!(output, "ready"),
        Report::Done(frames) => writeln!(output, "done frames={frames}"),
        Report::Frame {
            sequence,
            width,
            height,
            rgba,
            pixels,
        } => {
            let digest = Sha256::digest(&rgba);
            write!(
                output,
                "frame {sequence} width={width} height={height} sha256={digest:x}"
            )?;
            if pixels {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                output.write_all(b" rgba=")?;
                for byte in rgba {
                    output
                        .write_all(&[HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 15)]])?;
                }
            }
            writeln!(output)
        }
    }
}

pub(super) use xenora::host_output::check_connection;
