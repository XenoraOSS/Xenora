// SPDX-License-Identifier: GPL-3.0-or-later

use std::error::Error;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{IsTerminal, Read, Write};
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use xenora_loader::scheduler::DEFAULT_QUANTUM;
use xenora_loader::startup::{
    StartupEvent, StartupLayout, run_synthetic_startup_scheduled_with_budget,
    run_synthetic_startup_with_observer,
};
use xenora_loader::{
    ExitReason, ImportRegistry, Register, ServicesState, SystemClock, register_standard_services,
};
use xenora_metal::NativeCompute;
use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};

#[path = "runner_diagnostics.rs"]
mod diagnostics;
#[path = "runner_export.rs"]
mod export;
use diagnostics::{Collector, FailurePhase, Reporter, Stage, Termination};
use export::SupervisorExport;

const FIXTURE: &[u8] = include_bytes!("../../../fixtures/pe/trusted-runner.exe");
const SERVICES_FIXTURE: &[u8] = include_bytes!("../../../fixtures/pe/runtime-services.exe");
const WALL_LIMIT: Duration = Duration::from_secs(15);
const DEFAULT_STEPS: usize = 100;
const MAX_STEPS: usize = 10_000;
const IMAGE_BASE: u64 = 0x150000000;
const TICKET_ADDRESS: u64 = IMAGE_BASE + 0x3000;
const TICKET: u64 = 17;
const DRAW_RESULT: u64 = 0x42;
const EXTENT: [usize; 2] = [4, 3];
const EXPECTED_PIXEL: [u8; 4] = [255, 0, 0, 255];
/// The longest wall deadline a program may be given with `--wall-seconds`.
/// Its default is `WALL_LIMIT`, and it has no step limit unless one is given.
const MAX_WALL_SECONDS: u64 = 3600;
/// Default host memory budget for the executable snapshot.
const DEFAULT_EXECUTABLE_BUDGET: u64 = 1024 * 1024 * 1024;
/// How long the supervisor waits past a program's deadline for the worker to
/// show the program's output and stop on its own.
const DEADLINE_GRACE: Duration = Duration::from_secs(2);
/// Exit codes of a program run that are not the guest's own, after
/// `timeout(1)`: the wall deadline, and any other failure of the runner or
/// the guest.
const DEADLINE_CODE: u8 = 124;
const FAILURE_CODE: u8 = 125;
const USAGE: &str = "usage: xenora run --pe <executable> --experimental [--package <module>]... [--module <name>=<path>]... [--trace] [--startup-report] [--max-steps <n>] [--wall-seconds <1..=3600>] [--sha256 <64 hex digits>] [--max-executable-bytes <n>] [--max-mapped-image-bytes <n>] [--export-guest-file <guest-path> <host-path>] [-- <program argument>...]
       --package names another explicit image to load beside the entry executable; it may be repeated and is never searched for on the host
       --module admits a bounded logical name for a regular-file image held in the closed-world provisioned store; it is not loaded until a later runtime-load step
       a hosted export copies up to 16 MiB from the synthetic C: drive to a new UTF-8 filename in an existing directory
       an embedded fixture takes --metal-hud instead of --wall-seconds, and its --max-steps is in 1..=10000";

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProvisionedModuleArg {
    name: String,
    path: PathBuf,
}

fn parse_provisioned_module(argument: &OsString) -> Result<ProvisionedModuleArg, String> {
    let text = argument
        .to_str()
        .ok_or("--module needs a UTF-8 name=path argument")?;
    let (name, path) = text
        .split_once('=')
        .ok_or("--module needs a name=path argument")?;
    if name.is_empty() || path.is_empty() {
        return Err("--module needs nonempty name and path".into());
    }
    Ok(ProvisionedModuleArg {
        name: name.to_owned(),
        path: PathBuf::from(path),
    })
}

enum GuestExport {
    Host {
        guest_path: String,
        destination: PathBuf,
    },
    Worker {
        guest_path: String,
        descriptor: RawFd,
    },
}

struct Options {
    path: PathBuf,
    /// Explicit package modules loaded beside the entry executable.
    package: Vec<PathBuf>,
    /// Authorized module snapshots admitted to the closed-world store.
    modules: Vec<ProvisionedModuleArg>,
    max_steps: Option<usize>,
    wall_seconds: Option<u64>,
    executable_budget: u64,
    mapped_image_budget: u64,
    worker: bool,
    /// Set by the supervisor for its worker: run the file as a program, not
    /// as an embedded fixture.
    program: bool,
    trace: bool,
    startup_report: bool,
    report_fd: Option<RawFd>,
    export: Option<GuestExport>,
    hud: bool,
    /// If given, the file runs only when its SHA-256 is this.
    digest: Option<[u8; 32]>,
    /// What the program receives after its own name.
    arguments: Vec<String>,
}

fn export_guest_path(argument: Option<OsString>) -> Result<String, String> {
    argument
        .and_then(|path| path.into_string().ok())
        .filter(|path| !path.is_empty())
        .ok_or_else(|| "guest file path must be nonempty UTF-8".to_owned())
}

fn parse_sha256(text: &str) -> Option<[u8; 32]> {
    let (pairs, rest) = text.as_bytes().as_chunks::<2>();
    if pairs.len() != 32 || !rest.is_empty() {
        return None;
    }
    // Digits only: no sign, no space, and no byte of a longer character.
    let digit = |byte: u8| {
        char::from(byte)
            .to_digit(16)
            .and_then(|value| u8::try_from(value).ok())
    };
    let mut digest = [0; 32];
    for (byte, [high, low]) in digest.iter_mut().zip(pairs) {
        *byte = digit(*high)? << 4 | digit(*low)?;
    }
    Some(digest)
}

impl Options {
    fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let mut path = None;
        let mut package = Vec::new();
        let mut modules = Vec::new();
        let mut max_steps = None;
        let mut digest = None;
        let mut arguments = Vec::new();
        let (mut experimental, mut trace, mut hud, mut worker) = (false, false, false, false);
        let mut program = false;
        let mut startup_report = false;
        let mut report_fd = None;
        let mut export = None;
        let mut wall_seconds = None;
        let mut executable_budget = None;
        let mut mapped_image_budget = None;
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--pe") if path.is_none() => {
                    path = Some(PathBuf::from(args.next().ok_or("missing --pe path")?));
                }
                Some("--package") => {
                    let module = args.next().ok_or("missing --package path")?;
                    if module.is_empty() {
                        return Err("--package needs a nonempty path".into());
                    }
                    package.push(PathBuf::from(module));
                }
                Some("--module") => {
                    modules.push(parse_provisioned_module(
                        &args.next().ok_or("missing --module name=path")?,
                    )?);
                }
                Some("--max-steps") if max_steps.is_none() => {
                    max_steps = Some(
                        args.next()
                            .and_then(|arg| arg.to_str()?.parse::<usize>().ok())
                            .ok_or("--max-steps needs a number")?,
                    );
                }
                Some("--wall-seconds") if wall_seconds.is_none() => {
                    wall_seconds = Some(
                        args.next()
                            .and_then(|arg| arg.to_str()?.parse::<u64>().ok())
                            .ok_or("--wall-seconds needs a number")?,
                    );
                }
                Some("--max-executable-bytes") if executable_budget.is_none() => {
                    executable_budget = Some(
                        args.next()
                            .and_then(|arg| arg.to_str()?.parse::<u64>().ok())
                            .filter(|&value| value > 0 && value < u64::MAX)
                            .ok_or("--max-executable-bytes needs a positive byte count")?,
                    );
                }
                Some("--max-mapped-image-bytes") if mapped_image_budget.is_none() => {
                    mapped_image_budget = Some(
                        args.next()
                            .and_then(|arg| arg.to_str()?.parse::<u64>().ok())
                            .filter(|&value| value > 0)
                            .ok_or("--max-mapped-image-bytes needs a positive byte count")?,
                    );
                }
                Some("--sha256") if digest.is_none() => {
                    digest = Some(
                        args.next()
                            .and_then(|arg| parse_sha256(arg.to_str()?))
                            .ok_or("--sha256 needs 64 hexadecimal digits")?,
                    );
                }
                Some("--export-guest-file") if export.is_none() => {
                    export = Some(GuestExport::Host {
                        guest_path: export_guest_path(args.next())?,
                        destination: PathBuf::from(args.next().ok_or("missing host export path")?),
                    });
                }
                Some("--guest-file-export-worker") if export.is_none() => {
                    export = Some(GuestExport::Worker {
                        guest_path: export_guest_path(args.next())?,
                        descriptor: args
                            .next()
                            .and_then(|arg| arg.to_str()?.parse().ok())
                            .filter(|&fd| fd > 2)
                            .ok_or("invalid guest file export descriptor")?,
                    });
                }
                Some("--") => {
                    for argument in args.by_ref() {
                        arguments.push(
                            argument
                                .into_string()
                                .map_err(|_| "program arguments must be UTF-8")?,
                        );
                    }
                }
                Some("--experimental") if !experimental => experimental = true,
                Some("--trace") if !trace => trace = true,
                Some("--startup-report") if !startup_report => startup_report = true,
                Some("--startup-report-worker") if report_fd.is_none() => {
                    report_fd = Some(
                        args.next()
                            .and_then(|arg| arg.to_str()?.parse().ok())
                            .ok_or("invalid startup diagnostic descriptor")?,
                    );
                }
                Some("--metal-hud") if !hud => hud = true,
                Some("--worker") if !worker => worker = true,
                Some("--program") if !program => program = true,
                _ => return Err(format!("unknown or repeated run argument {arg:?}")),
            }
        }
        if !experimental {
            return Err("run requires --experimental".into());
        }
        if program && !worker {
            return Err("unknown or repeated run argument \"--program\"".into());
        }
        if !package.is_empty() && worker && !program {
            return Err("--package applies to a program".into());
        }
        if (worker && startup_report) || (!worker && report_fd.is_some()) {
            return Err("invalid startup report option for this runner".into());
        }
        match &export {
            Some(GuestExport::Host { .. }) if worker => {
                return Err("--export-guest-file applies to the run supervisor".into());
            }
            Some(GuestExport::Worker { descriptor, .. })
                if !program || report_fd == Some(*descriptor) =>
            {
                return Err("invalid guest file export option for this runner".into());
            }
            _ => {}
        }
        Ok(Self {
            path: path.ok_or("missing --pe path")?,
            package,
            modules,
            max_steps,
            wall_seconds,
            executable_budget: executable_budget.unwrap_or(DEFAULT_EXECUTABLE_BUDGET),
            mapped_image_budget: mapped_image_budget
                .unwrap_or(xenora_loader::pe::DEFAULT_MAPPED_IMAGE_BUDGET),
            worker,
            program,
            trace,
            startup_report,
            report_fd,
            export,
            hud,
            digest,
            arguments,
        })
    }

    /// The entry step limit and the wall deadline for the kind of run, or why
    /// the options do not suit it. A program has no step limit unless one is
    /// given: its wall deadline is what bounds it.
    fn limits(&self, program: bool) -> Result<(usize, Duration), String> {
        if !program {
            let steps = self.max_steps.unwrap_or(DEFAULT_STEPS);
            if !(1..=MAX_STEPS).contains(&steps) {
                return Err(format!("--max-steps must be in 1..={MAX_STEPS}"));
            }
            if self.wall_seconds.is_some() {
                return Err("--wall-seconds applies to a program, not an embedded fixture".into());
            }
            if self.export.is_some() {
                return Err("--export-guest-file applies to a hosted program".into());
            }
            return Ok((steps, WALL_LIMIT));
        }
        if self.max_steps == Some(0) {
            return Err("--max-steps must be at least 1".into());
        }
        let wall = self.wall_seconds.unwrap_or(WALL_LIMIT.as_secs());
        if !(1..=MAX_WALL_SECONDS).contains(&wall) {
            return Err(format!("--wall-seconds must be in 1..={MAX_WALL_SECONDS}"));
        }
        if self.hud {
            return Err("--metal-hud applies to the embedded fixtures only".into());
        }
        Ok((
            self.max_steps.unwrap_or(usize::MAX),
            Duration::from_secs(wall),
        ))
    }
}

pub fn command(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    // An embedded fixture, given as it is, keeps its own path: its draw is
    // verified and its result is checked. Every other file, and any file
    // given a digest or arguments, runs as a program.
    let program = if options.worker {
        options.program
    } else {
        options.digest.is_some()
            || !options.arguments.is_empty()
            || !options.package.is_empty()
            || !options.modules.is_empty()
            || verify_fixture(&options.path).is_err()
    };
    let (steps, wall) = match options.limits(program) {
        Ok(limits) => limits,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if let Some(GuestExport::Worker { descriptor, .. }) = &options.export
        && let Err(error) = diagnostics::validate_endpoint(
            *descriptor,
            "guest export descriptor must be outside stdio",
        )
    {
        eprintln!("run failed: cannot open guest file export channel: {error}");
        return ExitCode::from(FAILURE_CODE);
    }
    let reporter = if let Some(fd) = options.report_fd {
        // SAFETY: only the fresh worker process accepts this internal option.
        // It has not adopted any inherited non-stdio descriptors or started
        // threads, and from_inherited checks that the descriptor is open.
        match unsafe { Reporter::from_inherited(fd) } {
            Ok(reporter) => reporter,
            Err(error) => {
                eprintln!("run failed: cannot open startup diagnostic channel: {error}");
                return if program {
                    ExitCode::from(FAILURE_CODE)
                } else {
                    ExitCode::FAILURE
                };
            }
        }
    } else {
        Reporter::default()
    };
    let result = match (options.worker, program) {
        (true, true) => run_program(&options, steps, wall, &reporter),
        (true, false) => run_worker(&options, steps, &reporter).map(|()| ExitCode::SUCCESS),
        (false, _) => launch_worker(&options, program, steps, wall),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            reporter.failed(FailurePhase::Worker, &error);
            eprintln!("run failed: {error}");
            if program {
                ExitCode::from(FAILURE_CODE)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn launch_worker(
    options: &Options,
    program: bool,
    steps: usize,
    wall: Duration,
) -> Result<ExitCode, Box<dyn Error>> {
    let started = Instant::now();
    // A program's worker stops itself at the deadline, after showing what the
    // program wrote. The supervisor steps in only if it does not.
    let limit = if program { wall + DEADLINE_GRACE } else { wall };
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["run", "--experimental", "--worker"]);
    if program {
        // A program's own output is the run's output: no read-outs unless
        // they were asked for.
        command.arg("--program");
        command
            .arg("--max-mapped-image-bytes")
            .arg(options.mapped_image_budget.to_string());
        command
            .arg("--max-executable-bytes")
            .arg(options.executable_budget.to_string());
        if options.trace {
            command.arg("--trace");
        }
        if let Some(digest) = options.digest {
            command.arg("--sha256").arg(hex(digest));
        }
    } else {
        command.args(["--trace", "--metal-hud"]);
    }
    if program {
        command
            .arg("--wall-seconds")
            .arg(wall.as_secs().to_string());
        if let Some(steps) = options.max_steps {
            command.arg("--max-steps").arg(steps.to_string());
        }
    } else {
        command.arg("--max-steps").arg(steps.to_string());
    }
    let collector = if options.startup_report {
        let (collector, fd) = Collector::attach(&mut command)?;
        command.arg("--startup-report-worker").arg(fd.to_string());
        Some(collector)
    } else {
        None
    };
    let staged_export = if let Some(GuestExport::Host {
        guest_path,
        destination,
    }) = &options.export
    {
        SupervisorExport::attach(&mut command, destination).map(|(export, fd)| {
            command
                .arg("--guest-file-export-worker")
                .arg(guest_path)
                .arg(fd.to_string());
            Some(export)
        })
    } else {
        Ok(None)
    };
    command.arg("--pe").arg(&options.path);
    if program {
        for module in &options.package {
            command.arg("--package").arg(module);
        }
        for module in &options.modules {
            let mut argument = OsString::from(&module.name);
            argument.push("=");
            argument.push(&module.path);
            command.arg("--module").arg(argument);
        }
        command.arg("--").args(&options.arguments);
    }

    let launched = staged_export.and_then(|export| command.spawn().map(|child| (child, export)));
    // Release the supervisor's copies of the worker endpoints even if launch failed.
    drop(command);
    let (stopped, staged_export) = match launched {
        Ok((child, export)) => (wait_for_worker(child, started, limit), export),
        Err(error) => (Err(error), None),
    };
    let publication = match (&stopped, staged_export) {
        (Ok(Some(status)), Some(export)) if status.success() => export.publish(),
        _ => Ok(()),
    };
    // Emit after the worker's output has ended. The leading newline isolates
    // this record even when guest stderr ends with an unterminated line.
    if let Some(collector) = collector {
        let termination = match (&stopped, &publication) {
            (_, Err(error)) | (Err(error), _) => Termination::SupervisorError(error),
            (Ok(Some(status)), Ok(())) => Termination::Exited(*status),
            (Ok(None), Ok(())) => Termination::Deadline,
        };
        eprintln!(
            "\nxenora-startup {}",
            collector.report(termination, program).json()
        );
    }
    publication?;
    match stopped? {
        Some(status) => match status.code() {
            Some(code) => Ok(ExitCode::from(u8::try_from(code)?)),
            None => Err(format!("worker terminated: {status}").into()),
        },
        None if program => {
            eprintln!("{}", deadline_message(wall));
            Ok(ExitCode::from(DEADLINE_CODE))
        }
        None => Err("experimental worker exceeded its 15 second wall deadline".into()),
    }
}

fn wait_for_worker(
    mut child: Child,
    started: Instant,
    limit: Duration,
) -> std::io::Result<Option<ExitStatus>> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) if started.elapsed() < limit => {
                std::thread::sleep(Duration::from_millis(10));
            }
            status => {
                let _ = child.kill();
                child.wait()?;
                status?;
                return Ok(None);
            }
        }
    }
}

fn open_regular(
    path: &Path,
    expected_len: Option<u64>,
    rejection: &'static str,
) -> Result<File, Box<dyn Error>> {
    let valid_metadata = |metadata: fs::Metadata| {
        metadata.is_file() && expected_len.is_none_or(|len| metadata.len() == len)
    };
    if !valid_metadata(fs::metadata(path)?) {
        return Err(rejection.into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // Darwin <sys/fcntl.h>. A path can be replaced with a FIFO after
        // the first check. A nonblocking open lets the descriptor check
        // refuse it without waiting for a writer.
        const O_NONBLOCK: i32 = 0x0004;
        options.custom_flags(O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !valid_metadata(file.metadata()?) {
        return Err(rejection.into());
    }
    Ok(file)
}

fn verify_fixture(path: &Path) -> Result<&'static [u8], Box<dyn Error>> {
    let rejection =
        "execution denied: input must match an embedded trusted PE fixture byte for byte";
    let file = open_regular(path, Some(FIXTURE.len() as u64), rejection)?;
    // Bound the read even if another process changes the file after opening.
    let mut bytes = Vec::with_capacity(FIXTURE.len() + 1);
    file.take(FIXTURE.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    [FIXTURE, SERVICES_FIXTURE]
        .into_iter()
        .find(|fixture| *fixture == bytes)
        .ok_or_else(|| rejection.into())
}

fn hex(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn deadline_message(wall: Duration) -> String {
    format!(
        "run failed: the program exceeded the {} second wall deadline",
        wall.as_secs()
    )
}

/// What a program's deadline needs: the state that holds its output, and the
/// lock the run takes before it writes that output itself.
type Finishing = (Arc<ServicesState>, Arc<Mutex<()>>);

/// Stop the process at the wall deadline. For a program, what it had written
/// is shown first. The supervisor also enforces the deadline; the worker
/// keeps its own in case it is invoked directly.
fn start_deadline(
    wall: Duration,
    program: Option<Finishing>,
    reporter: Reporter,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("xenora-deadline".into())
        .spawn(move || {
            std::thread::sleep(wall);
            if let Some((state, finishing)) = program {
                // A run that has ended holds this lock while it writes its
                // own output and exits; then this waits and writes nothing.
                let _turn = finishing.lock();
                reporter.deadline();
                let mut stdout = std::io::stdout().lock();
                let _ = stdout.write_all(&state.standard_output());
                let _ = stdout.flush();
                let _ = std::io::stderr().write_all(&state.standard_error());
                eprintln!("{}", deadline_message(wall));
            } else {
                reporter.deadline();
            }
            std::process::exit(i32::from(DEADLINE_CODE));
        })
        .map(drop)
}

/// Read a program. If the user gave a SHA-256, the file is admitted only when
/// its bytes have it; the bytes that were hashed are the bytes that run, so a
/// change to the file afterwards cannot matter.
///
/// The snapshot is bounded even if the file grows after its metadata is read.
fn read_program(
    path: &Path,
    digest: Option<[u8; 32]>,
    budget: u64,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut file = open_regular(
        path,
        None,
        "execution denied: a program must be a regular file",
    )?;
    if file.metadata()?.len() > budget {
        return Err(
            format!("execution denied: executable exceeds the {budget} byte budget").into(),
        );
    }
    let bytes = read_program_snapshot(&mut file, budget)?;
    if let Some(expected) = digest
        && Sha256::digest(&bytes).as_slice() != expected
    {
        return Err("execution denied: the file's SHA-256 is not the one given".into());
    }
    Ok(bytes)
}

fn read_program_snapshot(file: &mut impl Read, budget: u64) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        let next = (bytes.len() as u64)
            .checked_add(n as u64)
            .ok_or("execution denied: executable size overflow")?;
        if next > budget {
            return Err(
                format!("execution denied: executable exceeds the {budget} byte budget").into(),
            );
        }
        bytes
            .try_reserve_exact(n)
            .map_err(|_| "execution denied: cannot allocate executable snapshot")?;
        bytes.extend_from_slice(&chunk[..n]);
    }
    Ok(bytes)
}

/// The host exit code for a guest exit status. A host exit code has eight
/// bits and a guest status thirty-two: the low eight are kept, except that a
/// status other than zero never becomes zero.
fn host_exit_code(status: u32) -> u8 {
    match status.to_le_bytes()[0] {
        0 if status != 0 => 1,
        low => low,
    }
}

/// The most the guest's standard input holds, in bytes. Documented in
/// docs/ as the deterministic-input limit of `xenora run`.
pub(crate) const MAX_STANDARD_INPUT: usize = 1 << 20;

/// Run a file as a program: no embedded snapshot, no fixture draw service
/// and no expected result. Its arguments, output and exit status are its own.
fn run_program(
    options: &Options,
    steps: usize,
    wall: Duration,
    reporter: &Reporter,
) -> Result<ExitCode, Box<dyn Error>> {
    let guest_export = if let Some(GuestExport::Worker {
        guest_path,
        descriptor,
    }) = &options.export
    {
        // SAFETY: the fresh worker owns the inherited non-stdio descriptor.
        // The options parser keeps it distinct from the startup report socket.
        let stage = unsafe { export::from_inherited(*descriptor) }?;
        Some((guest_path.as_str(), stage))
    } else {
        None
    };
    let state = Arc::new(ServicesState::new(Arc::new(SystemClock)));
    let finishing = Arc::new(Mutex::new(()));
    start_deadline(
        wall,
        Some((Arc::clone(&state), Arc::clone(&finishing))),
        reporter.clone(),
    )?;
    let bytes = read_program(&options.path, options.digest, options.executable_budget)
        .inspect_err(|error| reporter.failed(FailurePhase::Admission, error))?;
    let name = options
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("the program's file name must be UTF-8")?;
    if options.trace {
        let policy = if options.digest.is_some() {
            "sha256"
        } else {
            "any"
        };
        let step_limit = options
            .max_steps
            .map_or_else(|| "none".to_owned(), |steps| steps.to_string());
        eprintln!(
            "trace program={name} policy={policy} entry_step_limit={step_limit} wall_limit_ms={}",
            wall.as_millis()
        );
    }
    let mut arguments = vec![name];
    arguments.extend(options.arguments.iter().map(String::as_str));
    state.set_program_arguments(&arguments)?;
    // The guest's standard input is the worker's own, read in full before
    // the program starts so that a run stays deterministic. A terminal means
    // no input. Oversized input is rejected before guest execution.
    let input = if std::io::stdin().is_terminal() {
        Vec::new()
    } else {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take((MAX_STANDARD_INPUT + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_STANDARD_INPUT {
            return Err(format!(
                "the standard input exceeds the {MAX_STANDARD_INPUT} byte limit; feed it less, as a run must stay deterministic"
            )
            .into());
        }
        bytes
    };
    state.set_standard_input(&input);
    let mut imports = ImportRegistry::new();
    register_standard_services(&mut imports, Arc::clone(&state));

    // `--module` admits an image to the closed-world store. It does not load
    // or execute the image: later lifecycle work may only name these entries.
    let mut provisioned = xenora_loader::ProvisionedModuleStore::new();
    let mut reserved_names = vec![name.to_ascii_lowercase()];
    for path in &options.package {
        if let Some(file_name) = path.file_name().and_then(|name| name.to_str()) {
            reserved_names.push(file_name.to_ascii_lowercase());
        }
    }
    for module in &options.modules {
        let canonical = module.name.to_ascii_lowercase();
        if reserved_names.contains(&canonical) {
            return Err(format!(
                "module name {:?} collides with the program or another admitted image",
                module.name
            )
            .into());
        }
        let bytes = read_program(&module.path, None, options.executable_budget)
            .inspect_err(|error| reporter.failed(FailurePhase::Admission, error))?;
        provisioned
            .insert(module.name.clone(), bytes, &imports)
            .map_err(|error| -> Box<dyn Error> { Box::new(error) })?;
        reserved_names.push(canonical);
    }
    if options.trace && !provisioned.is_empty() {
        eprintln!(
            "trace provisioned-modules={} store-bytes={}",
            provisioned.len(),
            provisioned.total_bytes()
        );
    }

    let mut entered = false;
    let execution = if options.package.is_empty() {
        // The entry point runs under the deterministic cooperative scheduler,
        // so a program that creates threads with CreateThread runs them all,
        // one guest thread at a time. A program that creates none behaves as
        // before: its main thread takes every turn.
        run_synthetic_startup_scheduled_with_budget(
            &bytes,
            StartupLayout {
                image_base: IMAGE_BASE,
                stack_top: 0x200010000,
                tls_base: 0x300000000,
            },
            imports,
            Arc::clone(&state),
            steps,
            DEFAULT_QUANTUM,
            options.mapped_image_budget,
            |event| {
                entered |= matches!(event, StartupEvent::Entering { .. });
                reporter.observed(&event);
                if options.trace {
                    eprintln!("{}", startup_line(&event));
                }
            },
        )
        .map(|startup| startup.execution)
    } else {
        let mut module_bytes = Vec::with_capacity(options.package.len());
        for module in &options.package {
            module_bytes.push(
                read_program(module, None, options.executable_budget)
                    .inspect_err(|error| reporter.failed(FailurePhase::Admission, error))?,
            );
        }
        let mut package = Vec::with_capacity(1 + module_bytes.len());
        package.push(xenora_loader::module_graph::PackageModule {
            file_name: name,
            bytes: &bytes,
        });
        let mut names = vec![name];
        for (path, module_bytes) in options.package.iter().zip(&module_bytes) {
            let file_name = path
                .file_name()
                .and_then(|file_name| file_name.to_str())
                .ok_or("a package module's file name must be UTF-8")?;
            if names.contains(&file_name) {
                return Err(format!("package module {file_name:?} is named more than once").into());
            }
            names.push(file_name);
            package.push(xenora_loader::module_graph::PackageModule {
                file_name,
                bytes: module_bytes,
            });
        }
        if options.trace {
            eprintln!(
                "trace package modules={} entry={name} entry_step_limit={} wall_limit_ms={}",
                package.len(),
                options
                    .max_steps
                    .map_or_else(|| "none".to_owned(), |steps| steps.to_string()),
                wall.as_millis()
            );
        }
        xenora_loader::run_package_startup(
            &package,
            xenora_loader::PackageStartupLayout {
                arena: xenora_loader::module_layout::ModuleLayoutArena {
                    base: IMAGE_BASE,
                    size: options.mapped_image_budget,
                },
                stack_top: 0x200010000,
                thread_tls_base: 0x300000000,
            },
            0,
            imports,
            steps,
            Some((Arc::clone(&state), DEFAULT_QUANTUM)),
        )
        .map_err(|error| -> Box<dyn Error> { Box::new(error) })
        .map(|result| {
            entered = result
                .diagnostics
                .initialization
                .iter()
                .any(|record| record.phase == xenora_loader::InitializationPhase::ProcessEntry);
            if options.trace {
                eprintln!(
                    "trace package imports={} initialization={}",
                    result.diagnostics.imports.len(),
                    result.diagnostics.initialization.len()
                );
            }
            result.execution
        })
    };
    // Whatever the program wrote is shown, however the run ended. From here
    // the deadline no longer writes it.
    let _turn = finishing.lock();
    let execution = execution.inspect_err(|error| reporter.failed(FailurePhase::Startup, error));
    if execution.is_ok() && entered {
        reporter.stage(Stage::EntryReturned);
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&state.standard_output())?;
    stdout.flush()?;
    std::io::stderr().write_all(&state.standard_error())?;
    let execution = execution?;
    if options.trace {
        eprintln!(
            "trace stage=exit entry_steps={} exit={:?}",
            execution.instructions_executed, execution.exit_reason
        );
    }
    match execution.exit_reason {
        ExitReason::ProcessExit(status) => {
            if status == 0
                && let Some((guest_path, stage)) = guest_export
            {
                export::stage_copy(stage, &state, guest_path)
                    .inspect_err(|error| reporter.failed(FailurePhase::Verification, error))?;
            }
            let code = host_exit_code(status);
            if u32::from(code) != status {
                eprintln!("guest exit status {status} ({status:#x}); host exit code {code}");
            }
            reporter.guest_exit(status);
            Ok(ExitCode::from(code))
        }
        other => {
            let message = format!(
                "the program stopped without exiting after {} entry steps: {other:?}",
                execution.instructions_executed
            );
            reporter.failed(FailurePhase::Execution, &message);
            Err(message.into())
        }
    }
}

fn run_worker(options: &Options, steps: usize, reporter: &Reporter) -> Result<(), Box<dyn Error>> {
    start_deadline(WALL_LIMIT, None, reporter.clone())?;
    let fixture = verify_fixture(&options.path)
        .inspect_err(|error| reporter.failed(FailurePhase::Admission, error))?;
    let (name, ticket, bound_imports) = if fixture == SERVICES_FIXTURE {
        ("runtime-services", 23, 3)
    } else {
        ("trusted-runner", TICKET, 1)
    };
    if options.trace {
        println!(
            "trace fixture={name} policy=exact-bytes entry_step_limit={} wall_limit_ms=15000",
            steps
        );
    }

    let capture = Arc::new(Mutex::new(None));
    let draw_capture = Arc::clone(&capture);
    let mut imports = ImportRegistry::new();
    register_standard_services(
        &mut imports,
        Arc::new(ServicesState::new(Arc::new(SystemClock))),
    );
    let trace = options.trace;
    imports.register("xenora.dll", "fixture_draw", move |args, _, memory| {
        let ticket_address = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        let mut result = draw_capture
            .lock()
            .map_err(|_| ExitReason::IllegalInstruction(0xff))?;
        if result.is_some() {
            return Err(ExitReason::IllegalInstruction(0xff));
        }
        if ticket_address != TICKET_ADDRESS
            || memory
                .read_u64(ticket_address)
                .map_err(ExitReason::MemoryFault)?
                != ticket
        {
            *result = Some(Err(
                "guest draw ticket differed from the expected value".into()
            ));
            return Err(ExitReason::IllegalInstruction(0xff));
        }
        if trace {
            println!("trace import=xenora.dll!fixture_draw tls_ticket={ticket}");
        }
        let frame = draw().map_err(|error| error.to_string());
        let drawn = frame.is_ok();
        *result = Some(frame);
        if drawn {
            Ok(DRAW_RESULT)
        } else {
            Err(ExitReason::IllegalInstruction(0xff))
        }
    });

    let started = Instant::now();
    // Use only the embedded snapshot. The file is solely an exact-byte gate.
    let mut entered = false;
    let startup = run_synthetic_startup_with_observer(
        fixture,
        StartupLayout {
            image_base: IMAGE_BASE,
            stack_top: 0x200010000,
            tls_base: 0x300000000,
        },
        imports,
        steps,
        |event| {
            entered |= matches!(event, StartupEvent::Entering { .. });
            reporter.observed(&event);
            if options.trace {
                trace_startup(event);
            }
        },
    )
    .inspect_err(|error| reporter.failed(FailurePhase::Startup, error))?;
    if entered {
        reporter.stage(Stage::EntryReturned);
    }
    let host_ms = started.elapsed().as_secs_f64() * 1000.0;
    let result = capture
        .lock()
        .map_err(|_| "fixture draw result lock failed")?
        .take();
    let exit = startup.execution.exit_reason;
    let rax = startup.execution.final_state.get(Register::Rax);
    if options.trace {
        println!(
            "trace stage=exit entry_steps={} exit={exit:?} rax={rax:#x}",
            startup.execution.instructions_executed,
        );
    }
    if let Some(Err(error)) = &result {
        let message = format!("fixture draw failed: {error}");
        reporter.failed(FailurePhase::Verification, &message);
        return Err(message.into());
    }
    if exit != ExitReason::Halt {
        reporter.failed(FailurePhase::Execution, exit);
        return Err(exit.into());
    }
    if startup.relocations.applied_dir64 != 4
        || startup.bound_imports != bound_imports
        || startup.tls_callbacks != 1
        || rax != DRAW_RESULT
    {
        let message = "trusted fixture startup or result differs from the expected values";
        reporter.failed(FailurePhase::Verification, message);
        return Err(message.into());
    }
    let frame = result
        .ok_or("guest halted without a draw")
        .inspect_err(|error| reporter.failed(FailurePhase::Verification, error))??;
    reporter.stage(Stage::FramebufferVerified);
    if options.trace {
        println!(
            "trace stage=framebuffer extent={}x{} verified_pixels={}",
            EXTENT[0],
            EXTENT[1],
            EXTENT[0] * EXTENT[1],
        );
    }
    if options.hud {
        let gpu_time = frame.gpu_seconds.map_or_else(
            || "unavailable".to_owned(),
            |seconds| format!("{:.3}", seconds * 1000.0),
        );
        println!(
            "Metal HUD | device={} | offscreen={}x{} | draw_calls=1 | verified_pixels={} | host_render_readback_ms={:.3} | host_startup_ms={host_ms:.3} | gpu_command_ms={gpu_time}",
            frame.device,
            EXTENT[0],
            EXTENT[1],
            EXTENT[0] * EXTENT[1],
            frame.host_render_readback_ms,
        );
        let metrics = startup.dbt_metrics;
        println!(
            "DBT | entry_steps={} | native_steps_tls_and_entry={} | blocks_compiled={} | native_bytes={} | native_dispatches={} | service_calls={} | interpreter_fallbacks={}",
            startup.execution.instructions_executed,
            metrics.instructions_executed,
            metrics.blocks_compiled,
            metrics.native_bytes_emitted,
            metrics.dbt_dispatches,
            metrics.service_calls,
            metrics.interpreter_fallbacks,
        );
    }
    println!("Trusted fixture completed one verified offscreen Metal draw.");
    reporter.verified();
    Ok(())
}

fn trace_startup(event: StartupEvent) {
    println!("{}", startup_line(&event));
}

fn startup_line(event: &StartupEvent) -> String {
    match event {
        StartupEvent::Parsed {
            sections,
            entry_rva,
        } => format!("trace stage=parsed sections={sections} entry_rva={entry_rva:#x}"),
        StartupEvent::Mapped { entry_va } => {
            format!("trace stage=mapped base={IMAGE_BASE:#x} entry_va={entry_va:#x}")
        }
        StartupEvent::Relocated { applied_dir64 } => {
            format!("trace stage=relocated dir64_applied={applied_dir64}")
        }
        StartupEvent::ImportsBound { count } => {
            format!("trace stage=imports_bound bound_imports={count}")
        }
        StartupEvent::TlsCompleted {
            callbacks,
            native_steps,
        } => format!(
            "trace stage=tls_completed tls_callbacks={callbacks} native_steps={native_steps}"
        ),
        StartupEvent::Entering { step_limit } => {
            format!("trace stage=entry step_limit={step_limit}")
        }
    }
}

struct Frame {
    device: String,
    host_render_readback_ms: f64,
    gpu_seconds: Option<f64>,
}

fn draw() -> Result<Frame, Box<dyn Error>> {
    let shader = compile_graphics(
        Architecture::Gfx1030,
        include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin"),
        include_bytes!("../../../fixtures/rdna2/x9-constant-ps.bin"),
    )?;
    let gpu = NativeCompute::new()?;
    let started = Instant::now();
    let (pixels, gpu_seconds) = gpu.render_rgba8_profiled(
        &shader.msl,
        [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
        &[
            [-1.0, -1.0, 0.5, 1.0],
            [3.0, -1.0, 0.5, 1.0],
            [-1.0, 3.0, 0.5, 1.0],
        ],
        &[[0.0; 4]; 3],
        EXTENT,
        [19, 43, 93, 251],
    )?;
    let host_render_readback_ms = started.elapsed().as_secs_f64() * 1000.0;
    if pixels != EXPECTED_PIXEL.repeat(EXTENT[0] * EXTENT[1]) {
        return Err("offscreen readback did not match opaque red at every pixel".into());
    }
    Ok(Frame {
        device: gpu.device_name().to_owned(),
        host_render_readback_ms,
        gpu_seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        GuestExport, Options, hex, host_exit_code, parse_sha256, read_program,
        read_program_snapshot,
    };
    use std::ffi::OsString;

    #[test]
    fn guest_export_options_keep_supervisor_worker_and_guest_arguments_distinct() {
        fn parse(extra: &[&str]) -> Result<Options, String> {
            Options::parse(
                ["--pe", "fixture.exe", "--experimental"]
                    .into_iter()
                    .chain(extra.iter().copied())
                    .map(OsString::from),
            )
        }

        assert!(parse(&["--export-guest-file", "source"]).is_err());
        assert!(
            parse(&[
                "--export-guest-file",
                "source",
                "output",
                "--export-guest-file",
                "again",
                "second"
            ])
            .is_err()
        );
        assert!(parse(&["--guest-file-export-worker", "source", "4"]).is_err());
        assert!(
            parse(&[
                "--worker",
                "--program",
                "--export-guest-file",
                "source",
                "output"
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "--worker",
                "--program",
                "--startup-report-worker",
                "4",
                "--guest-file-export-worker",
                "source",
                "4"
            ])
            .is_err()
        );
        let worker = parse(&[
            "--worker",
            "--program",
            "--startup-report-worker",
            "4",
            "--guest-file-export-worker",
            "source",
            "5",
        ])
        .unwrap();
        assert!(matches!(
            worker.export,
            Some(GuestExport::Worker { descriptor: 5, .. })
        ));
        let embedded = parse(&["--export-guest-file", "source", "output"]).unwrap();
        assert!(embedded.limits(false).is_err());
        let guest = parse(&["--", "--export-guest-file", "source", "output"]).unwrap();
        assert!(guest.export.is_none());
        assert_eq!(guest.arguments, ["--export-guest-file", "source", "output"]);
    }

    #[test]
    fn program_budget_options_are_checked() {
        let args = |extra: &[&str]| {
            Options::parse(
                ["--pe", "test.exe", "--experimental"]
                    .into_iter()
                    .chain(extra.iter().copied())
                    .map(OsString::from),
            )
        };
        let options = args(&[
            "--max-executable-bytes",
            "700000000",
            "--max-mapped-image-bytes",
            "400000000",
        ])
        .unwrap();
        assert_eq!(options.executable_budget, 700_000_000);
        assert_eq!(options.mapped_image_budget, 400_000_000);
        assert!(args(&["--max-mapped-image-bytes", "0"]).is_err());
        assert!(args(&["--max-executable-bytes", "0"]).is_err());
        assert!(
            args(&[
                "--max-mapped-image-bytes",
                "1",
                "--max-mapped-image-bytes",
                "2"
            ])
            .is_err()
        );
    }

    #[test]
    fn executable_snapshot_respects_budget_for_sparse_files() {
        let path = std::env::temp_dir().join(format!("xenora-budget-{}", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(1024 * 1024 + 1).unwrap();
        drop(file);
        assert!(read_program(&path, None, 1024 * 1024).is_err());
        let bytes = read_program(&path, None, 1024 * 1024 + 1).unwrap();
        assert_eq!(bytes.len(), 1024 * 1024 + 1);
        assert!(bytes.iter().all(|&byte| byte == 0));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn executable_snapshot_rejects_growth_past_budget() {
        let mut source = std::io::Cursor::new(vec![7; 65_537]);
        assert!(read_program_snapshot(&mut source, 65_536).is_err());
        let mut source = std::io::Cursor::new(vec![7; 65_536]);
        assert_eq!(
            read_program_snapshot(&mut source, 65_536).unwrap().len(),
            65_536
        );
    }

    #[test]
    fn a_nonzero_status_never_becomes_a_zero_exit_code() {
        // (guest status, host exit code)
        for (status, code) in [
            (0, 0),
            (1, 1),
            (255, 255),
            (256, 1),
            (0x00A1_A1A1, 0xA1),
            (0x8000_0000, 1),
            (u32::MAX, 255),
        ] {
            assert_eq!(host_exit_code(status), code, "{status:#x}");
        }
    }

    #[test]
    fn a_digest_is_sixty_four_hexadecimal_digits() {
        let mut expected = [0; 32];
        for (index, byte) in expected.iter_mut().enumerate() {
            *byte = u8::try_from(index * 8 + 1).unwrap();
        }
        let lower: String = expected.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(parse_sha256(&lower), Some(expected));
        assert_eq!(parse_sha256(&lower.to_uppercase()), Some(expected));
        assert_eq!(hex(expected), lower);
        for wrong in [
            &lower[..63],
            &format!("{lower}0"),
            &format!("{}g", &lower[..63]),
            &format!("+{}", &lower[..63]),
            &format!(" {}", &lower[..63]),
            // Sixty-four bytes, but not sixty-four digits.
            &format!("{}\u{e9}", &lower[..62]),
            "",
        ] {
            assert_eq!(parse_sha256(wrong), None, "{wrong:?}");
        }
    }
}
