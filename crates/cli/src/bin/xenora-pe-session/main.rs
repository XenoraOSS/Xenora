// SPDX-License-Identifier: GPL-3.0-or-later

mod host_output;
mod supervisor;
mod wire;
mod worker;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use xenora::hosted_graphics::Backend;
use xenora_loader::module_graph::MAX_PACKAGE_MODULES;

const USAGE: &str = "usage: xenora-pe-session --pe <executable> --sha256 <64 hex digits> --experimental [--package <dll> <64 hex digits>]... [--graphics-backend <reference|metal>] [--phase-seconds <1..=60>] [--max-steps <1..=10000000>] [--pixels] [--deterministic-clock]";
const DEFAULT_STEPS: usize = 2_000_000;
const MAX_STEPS: usize = 10_000_000;
const DEFAULT_SECONDS: u64 = 15;
const MAX_SECONDS: u64 = 60;

struct PinnedImage {
    path: PathBuf,
    digest: [u8; 32],
}

impl PinnedImage {
    fn digest_hex(&self) -> String {
        self.digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

struct Options {
    executable: PinnedImage,
    companions: Vec<PinnedImage>,
    backend: Backend,
    phase_limit: Duration,
    steps: usize,
    pixels: bool,
    deterministic_clock: bool,
    worker: bool,
}

impl Options {
    fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let (mut path, mut digest, mut seconds, mut steps) = (None, None, None, None);
        let (mut experimental, mut pixels, mut worker) = (false, false, false);
        let mut deterministic_clock = false;
        let mut companions = Vec::new();
        let mut backend = None;
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--pe") if path.is_none() => {
                    path = Some(parse_path(args.next()).ok_or("--pe needs a path")?);
                }
                Some("--sha256") if digest.is_none() => {
                    digest = Some(
                        args.next()
                            .and_then(|value| parse_digest(value.to_str()?))
                            .ok_or("--sha256 needs 64 hexadecimal digits")?,
                    );
                }
                Some("--package") => {
                    if companions.len() == MAX_PACKAGE_MODULES - 1 {
                        return Err(format!(
                            "a PE session accepts at most {} companion DLLs",
                            MAX_PACKAGE_MODULES - 1
                        ));
                    }
                    let path = parse_path(args.next()).ok_or("--package needs a DLL path")?;
                    let digest = args
                        .next()
                        .and_then(|value| parse_digest(value.to_str()?))
                        .ok_or("--package needs 64 hexadecimal digits after its DLL path")?;
                    companions.push(PinnedImage { path, digest });
                }
                Some("--graphics-backend") if backend.is_none() => {
                    backend = Some(
                        args.next()
                            .and_then(|value| Backend::parse(value.to_str()?))
                            .ok_or("--graphics-backend needs reference or metal")?,
                    );
                }
                Some("--phase-seconds") if seconds.is_none() => {
                    seconds = Some(
                        args.next()
                            .and_then(|value| value.to_str()?.parse().ok())
                            .filter(|value| (1..=MAX_SECONDS).contains(value))
                            .ok_or("--phase-seconds must be in 1..=60")?,
                    );
                }
                Some("--max-steps") if steps.is_none() => {
                    steps = Some(
                        args.next()
                            .and_then(|value| value.to_str()?.parse().ok())
                            .filter(|value| (1..=MAX_STEPS).contains(value))
                            .ok_or("--max-steps must be in 1..=10000000")?,
                    );
                }
                Some("--experimental") if !experimental => experimental = true,
                Some("--pixels") if !pixels => pixels = true,
                Some("--deterministic-clock") if !deterministic_clock => deterministic_clock = true,
                Some("--worker") if !worker => worker = true,
                _ => return Err(format!("unknown or repeated session argument {arg:?}")),
            }
        }
        if !experimental {
            return Err("a PE session requires --experimental".into());
        }
        Ok(Self {
            executable: PinnedImage {
                path: path.ok_or("missing --pe path")?,
                digest: digest.ok_or("missing --sha256 digest")?,
            },
            companions,
            backend: backend.unwrap_or(Backend::Reference),
            phase_limit: Duration::from_secs(seconds.unwrap_or(DEFAULT_SECONDS)),
            steps: steps.unwrap_or(DEFAULT_STEPS),
            pixels,
            deterministic_clock,
            worker,
        })
    }
}

fn parse_path(value: Option<OsString>) -> Option<PathBuf> {
    value
        .filter(|path| !path.is_empty() && !path.as_encoded_bytes().starts_with(b"--"))
        .map(PathBuf::from)
}

fn parse_digest(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut digest = [0; 32];
    let (pairs, _) = text.as_bytes().as_chunks::<2>();
    for (byte, pair) in digest.iter_mut().zip(pairs) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(digest)
}

fn main() -> ExitCode {
    let options = match Options::parse(std::env::args_os().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = if options.worker {
        worker::run(&options).map_err(supervisor::Failure::Session)
    } else {
        supervisor::run(&options)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(supervisor::Failure::Deadline(phase)) => {
            eprintln!("xenora-pe-session: {phase} exceeded the phase deadline");
            ExitCode::from(124)
        }
        Err(supervisor::Failure::Session(message)) => {
            eprintln!("xenora-pe-session: {message}");
            ExitCode::from(125)
        }
    }
}
