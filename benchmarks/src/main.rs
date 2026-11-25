// SPDX-License-Identifier: GPL-3.0-or-later

//! Correctness-gated benchmark harness for the Phase 01 corpus.
//!
//! Reads `benchmarks/manifest.toml`, verifies every fixture against the
//! independent reference and the native GPU before timing it, then writes
//! metadata, raw JSON and a generated markdown report. A fixture that fails
//! correctness is excluded and listed as such; it is never timed.

use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use xenora_metal::{NativeCompute, RunProfile};
use xenora_shader::{Architecture, ENTRY_POINT, compile};

const WARMUP_REPS: usize = 2;
const DEFAULT_REPS: usize = 7;
const BATCH: usize = 8;

#[derive(Debug, Clone)]
struct Fixture {
    name: String,
    stage: String,
    source: String,
    path: String,
    input: Vec<u32>,
    expected: Vec<u32>,
    sizes: Vec<usize>,
}

#[derive(Debug, Clone, Default)]
struct Stats {
    median: f64,
    min: f64,
    max: f64,
    q1: f64,
    q3: f64,
    reps: usize,
}

impl Stats {
    fn from_samples(mut samples: Vec<f64>) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = samples.len();
        let median = percentile(&samples, 0.5);
        Self {
            median,
            min: samples[0],
            max: samples[n - 1],
            q1: percentile(&samples, 0.25),
            q3: percentile(&samples, 0.75),
            reps: n,
        }
    }
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let position = fraction * (sorted.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let weight = position - lower as f64;
        sorted[lower] * (1.0 - weight) + sorted[upper] * weight
    }
}

#[derive(Debug, Clone)]
struct SizeResult {
    words: usize,
    compile: Stats,
    library: Stats,
    pipeline: Stats,
    gpu: Stats,
    host: Stats,
    /// Host submission stages inside `host`, plus readback after it.
    submit: SubmitStats,
    /// Fraction of samples whose GPU timestamp delta was zero.
    gpu_zero_fraction: f64,
}

/// Host-side split of one dispatch's submission path.
///
/// `other` is `host` minus library, pipeline and the four stages below it,
/// per sample: compile options, function lookup, bounds and validation checks.
#[derive(Debug, Clone, Default)]
struct SubmitStats {
    alloc: Stats,
    encode: Stats,
    commit: Stats,
    wait: Stats,
    readback: Stats,
    other: Stats,
}

#[derive(Debug, Clone, Default)]
struct SubmitSamples {
    alloc: Vec<f64>,
    encode: Vec<f64>,
    commit: Vec<f64>,
    wait: Vec<f64>,
    readback: Vec<f64>,
    other: Vec<f64>,
}

impl SubmitSamples {
    fn push(&mut self, profile: &RunProfile) {
        self.alloc.push(profile.alloc_seconds);
        self.encode.push(profile.encode_seconds);
        self.commit.push(profile.commit_seconds);
        self.wait.push(profile.wait_seconds);
        self.readback.push(profile.readback_seconds);
        self.other.push(
            profile.host_seconds
                - profile.library_seconds
                - profile.pipeline_seconds
                - profile.alloc_seconds
                - profile.encode_seconds
                - profile.commit_seconds
                - profile.wait_seconds,
        );
    }

    fn stats(self) -> SubmitStats {
        SubmitStats {
            alloc: Stats::from_samples(self.alloc),
            encode: Stats::from_samples(self.encode),
            commit: Stats::from_samples(self.commit),
            wait: Stats::from_samples(self.wait),
            readback: Stats::from_samples(self.readback),
            other: Stats::from_samples(self.other),
        }
    }
}

#[derive(Debug, Clone)]
struct FixtureResult {
    name: String,
    stage: String,
    source: String,
    status: String,
    reason: String,
    sizes: Vec<SizeResult>,
    batch: Option<BatchResult>,
}

#[derive(Debug, Clone)]
struct BatchResult {
    words: usize,
    sync: Stats,
    pipelined: Stats,
}

mod startup;
mod startup_report;

fn main() -> Result<(), Box<dyn Error>> {
    // Taken before anything else so a fresh startup child can report the time
    // from its parent's spawn to `main`.
    let entered = SystemTime::now();
    let mut args = std::env::args().skip(1).peekable();
    match args.peek().map(String::as_str) {
        Some("startup") => return startup::run(args.skip(1)),
        Some("startup-child") => return startup::child(entered, args.skip(1)),
        _ => {}
    }
    let mut output = PathBuf::from("benchmarks/results");
    let mut reps = DEFAULT_REPS;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => {
                output = PathBuf::from(args.next().ok_or("--output needs a path")?);
            }
            "--reps" => {
                reps = args
                    .next()
                    .ok_or("--reps needs a count")?
                    .parse()
                    .map_err(|_| "--reps must be a positive integer")?;
            }
            "--help" | "-h" => {
                println!(
                    "usage: xenora-bench [--output DIR] [--reps N]\n       \
                     xenora-bench startup [--fixture NAME] [--words N] [--processes N] \
                     [--reps N] [--output DIR]"
                );
                return Ok(());
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    if reps == 0 {
        return Err("--reps must be at least 1".into());
    }

    let manifest_text = fs::read_to_string("benchmarks/manifest.toml")?;
    let fixtures = parse_manifest(&manifest_text)?;
    if fixtures.is_empty() {
        return Err("manifest lists no fixtures".into());
    }

    let metal = NativeCompute::new().map_err(|e| e.to_string())?;
    let device = metal.device_name();
    let metadata = Metadata::collect(&device, reps);

    let mut results = Vec::with_capacity(fixtures.len());
    for fixture in &fixtures {
        results.push(run_fixture(&metal, fixture, reps));
    }

    let run_id = metadata.run_id.clone();
    let hardware_dir = slug(&device);
    let run_dir = output.join(&hardware_dir).join(&run_id);
    fs::create_dir_all(&run_dir)?;

    fs::write(run_dir.join("metadata.json"), metadata.to_json())?;
    fs::write(run_dir.join("results.json"), results_json(&results))?;
    fs::write(
        run_dir.join("report.md"),
        report_markdown(&metadata, &results),
    )?;

    println!("Benchmark results written to {}", run_dir.display());
    for result in &results {
        println!(
            "  {:<16} {:<10} {}",
            result.name,
            result.status,
            if result.reason.is_empty() {
                String::new()
            } else {
                format!("({})", result.reason)
            }
        );
    }

    let excluded = results.iter().filter(|r| r.status != "benchmarked").count();
    if excluded > 0 {
        return Err(format!("{excluded} fixture(s) excluded from timing").into());
    }
    Ok(())
}

fn run_fixture(metal: &NativeCompute, fixture: &Fixture, reps: usize) -> FixtureResult {
    let mut result = FixtureResult {
        name: fixture.name.clone(),
        stage: fixture.stage.clone(),
        source: fixture.source.clone(),
        status: "not-implemented".to_string(),
        reason: String::new(),
        sizes: Vec::new(),
        batch: None,
    };

    let code = match fs::read(&fixture.path) {
        Ok(code) => code,
        Err(error) => {
            result.status = "excluded".to_string();
            result.reason = format!("cannot read {}: {error}", fixture.path);
            return result;
        }
    };

    let compiled = match compile(Architecture::Gfx1030, &code) {
        Ok(compiled) => compiled,
        Err(error) => {
            result.status = "excluded".to_string();
            result.reason = format!("compile failed: {error}");
            return result;
        }
    };

    // Independent reference on the declared base input.
    let reference = match xenora_ref::execute(&code, &fixture.input) {
        Ok(reference) => reference,
        Err(error) => {
            result.status = "excluded".to_string();
            result.reason = format!("reference execution failed: {error}");
            return result;
        }
    };
    if reference != fixture.expected {
        result.status = "excluded".to_string();
        result.reason = "reference disagrees with hand-derived expected values".to_string();
        return result;
    }

    // Native correctness at the smallest declared size before any timing.
    let base_words = *fixture.sizes.first().unwrap_or(&fixture.input.len());
    let base_input = expand(&fixture.input, base_words);
    let base_expected = expand(&fixture.expected, base_words);
    match metal.run_words(&compiled.msl, ENTRY_POINT, &base_input) {
        Ok(actual) if actual == base_expected => {}
        Ok(_) => {
            result.status = "excluded".to_string();
            result.reason = "GPU output disagrees with known answers".to_string();
            return result;
        }
        Err(error) => {
            result.status = "excluded".to_string();
            result.reason = format!("GPU execution failed: {error}");
            return result;
        }
    }

    let mut sizes = Vec::with_capacity(fixture.sizes.len());
    for &words in &fixture.sizes {
        let input = expand(&fixture.input, words);
        let expected = expand(&fixture.expected, words);
        let mut compile_samples = Vec::with_capacity(reps);
        let mut library_samples = Vec::with_capacity(reps);
        let mut pipeline_samples = Vec::with_capacity(reps);
        let mut gpu_samples = Vec::with_capacity(reps);
        let mut host_samples = Vec::with_capacity(reps);
        let mut submit_samples = SubmitSamples::default();
        let mut gpu_zero = 0usize;

        let mut mismatch = None;
        for rep in 0..(WARMUP_REPS + reps) {
            let compile_start = std::time::Instant::now();
            let recompiled = match compile(Architecture::Gfx1030, &code) {
                Ok(compiled) => compiled,
                Err(error) => {
                    mismatch = Some(format!("compile failed at {words} words: {error}"));
                    break;
                }
            };
            let compile_seconds = compile_start.elapsed().as_secs_f64();

            let profile = match metal.run_words_profiled(&recompiled.msl, ENTRY_POINT, &input) {
                Ok(profile) => profile,
                Err(error) => {
                    mismatch = Some(format!("GPU run failed at {words} words: {error}"));
                    break;
                }
            };
            if profile.output != expected {
                mismatch = Some(format!("GPU mismatch at {words} words"));
                break;
            }
            if rep < WARMUP_REPS {
                continue;
            }
            compile_samples.push(compile_seconds);
            library_samples.push(profile.library_seconds);
            pipeline_samples.push(profile.pipeline_seconds);
            gpu_samples.push(profile.gpu_seconds);
            host_samples.push(profile.host_seconds);
            submit_samples.push(&profile);
            if profile.gpu_seconds <= 0.0 {
                gpu_zero += 1;
            }
        }

        if let Some(reason) = mismatch {
            result.status = "excluded".to_string();
            result.reason = reason;
            return result;
        }

        let measured = gpu_samples.len();
        sizes.push(SizeResult {
            words,
            compile: Stats::from_samples(compile_samples),
            library: Stats::from_samples(library_samples),
            pipeline: Stats::from_samples(pipeline_samples),
            gpu: Stats::from_samples(gpu_samples),
            host: Stats::from_samples(host_samples),
            submit: submit_samples.stats(),
            gpu_zero_fraction: if measured == 0 {
                0.0
            } else {
                gpu_zero as f64 / measured as f64
            },
        });
    }

    let batch_words = fixture
        .sizes
        .iter()
        .copied()
        .min_by_key(|words| words.abs_diff(1024))
        .unwrap_or(1024);
    match measure_batch(metal, &code, fixture, batch_words, reps) {
        Ok(batch) => result.batch = Some(batch),
        Err(reason) => {
            result.status = "excluded".to_string();
            result.reason = reason;
            return result;
        }
    }
    result.sizes = sizes;
    result.status = "benchmarked".to_string();
    result
}

fn measure_batch(
    metal: &NativeCompute,
    code: &[u8],
    fixture: &Fixture,
    words: usize,
    reps: usize,
) -> Result<BatchResult, String> {
    let input = expand(&fixture.input, words);
    let expected = expand(&fixture.expected, words);
    let mut sync = Vec::with_capacity(reps);
    let mut pipelined = Vec::with_capacity(reps);
    for rep in 0..(WARMUP_REPS + reps) {
        for (samples, variant) in [(&mut sync, "sync"), (&mut pipelined, "pipelined")] {
            let mut tokens = Vec::with_capacity(BATCH);
            let mut outputs = Vec::with_capacity(BATCH);
            let start = std::time::Instant::now();
            for _ in 0..BATCH {
                let compiled = compile(Architecture::Gfx1030, code).map_err(|error| {
                    format!("{variant} batch compile failed at {words} words: {error}")
                })?;
                if variant == "sync" {
                    let profile = metal
                        .run_words_profiled(&compiled.msl, ENTRY_POINT, &input)
                        .map_err(|error| {
                            format!("{variant} batch run failed at {words} words: {error}")
                        })?;
                    outputs.push(profile.output);
                } else {
                    let token = metal
                        .submit_words(&compiled.msl, ENTRY_POINT, &input)
                        .map_err(|error| {
                            format!("{variant} batch submit failed at {words} words: {error}")
                        })?;
                    tokens.push(token);
                }
            }
            for token in tokens {
                let profile = metal.wait(token).map_err(|error| {
                    format!("{variant} batch wait failed at {words} words: {error}")
                })?;
                outputs.push(profile.output);
            }
            let per_dispatch = start.elapsed().as_secs_f64() / BATCH as f64;
            if outputs.iter().any(|output| *output != expected) {
                return Err(format!("{variant} batch GPU mismatch at {words} words"));
            }
            if rep >= WARMUP_REPS {
                samples.push(per_dispatch);
            }
        }
    }
    Ok(BatchResult {
        words,
        sync: Stats::from_samples(sync),
        pipelined: Stats::from_samples(pipelined),
    })
}

fn expand(base: &[u32], words: usize) -> Vec<u32> {
    (0..words).map(|i| base[i % base.len()]).collect()
}

// --- Manifest parsing (documented subset: [[fixture]] tables, string and int arrays) ---

fn parse_manifest(text: &str) -> Result<Vec<Fixture>, String> {
    let mut fixtures = Vec::new();
    let mut current: Option<Fixture> = None;
    for (line_no, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line == "[[fixture]]" {
            if let Some(fixture) = current.take() {
                fixtures.push(finish(fixture, line_no)?);
            }
            current = Some(Fixture {
                name: String::new(),
                stage: String::new(),
                source: String::new(),
                path: String::new(),
                input: Vec::new(),
                expected: Vec::new(),
                sizes: Vec::new(),
            });
            continue;
        }
        let Some(fixture) = current.as_mut() else {
            return Err(format!(
                "line {}: key outside a [[fixture]] table",
                line_no + 1
            ));
        };
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: expected key = value", line_no + 1))?;
        let key = key.trim();
        let value = value.trim();
        match key {
            "name" => fixture.name = parse_string(value)?,
            "stage" => fixture.stage = parse_string(value)?,
            "source" => fixture.source = parse_string(value)?,
            "path" => fixture.path = parse_string(value)?,
            "input" => fixture.input = parse_int_array(value)?,
            "expected" => fixture.expected = parse_int_array(value)?,
            "sizes" => {
                fixture.sizes = parse_int_array(value)?
                    .into_iter()
                    .map(|n| n as usize)
                    .collect();
            }
            other => return Err(format!("line {}: unknown key {other}", line_no + 1)),
        }
    }
    if let Some(fixture) = current.take() {
        fixtures.push(finish(fixture, 0)?);
    }
    Ok(fixtures)
}

fn finish(fixture: Fixture, line_no: usize) -> Result<Fixture, String> {
    let context = if fixture.name.is_empty() {
        format!("fixture ending near line {line_no}")
    } else {
        format!("fixture {}", fixture.name)
    };
    if fixture.name.is_empty()
        || fixture.stage.is_empty()
        || fixture.source.is_empty()
        || fixture.path.is_empty()
    {
        return Err(format!("{context}: missing required string field"));
    }
    if fixture.input.is_empty() || fixture.expected.len() != fixture.input.len() {
        return Err(format!(
            "{context}: input and expected must be equal-length"
        ));
    }
    if fixture.sizes.is_empty() || fixture.sizes.contains(&0) {
        return Err(format!(
            "{context}: sizes must be a nonempty list of positive integers"
        ));
    }
    Ok(fixture)
}

fn parse_string(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    let inner = trimmed
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .ok_or_else(|| format!("expected a quoted string, got {trimmed}"))?;
    Ok(inner.to_string())
}

fn parse_int_array(value: &str) -> Result<Vec<u32>, String> {
    let trimmed = value.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .ok_or_else(|| format!("expected an integer array, got {trimmed}"))?;
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    inner
        .split(',')
        .map(|item| {
            item.trim()
                .parse::<u32>()
                .map_err(|_| format!("invalid integer {}", item.trim()))
        })
        .collect()
}

// --- Metadata and output ---

struct Metadata {
    run_id: String,
    timestamp: String,
    device: String,
    host_arch: String,
    os_version: String,
    os_build: String,
    rustc: String,
    xcode: String,
    commit: String,
    profile: String,
    api_validation: String,
    shader_validation: String,
    power: String,
    thermal: String,
    reps: usize,
    warmup: usize,
}

impl Metadata {
    fn collect(device: &str, reps: usize) -> Self {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            run_id: format!("run-{seconds}"),
            timestamp: format!("unix:{seconds}"),
            device: device.to_string(),
            host_arch: std::env::consts::ARCH.to_string(),
            os_version: command_line("sw_vers", &["-productVersion"]),
            os_build: command_line("sw_vers", &["-buildVersion"]),
            rustc: command_line("rustc", &["-vV"])
                .lines()
                .next()
                .unwrap_or("unknown")
                .to_string(),
            xcode: command_line("xcodebuild", &["-version"])
                .lines()
                .next()
                .unwrap_or("unknown")
                .to_string(),
            commit: command_line("git", &["rev-parse", "HEAD"]),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .to_string(),
            api_validation: env_or("MTL_DEBUG_LAYER"),
            shader_validation: env_or("MTL_SHADER_VALIDATION"),
            power: power_source(),
            thermal: thermal_state(),
            reps,
            warmup: WARMUP_REPS,
        }
    }

    fn to_json(&self) -> String {
        format!(
            "{{\n  \"run_id\": {},\n  \"timestamp\": {},\n  \"device\": {},\n  \"host_arch\": {},\n  \"os_version\": {},\n  \"os_build\": {},\n  \"rustc\": {},\n  \"xcode\": {},\n  \"commit\": {},\n  \"profile\": {},\n  \"api_validation\": {},\n  \"shader_validation\": {},\n  \"power\": {},\n  \"thermal\": {},\n  \"reps\": {},\n  \"warmup\": {}\n}}\n",
            json_str(&self.run_id),
            json_str(&self.timestamp),
            json_str(&self.device),
            json_str(&self.host_arch),
            json_str(&self.os_version),
            json_str(&self.os_build),
            json_str(&self.rustc),
            json_str(&self.xcode),
            json_str(&self.commit),
            json_str(&self.profile),
            json_str(&self.api_validation),
            json_str(&self.shader_validation),
            json_str(&self.power),
            json_str(&self.thermal),
            self.reps,
            self.warmup
        )
    }
}

fn command_line(program: &str, args: &[&str]) -> String {
    match Command::new(program).args(args).output() {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => "unavailable".to_string(),
    }
}

fn env_or(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| "unset".to_string())
}

/// Report the power source line from `pmset`, or `unavailable` off macOS.
fn power_source() -> String {
    let output = command_line("pmset", &["-g", "batt"]);
    for line in output.lines() {
        let line = line.trim();
        if line.contains("AC Power") {
            return "AC Power".to_string();
        }
        if line.contains("Battery Power") {
            return "Battery Power".to_string();
        }
    }
    "unavailable".to_string()
}

/// Report the CPU thermal and speed limits from `pmset`, compactly.
fn thermal_state() -> String {
    let output = command_line("pmset", &["-g", "therm"]);
    let mut parts = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        for key in ["CPU_Scheduler_Limit", "CPU_Speed_Limit"] {
            if let Some(value) = line.strip_prefix(key) {
                parts.push(format!("{key}{}", value.trim()));
            }
        }
    }
    if !parts.is_empty() {
        parts.join(", ")
    } else if output.contains("No thermal warning level has been recorded") {
        "no warnings recorded".to_string()
    } else {
        "unavailable".to_string()
    }
}

fn slug(device: &str) -> String {
    device
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn json_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn stats_json(stats: &Stats) -> String {
    format!(
        "{{ \"median\": {:.9e}, \"min\": {:.9e}, \"max\": {:.9e}, \"q1\": {:.9e}, \"q3\": {:.9e}, \"reps\": {} }}",
        stats.median, stats.min, stats.max, stats.q1, stats.q3, stats.reps
    )
}

fn results_json(results: &[FixtureResult]) -> String {
    let mut out = String::from("{\n  \"fixtures\": [\n");
    for (index, result) in results.iter().enumerate() {
        let _ = writeln!(
            out,
            "    {{\n      \"name\": {},\n      \"stage\": {},\n      \"source\": {},\n      \"status\": {},\n      \"reason\": {},\n      \"sizes\": [",
            json_str(&result.name),
            json_str(&result.stage),
            json_str(&result.source),
            json_str(&result.status),
            json_str(&result.reason)
        );
        for (size_index, size) in result.sizes.iter().enumerate() {
            let _ = writeln!(
                out,
                "        {{ \"words\": {}, \"compile\": {}, \"library\": {}, \"pipeline\": {}, \"gpu\": {}, \"host\": {}, \"alloc\": {}, \"encode\": {}, \"commit\": {}, \"wait\": {}, \"readback\": {}, \"host_other\": {}, \"gpu_zero_fraction\": {:.4} }}{}",
                size.words,
                stats_json(&size.compile),
                stats_json(&size.library),
                stats_json(&size.pipeline),
                stats_json(&size.gpu),
                stats_json(&size.host),
                stats_json(&size.submit.alloc),
                stats_json(&size.submit.encode),
                stats_json(&size.submit.commit),
                stats_json(&size.submit.wait),
                stats_json(&size.submit.readback),
                stats_json(&size.submit.other),
                size.gpu_zero_fraction,
                if size_index + 1 == result.sizes.len() {
                    ""
                } else {
                    ","
                }
            );
        }
        out.push_str("      ]");
        if let Some(batch) = &result.batch {
            let _ = write!(
                out,
                ",\n      \"batch\": {{ \"dispatches\": {BATCH}, \"words\": {}, \"sync\": {}, \"pipelined\": {} }}",
                batch.words,
                stats_json(&batch.sync),
                stats_json(&batch.pipelined)
            );
        }
        let _ = writeln!(
            out,
            "\n    }}{}",
            if index + 1 == results.len() { "" } else { "," }
        );
    }
    out.push_str("  ]\n}\n");
    out
}

fn report_markdown(metadata: &Metadata, results: &[FixtureResult]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Xenora benchmark report");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Generated by `xenora-bench`. Every timed fixture passed its correctness test in this run. \
         See `docs/testing/benchmarks.md` for the single-dispatch protocol."
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "## Environment");
    let _ = writeln!(out);
    let _ = writeln!(out, "| Field | Value |");
    let _ = writeln!(out, "| --- | --- |");
    for (key, value) in [
        ("Device", metadata.device.as_str()),
        ("Host arch", metadata.host_arch.as_str()),
        ("macOS", metadata.os_version.as_str()),
        ("Build", metadata.os_build.as_str()),
        ("Rust", metadata.rustc.as_str()),
        ("Xcode", metadata.xcode.as_str()),
        ("Commit", metadata.commit.as_str()),
        ("Profile", metadata.profile.as_str()),
        ("API validation", metadata.api_validation.as_str()),
        ("Shader validation", metadata.shader_validation.as_str()),
        ("Power source", metadata.power.as_str()),
        ("Thermal", metadata.thermal.as_str()),
    ] {
        let _ = writeln!(out, "| {key} | {} |", value.replace('|', "\\|"));
    }
    let _ = writeln!(
        out,
        "| Repetitions | {} measured, {} warm-up |",
        metadata.reps, metadata.warmup
    );
    if metadata.power != "AC Power" {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "> **Protocol note.** The machine was not confirmed on AC power. \
             The benchmark protocol in `docs/testing/benchmarks.md` requires AC power, \
             so these timings are not protocol-compliant release evidence."
        );
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "## Coverage");
    let _ = writeln!(out);
    let _ = writeln!(out, "| Fixture | Stage | Source | Status |");
    let _ = writeln!(out, "| --- | --- | --- | --- |");
    for result in results {
        let status = if result.reason.is_empty() {
            result.status.clone()
        } else {
            format!("{} ({})", result.status, result.reason)
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            result.name,
            result.stage,
            result.source,
            status.replace('|', "\\|")
        );
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "## Timings");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Durations in microseconds. `compile` is the Rust decode/IR/MSL pipeline, \
         `library` and `pipeline` are Metal compilation stages, `gpu` is the command-buffer \
         GPU time, and `host` is the wall time of one dispatch. Values are medians with \
         interquartile range; minimum and maximum are in `results.json`."
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| Fixture | Words | compile | library | pipeline | gpu | host | IQR(gpu) | zero-gpu |"
    );
    let _ = writeln!(
        out,
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    );
    for result in results.iter().filter(|r| r.status == "benchmarked") {
        for size in &result.sizes {
            let _ = writeln!(
                out,
                "| {} | {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.0}% |",
                result.name,
                size.words,
                size.compile.median * 1e6,
                size.library.median * 1e6,
                size.pipeline.median * 1e6,
                size.gpu.median * 1e6,
                size.host.median * 1e6,
                (size.gpu.q3 - size.gpu.q1) * 1e6,
                size.gpu_zero_fraction * 100.0
            );
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "## Submission split");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Medians in microseconds of the host stages inside `host`: `alloc` creates the input and \
         output shared buffers, `encode` creates the command buffer and encodes the dispatch, \
         `commit` and `wait` time `commit` and `waitUntilCompleted`, and `other` is the per-sample \
         remainder of `host` after library, pipeline and these four stages. `readback` copies \
         the output after `host` ends."
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| Fixture | Words | host | alloc | encode | commit | wait | other | readback |"
    );
    let _ = writeln!(
        out,
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    );
    for result in results.iter().filter(|r| r.status == "benchmarked") {
        for size in &result.sizes {
            let stages = &size.submit;
            let _ = writeln!(
                out,
                "| {} | {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} |",
                result.name,
                size.words,
                size.host.median * 1e6,
                stages.alloc.median * 1e6,
                stages.encode.median * 1e6,
                stages.commit.median * 1e6,
                stages.wait.median * 1e6,
                stages.other.median * 1e6,
                stages.readback.median * 1e6
            );
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Batched submission");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Median host microseconds per dispatch in batches of {BATCH}, including recompilation."
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| Fixture | Words | Sync (µs) | Pipelined (µs) | Sync / pipelined |"
    );
    let _ = writeln!(out, "| --- | --- | --- | --- | --- |");
    for result in results {
        if let Some(batch) = &result.batch {
            let _ = writeln!(
                out,
                "| {} | {} | {:.2} | {:.2} | {:.2}× |",
                result.name,
                batch.words,
                batch.sync.median * 1e6,
                batch.pipelined.median * 1e6,
                batch.sync.median / batch.pipelined.median
            );
        }
    }
    out
}
