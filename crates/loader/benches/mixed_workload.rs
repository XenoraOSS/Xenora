// SPDX-License-Identifier: GPL-3.0-or-later

//! Mixed CPU-to-GPU workload benchmark (#147).
//!
//! Each sample runs the original guest loop from `tests/mixed_workload.rs`
//! under the ARM64 DBT, reads the guest buffer through checked guest memory
//! and dispatches the translated `add` fixture on native Metal. A sample is
//! kept only when the guest checksum, every guest word and every GPU word
//! match the independently derived expectations; any rejected sample makes
//! the run exit nonzero without reporting that size.
//!
//! Phases, reported separately:
//! - `cpu`: host wall time of `DbtEngine::run` for the guest loop.
//! - `submission`: guest buffer read plus Metal buffer allocation, encoding
//!   and `commit`, all host wall time.
//! - `gpu`: `GPUEndTime - GPUStartTime` of the command buffer.
//! - `wait`: host time in `waitUntilCompleted` (includes the GPU time).
//! - `readback`: host copy of the output words after completion.
//! - `total`: host wall time of the whole sample, excluding the check.
//!
//! Run with
//! `MTL_DEBUG_LAYER=0 MTL_SHADER_VALIDATION=0 cargo bench --locked -p xenora-loader --bench mixed_workload -- --reps 201`.

#[allow(dead_code)]
#[path = "../tests/mixed_workload.rs"]
mod workload;

use std::process::{Command, ExitCode};
use std::time::Instant;
use workload::{MixedWorkload, OUTPUT_WORDS, add_kernel};
use xenora_metal::NativeCompute;
use xenora_shader::ENTRY_POINT;

const DEFAULT_REPS: usize = 101;
const DEFAULT_WARMUP: usize = 10;
const DEFAULT_SIZES: [u32; 3] = [64, 1024, OUTPUT_WORDS];
const PHASES: [&str; 6] = ["cpu", "submission", "gpu", "wait", "readback", "total"];

struct Options {
    reps: usize,
    warmup: usize,
    sizes: Vec<u32>,
}

fn options() -> Options {
    let mut options = Options {
        reps: DEFAULT_REPS,
        warmup: DEFAULT_WARMUP,
        sizes: DEFAULT_SIZES.to_vec(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().expect("missing option value");
        match arg.as_str() {
            "--reps" => options.reps = value().parse().expect("--reps takes a count"),
            "--warmup" => options.warmup = value().parse().expect("--warmup takes a count"),
            "--sizes" => {
                options.sizes = value()
                    .split(',')
                    .map(|size| size.parse().expect("--sizes takes word counts"))
                    .collect();
            }
            // Cargo passes `--bench`; other harness flags are ignored too.
            _ => {}
        }
    }
    assert!(options.reps > 0, "--reps must be positive");
    for &size in &options.sizes {
        assert!(
            (1..=OUTPUT_WORDS).contains(&size),
            "sizes must be 1..={OUTPUT_WORDS} words"
        );
    }
    options
}

fn command(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|_| "unknown".to_owned())
}

fn print_metadata(gpu: &NativeCompute, options: &Options) {
    let power = command("pmset", &["-g", "batt"]);
    let power = power
        .lines()
        .find(|line| line.contains("drawing from"))
        .unwrap_or("unknown");
    let therm = command("pmset", &["-g", "therm"]);
    let therm: Vec<_> = therm
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let dirty = !command("git", &["status", "--porcelain"]).is_empty();
    println!("# mixed_workload metadata");
    println!(
        "host_cpu: {}",
        command("sysctl", &["-n", "machdep.cpu.brand_string"])
    );
    println!("logical_cpus: {}", command("sysctl", &["-n", "hw.ncpu"]));
    println!("gpu: {}", gpu.device_name());
    println!(
        "os: macOS {} ({})",
        command("sw_vers", &["-productVersion"]),
        command("sw_vers", &["-buildVersion"])
    );
    println!(
        "commit: {}{}",
        command("git", &["rev-parse", "--short", "HEAD"]),
        if dirty { " (dirty worktree)" } else { "" }
    );
    println!(
        "profile: {}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    for variable in ["MTL_DEBUG_LAYER", "MTL_SHADER_VALIDATION"] {
        println!(
            "{variable}: {}",
            std::env::var(variable).unwrap_or_else(|_| "unset".to_owned())
        );
    }
    println!("power: {power}");
    println!("thermal: {}", therm.join("; "));
    println!("load_average: {}", command("sysctl", &["-n", "vm.loadavg"]));
    println!(
        "samples: {} warm-up then {} measured per size, each checked before it is kept",
        options.warmup, options.reps
    );
}

/// Linear interpolation between order statistics, as in `xenora-bench`.
fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let position = fraction * (sorted.len() - 1) as f64;
    let (lower, upper) = (position.floor() as usize, position.ceil() as usize);
    let weight = position - lower as f64;
    sorted[lower] * (1.0 - weight) + sorted[upper] * weight
}

fn report(phase: &str, samples: &mut [f64]) {
    samples.sort_by(f64::total_cmp);
    let n = samples.len();
    let mean = samples.iter().sum::<f64>() / n as f64;
    let variance = if n > 1 {
        samples.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / (n - 1) as f64
    } else {
        0.0
    };
    let us = |seconds: f64| seconds * 1e6;
    println!(
        "| {phase} | {n} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.4} | {:.1} |",
        us(percentile(samples, 0.5)),
        us(percentile(samples, 0.9)),
        us(percentile(samples, 0.99)),
        us(samples[0]),
        us(samples[n - 1]),
        us(mean),
        us(us(variance)),
        if mean > 0.0 {
            100.0 * variance.sqrt() / mean
        } else {
            0.0
        },
    );
}

fn main() -> ExitCode {
    let options = options();
    let gpu = NativeCompute::new().expect("native Metal device");
    let kernel = add_kernel();
    print_metadata(&gpu, &options);
    let mut failed = false;

    for &count in &options.sizes {
        let mut workload = MixedWorkload::new();
        let mut phases: [Vec<f64>; 6] = Default::default();
        let (mut rejected, mut gpu_zero, mut rebuilt) = (0usize, 0usize, 0usize);
        let mut first_mismatch = None;
        let fallbacks = workload.dbt.metrics.interpreter_fallbacks;

        for sample in 0..options.warmup + options.reps {
            workload.clear_output();
            let start = Instant::now();
            let cpu = workload.run_cpu(count);
            let cpu_seconds = start.elapsed().as_secs_f64();
            let read_start = Instant::now();
            let guest = workload.guest_words(count).expect("read the guest buffer");
            let read_seconds = read_start.elapsed().as_secs_f64();
            let profile = gpu
                .run_words_profiled(&kernel, ENTRY_POINT, &guest)
                .expect("dispatch on Metal");
            let total = start.elapsed().as_secs_f64();

            if let Err(mismatch) = MixedWorkload::check(&cpu, &guest, &profile.output, count) {
                rejected += 1;
                first_mismatch.get_or_insert(mismatch);
                continue;
            }
            if sample < options.warmup {
                continue;
            }
            if profile.library_seconds > 0.0 || profile.pipeline_seconds > 0.0 {
                rebuilt += 1;
            }
            if profile.gpu_seconds <= 0.0 {
                gpu_zero += 1;
            }
            let submission = read_seconds
                + profile.alloc_seconds
                + profile.encode_seconds
                + profile.commit_seconds;
            for (store, value) in phases.iter_mut().zip([
                cpu_seconds,
                submission,
                profile.gpu_seconds,
                profile.wait_seconds,
                profile.readback_seconds,
                total,
            ]) {
                store.push(value);
            }
        }

        println!();
        println!("## {count} words");
        if rejected > 0 {
            println!(
                "EXCLUDED: {rejected} of {} samples failed correctness (first: {:?}); no timings reported",
                options.warmup + options.reps,
                first_mismatch.expect("a mismatch was recorded")
            );
            failed = true;
            continue;
        }
        println!(
            "correctness: {} of {} samples passed; interpreter fallbacks: {}; DBT blocks compiled: {}; \
             pipeline rebuilds in measured samples: {rebuilt}; zero GPU timestamps: {gpu_zero}",
            options.warmup + options.reps,
            options.warmup + options.reps,
            workload.dbt.metrics.interpreter_fallbacks - fallbacks,
            workload.dbt.metrics.blocks_compiled,
        );
        println!(
            "| phase | n | median µs | p90 µs | p99 µs | min µs | max µs | mean µs | variance µs² | CV % |"
        );
        println!("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
        for (phase, samples) in PHASES.iter().zip(phases.iter_mut()) {
            report(phase, samples);
        }
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
