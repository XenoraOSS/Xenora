// SPDX-License-Identifier: GPL-3.0-or-later
//! Original nested countdown fixtures; execution pixels are independent evidence.
use xenora_shader::{Architecture::Gfx1030, compile_graphics_pipeline};

const CONSTANT_VS: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin");
const NESTED_PS: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-nested-loop-vgpr-ps.bin");

#[test]
fn original_nested_countdown_reinitializes_inner_and_carries_vgpr() {
    let compiled = compile_graphics_pipeline(Gfx1030, CONSTANT_VS, NESTED_PS)
        .expect("original nested MIN-bounded countdown must compile");
    assert_eq!(compiled.vertex_uniform_mask, 0);
    assert_eq!(compiled.pixel_uniform_mask, 3);
    assert_eq!(compiled.required_uniform_mask, 3);
    assert_eq!(compiled.color_targets, 1);
    assert_eq!(compiled.sampled_pairs, 0);
    assert!(!compiled.uses_texture);
    // A shared CFG dispatcher may implement both loops with one real while.
    let tokens: Vec<_> = compiled
        .shader
        .msl
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .collect();
    assert!(
        tokens.contains(&"while"),
        "nested loops must retain real iteration"
    );
}

#[test]
fn original_nested_carried_state_siblings_and_three_levels_compile() {
    let cases: &[(&[u8], u8)] = &[
        (
            include_bytes!("../../../fixtures/rdna2/x9-nested-loop-changing-bound-ps.bin"),
            1,
        ),
        (
            include_bytes!("../../../fixtures/rdna2/x9-nested-loop-scalar-siblings-ps.bin"),
            7,
        ),
        (
            include_bytes!("../../../fixtures/rdna2/x9-nested-loop-swap-ps.bin"),
            15,
        ),
        (
            include_bytes!("../../../fixtures/rdna2/x9-nested-loop-depth-three-ps.bin"),
            7,
        ),
    ];
    for (pixel, mask) in cases {
        let compiled = compile_graphics_pipeline(Gfx1030, CONSTANT_VS, pixel).unwrap();
        assert_eq!(compiled.required_uniform_mask, *mask);
        assert_eq!(compiled.sampled_pairs, 0);
    }
}

#[test]
fn original_zero_outer_prunes_unbounded_child_semantics() {
    let pixel =
        include_bytes!("../../../fixtures/rdna2/x9-nested-loop-zero-outer-unbounded-child-ps.bin");
    let compiled = compile_graphics_pipeline(Gfx1030, CONSTANT_VS, pixel).unwrap();
    assert_eq!(compiled.required_uniform_mask, 0);
}

#[test]
fn original_nested_work_budget_accepts_65536_and_rejects_65537() {
    // O*(5*I+7)+8+P counts decoded instructions, including final headers.
    // O=60, I=217, P=8:65536. One extra original preheader MOV gives65537.
    let accepted = include_bytes!("../../../fixtures/rdna2/x9-nested-loop-budget-accept-ps.bin");
    compile_graphics_pipeline(Gfx1030, CONSTANT_VS, accepted).unwrap();
    assert_work_rejection(include_bytes!(
        "../../../fixtures/rdna2/x9-nested-loop-budget-one-over-ps.bin"
    ));
}

#[test]
fn original_nested_work_budget_counts_final_child_headers() {
    // O=62, I=210, P=0:65542. Omitting the final two child-header instructions
    // on each invocation undercounts by124, incorrectly admitting65418.
    assert_work_rejection(include_bytes!(
        "../../../fixtures/rdna2/x9-nested-loop-budget-reject-ps.bin"
    ));
}

fn assert_work_rejection(pixel: &[u8]) {
    let error = compile_graphics_pipeline(Gfx1030, CONSTANT_VS, pixel).unwrap_err();
    let xenora_shader::CompileError::GraphicsStage {
        stage: "pixel",
        error,
    } = error
    else {
        panic!("{error:?}")
    };
    assert!(
        matches!(
            *error,
            xenora_shader::CompileError::Verify {
                reason: "graphics loop work limit exceeded",
                ..
            }
        ),
        "{error:?}"
    );
}

fn assert_pixel_verifier_rejection(pixel: &[u8]) {
    let error = compile_graphics_pipeline(Gfx1030, CONSTANT_VS, pixel).unwrap_err();
    match error {
        xenora_shader::CompileError::GraphicsStage {
            stage: "pixel",
            error,
        } => {
            assert!(
                matches!(*error, xenora_shader::CompileError::Verify { .. }),
                "{error:?}"
            );
        }
        other => panic!("expected pixel verification failure: {other:?}"),
    }
}
macro_rules! reject_nested {
    ($name:ident, $fixture:literal) => {
        #[test]
        fn $name() {
            assert_pixel_verifier_rejection(include_bytes!(concat!(
                "../../../fixtures/rdna2/",
                $fixture,
                ".bin"
            )));
        }
    };
}
reject_nested!(
    nested_rejects_runtime_outer_unbounded_child,
    "x9-nested-loop-reject-runtime-unbounded-child-ps"
);
#[test]
fn nested_accepts_retained_child_counter_bounded_on_every_entry() {
    let pixel =
        include_bytes!("../../../fixtures/rdna2/x9-nested-loop-retained-child-counter-ps.bin");
    let compiled = compile_graphics_pipeline(Gfx1030, CONSTANT_VS, pixel).unwrap();
    assert_eq!(compiled.required_uniform_mask, 3);
}
reject_nested!(
    nested_rejects_bypassed_child_reset,
    "x9-nested-loop-reject-bypass-reset-ps"
);
reject_nested!(
    nested_rejects_child_writing_outer_counter,
    "x9-nested-loop-reject-child-writes-outer-ps"
);
reject_nested!(
    nested_rejects_outer_decrement_inside_child,
    "x9-nested-loop-reject-outer-decrement-in-child-ps"
);
reject_nested!(
    nested_rejects_shared_counter,
    "x9-nested-loop-reject-shared-counter-ps"
);
reject_nested!(
    nested_rejects_bypassed_child_decrement,
    "x9-nested-loop-reject-bypassed-decrement-ps"
);

#[test]
fn original_nested_child_result_alias_and_final_scc_compile() {
    let pixel = include_bytes!("../../../fixtures/rdna2/x9-nested-loop-alias-scc-ps.bin");
    let compiled = compile_graphics_pipeline(Gfx1030, CONSTANT_VS, pixel).unwrap();
    assert_eq!(compiled.required_uniform_mask, 15);
    // Independent execution tuples (n,m,expected sum,expected child result):
    // (0,4,0,1), (1,0,8,1), (1,2,8,3), (2,2,18,5),
    // (3,2,30,7), (4,4,56,17). Native pixel coverage follows assembly.
}
