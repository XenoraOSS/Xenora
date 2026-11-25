// SPDX-License-Identifier: GPL-3.0-or-later
//! Original assembled programs at compiler admission boundaries.
use xenora_shader::{Architecture::Gfx1030, CompileError, compile_graphics_pipeline};
const VS: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin");

#[test]
fn counted_loop_trip_bound_accepts_256_and_rejects_257() {
    compile_graphics_pipeline(
        Gfx1030,
        VS,
        include_bytes!("../../../fixtures/rdna2/x9-loop-limit-trip-256-ps.bin"),
    )
    .unwrap();
    let err = compile_graphics_pipeline(
        Gfx1030,
        VS,
        include_bytes!("../../../fixtures/rdna2/x9-loop-limit-trip-257-ps.bin"),
    )
    .unwrap_err();
    assert_reason(err, "loop trip count lacks a proven bound at most 256");
}

#[test]
fn counted_loops_preserve_the_existing_bytecode_limit() {
    // The existing 1024-byte decoder admission limit is stricter than the
    // repeated-work bound for this restricted nonnested countdown grammar.
    compile_graphics_pipeline(
        Gfx1030,
        VS,
        include_bytes!("../../../fixtures/rdna2/x9-loop-limit-stream-1024-ps.bin"),
    )
    .unwrap();
    let err = compile_graphics_pipeline(
        Gfx1030,
        VS,
        include_bytes!("../../../fixtures/rdna2/x9-loop-limit-stream-1028-ps.bin"),
    )
    .unwrap_err();
    let CompileError::GraphicsStage {
        stage: "pixel",
        error,
    } = err
    else {
        panic!("{err:?}")
    };
    assert!(
        matches!(*error, CompileError::InvalidLength { bytes: 1028 }),
        "{error:?}"
    );
}

fn assert_reason(err: CompileError, expected: &str) {
    let CompileError::GraphicsStage {
        stage: "pixel",
        error,
    } = err
    else {
        panic!("{err:?}")
    };
    assert!(
        matches!(*error, CompileError::Verify { reason, .. } if reason == expected),
        "{error:?}"
    );
}

#[test]
fn sequential_counted_loops_accept_eight_and_reject_nine() {
    compile_graphics_pipeline(
        Gfx1030,
        VS,
        include_bytes!("../../../fixtures/rdna2/x9-loop-limit-sequential-8-ps.bin"),
    )
    .unwrap();
    let err = compile_graphics_pipeline(
        Gfx1030,
        VS,
        include_bytes!("../../../fixtures/rdna2/x9-loop-limit-sequential-9-ps.bin"),
    )
    .unwrap_err();
    assert_reason(err, "graphics loop analysis limit exceeded");
}
