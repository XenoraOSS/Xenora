// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt::{self, Write as _};
use std::io::{self, Read, Write as _};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};

use xenora_loader::startup::StartupEvent;

const SCHEMA_VERSION: u8 = 1;
const MAX_MESSAGE: usize = 1024;
const MAX_STREAM: usize = 2048;
const STAGE: u8 = 1;
const VERIFIED: u8 = 2;
const GUEST_EXIT: u8 = 3;
const FAILURE: u8 = 4;
const DEADLINE: u8 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Stage {
    Parsed,
    Mapped,
    Relocated,
    ImportsBound,
    TlsCompleted,
    EntryStarted,
    EntryReturned,
    FramebufferVerified,
}

const STAGES: [Stage; 8] = [
    Stage::Parsed,
    Stage::Mapped,
    Stage::Relocated,
    Stage::ImportsBound,
    Stage::TlsCompleted,
    Stage::EntryStarted,
    Stage::EntryReturned,
    Stage::FramebufferVerified,
];

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Self::Parsed => "parsed",
            Self::Mapped => "mapped",
            Self::Relocated => "relocated",
            Self::ImportsBound => "imports_bound",
            Self::TlsCompleted => "tls_completed",
            Self::EntryStarted => "entry_started",
            Self::EntryReturned => "entry_returned",
            Self::FramebufferVerified => "framebuffer_verified",
        }
    }

    fn completed(self) -> bool {
        self != Self::EntryStarted
    }
}

#[derive(Clone, Copy)]
#[repr(u8)]
pub(super) enum FailurePhase {
    Worker,
    Admission,
    Startup,
    Execution,
    Verification,
}

const FAILURE_PHASES: [FailurePhase; 5] = [
    FailurePhase::Worker,
    FailurePhase::Admission,
    FailurePhase::Startup,
    FailurePhase::Execution,
    FailurePhase::Verification,
];

impl FailurePhase {
    fn name(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Admission => "admission",
            Self::Startup => "startup",
            Self::Execution => "execution",
            Self::Verification => "verification",
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Reporter(Option<Arc<Mutex<Writer>>>);

struct Writer {
    channel: UnixStream,
    terminal: bool,
}

impl Reporter {
    pub(super) fn from_stdin() -> io::Result<Self> {
        // The supervisor uses worker fd 0 as a private socket. Guest stdout
        // and stderr never pass through this protocol.
        let fd = io::stdin().as_fd().try_clone_to_owned()?;
        let channel = UnixStream::from(fd);
        channel.peer_addr()?;
        Ok(Self(Some(Arc::new(Mutex::new(Writer {
            channel,
            terminal: false,
        })))))
    }

    pub(super) fn observed(&self, event: &StartupEvent) {
        self.stage(match event {
            StartupEvent::Parsed { .. } => Stage::Parsed,
            StartupEvent::Mapped { .. } => Stage::Mapped,
            StartupEvent::Relocated { .. } => Stage::Relocated,
            StartupEvent::ImportsBound { .. } => Stage::ImportsBound,
            StartupEvent::TlsCompleted { .. } => Stage::TlsCompleted,
            StartupEvent::Entering { .. } => Stage::EntryStarted,
        });
    }

    pub(super) fn stage(&self, stage: Stage) {
        self.send(STAGE, &[stage as u8]);
    }

    pub(super) fn verified(&self) {
        self.send(VERIFIED, &[]);
    }

    pub(super) fn guest_exit(&self, status: u32) {
        self.send(GUEST_EXIT, &status.to_le_bytes());
    }

    pub(super) fn deadline(&self) {
        self.send(DEADLINE, &[]);
    }

    pub(super) fn failed(&self, phase: FailurePhase, error: impl fmt::Display) {
        if self.0.is_none() {
            return;
        }
        let mut message = BoundedMessage::default();
        let _ = write!(message, "{error}");
        let mut payload = vec![phase as u8, u8::from(message.truncated)];
        payload.extend_from_slice(message.text.as_bytes());
        self.send(FAILURE, &payload);
    }

    fn send(&self, tag: u8, payload: &[u8]) {
        let Some(writer) = &self.0 else { return };
        let Ok(mut writer) = writer.lock() else {
            return;
        };
        if writer.terminal {
            return;
        }
        let mut record = Vec::with_capacity(3 + payload.len());
        record.push(tag);
        record.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        record.extend_from_slice(payload);
        // A failed or partial diagnostic write must not affect guest execution.
        // Stop emitting records if a valid boundary can no longer be assumed.
        let sent = writer.channel.write_all(&record).is_ok();
        writer.terminal = tag != STAGE || !sent;
    }
}

#[derive(Default)]
struct BoundedMessage {
    text: String,
    truncated: bool,
}

impl fmt::Write for BoundedMessage {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if !self.truncated {
            let until = value.floor_char_boundary(value.len().min(MAX_MESSAGE - self.text.len()));
            self.text.push_str(&value[..until]);
            self.truncated = until < value.len();
        }
        Ok(())
    }
}

pub(super) struct Collector(UnixStream);

impl Collector {
    pub(super) fn attach(command: &mut Command) -> io::Result<Self> {
        let (supervisor, worker) = UnixStream::pair()?;
        supervisor.set_nonblocking(true)?;
        worker.set_nonblocking(true)?;
        command.stdin(Stdio::from(OwnedFd::from(worker)));
        Ok(Self(supervisor))
    }

    pub(super) fn report(self, termination: Termination<'_>, program: bool) -> Report {
        let mut wire = Vec::with_capacity(MAX_STREAM + 1);
        let transport = self.0.take((MAX_STREAM + 1) as u64).read_to_end(&mut wire);
        let over_limit = wire.len() > MAX_STREAM;
        wire.truncate(MAX_STREAM);
        let mut report = Report::decode(&wire, program);
        if over_limit || transport.is_err_and(|error| error.kind() != io::ErrorKind::WouldBlock) {
            report.protocol = "malformed";
        }
        report.finish(termination, program);
        report
    }
}

#[derive(Clone, Copy)]
pub(super) enum Termination<'a> {
    Exited(ExitStatus),
    Deadline,
    SupervisorError(&'a dyn fmt::Display),
}

enum Outcome {
    Verified,
    GuestExit(u32),
    Failure {
        phase: FailurePhase,
        message: String,
        truncated: bool,
    },
    Deadline,
    Unknown,
    SupervisorError {
        message: String,
        truncated: bool,
    },
}

pub(super) struct Report {
    stages: Vec<Stage>,
    outcome: Outcome,
    protocol: &'static str,
    exit_code: Option<i32>,
    signal: Option<i32>,
}

impl Report {
    fn decode(mut wire: &[u8], program: bool) -> Self {
        let mut report = Self {
            stages: Vec::new(),
            outcome: Outcome::Unknown,
            protocol: "partial",
            exit_code: None,
            signal: None,
        };
        while !wire.is_empty() {
            if !matches!(report.outcome, Outcome::Unknown) {
                report.protocol = "malformed";
                break;
            }
            let Some(([tag, lo, hi], tail)) = wire.split_first_chunk::<3>() else {
                report.protocol = "truncated";
                break;
            };
            let size = usize::from(u16::from_le_bytes([*lo, *hi]));
            if size > MAX_MESSAGE + 2 {
                report.protocol = "malformed";
                break;
            }
            let Some((payload, rest)) = tail.split_at_checked(size) else {
                report.protocol = "truncated";
                break;
            };
            if !report.apply(*tag, payload, program) {
                report.protocol = "malformed";
                break;
            }
            wire = rest;
        }
        if report.protocol == "partial" && !matches!(report.outcome, Outcome::Unknown) {
            report.protocol = "complete";
        }
        report
    }

    fn apply(&mut self, tag: u8, payload: &[u8], program: bool) -> bool {
        let last = self.stages.last().copied();
        self.outcome = match (tag, payload) {
            (STAGE, [id])
                if STAGES.get(self.stages.len()).is_some_and(|stage| {
                    *id == *stage as u8 && !(program && *stage == Stage::FramebufferVerified)
                }) =>
            {
                self.stages.push(STAGES[self.stages.len()]);
                return true;
            }
            (VERIFIED, []) if !program && last == Some(Stage::FramebufferVerified) => {
                Outcome::Verified
            }
            (GUEST_EXIT, [a, b, c, d])
                if program && matches!(last, Some(Stage::TlsCompleted | Stage::EntryReturned)) =>
            {
                Outcome::GuestExit(u32::from_le_bytes([*a, *b, *c, *d]))
            }
            (FAILURE, [phase, truncated @ (0 | 1), message @ ..]) => {
                let Some(&phase) = FAILURE_PHASES.get(usize::from(*phase)) else {
                    return false;
                };
                let Ok(message) = std::str::from_utf8(message) else {
                    return false;
                };
                Outcome::Failure {
                    phase,
                    message: message.into(),
                    truncated: *truncated != 0,
                }
            }
            (DEADLINE, []) => Outcome::Deadline,
            _ => return false,
        };
        true
    }

    fn finish(&mut self, termination: Termination<'_>, program: bool) {
        match termination {
            Termination::Exited(status) => {
                self.exit_code = status.code();
                self.signal = status.signal();
                let expected = match self.outcome {
                    Outcome::Verified => Some(0),
                    Outcome::GuestExit(guest) => Some(i32::from(super::host_exit_code(guest))),
                    Outcome::Failure { .. } => Some(if program {
                        i32::from(super::FAILURE_CODE)
                    } else {
                        1
                    }),
                    Outcome::Deadline => Some(i32::from(super::DEADLINE_CODE)),
                    _ => None,
                };
                if expected.is_some() && (expected != status.code() || self.protocol != "complete")
                {
                    self.outcome = Outcome::Unknown;
                    if self.protocol == "complete" {
                        self.protocol = "malformed";
                    }
                }
            }
            Termination::Deadline => self.outcome = Outcome::Deadline,
            Termination::SupervisorError(error) => {
                let mut message = BoundedMessage::default();
                let _ = write!(message, "{error}");
                self.outcome = Outcome::SupervisorError {
                    message: message.text,
                    truncated: message.truncated,
                };
            }
        }
    }

    pub(super) fn json(&self) -> String {
        let mut json = format!("{{\"schema_version\":{SCHEMA_VERSION},\"events\":[");
        for (index, stage) in self.stages.iter().enumerate() {
            if index != 0 {
                json.push(',');
            }
            let _ = write!(
                json,
                "{{\"stage\":\"{}\",\"completed\":{}}}",
                stage.name(),
                stage.completed()
            );
        }
        json.push_str("],\"last_completed_stage\":");
        if let Some(stage) = self.stages.iter().rev().find(|stage| stage.completed()) {
            json_string(&mut json, stage.name());
        } else {
            json.push_str("null");
        }
        let _ = write!(
            json,
            ",\"protocol\":\"{}\",\"outcome\":{{\"kind\":",
            self.protocol
        );
        let kind = match self.outcome {
            Outcome::Verified => "verified",
            Outcome::GuestExit(_) => "guest_exit",
            Outcome::Failure { .. } => "known_failure",
            Outcome::Deadline => "wall_timeout",
            Outcome::Unknown => "unknown_worker_termination",
            Outcome::SupervisorError { .. } => "supervisor_error",
        };
        json_string(&mut json, kind);
        match &self.outcome {
            Outcome::GuestExit(guest) => {
                let _ = write!(json, ",\"guest_status\":{guest}");
            }
            Outcome::Failure {
                phase,
                message,
                truncated,
            } => {
                let _ = write!(json, ",\"phase\":\"{}\"", phase.name());
                append_message(&mut json, message, *truncated);
            }
            Outcome::SupervisorError { message, truncated } => {
                append_message(&mut json, message, *truncated)
            }
            _ => {}
        }
        if let Some(code) = self.exit_code {
            let _ = write!(json, ",\"worker_code\":{code}");
        }
        if let Some(signal) = self.signal {
            let _ = write!(json, ",\"worker_signal\":{signal}");
        }
        json.push_str("}}");
        json
    }
}

fn append_message(json: &mut String, message: &str, truncated: bool) {
    json.push_str(",\"message\":");
    json_string(json, message);
    let _ = write!(json, ",\"message_truncated\":{truncated}");
}

fn json_string(output: &mut String, value: &str) {
    output.push('"');
    for ch in value.chars() {
        match ch {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{0}'..='\u{1f}' => {
                let _ = write!(output, "\\u{:04x}", u32::from(ch));
            }
            _ => output.push(ch),
        }
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn fake_worker(script: &str, limit: Duration) -> Report {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        let collector = Collector::attach(&mut command).unwrap();
        let status =
            super::super::wait_for_worker(command.spawn().unwrap(), Instant::now(), limit).unwrap();
        collector.report(
            status.map_or(Termination::Deadline, Termination::Exited),
            true,
        )
    }

    #[test]
    fn signal_preserves_complete_records_and_identifies_early_worker_exit() {
        // The first record is Parsed. The second record starts a Mapped
        // event, but the process dies before sending its payload.
        let report = fake_worker(
            r#"printf '\001\001\000\000\001\001\000' >&0; kill -TERM "$$""#,
            Duration::from_secs(2),
        );
        let json = report.json();
        assert_eq!(report.stages, [Stage::Parsed]);
        assert_eq!(report.protocol, "truncated");
        assert_eq!(report.signal, Some(15));
        assert!(
            json.contains(r#""kind":"unknown_worker_termination","worker_signal":15"#),
            "{json}"
        );
    }

    #[test]
    fn supervisor_deadline_keeps_the_last_completed_stage() {
        let report = fake_worker(
            r#"printf '\001\001\000\000\001\001\000\001' >&0; while :; do :; done"#,
            Duration::from_millis(250),
        );
        assert_eq!(report.stages, [Stage::Parsed, Stage::Mapped]);
        assert_eq!(
            report.json(),
            r#"{"schema_version":1,"events":[{"stage":"parsed","completed":true},{"stage":"mapped","completed":true}],"last_completed_stage":"mapped","protocol":"partial","outcome":{"kind":"wall_timeout"}}"#
        );
    }

    #[test]
    fn malformed_record_does_not_jump_to_a_later_stage_or_synthesize_success() {
        for suffix in [
            &[1, 1, 0, 2, 1, 1, 0, 1][..], // Relocated without Mapped; a later record is ignored.
            &[42, 0, 0],                   // Unknown tag.
            &[1, 255, 255],                // Oversized record.
            &[4, 3, 0, 2, 0, 0xff],        // Invalid UTF-8 in failure message.
            &[2, 0, 0],                    // Fixture verification on a program stream.
        ] {
            let mut wire = vec![1, 1, 0, 0];
            wire.extend_from_slice(suffix);
            let report = Report::decode(&wire, true);
            assert_eq!(report.protocol, "malformed");
            assert_eq!(report.stages, [Stage::Parsed]);
            assert!(matches!(report.outcome, Outcome::Unknown));
        }
    }

    #[test]
    fn guest_exit_after_tls_needs_no_entry_event() {
        let mut wire = vec![];
        for stage in &STAGES[..=Stage::TlsCompleted as usize] {
            wire.extend([STAGE, 1, 0, *stage as u8]);
        }

        let guest = [GUEST_EXIT, 4, 0, 73, 0, 0, 0];
        let mut premature = wire[..wire.len() - 4].to_vec();
        premature.extend(guest);
        assert_eq!(Report::decode(&premature, true).protocol, "malformed");

        wire.extend(guest);
        let report = Report::decode(&wire, true);
        assert_eq!(report.stages.last(), Some(&Stage::TlsCompleted));
        assert_eq!(report.protocol, "complete");
        assert!(matches!(report.outcome, Outcome::GuestExit(73)));
    }

    #[test]
    fn a_worker_exit_code_alone_does_not_imply_a_timeout() {
        let report = fake_worker("exit 124", Duration::from_secs(2));
        assert_eq!(report.exit_code, Some(124));
        assert!(matches!(report.outcome, Outcome::Unknown));
    }

    #[test]
    fn diagnostic_transport_caps_storage() {
        let report = fake_worker(
            r#"printf '\001\001\000\000' >&0; /bin/dd if=/dev/zero bs=4096 count=1 >&0 2>/dev/null; exit 125"#,
            Duration::from_secs(2),
        );
        assert_eq!(report.protocol, "malformed");
        assert_eq!(report.stages, [Stage::Parsed]);
    }

    #[test]
    fn failure_text_preserves_utf8_and_escapes_control_characters() {
        let mut message = BoundedMessage::default();
        write!(message, "{}😀rest", "x".repeat(MAX_MESSAGE - 2)).unwrap();
        assert_eq!(message.text, "x".repeat(MAX_MESSAGE - 2));
        assert!(message.truncated);

        let mut escaped = String::new();
        json_string(&mut escaped, "ž\n\u{001b}\"\\");
        assert_eq!(escaped, r#""ž\u000a\u001b\"\\""#);
    }
}
