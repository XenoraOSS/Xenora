// SPDX-License-Identifier: GPL-3.0-or-later
//! Literal column positions from original nested vertex loops.
use xenora_metal::*;
use xenora_shader::{
    Architecture::Gfx1030, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics_pipeline,
};

#[test]
fn translated_nested_vertex_counts_move_exact_columns() {
    let gpu = NativeCompute::new().unwrap();
    let compiled = compile_graphics_pipeline(
        Gfx1030,
        include_bytes!("../../../../fixtures/rdna2/x9-nested-loop-geometry-vs.bin"),
        include_bytes!("../../../../fixtures/rdna2/x9-constant-ps.bin"),
    )
    .unwrap();
    assert_eq!(compiled.vertex_uniform_mask, 3);
    assert_eq!(compiled.pixel_uniform_mask, 0);
    // Two-pixel-wide rectangle. A 1/16 clip-space shift is one pixel at width32.
    let positions = [
        [-1., -1., 0.5, 1.],
        [-0.875, -1., 0.5, 1.],
        [-1., 1., 0.5, 1.],
        [-1., 1., 0.5, 1.],
        [-0.875, -1., 0.5, 1.],
        [-0.875, 1., 0.5, 1.],
    ];
    let colors = [[0.; 4]; 6];
    for (outer, inner, first) in [
        (0, 4, 0),
        (4, 0, 0),
        (1, 1, 1),
        (2, 3, 6),
        (3, 2, 6),
        (4, 4, 16),
        (u32::MAX, u32::MAX, 16),
        (1, 2, 2),
        (0, 0, 0),
    ] {
        let draws = [TriangleDraw {
            vertices: &positions,
            colors: &colors,
            viewport: [32, 2],
        }];
        let pass = TriangleTargetsPass {
            draws: &draws,
            states: &[TriangleState::default()],
            extent: [32, 2],
            clears: &[[17, 31, 47, 255]],
            clear_depth: None,
        };
        let actual = gpu
            .submit_view_binding_triangle_targets_with_uniforms_rgba8(
                &[TriangleShader {
                    source: &compiled.shader.msl,
                    entries: [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
                }],
                &pass,
                &[vec![]],
                None,
                &[None],
                &[DrawUniforms {
                    required_mask: 3,
                    values: Some([outer, inner, 0, 0, 0, 0, 0, 0]),
                }],
            )
            .unwrap()
            .wait()
            .unwrap();
        let mut row = [[17, 31, 47, 255]; 32];
        row[first] = [255, 0, 0, 255];
        row[first + 1] = [255, 0, 0, 255];
        assert_eq!(
            actual,
            vec![row.as_flattened().repeat(2)],
            "outer={outer}, inner={inner}"
        );
    }
}
