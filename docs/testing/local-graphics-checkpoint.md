# Local graphics checkpoint

This summary records the accumulated graphics checkpoint and the subsequent
nested-view, compute-cache and sampler-handle increments, reviewed on 25 November 2026. It is not release certification or a
claim of Xbox hardware equivalence. Supporting raw logs, mutation records and
source manifests remain local and are not distributed with the repository.

## Reusable sampler handles

Sampler handles are immutable descriptors reused across recorded draws. Binding
copies their values, so destroying a handle afterward does not invalidate recorded
or pending work. New binds using destroyed handles reject. The current scope retains
nearest/clamp sampling and existing view-relative LOD validation.

Run the focused CPU selections from the repository root:

```sh
CARGO_BUILD_JOBS=4 cargo test --locked -p xenora-loader --test pipeline cpu_sampler -- --nocapture
CARGO_BUILD_JOBS=4 cargo test --locked -p xenora-loader --test graphics_service cpu_guest_sampler -- --nocapture
```

These selections cover recording snapshots, stale handles, atomic table validation,
LOD checks, guest memory preflight and shared object/command budgets. The focused
run passed all seven (three pipeline, four service). For the original guest pixel
tests, enable the Metal validation environment shown below and run:

```sh
CARGO_BUILD_JOBS=4 cargo test --locked -p xenora-ffi --features gpu-tests --test guest_graphics original_guest_sampler_handles -- --nocapture --test-threads=1
```

Both guest tests passed through synchronous, asynchronous and queued adapters.
They check cross-slot sampling, distinct recorded descriptors, handle destruction
before collection, stale-second-handle failure atomicity and view-relative LOD
rejection preserving previous output. A deliberate mutation forcing explicit mip0
failed with incorrect readback pixels; source hashes matched after restoration.
All nine repository gates passed: **848 workspace tests passed, zero failed,82
ignored**; the GPU script reported **330 passed, zero failed, one manual test
ignored**. GPU totals include CPU checks and overlapping coverage. Native guest
execution was ARM64 with system Metal and API/shader validation enabled.
Independent source and literal-oracle reviews found no scoped blocker. Broader
descriptor heaps, filter modes and full Stage2/3 remain open.

## Latest nested-view and compute-cache checkpoint

All nine gates passed after the nested-view extension and selective upstream cache
adaptation: **841 workspace tests passed, zero failed,80 ignored**; the GPU script
reported **328 passed, zero failed, one manual test ignored**. GPU-script totals
include CPU checks and overlap with the workspace run; they are not unique GPU-test
counts. Six focused nested-view CPU tests, two original guest tests across three
adapters, and four native cache tests passed with the applicable Metal validation.

The cache tests first exposed three failures in the uncached implementation. A
separate injected wrong-mip-base fault failed the nested-view assertion (expected
root mip3, got mip1); the original source was restored before the full gates.
Independent source and test reviews found no scoped implementation blocker. Changes
are limited to read-only nested views and bounded compute pipeline reuse; broader
Stage2/3 remain open. No benchmark speedup is claimed.

## Earlier produced-geometry evidence

All nine local gates completed with exit status zero. Independent review checked
the terminal records, summed test-result lines in workspace and GPU logs, and
verified all 16 files in the produced-geometry source manifest against the working
tree. That manifest covers the final increment, not every accumulated change.
The only reported post-gate code change moved rustdoc onto
`queued_submission_capacity`; formatting and diff checks were rerun afterward.

| Suite | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| Workspace | 835 | 0 | 78 |
| GPU script | 322 | 0 | 1 |

The GPU-script total includes CPU checks and overlapping coverage. It is not 322
independent GPU tests; do not add these totals as unique coverage. Ignored tests
are not passes. The GPU script's remaining ignored case is manual. Counts describe
this checkpoint and may change as tests are added.

Host records identify ARM64 native test binaries and LLVM fixture tools, with
system Metal and Foundation linkage. Native tests ran with Metal API and shader
validation enabled. This is local Apple Silicon evidence, not verification across
all Apple GPU generations.

For produced geometry, eight native tests and two original PE guest tests passed.
Eight injected fault types were detected after strengthening the poll-before-collect
regression. The initial surviving poll-validation mutation remains in the local
record and is not counted as an initial detection. Faults exercise waits, fetch
ranges and validation, index interpretation, ownership and failure publication.
Mutation sensitivity does not prove absence of defects.

## Run focused graphics checks

Run these commands from the repository root on a Metal-capable Apple Silicon Mac.
They build and execute tests; no application window opens. The rendering tests
create offscreen targets, read back pixels and compare them with independently
specified expected colors or geometry. Negative cases assert rejection and that
previous color/depth results remain intact. An `ok` result means those assertions
passed, not that a person inspected an image on screen.

Enable native validation before running either targeted GPU command:

```sh
export CARGO_BUILD_JOBS=4
export MTL_DEBUG_LAYER=1
export MTL_SHADER_VALIDATION=1
export MTL_SHADER_VALIDATION_REPORT_TO_STDERR=1
export MTL_SHADER_VALIDATION_ABORT_ON_FAULT=1
```

Run the native produced-geometry cases, then the original guest programs through
the production adapter:

```sh
cargo test --locked -p xenora-metal --features gpu-tests --test native_compute produced_geometry:: -- --nocapture --test-threads=1
cargo test --locked -p xenora-ffi --features gpu-tests --test guest_graphics original_guest_produced_geometry_ -- --nocapture --test-threads=1
```

The reviewed produced-geometry checkpoint reported eight passing native tests and
two passing guest tests for these selections. Expect exit status zero, no failures
and the selected test names marked `ok`; unrelated tests appear as filtered out.
Zero selected tests is not evidence of success: check the names with `-- --list`
if a later revision changes the filters.

CPU-only produced-geometry and nested-view checks can be selected separately:

```sh
cargo test --locked -p xenora-loader --test produced_geometry -- --nocapture
cargo test --locked -p xenora-loader --test pipeline cpu_nested_view -- --nocapture
cargo test --locked -p xenora-loader --test graphics_service cpu_guest_child_view -- --nocapture
```

The nested-view pipeline filter selects checks for relative mip-range
normalization, parent destruction, invalid ranges/formats, superseded references
and sampled/output backing aliases. The separate `cpu_guest_child_view` filter
selects guest ABI preflight, independent lifetime, object-budget and root-format
checks; these names do not contain `nested`. All six focused CPU checks passed
(three per selection). Full-gate results for this increment are recorded above.

Run the original nested-view guest programs with the validation environment above:

```sh
cargo test --locked -p xenora-ffi --features gpu-tests --test guest_graphics original_guest_nested_views -- --nocapture --test-threads=1
```

The local guest log records two passing tests with Metal API and shader validation.
These exercise nonzero mip-base composition, linear/sRGB interpretation, parent
bounds and independent root lifetime across the three adapters. This is targeted
evidence, not full-suite or full-stage acceptance. The nested-loop shader tests
are a different feature and are not substitutes for view tests.

The adapted compute pipeline cache has its own targeted selection (using the GPU
validation environment above):

```sh
cargo test --locked -p xenora-metal --features gpu-tests --test native_compute pipeline_cache:: -- --nocapture --test-threads=1
```

This selects four cache tests. The local targeted run passed all four with Metal
API and shader validation enabled, after a baseline with three failures and one
pass. These results are separate from the earlier checkpoint totals above.
See [selective upstream imports](upstream-imports.md)
for provenance and adaptation scope.

For the full native/guest validation suite, run:

```sh
scripts/test-gpu.sh
```

The script sets validation variables itself. Its aggregate includes CPU checks;
see the checkpoint totals and limitations above. For the complete nine gate
categories, use the commands below.

## Reproducing the gates

Use native ARM64 macOS with Rust, Xcode/Metal tools, LLVM fixture tools and the
script dependencies described in the repository testing guides. These commands
reproduce the gate categories used for the latest checkpoint. Run
GPU work serially, with at most four Cargo build jobs.

```sh
export CARGO_BUILD_JOBS=4
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
scripts/test-gpu.sh
scripts/assemble-fixture.sh --check
python3 scripts/audit/status.py --check
scripts/audit/claims.sh
scripts/audit/dependencies.sh
git diff --check
```

`test-gpu.sh` enables Metal API and shader validation and selects native and guest
checks. Clean formatting and diff checks produce no output. Record command exits
and full test results when reproducing; do not infer success from this summary.

## Acceptance and limits

Independent criterion-level review accepted the original Stage 1 rendering
milestone within its documented contracts: larger indexed draws, per-draw state,
depth, blending and multiple render targets, including integrated synthetic scenes
and actual original guest programs. This reconciles older pending-acceptance
statements for bounded ordered-pass, region-state and lifetime interfaces.

Independent review also accepted the subsequent bounded increments through
GPU-produced geometry. The latter consumes exact pending buffer versions, keeps
geometry payloads on the GPU, checks logical fetch ranges and finite attributes,
retains source and scratch ownership, and rejects invalid passes and dependent
publication. Legacy and host-only paths retain their documented contracts.
See [interfaces](../interfaces.md) and the [guest graphics ABI](../graphics-call-v1.md)
for capability, capacity, layout and validation restrictions.

Full Stage 2 and Stage 3 remain open. General resource/view/format coverage,
broader synchronization, general shader control flow and divergent execution are
not established. Additional geometry formats, instancing and indirect draws are
outside this increment. Original synthetic fixtures do not demonstrate commercial
Xbox game execution, console hardware equivalence or a process-wide memory ceiling.
Payload budgets cover explicitly documented owned allocations rather than total
driver or process memory.
