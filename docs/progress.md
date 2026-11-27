# Engineering progress

Xenora is an experimental research project. Verified work is restricted to
independently authored synthetic programs and shaders; commercial game
execution and authentic Xbox startup are unsupported. Progress is measured by
reproducible tests, not by estimated game compatibility percentages.

## Reading the progress graphic

- **Decoded forms** counts rows marked decoded in the [status registry](status.toml), out of all rows in that reviewed gfx1030 instruction inventory.
- **Accepted Metal forms** counts rows with accepted native fixture readback, over the same inventory. See the [form-by-form results](shaders/instruction-coverage.md) and [implementation checkpoint](testing/phase-02-baseline.md#implementation-checkpoint) for class A evidence from Apple M5.

## Feature status

<!-- status:features:start -->
| Feature | Target | Implementation | Correctness | Hardware | Evidence | Limits |
| --- | --- | --- | --- | --- | --- | --- |
| native-compute | arm64 macOS | implemented | initial-native-suite-passed | verified on Apple M5 | A | Trusted MSL with bounded word buffers and an original native offscreen rendering test on Apple M5. |
| synthetic-gfx1030 | gfx1030 Wave32 | three-fixture-demo-implemented | three-way-output-passed | verified on Apple M5 | A, B | Synthetic memory contract, statically resolved forward control flow, exact positive normal FP32 only. |
| guest-graphics-path | arm64 macOS | implemented | guest-frame-readback-passed | verified on Apple M4 | A | One triangle per draw, one instance, a float4 position plus an optional float4 texture coordinate per vertex, R8G8B8A8 render targets and textures of at most 64 by 64 pixels, a point sampler with clamp addressing, and synchronous draw execution and readback. Constant buffers, upload and readback heaps, render target views and depth views are not yet exposed to the guest. |
| guest-cpp-path | arm64 macOS | implemented | guest-cpp-program-passed | not-applicable | A | The hosted program uses the Itanium C++ ABI guard services for thread-safe function-local statics. C++ exceptions and stack unwinding are not implemented and are not exercised; the program is built with -fno-exceptions and -fno-rtti. One worker thread, one guest at a time under the deterministic cooperative scheduler, and a bounded command-line and file environment. |
| xbox-execution | Xbox Series X\|S | unsupported | untested | unverified | U | Game execution, proprietary guest containers and full guest execution-mask divergence are unsupported. The console launch ABI is unknown. |
<!-- status:features:end -->

## Reproduce

Run on native Apple Silicon with Rust 1.100.0, Xcode 27.2, Python 3.11 or later and LLVM 23.1.6. The verified host was Apple M5 with macOS 27.3. Metal's safe and precise compile options must be available. Use up to four Cargo jobs total across at most two builds, one GPU workload and one fuzz or mutation campaign at a time. The final checkpoint used two Cargo jobs.

```sh
export CARGO_BUILD_JOBS=2
scripts/bootstrap.sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
scripts/assemble-fixture.sh --check
LLVM_BIN="$(brew --prefix llvm)/bin" cargo test --locked -p xenora-decoder --test gfx1030 -- --include-ignored --nocapture
scripts/test-gpu.sh
scripts/demo.sh
scripts/audit/native.sh
scripts/audit/claims.sh
scripts/audit/dependencies.sh
```

The independent CPU validation tests are part of the workspace. The native suite and optional LLVM oracle run separately so CPU-only CI can report them accurately. Final local logs are under ignored `artifacts/`. Keep the historical lab and candidate logs separate.

## Remaining work

See the [current limitations](../README.md#current-limitations) for the outstanding items. The active research steps are: finish the remaining RDNA2 wave execution and mesh stages, translated texture sampling, broader resource binding and synchronization. Close independent claims, status, history and review findings against the current candidates, and verify the [runtime redistribution blockers](licenses.md#runtime-redistribution-review) before closing the redistribution gate (G11).
