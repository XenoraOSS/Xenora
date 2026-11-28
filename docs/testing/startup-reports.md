# Structured startup reports

Pass `--startup-report` to `xenora run --experimental --pe <file>` to get
one line on standard error beginning with `xenora-startup `, followed by
a JSON object. Read the last line with that prefix after the process
exits. The supervisor writes a newline before the report so an unfinished
line from the worker cannot absorb it. If standard error is empty or already
ends in a newline, this appears as a blank line before the report. The
guest's output and shell exit code keep their usual meaning.

Version 1 has `schema_version`, an ordered `events` array, a
`last_completed_stage` string or null, a `protocol` status, and an
`outcome` object. Events have a `stage` and `completed` flag. In order,
they are `parsed`, `mapped`, `relocated`, `imports_bound`,
`tls_completed`, `entry_started`, `entry_returned`, and (for a validated
embedded draw) `framebuffer_verified`. `entry_started` marks the
transition into guest code, so a timeout during entry can still show
`tls_completed` as the last completed stage. `entry_returned` says the
CPU engine returned, including faults or step-limit exhaustion. If a
thread-local storage callback in a standalone PE exits the process, the last
completed stage is `tls_completed` and the report omits entry events. The
record reflects this run's observations. Static PE findings are reported by
the probe.

Explicit multi-image packages use the same stage names for whole-package
phases. Package loading has no single-image parse event stream, so the
runner emits `parsed` through `imports_bound` only after the package loader
proves that those phases completed. If the root executable is entered, the
entry observer proves that module TLS callbacks and DLL attach completed;
the report then includes `tls_completed` and `entry_started` before the
entry result. A DLL that returns false during attach is a `known_failure`
before TLS completion or entry. If a TLS callback or `DllMain` exits the
guest during package initialization, the report carries `guest_exit` with
`imports_bound` as the last completed stage. Package detach callbacks run
after the entry outcome and preserve its exit status.

Outcome kinds are `verified` for an embedded fixture with a verified
frame, `guest_exit` with the full 32-bit `guest_status`,
`known_failure` with the reporting `phase` and `message`,
`wall_timeout`, `unknown_worker_termination`, and `supervisor_error`.
A numeric `worker_code` or `worker_signal` appears for an observed worker
exit. Message text is limited to 1,024 UTF-8 bytes and carries
`message_truncated`. Software should use the stage and outcome fields
for decisions, and show messages as diagnostic text.

The worker inherits standard input with or without reporting. Piped input
has the same 1 MiB limit, is read to EOF before guest entry, and remains
subject to the wall deadline. Terminal input is treated as empty. The
worker sends diagnostic records over a separate private socket.
The supervisor inspects at most 2,048 diagnostic bytes and keeps only
the ordered, valid stage prefix. The protocol status is
`complete` when an intact outcome agrees with process termination,
`partial` when no outcome arrived, `truncated` for an unfinished
record, or `malformed` for invalid or contradictory data. The
supervisor's own deadline can determine an outcome even with a
partial stream. The existing wall limits and exact-byte admission
for embedded fixtures apply with or without the report.
