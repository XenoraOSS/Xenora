// SPDX-License-Identifier: GPL-3.0-or-later

//! Metal startup distributions: fresh processes versus repeated submissions (#178).
//!
//! `xenora-bench startup` checks one manifest fixture against the independent
//! reference, then measures it two ways:
//!
//! - **Fresh process.** It launches its own executable `--processes` times. Each
//!   child creates the Metal device and queue, translates the fixture, runs it
//!   once with `run_words_profiled`, checks the output and prints one sample line.
//! - **Repeated.** In this process it creates one device, then translates and runs
//!   the same fixture `--reps` times. The first run is reported on its own.
//!
//! Every sample must produce the expected words. One wrong output, failed child or
//! unparsable sample line stops the run before anything is written. The
//! numbers describe this host only; two distributions do not explain each other.

use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use xenora_metal::NativeCompute;
use xenora_shader::{Architecture, ENTRY_POINT, compile};

use crate::startup_report::Report;
use crate::{Fixture, Metadata, expand, parse_manifest, percentile, slug};

const DEFAULT_PROCESSES: usize = 30;
const DEFAULT_REPS: usize = 100;
/// A fresh process that has not exited by then is killed and fails the run.
const CHILD_TIMEOUT: Duration = Duration::from_secs(60);

/// Phases of one run, in seconds. `spawn_to_main` exists only for fresh processes.
pub(crate) const PHASES: [&str; 8] = [
    "spawn_to_main",
    "device",
    "translate",
    "library",
    "pipeline",
    "submit",
    "gpu",
    "readback",
];

/// One run's phase timings, indexed like [`PHASES`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Sample(pub(crate) [f64; PHASES.len()]);

impl Sample {
    fn line(&self) -> String {
        let fields: Vec<String> = PHASES
            .iter()
            .zip(self.0)
            .map(|(name, value)| format!("{name}={value:e}"))
            .collect();
        format!("startup-sample {}", fields.join(" "))
    }

    /// Parses exactly the line [`Sample::line`] prints: every phase once, in order.
    pub(crate) fn parse(line: &str) -> Result<Self, String> {
        let rest = line
            .strip_prefix("startup-sample ")
            .ok_or("missing startup-sample prefix")?;
        let fields: Vec<&str> = rest.split(' ').collect();
        if fields.len() != PHASES.len() {
            return Err(format!(
                "expected {} fields, got {}",
                PHASES.len(),
                fields.len()
            ));
        }
        let mut sample = Self::default();
        for (i, (field, name)) in fields.iter().zip(PHASES).enumerate() {
            let value = field
                .strip_prefix(name)
                .and_then(|v| v.strip_prefix('='))
                .ok_or_else(|| format!("field {i} is not {name}=<seconds>"))?;
            let value: f64 = value.parse().map_err(|_| format!("bad {name} value"))?;
            if !value.is_finite() || value < 0.0 {
                return Err(format!("{name} must be finite and non-negative"));
            }
            sample.0[i] = value;
        }
        Ok(sample)
    }
}

/// Median, tail and extremes of one phase.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Summary {
    pub(crate) n: usize,
    pub(crate) min: f64,
    pub(crate) median: f64,
    pub(crate) p90: f64,
    pub(crate) p99: f64,
    pub(crate) max: f64,
}

pub(crate) fn summarize(values: &[f64]) -> Option<Summary> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(Summary {
        n: sorted.len(),
        min: sorted[0],
        median: percentile(&sorted, 0.5),
        p90: percentile(&sorted, 0.9),
        p99: percentile(&sorted, 0.99),
        max: sorted[sorted.len() - 1],
    })
}

struct Options {
    fixture: Option<String>,
    words: Option<usize>,
    processes: usize,
    reps: usize,
    output: PathBuf,
}

fn count(value: Option<String>, flag: &str) -> Result<usize, String> {
    let n: usize = value
        .ok_or_else(|| format!("{flag} needs a count"))?
        .parse()
        .map_err(|_| format!("{flag} must be a positive integer"))?;
    if n == 0 {
        return Err(format!("{flag} must be at least 1"));
    }
    Ok(n)
}

fn parse_options(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        fixture: None,
        words: None,
        processes: DEFAULT_PROCESSES,
        reps: DEFAULT_REPS,
        output: PathBuf::from("artifacts/startup"),
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fixture" => options.fixture = Some(args.next().ok_or("--fixture needs a name")?),
            "--words" => options.words = Some(count(args.next(), "--words")?),
            "--processes" => options.processes = count(args.next(), "--processes")?,
            "--reps" => options.reps = count(args.next(), "--reps")?,
            "--output" => {
                options.output = PathBuf::from(args.next().ok_or("--output needs a path")?);
            }
            other => return Err(format!("unknown startup argument {other}")),
        }
    }
    Ok(options)
}

/// The fixture to time and its word count, after the reference agrees with it.
fn select(options: &Options) -> Result<(Fixture, usize), Box<dyn Error>> {
    let fixtures = parse_manifest(&fs::read_to_string("benchmarks/manifest.toml")?)?;
    let fixture = match &options.fixture {
        Some(name) => fixtures.into_iter().find(|f| &f.name == name),
        None => fixtures.into_iter().next(),
    }
    .ok_or("fixture not found in benchmarks/manifest.toml")?;
    let code = fs::read(&fixture.path)?;
    let reference = xenora_ref::execute(&code, &fixture.input).map_err(|e| e.to_string())?;
    if reference != fixture.expected {
        return Err("reference disagrees with hand-derived expected values".into());
    }
    let words = options
        .words
        .unwrap_or_else(|| *fixture.sizes.first().unwrap_or(&fixture.input.len()));
    Ok((fixture, words))
}

/// Translates and runs the fixture once on `metal`, checking the output.
fn run_once(
    metal: &NativeCompute,
    fixture: &Fixture,
    words: usize,
    sample: &mut Sample,
) -> Result<(), Box<dyn Error>> {
    let input = expand(&fixture.input, words);
    let expected = expand(&fixture.expected, words);
    let start = Instant::now();
    let code = fs::read(&fixture.path)?;
    let compiled = compile(Architecture::Gfx1030, &code).map_err(|e| e.to_string())?;
    sample.0[2] = start.elapsed().as_secs_f64();
    let profile = metal
        .run_words_profiled(&compiled.msl, ENTRY_POINT, &input)
        .map_err(|e| e.to_string())?;
    if profile.output != expected {
        return Err(format!("{} output disagrees with known answers", fixture.name).into());
    }
    sample.0[3] = profile.library_seconds;
    sample.0[4] = profile.pipeline_seconds;
    sample.0[5] = profile.host_seconds - profile.library_seconds - profile.pipeline_seconds;
    sample.0[6] = profile.gpu_seconds;
    sample.0[7] = profile.readback_seconds;
    Ok(())
}

fn unix_nanos(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

/// Entry point of one fresh child process, called first thing in `main`.
///
/// `entered` is the wall-clock time at `main`; the parent passes the wall-clock
/// time just before it spawned this process.
pub(crate) fn child(
    entered: SystemTime,
    mut args: impl Iterator<Item = String>,
) -> Result<(), Box<dyn Error>> {
    let spawned: u128 = args.next().ok_or("missing spawn time")?.parse()?;
    let options = parse_options(args)?;
    let (fixture, words) = select(&options)?;
    let mut sample = Sample::default();
    sample.0[0] = unix_nanos(entered).saturating_sub(spawned) as f64 / 1e9;
    let start = Instant::now();
    let metal = NativeCompute::new().map_err(|e| e.to_string())?;
    sample.0[1] = start.elapsed().as_secs_f64();
    run_once(&metal, &fixture, words, &mut sample)?;
    println!("{}", sample.line());
    Ok(())
}

fn forwarded(fixture: &Fixture, words: usize) -> Vec<String> {
    vec![
        "--fixture".into(),
        fixture.name.clone(),
        "--words".into(),
        words.to_string(),
    ]
}

/// Runs one fresh child and returns its spawn-to-exit wall time and stdout.
///
/// The child inherits this process's environment on purpose, so its `MTL_*`
/// validation settings are the ones the run's metadata records.
fn spawn_child(
    exe: &std::path::Path,
    fixture: &Fixture,
    words: usize,
) -> Result<(f64, String), Box<dyn Error>> {
    let started = Instant::now();
    let spawned = unix_nanos(SystemTime::now());
    let mut child = Command::new(exe)
        .arg("startup-child")
        .arg(spawned.to_string())
        .args(forwarded(fixture, words))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Drain both pipes while waiting, so a child that writes more than a pipe
    // buffer (validation logs, for example) cannot block and look like a hang.
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut text);
            }
            text
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > CHILD_TIMEOUT {
            child.kill()?;
            child.wait()?;
            return Err(format!("no exit within {} s", CHILD_TIMEOUT.as_secs()).into());
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let seconds = started.elapsed().as_secs_f64();
    let stdout = out.join().map_err(|_| "stdout reader panicked")?;
    let stderr = err.join().map_err(|_| "stderr reader panicked")?;
    if !status.success() {
        return Err(format!("{status}: {}", stderr.trim()).into());
    }
    Ok((seconds, stdout))
}

/// Runs both regimes and writes `startup.json` and `report.md`.
pub(crate) fn run(args: impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
    let options = parse_options(args)?;
    let (fixture, words) = select(&options)?;
    let exe = std::env::current_exe()?;

    let mut fresh = Vec::with_capacity(options.processes);
    let mut wall = Vec::with_capacity(options.processes);
    for i in 0..options.processes {
        let (seconds, stdout) = spawn_child(&exe, &fixture, words)
            .map_err(|e| format!("fresh process {i} failed: {e}"))?;
        wall.push(seconds);
        let line = stdout
            .lines()
            .find(|l| l.starts_with("startup-sample "))
            .ok_or_else(|| format!("fresh process {i} printed no sample"))?;
        fresh.push(Sample::parse(line).map_err(|e| format!("fresh process {i}: {e}"))?);
    }

    let mut first = Sample::default();
    let start = Instant::now();
    let metal = NativeCompute::new().map_err(|e| e.to_string())?;
    first.0[1] = start.elapsed().as_secs_f64();
    run_once(&metal, &fixture, words, &mut first)?;
    let mut repeated = Vec::with_capacity(options.reps);
    for _ in 0..options.reps {
        let mut sample = Sample::default();
        run_once(&metal, &fixture, words, &mut sample)?;
        repeated.push(sample);
    }

    let metadata = Metadata::collect(&metal.device_name(), options.reps);
    let dir = options
        .output
        .join(slug(&metadata.device))
        .join(&metadata.run_id);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("metadata.json"), metadata.to_json())?;
    let report = Report {
        fixture: &fixture.name,
        words,
        fresh: &fresh,
        wall: &wall,
        first: &first,
        repeated: &repeated,
    };
    fs::write(dir.join("startup.json"), report.json())?;
    let markdown = report.markdown(&metadata);
    fs::write(dir.join("report.md"), &markdown)?;
    println!("{markdown}");
    println!("Startup results written to {}", dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_lines_round_trip() {
        let sample = Sample([0.5, 1e-3, 2.5e-4, 0.0, 7.0, 1e-9, 3.25, 42.0]);
        assert_eq!(Sample::parse(&sample.line()), Ok(sample));
    }

    #[test]
    fn sample_lines_reject_damaged_input() {
        let line = Sample([1.0; PHASES.len()]).line();
        assert!(Sample::parse(&line.replacen("startup-sample", "sample", 1)).is_err());
        assert!(Sample::parse(&line.replacen("device=", "devise=", 1)).is_err());
        assert!(Sample::parse(&line.replacen("gpu=1e0", "gpu=-1e0", 1)).is_err());
        assert!(Sample::parse(&line.replacen("gpu=1e0", "gpu=NaN", 1)).is_err());
        assert!(Sample::parse(&line.replacen("gpu=1e0", "gpu=x", 1)).is_err());
        assert!(Sample::parse(line.rsplit_once(' ').unwrap().0).is_err());
        assert!(Sample::parse(&format!("{line} extra=1")).is_err());
        // Phases out of order are refused even when every name is present.
        let swapped = line
            .replacen("device=", "TMP=", 1)
            .replacen("translate=", "device=", 1)
            .replacen("TMP=", "translate=", 1);
        assert!(Sample::parse(&swapped).is_err());
    }

    #[test]
    fn summary_uses_interpolated_percentiles() {
        // 1..=100: position 0.9 * 99 = 89.1 lies between 90 and 91, and
        // 0.99 * 99 = 98.01 between 99 and 100.
        let values: Vec<f64> = (1..=100).rev().map(f64::from).collect();
        let s = summarize(&values).unwrap();
        assert_eq!((s.n, s.min, s.max), (100, 1.0, 100.0));
        assert!((s.median - 50.5).abs() < 1e-9);
        assert!((s.p90 - 90.1).abs() < 1e-9);
        assert!((s.p99 - 99.01).abs() < 1e-9);
        assert_eq!(summarize(&[]), None);
        let one = summarize(&[3.0]).unwrap();
        assert_eq!(
            (one.median, one.p90, one.p99, one.max),
            (3.0, 3.0, 3.0, 3.0)
        );
    }

    #[test]
    fn options_reject_bad_counts_and_flags() {
        let parse = |args: &[&str]| parse_options(args.iter().map(|s| s.to_string()));
        let defaults = parse(&[]).unwrap();
        assert_eq!(
            (defaults.processes, defaults.reps),
            (DEFAULT_PROCESSES, DEFAULT_REPS)
        );
        let set = parse(&["--processes", "3", "--reps", "5", "--words", "64"]).unwrap();
        assert_eq!((set.processes, set.reps, set.words), (3, 5, Some(64)));
        assert!(parse(&["--processes", "0"]).is_err());
        assert!(parse(&["--reps", "-1"]).is_err());
        assert!(parse(&["--words"]).is_err());
        assert!(parse(&["--fast"]).is_err());
    }
}
