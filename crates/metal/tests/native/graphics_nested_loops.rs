// SPDX-License-Identifier: GPL-3.0-or-later
//! Original nested loops with literal pixel oracles, independent of scalar evaluators.
use xenora_metal::*;
use xenora_shader::{
    Architecture::Gfx1030, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics_pipeline,
};
const VS: &[u8] = include_bytes!("../../../../fixtures/rdna2/x9-constant-vs.bin");
const POS: [[f32; 4]; 3] = [[-1., -1., 0.5, 1.], [3., -1., 0.5, 1.], [-1., 3., 0.5, 1.]];
fn pixel_grid(ps: &[u8], words: &[[u32; 8]], expected: &[[u8; 4]], bindings: Vec<SampledBinding>) {
    let gpu = NativeCompute::new().unwrap();
    pixel_grid_on(&gpu, ps, words, expected, bindings);
}
fn pixel_grid_on(
    gpu: &NativeCompute,
    ps: &[u8],
    words: &[[u32; 8]],
    expected: &[[u8; 4]],
    bindings: Vec<SampledBinding>,
) {
    let vs = if bindings.is_empty() {
        VS
    } else {
        include_bytes!("../../../../fixtures/rdna2/x9-varying-vs.bin")
    };
    let compiled = compile_graphics_pipeline(Gfx1030, vs, ps).unwrap();
    let uv = [[0.5, 0.5, 0., 1.]; 3];
    let draws: Vec<_> = words
        .iter()
        .map(|_| TriangleDraw {
            vertices: &POS,
            colors: &uv,
            viewport: [words.len(), 1],
        })
        .collect();
    let states: Vec<_> = (0..words.len())
        .map(|x| TriangleState {
            scissor: Some([x, 0, 1, 1]),
            ..Default::default()
        })
        .collect();
    let shaders: Vec<_> = words
        .iter()
        .map(|_| TriangleShader {
            source: &compiled.shader.msl,
            entries: [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
        })
        .collect();
    let uniforms: Vec<_> = words
        .iter()
        .map(|values| DrawUniforms {
            required_mask: compiled.required_uniform_mask,
            values: Some(*values),
        })
        .collect();
    let pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent: [words.len(), 1],
        clears: &[[17, 31, 47, 255]],
        clear_depth: None,
    };
    let output = gpu
        .submit_view_binding_triangle_targets_with_uniforms_rgba8(
            &shaders,
            &pass,
            &vec![bindings; words.len()],
            None,
            &[None],
            &uniforms,
        )
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(output, vec![expected.as_flattened().to_vec()]);
}

#[test]
fn translated_nested_runtime_counts_literal_product_grid() {
    // Rows outer=0..4, columns inner=0..4; UNORM bytes for n*m/16.
    let gray = [
        [0, 0, 0, 0, 0],
        [0, 16, 32, 48, 64],
        [0, 32, 64, 96, 128],
        [0, 48, 96, 143, 191],
        [0, 64, 128, 191, 255],
    ];
    let mut words = Vec::new();
    let mut expected = Vec::new();
    for (n, row) in gray.into_iter().enumerate() {
        for (m, value) in row.into_iter().enumerate() {
            words.push([n as u32, m as u32, 0, 0, 0, 0, 0, 0]);
            expected.push([value, value, value, 255]);
        }
    }
    pixel_grid(
        include_bytes!("../../../../fixtures/rdna2/x9-nested-loop-vgpr-ps.bin"),
        &words,
        &expected,
        vec![],
    );
}

#[test]
fn translated_nested_parent_carried_child_bound_changes_each_iteration() {
    let words = [
        [0, 0, 0, 0, 0, 0, 0, 0],
        [1, 0, 0, 0, 0, 0, 0, 0],
        [2, 0, 0, 0, 0, 0, 0, 0],
        [3, 0, 0, 0, 0, 0, 0, 0],
        [4, 0, 0, 0, 0, 0, 0, 0],
        [9, 0, 0, 0, 0, 0, 0, 0],
    ];
    // Successive child trips 1,2,3,4 accumulate 0,1,3,6,10 sixteenths.
    let expected = [
        [0, 0, 0, 255],
        [16, 16, 16, 255],
        [48, 48, 48, 255],
        [96, 96, 96, 255],
        [159, 159, 159, 255],
        [159, 159, 159, 255],
    ];
    pixel_grid(
        include_bytes!("../../../../fixtures/rdna2/x9-nested-loop-changing-bound-ps.bin"),
        &words,
        &expected,
        vec![],
    );
}

#[test]
fn translated_nested_scalar_before_inside_after_and_sibling_children() {
    // Initial 1; each outer adds 2, first child adds 3*m, sibling adds 5*m,
    // then outer adds 7. Literal examples of 1+n*(9+8*m).
    let cases = [
        (0, 4, 1),
        (1, 0, 10),
        (1, 1, 18),
        (2, 1, 35),
        (2, 3, 67),
        (3, 2, 76),
        (4, 4, 165),
    ];
    let mut words = Vec::new();
    let mut expected = Vec::new();
    for (n, m, result) in cases {
        for mismatch in [false, true] {
            words.push([n, m, result + u32::from(mismatch), 0, 0, 0, 0, 0]);
            expected.push(if mismatch {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            });
        }
    }
    pixel_grid(
        include_bytes!("../../../../fixtures/rdna2/x9-nested-loop-scalar-siblings-ps.bin"),
        &words,
        &expected,
        vec![],
    );
}

#[test]
fn translated_nested_parallel_swap_survives_child_and_parent_edges() {
    let cases = [
        (0, 3, 7, 11),
        (1, 0, 7, 11),
        (1, 1, 11, 7),
        (1, 2, 7, 11),
        (2, 3, 7, 11),
        (3, 3, 11, 7),
        (4, 4, 7, 11),
    ];
    let mut words = Vec::new();
    let mut expected = Vec::new();
    for (n, m, a, b) in cases {
        for mismatch in [false, true] {
            words.push([n, m, a, b + u32::from(mismatch), 0, 0, 0, 0]);
            expected.push(if mismatch {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            });
        }
    }
    pixel_grid(
        include_bytes!("../../../../fixtures/rdna2/x9-nested-loop-swap-ps.bin"),
        &words,
        &expected,
        vec![],
    );
}

#[test]
fn translated_nested_three_levels_reinitialize_each_descendant() {
    let cases = [
        (0, 4, 4, 0),
        (4, 0, 4, 0),
        (4, 4, 0, 0),
        (1, 1, 1, 4),
        (2, 2, 2, 32),
        (2, 3, 4, 96),
        (4, 4, 4, 255),
        (9, 9, 9, 255),
    ];
    let mut words = Vec::new();
    let mut expected = Vec::new();
    for (n, m, k, gray) in cases {
        words.push([n, m, k, 0, 0, 0, 0, 0]);
        expected.push([gray, gray, gray, 255]);
    }
    pixel_grid(
        include_bytes!("../../../../fixtures/rdna2/x9-nested-loop-depth-three-ps.bin"),
        &words,
        &expected,
        vec![],
    );
}

#[test]
fn translated_nested_retained_child_counter_is_not_reset() {
    // After the first outer iteration the child remains exhausted. These differ
    // from reinitialized n*m/16, especially (3,2) and (4,1).
    let cases = [
        (0, 4, 0),
        (1, 0, 0),
        (1, 2, 32),
        (2, 2, 32),
        (3, 2, 32),
        (4, 1, 16),
        (4, 4, 64),
        (9, 9, 64),
    ];
    let mut words = Vec::new();
    let mut expected = Vec::new();
    for (n, m, gray) in cases {
        words.push([n, m, 0, 0, 0, 0, 0, 0]);
        expected.push([gray, gray, gray, 255]);
    }
    pixel_grid(
        include_bytes!("../../../../fixtures/rdna2/x9-nested-loop-retained-child-counter-ps.bin"),
        &words,
        &expected,
        vec![],
    );
}

#[test]
fn translated_nested_child_alias_and_final_scc_survive_parent_reentry() {
    // sum=8*n+m*n*(n-1)/2; child result=1+n*m. The +7 per outer
    // iteration comes from CSELECT consuming false child-final-header SCC,
    // before parent decrement or any other flag-producing instruction.
    let cases = [
        (0, 4, 0, 1),
        (1, 0, 8, 1),
        (1, 2, 8, 3),
        (2, 2, 18, 5),
        (3, 2, 30, 7),
        (4, 4, 56, 17),
        (9, 9, 56, 17),
    ];
    let mut words = Vec::new();
    let mut expected = Vec::new();
    for (n, m, sum, result) in cases {
        for (a, b, color) in [
            (0, 0, [255, 255, 0, 255]),
            (1, 0, [0, 255, 0, 255]),
            (0, 1, [255, 0, 0, 255]),
        ] {
            words.push([n, m, sum + a, result + b, 0, 0, 0, 0]);
            expected.push(color);
        }
    }
    pixel_grid(
        include_bytes!("../../../../fixtures/rdna2/x9-nested-loop-alias-scc-ps.bin"),
        &words,
        &expected,
        vec![],
    );
}

#[test]
fn translated_nested_sampling_preserves_both_pairs_across_child_reentry() {
    let gpu = NativeCompute::new().unwrap();
    let mut bindings = Vec::new();
    for (slot, color) in [(0, [16, 0, 0, 255]), (1, [0, 16, 0, 255])] {
        let texture = MetalRgba8Texture::upload(
            gpu.device(),
            MetalRgba8TextureDesc::new(1, 1, 1).unwrap(),
            &[Rgba8MipLevel::new_packed(0, 1, 1, &color)],
        )
        .unwrap();
        let sampler =
            MetalSampler::new(gpu.device(), xenora_texture::SamplerDesc::nearest_clamp()).unwrap();
        bindings.push(SampledBinding::Whole(
            BoundSampledTexture::bind(
                texture,
                sampler,
                BindingSlot::new(slot),
                BindingSlot::new(slot),
            )
            .unwrap(),
        ));
    }
    let ps = include_bytes!("../../../../fixtures/rdna2/x9-nested-loop-sampled-accum-ps.bin");
    let compiled = compile_graphics_pipeline(
        Gfx1030,
        include_bytes!("../../../../fixtures/rdna2/x9-varying-vs.bin"),
        ps,
    )
    .unwrap();
    assert_eq!(compiled.required_uniform_mask, 3);
    assert_eq!(compiled.sampled_pairs, 0x09);
    let cases = [
        (0, 4, [0, 0, 0, 255]),
        (4, 0, [0, 0, 0, 255]),
        (1, 1, [0, 16, 0, 255]),
        (1, 2, [16, 16, 0, 255]),
        (2, 2, [32, 32, 0, 255]),
        (3, 3, [48, 96, 0, 255]),
        (4, 4, [64, 192, 0, 255]),
        (9, 9, [64, 192, 0, 255]),
        (0, 0, [0, 0, 0, 255]),
    ];
    let mut words = Vec::new();
    let mut expected = Vec::new();
    for (n, m, color) in cases {
        words.push([n, m, 0, 0, 0, 0, 0, 0]);
        expected.push(color);
    }
    pixel_grid_on(&gpu, ps, &words, &expected, bindings);
}
