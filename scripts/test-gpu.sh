#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
cd "$(dirname "$0")/.."

export MTL_DEBUG_LAYER=1
export MTL_SHADER_VALIDATION=1
export MTL_SHADER_VALIDATION_REPORT_TO_STDERR=1
export MTL_SHADER_VALIDATION_ABORT_ON_FAULT=1
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-4}
# The full semantic-review batch is much larger than the individual GPU tests.
# Cold Apple Silicon runs can exceed two minutes; keep the default bounded but
# allow slower hosts to raise it without editing the script.
export XENORA_SEMANTIC_TIMEOUT=${XENORA_SEMANTIC_TIMEOUT:-300s}

# macOS does not ship GNU timeout. Python 3 is already a project requirement,
# so use a small POSIX-compatible fallback rather than making coreutils a
# prerequisite for the native suite.
if ! command -v timeout >/dev/null 2>&1; then
    timeout() {
        duration=$1
        shift
        python3 - "$duration" "$@" <<'PY'
import os
import re
import signal
import subprocess
import sys

text = sys.argv[1]
match = re.fullmatch(r"(\d+(?:\.\d+)?)([smh]?)", text)
if match is None:
    print(f"timeout: invalid duration {text!r}", file=sys.stderr)
    raise SystemExit(2)
seconds = float(match.group(1)) * {"": 1, "s": 1, "m": 60, "h": 3600}[match.group(2)]
try:
    process = subprocess.Popen(sys.argv[2:], start_new_session=True)
except FileNotFoundError as error:
    print(f"timeout: {error}", file=sys.stderr)
    raise SystemExit(127)
try:
    code = process.wait(timeout=seconds)
except subprocess.TimeoutExpired:
    os.killpg(process.pid, signal.SIGKILL)
    process.wait()
    print(f"timeout: command exceeded {seconds:g} seconds", file=sys.stderr)
    raise SystemExit(124)
raise SystemExit(code)
PY
    }
fi

cargo test --locked -p xenora-metal --features gpu-tests --test native_compute --no-run
timeout 30s cargo test --locked -p xenora-metal --features gpu-tests --test native_compute -- \
    --nocapture --test-threads=1

cargo test --locked -p xenora-loader --features gpu-tests --test dbt_render --no-run
timeout 30s cargo test --locked -p xenora-loader --features gpu-tests --test dbt_render -- \
    --nocapture --test-threads=1

# A synthetic guest loop is checked on the CPU and its words are dispatched to Metal (#147).
cargo test --locked -p xenora-loader --features gpu-tests --test mixed_workload --no-run
timeout 30s cargo test --locked -p xenora-loader --features gpu-tests --test mixed_workload -- \
    --nocapture --test-threads=1

cargo test --locked -p xenora-loader --features gpu-tests --test runtime_x15 --no-run
timeout 30s cargo test --locked -p xenora-loader --features gpu-tests --test runtime_x15 -- \
    test_fixture_c_service_to_metal_framebuffer --exact --nocapture --test-threads=1

cargo test --locked -p xenora-loader --features gpu-tests --test graphics_d3d12 --no-run
timeout 30s cargo test --locked -p xenora-loader --features gpu-tests --test graphics_d3d12 -- \
    --nocapture --test-threads=1

# Guest draws sample textures bound through descriptor tables (#303).
cargo test --locked -p xenora-loader --features gpu-tests --test graphics_textures --no-run
timeout 30s cargo test --locked -p xenora-loader --features gpu-tests --test graphics_textures -- \
    --nocapture --test-threads=1

# The native app interface runs the trusted fixture's guest draw on Metal.
cargo test --locked -p xenora-ffi --features gpu-tests --test native_frame --no-run
timeout 30s cargo test --locked -p xenora-ffi --features gpu-tests --test native_frame -- \
    --nocapture --test-threads=1

# The persistent frame loop runs the guest on its own thread and renders on Metal.
cargo test --locked -p xenora-ffi --features gpu-tests --test frame_loop --no-run
timeout 30s cargo test --locked -p xenora-ffi --features gpu-tests --test frame_loop -- \
    --nocapture --test-threads=1

# Interactive guest execution with dynamic input handling and rendering (Gate 4, #276).
cargo test --locked -p xenora-ffi --features gpu-tests --test interactive --no-run
timeout 30s cargo test --locked -p xenora-ffi --features gpu-tests --test interactive -- \
    --nocapture --test-threads=1

# Persistent GPU resources, dynamic texture updates, and resource retirement (Gate 5, #279).
cargo test --locked -p xenora-ffi --features gpu-tests --test resources --no-run
timeout 30s cargo test --locked -p xenora-ffi --features gpu-tests --test resources -- \
    --nocapture --test-threads=1

# Audio PCM streaming and sandboxed virtual filesystem integration (Gate 6, #284).
cargo test --locked -p xenora-ffi --features gpu-tests --test audio_vfs --no-run
timeout 30s cargo test --locked -p xenora-ffi --features gpu-tests --test audio_vfs -- \
    --nocapture --test-threads=1

# Native Metal known answers of the independent semantic review, including SOP2 scalar ALU
# and VOPC compares.
cargo test --locked -p xenora-semantic-review --no-run
timeout "$XENORA_SEMANTIC_TIMEOUT" cargo test --locked -p xenora-semantic-review -- --ignored --nocapture --test-threads=1
