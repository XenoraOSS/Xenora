# Benchmark protocol

The benchmark harness measures each fixture in its manifest that passes its
correctness test in the same run. It never reports a number for a shader that
failed or was skipped.

## Corpus

`benchmarks/manifest.toml` lists each fixture with its stage, source, binary path,
declared input and hand-derived expected output, and the word counts to time. The
manifest uses a documented subset of TOML (`[[fixture]]` tables with string and
integer-array values) parsed by the harness, so no external TOML dependency is
pulled into the workspace.

A fixture is added to the corpus by adding a table. If it passes correctness it is
benchmarked automatically; there is no separate opt-in list.

## Correctness gate

Before any timing, the harness requires all of the following for each fixture:

1. The Rust pipeline (`xenora-shader::compile`) accepts the binary.
2. The independent reference interpreter agrees with the hand-derived `expected`
   values on the declared input.
3. The native GPU produces the expected output for the smallest declared size.

If any step fails the fixture is marked `excluded` with a reason, listed in the
coverage table, and not timed. A nonempty exclusion set makes the harness exit
nonzero. Measurements are included only after all sizes for a fixture
pass. A debug smoke run using a temporary manifest with sizes 4 and
4097 for the add fixture verified that rejection at 4097 produces an
empty timing list for that fixture while the other two fixtures retain
all five sizes. The smoke run is a correctness check for reporting.

## What is measured

Per fixture and per word count, over `--reps` measured repetitions after two
warm-up repetitions:

| Field | Source | Meaning |
| --- | --- | --- |
| `compile` | host monotonic clock | Rust decode, lift, verify and MSL generation |
| `library` | host monotonic clock | `newLibraryWithSource` Metal compilation |
| `pipeline` | host monotonic clock | compute pipeline state creation |
| `gpu` | `GPUEndTime - GPUStartTime` | command-buffer GPU execution time |
| `host` | host monotonic clock | wall time of one compile-and-dispatch call |

`gpu_zero_fraction` records the share of samples whose GPU timestamp delta was
zero, which happens for very short kernels. A high value means the GPU timing is
at timer resolution and should not be read as a precise duration.

Statistics reported are median, minimum, maximum and the first and third quartiles.
The report never uses best-of.

## Protocol

- Release builds only for published numbers; the metadata records the profile.
- The machine must be otherwise idle, on AC power, with the thermal state noted.
- Validation is off for timing runs. The metadata records the `MTL_DEBUG_LAYER`
  and `MTL_SHADER_VALIDATION` values in effect so a validated run is never confused
  with a timing run.
- The fanless M5 MacBook Air throttles under sustained load; a single run does not
  characterize sustained performance.
- No comparison to Xbox hardware, other emulators or other platforms is made.

## Output

`benchmarks/results/<hardware>/<run-id>/` contains `metadata.json`, `results.json`
and `report.md`. Only small text reports are committed. Raw logs stay under the
ignored `artifacts/` directory.

## Reproduction

```sh
cargo build --release -p xenora-benchmarks
MTL_DEBUG_LAYER=0 MTL_SHADER_VALIDATION=0 ./target/release/xenora-bench --reps 7
```

## Startup distributions

`xenora-bench startup` measures what it costs to get one fixture result from
Metal, in two regimes:

- **Fresh process.** The harness launches its own executable `--processes`
  times (default 30), one after another. Each child creates the Metal device and
  queue, translates the fixture, runs it once and checks the output.
- **Repeated.** The harness process creates one device, runs the fixture once
  (reported as *first in process*), then translates and runs it `--reps` more
  times (default 100).

The fixture is the first one in `benchmarks/manifest.toml` unless `--fixture`
names another, at its smallest declared size unless `--words` sets one. The
independent reference must agree with the hand-derived expected values first,
and every sample in both regimes must produce the expected words. A wrong
output, a child that fails or does not exit within 60 seconds, or a damaged
sample line stops the run before anything is written.

| Phase | Source | Meaning |
| --- | --- | --- |
| `spawn_to_main` | wall clock, parent before spawn to child `main` | Process launch and Rust runtime start (fresh only) |
| `device` | host monotonic clock | `NativeCompute::new`: device and queue (fresh, and once in the repeated process) |
| `translate` | host monotonic clock | Read the fixture and run `xenora-shader::compile` |
| `library` | `RunProfile` | `newLibraryWithSource` |
| `pipeline` | `RunProfile` | Compute pipeline state creation |
| `submit` | `RunProfile` | `host` minus `library` and `pipeline`: buffers, encode, commit, wait |
| `gpu` | `GPUEndTime - GPUStartTime` | Command-buffer GPU execution |
| `readback` | `RunProfile` | Copy the output words back |
| process wall | host monotonic clock in the parent | Spawn to exit of one fresh child |

The report gives median, p90, p99 and maximum per phase. Percentiles
interpolate linearly, so with 30 processes p99 lies between the two largest
samples. `startup.json` also keeps every fresh sample in launch order.
Children inherit the parent's environment, so the `MTL_*` validation values in
`metadata.json` apply to both regimes. Results go to the ignored
`artifacts/startup/<hardware>/<run-id>/` unless `--output` says otherwise.

```sh
cargo build --release -p xenora-benchmarks
MTL_DEBUG_LAYER=0 MTL_SHADER_VALIDATION=0 ./target/release/xenora-bench startup
```

### Recorded run

Run `1795544761`, Apple M4, release build of `0fdb995` plus this harness, API
and shader validation off, AC power, no thermal warnings, started right after
the release build. Fixture `add`, 4 words, 30 fresh processes, 100 repeated
runs. All values in milliseconds.

| Phase | Fresh median | Fresh p90 | Fresh p99 | Fresh max | First in process | Repeated median | Repeated p90 | Repeated p99 | Repeated max |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `spawn_to_main` | 2.746 | 3.133 | 5.325 | 6.198 | – | – | – | – | – |
| `device` | 35.762 | 38.770 | 41.060 | 41.864 | 32.802 | – | – | – | – |
| `translate` | 0.061 | 0.071 | 0.084 | 0.089 | 0.067 | 0.016 | 0.023 | 0.041 | 0.043 |
| `library` | 0.402 | 0.434 | 18.595 | 25.976 | 0.399 | 0.006 | 0.010 | 0.018 | 0.019 |
| `pipeline` | 0.363 | 0.401 | 0.623 | 0.708 | 0.342 | 0.013 | 0.021 | 0.031 | 0.043 |
| `submit` | 1.665 | 1.894 | 1.951 | 1.953 | 1.647 | 0.214 | 0.384 | 0.440 | 3.941 |
| `gpu` | 0.007 | 0.008 | 0.010 | 0.011 | 0.008 | 0.007 | 0.008 | 0.061 | 1.742 |
| `readback` | 0.001 | 0.001 | 0.002 | 0.002 | 0.001 | < 0.001 | < 0.001 | < 0.001 | < 0.001 |
| process wall (spawn to exit) | 43.225 | 46.770 | 68.236 | 75.958 | – | – | – | – | – |

Observations, without causal claims:

- Device and queue creation is the largest fresh-process phase, about 36 ms of a
  43 ms median process. Within one process it is paid once.
- The `library` tail came from the first child launched after the build
  (25.976 ms; the other 29 were 0.36–0.45 ms). A second run immediately after,
  `1795544763`, had a fresh `library` maximum of 0.446 ms, with `device` median
  36.414 ms and process-wall median 44.111 ms. This fits an on-disk Metal
  compilation cache, but the run does not show that.
- After the first run in a process, `library` and `pipeline` fell to about
  0.01 ms and `submit` to about 0.2 ms. The `gpu` time for this 4-word kernel is
  at timer scale in both regimes.

### After compute pipeline reuse

Since #362, `NativeCompute` reuses a compute pipeline for identical MSL, and
`RunProfile` then reports `library` and `pipeline` as zero. Two runs on the same
Apple M4 at `9c1fd2e` (this harness on main `f630c97`) showed exactly that:
repeated `library` and `pipeline` were 0.000 ms at every percentile, while the
first run in the process still compiled (0.401 and 0.435 ms `library`). Fresh
processes are unaffected because each starts with an empty cache: `device`
medians were 37.490 and 35.969 ms, `library` medians 0.435 and 0.434 ms, and
process-wall medians 46.042 and 44.222 ms. Run 1's `library` tail (217.955 ms)
again came from the first child after a fresh build. These two runs do **not**
meet the protocol above: other build and test jobs were running (load average
about 8.6 on 10 cores), so the table recorded at `0fdb995` remains the reference
and these numbers show only the qualitative change.

Two runs on one host do not describe other Apple GPU generations, sustained
load, other fixtures or larger word counts.

## Limitations

The corpus currently covers three synthetic gfx1030 compute fixtures under the
[synthetic memory ABI](../adr/0003-demo-memory-model.md). It does not measure
graphics stages, guest containers, Wave64 or divergence. Translation-versus-native
baseline ratios are not reported because no hand-written native MSL baseline exists
yet for these fixtures.

## Exploratory AC run

Run `1795352394` used the release harness built at `cc0daa7` on Apple M5,
with AC power confirmed before and after, 30 seconds between the build and
timing, two warmups, 21 measured repetitions per size and Metal validation
off. Neither thermal nor performance warnings were recorded by
`pmset -g therm`. All three declared translated fixtures passed their
correctness checks. The recorded 1-, 5- and 15-minute load averages were
8.12, 8.68 and 6.31, so an idle environment was not established. G10 remains open for the full correctness corpus,
individual compiler stages and cold/warm timing, tool and capability metadata,
and noise-based variability reporting. See the
[generated report](../../benchmarks/results/apple-m5/run-1795352394/report.md).

## Guest service dispatch

`cargo bench --locked -p xenora-loader --bench service_dispatch` measures
original guest loops that call standard runtime services through a bound
import table under the ARM64 dynamic binary translator. Before timing, each
workload checks its result: the loop count reaches zero, the service returns
its documented value, and `qsort` leaves the array in the order Rust's own sort
produces. The empty loop is reported separately, so readers can subtract the
loop and translation overhead. Each figure is the median per-iteration time of
10 measured batches of 10,000 iterations, after one warm-up batch.

Measured on Apple M2 Pro (12 logical processors), release profile, AC power,
on the working tree after `ed9b93c`. The 1-minute load average was 7.82, so the
machine was not idle and results are indicative only.

| Workload | Before (ns) | After the block `strlen` scan (ns) |
| --- | --- | --- |
| Empty loop | 21.5 | 20.1 |
| `GetLastError` | 181.8 | 188.4 |
| `WaitForSingleObject`, signaled manual-reset event | 281.0 | 284.6 |
| `strlen`, 63 bytes | 1,990.4 | 223.1 |
| `HeapAlloc` + `HeapFree`, 64 bytes | 475.0 | 493.3 |
| `qsort`, 16 ints with a guest comparator | 10,242.0 | 11,396.7 |

The `strlen` change scans 256-unit blocks with one checked read each. It falls
back to per-unit reads only when a block crosses unmapped or unreadable
memory, so the existing exact-fault tests still pass. The other rows differ by up to 11 percent between runs on this loaded
machine; that is run-to-run variation, not a measured change. Raw logs are kept locally under
`artifacts/system-runtime/bench/`. No comparison with native Windows is made.
