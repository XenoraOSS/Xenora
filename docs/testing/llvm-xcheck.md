<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# LLVM ARM64/gfx1030 integer cross-check

Issue #982 records a supplemental compiler cross-check. It is not evidence of
AMD hardware behavior, Xbox hardware behavior or game compatibility.

## Program

`fixtures/llvm-xcheck/xcheck.ll` contains one target-independent integer
function, `xcheck_core`, and two entry points:

- `xcheck_arm64`, called by the host runner in `run-arm64.c`;
- `xcheck_gfx1030`, compiled by LLVM's AMDGPU backend.

For each 32-bit input word `x`, with all arithmetic modulo 2^32:

1. multiply the low 24 bits of `x` by `0x123456`;
2. XOR the product with the original `x`;
3. multiply the low 24 bits of that result by `0x15944c`;
4. XOR the second product with the first result.

The checked inputs and independently derived answers are in
`fixtures/llvm-xcheck/expected.txt`. They include zero, small values, high-bit
values, all-ones values and cases exercising the 24-bit truncation.

## Toolchain record

The local evidence run used Homebrew LLVM 23.1.6 on native Apple Silicon:

- host target `arm64-apple-darwin27.2.0` (`clang` reports the macOS override
  `arm64-apple-macosx27.2.0` when compiling the IR);
- AMDGPU target `amdgcn-amd-amdhsa`, CPU `gfx1030`, feature
  `+wavefrontsize32`;
- `llc` registered both `aarch64` and `amdgpu` targets.

`scripts/llvm-xcheck.sh` records the exact locally installed versions in
`artifacts/llvm-xcheck/toolchain.txt`, rebuilds and runs the ARM64 entry point,
compares its output with the independent answers, compiles the gfx1030 entry
point, and requires the adapter to preserve the emitted opcode, constant and
dataflow sequence.

## Synthetic Wave32 ABI mapping

LLVM emits the function input in `v1` and its result in `v0` for this simple
non-kernel function. Xenora's synthetic fixture ABI instead reserves `v0` as
the lane index, loads each lane's input word into `v1` through descriptor
`s[0:3]`, and stores each lane's output word through `s[4:7]`.

`fixtures/rdna2/llvm-xcheck.s` documents and performs the adaptation:

- the input is copied from `v1` to `v3`;
- LLVM's result/intermediate register `v0` is relocated to `v4`;
- the four opcode/constant/dataflow steps are otherwise unchanged;
- the final result is copied back to `v1` for the synthetic store;
- `v0` is never overwritten, so the synthetic lane index remains intact.

This is a hand-written synthetic adapter around compiler-selected arithmetic.
It is not AMDHSA initialization and does not implement the AMDHSA kernel
argument, dispatch, scratch or completion contract described in
`docs/interfaces.md`. A standard AMDHSA kernel emitted by LLVM still requires
that separate contract. The adapter is regenerated only after
`scripts/llvm-xcheck.sh` proves that LLVM 23.1.6 still selects the same
arithmetic sequence.

## Metal comparison

`crates/metal/tests/native/llvm_xcheck.rs` translates
`fixtures/rdna2/llvm-xcheck.bin` through Xenora's gfx1030 decoder and shader
compiler, dispatches the resulting Metal kernel and compares actual readback
with the same independent answers. This test runs as part of the GPU-only
`native_compute` target used by `scripts/test-gpu.sh`; compilation or queue
submission without completion does not count. The ARM64 half and the Metal
half are separate builds of the same target-independent LLVM IR, not a shared
implementation of the expected values.
