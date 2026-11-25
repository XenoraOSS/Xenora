// SPDX-License-Identifier: GPL-3.0-or-later
//! A bound may survive only guarded, nonwrapping decrements on every path.
use xenora_shader::{Architecture::Gfx1030, CompileError, compile_graphics_pipeline};
const VS: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin");

#[test]
fn retained_counter_does_not_inherit_a_bound_through_an_unguarded_step() {
    reject(include_bytes!(
        "../../../fixtures/rdna2/x9-nested-loop-reject-unguarded-counter-step-ps.bin"
    ));
}
#[test]
fn retained_counter_does_not_inherit_a_bound_through_an_increasing_backedge() {
    reject(include_bytes!(
        "../../../fixtures/rdna2/x9-nested-loop-reject-increasing-retained-counter-ps.bin"
    ));
}
fn reject(pixel: &[u8]) {
    let err = compile_graphics_pipeline(Gfx1030, VS, pixel).unwrap_err();
    let CompileError::GraphicsStage {
        stage: "pixel",
        error,
    } = err
    else {
        panic!("{err:?}")
    };
    assert!(
        matches!(
            *error,
            CompileError::Verify {
                reason: "loop trip count lacks a proven bound at most 256",
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn crossing_loop_intervals_are_rejected() {
    let err = compile_graphics_pipeline(
        Gfx1030,
        VS,
        include_bytes!("../../../fixtures/rdna2/x9-nested-loop-reject-crossing-ps.bin"),
    )
    .unwrap_err();
    let CompileError::GraphicsStage {
        stage: "pixel",
        error,
    } = err
    else {
        panic!("{err:?}")
    };
    // Edge entry/exit checks may reject before the explicit forest-overlap check.
    assert!(matches!(*error, CompileError::Verify { .. }), "{error:?}");
}
