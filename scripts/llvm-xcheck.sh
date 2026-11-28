#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu

cd "$(dirname "$0")/.."
llvm_bin=${LLVM_BIN:-$(brew --prefix llvm)/bin}
out=artifacts/llvm-xcheck
mkdir -p "$out"

printf '%s\n' '== toolchain ==' | tee "$out/toolchain.txt"
"$llvm_bin/clang" --version | tee -a "$out/toolchain.txt"
"$llvm_bin/llc" --version | tee -a "$out/toolchain.txt"
printf '%s\n' 'arm64 compile target: arm64-apple-darwin (clang maps it to the host macOS triple)' | tee -a "$out/toolchain.txt"
printf '%s\n' 'gfx compile target: amdgcn-amd-amdhsa, cpu gfx1030, wavefrontsize32' | tee -a "$out/toolchain.txt"

# Build and run the ARM64 entry point on the host.
"$llvm_bin/clang" --target=arm64-apple-darwin -O2 -c fixtures/llvm-xcheck/xcheck.ll -o "$out/xcheck-arm64.o"
"$llvm_bin/clang" --target=arm64-apple-darwin -O2 fixtures/llvm-xcheck/run-arm64.c "$out/xcheck-arm64.o" -o "$out/xcheck-arm64"
"$out/xcheck-arm64" > "$out/arm64-output.txt"
cmp fixtures/llvm-xcheck/expected.txt "$out/arm64-output.txt"
printf '%s\n' 'ARM64 host output matches the independent answers.'

# Build the gfx1030 entry point and require its arithmetic body to be exactly
# the sequence embedded in the synthetic adapter. This detects register or
# instruction selection drift instead of silently accepting it.
"$llvm_bin/llc" -mtriple=amdgcn-amd-amdhsa -mcpu=gfx1030 -mattr=+wavefrontsize32 -O2 fixtures/llvm-xcheck/xcheck.ll -o "$out/xcheck-gfx1030.s"
python3.12 - "$out/xcheck-gfx1030.s" > "$out/core-output.txt" <<'PY'
import sys
text = open(sys.argv[1], encoding="utf-8").read()
start = text.index("xcheck_gfx1030:")
end = text.index("s_setpc_b64", start)
compiler_expected = [
    "v_mul_u32_u24_e32 v0, 0x123456, v1",
    "v_xor_b32_e32 v0, v0, v1",
    "v_mul_u32_u24_e32 v1, 0x15944c, v0",
    "v_xor_b32_e32 v0, v1, v0",
]
adapter_expected = [
    "v_mul_u32_u24_e32 v4, 0x123456, v3",
    "v_xor_b32_e32 v4, v4, v3",
    "v_mul_u32_u24_e32 v3, 0x15944c, v4",
    "v_xor_b32_e32 v4, v3, v4",
]
actual = []
for line in text[start:end].splitlines():
    line = line.strip()
    if line.startswith("v_"):
        actual.append(line)
    elif line.startswith("s_") and not line.startswith("s_waitcnt"):
        raise SystemExit("unexpected scalar instruction in gfx1030 core: " + line)
if actual != compiler_expected:
    raise SystemExit("gfx1030 compiler sequence mismatch:\nactual:\n" + "\n".join(actual) + "\nexpected:\n" + "\n".join(compiler_expected))
mapped = [line.replace("v0", "v4").replace("v1", "v3") for line in actual]
if mapped != adapter_expected:
    raise SystemExit("adapter register relocation mismatch:\n" + "\n".join(mapped))
print("gfx1030 compiler core matches the adapter sequence.")
PY

# Assemble the complete synthetic fixture and compare the checked-in bytes.
"$llvm_bin/llvm-mc" -triple=amdgcn-amd-amdhsa -mcpu=gfx1030 -mattr=+wavefrontsize32 -filetype=obj fixtures/rdna2/llvm-xcheck.s -o "$out/llvm-xcheck.o"
"$llvm_bin/llvm-objcopy" -O binary --only-section=.text "$out/llvm-xcheck.o" "$out/llvm-xcheck.bin"
cmp fixtures/rdna2/llvm-xcheck.bin "$out/llvm-xcheck.bin"
printf '%s\n' 'Synthetic gfx1030 fixture bytes are reproducible.'
printf '%s\n' 'Run scripts/test-gpu.sh for the Xenora translation and Metal readback half of the comparison.'
