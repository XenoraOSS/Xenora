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

. scripts/lib/timeout.sh

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

# Native fences, compiled guest frames, submission lifetimes and threaded draws.
for target in graphics_fences guest_graphics_fences guest_graphics_path \
    guest_textured_path hosted_graphics_path shared_resource_lifecycle \
    threaded_guest_application; do
    cargo test --locked -p xenora-loader --features gpu-tests --test "$target" --no-run
    timeout 30s cargo test --locked -p xenora-loader --features gpu-tests --test "$target" -- \
        --nocapture --test-threads=1
done

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

# Verify both the persistent CLI session and the hosted runner's final Metal frame.
cargo test --locked -p xenora --test persistent_session_cli --no-run
timeout 30s cargo test --locked -p xenora --test persistent_session_cli -- \
    default_metal_backend_draws_every_frame_natively --ignored --exact --nocapture --test-threads=1

cargo test --locked -p xenora --test run_program --no-run
timeout 30s cargo test --locked -p xenora --test run_program -- \
    startup_report_ends_in_verified_metal_frame --ignored --exact --nocapture --test-threads=1

# Native Metal known answers of the independent semantic review, including SOP2 scalar ALU
# and VOPC compares.
cargo test --locked -p xenora-semantic-review --no-run
timeout "$XENORA_SEMANTIC_TIMEOUT" cargo test --locked -p xenora-semantic-review -- --ignored --nocapture --test-threads=1
