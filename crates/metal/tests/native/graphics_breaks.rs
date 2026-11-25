// SPDX-License-Identifier: GPL-3.0-or-later
//! Original conditional loop exits with literal pixel oracles, independent of scalar evaluators.
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
    pixel_grid_with_vertex(gpu, vs, ps, words, expected, bindings);
}
fn pixel_grid_with_vertex(
    gpu: &NativeCompute,
    vs: &[u8],
    ps: &[u8],
    words: &[[u32; 8]],
    expected: &[[u8; 4]],
    bindings: Vec<SampledBinding>,
) {
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
fn translated_break_before_and_after_decrement() {
    let words = [
        [0, 2, 0, 0, 0, 0, 0, 0],
        [4, 4, 0, 0, 0, 0, 0, 0],
        [4, 2, 0, 0, 0, 0, 0, 0],
        [4, 0, 0, 0, 0, 0, 0, 0],
        [1, 1, 0, 0, 0, 0, 0, 0],
        [u32::MAX, 3, 0, 0, 0, 0, 0, 0],
    ];
    for (ps, gray) in [
        (
            include_bytes!("../../../../fixtures/rdna2/x9-loop-break-before-ps.bin").as_slice(),
            [0, 0, 64, 128, 0, 32],
        ),
        (
            include_bytes!("../../../../fixtures/rdna2/x9-loop-break-after-ps.bin").as_slice(),
            [0, 128, 64, 128, 32, 32],
        ),
    ] {
        pixel_grid(ps, &words, &gray.map(|v| [v, v, v, 255]), vec![]);
    }
}

#[test]
fn translated_break_typed_high_bit_swaps_and_exit_scc() {
    // Literal (count,stop,wordA,wordB,red,blue): break SCC=true,
    // normal exhaustion SCC=false; zero trips preserve initial U32 words.
    let cases = [
        (0, 2, 0xffffffff, 0x80000000, 0, 0),
        (4, 4, 0x80000000, 0xffffffff, 32, 255),
        (4, 3, 0xffffffff, 0x80000000, 64, 255),
        (4, 2, 0x80000000, 0xffffffff, 96, 255),
        (4, 0, 0xffffffff, 0x80000000, 128, 0),
        (1, 0, 0x80000000, 0xffffffff, 32, 0),
    ];
    let mut words = Vec::new();
    let mut expected = Vec::new();
    for (n, stop, a, b, red, blue) in cases {
        for mismatch in [0, 1, 2] {
            words.push([
                n,
                stop,
                a ^ u32::from(mismatch == 1),
                b ^ u32::from(mismatch == 2),
                0,
                0,
                0,
                0,
            ]);
            expected.push([red, if mismatch == 0 { 255 } else { 0 }, blue, 255]);
        }
    }
    pixel_grid(
        include_bytes!("../../../../fixtures/rdna2/x9-loop-break-typed-ps.bin"),
        &words,
        &expected,
        vec![],
    );
}

#[test]
fn translated_break_recurring_child_exit_sites_preserve_swaps_and_scc() {
    // Child starts at3 every outer iteration. Even outer: two visits/true
    // exit; odd outer: three visits/false exit. No evaluator supplies pixels.
    let cases = [
        (0, 0, 0xffffffff, 0x80000000, 0),
        (1, 1, 0x80000000, 0xffffffff, 48),
        (2, 11, 0x80000000, 0xffffffff, 80),
        (3, 12, 0xffffffff, 0x80000000, 128),
        (4, 22, 0xffffffff, 0x80000000, 159),
    ];
    let mut words = Vec::new();
    let mut expected = Vec::new();
    for (n, sum, a, b, red) in cases {
        for mismatch in [0, 1, 2, 3] {
            words.push([
                n,
                sum + u32::from(mismatch == 1),
                a ^ u32::from(mismatch == 2),
                b ^ u32::from(mismatch == 3),
                0,
                0,
                0,
                0,
            ]);
            expected.push([
                red,
                if mismatch >= 2 { 0 } else { 255 },
                if mismatch == 1 { 0 } else { 255 },
                255,
            ]);
        }
    }
    pixel_grid(
        include_bytes!("../../../../fixtures/rdna2/x9-loop-break-recurring-ps.bin"),
        &words,
        &expected,
        vec![],
    );
}

#[test]
fn translated_break_runtime_sampling_union_and_constant_dead_sample() {
    let gpu = NativeCompute::new().unwrap();
    let ps = include_bytes!("../../../../fixtures/rdna2/x9-loop-break-sampled-ps.bin");
    let varying = include_bytes!("../../../../fixtures/rdna2/x9-varying-vs.bin");
    let compiled = compile_graphics_pipeline(Gfx1030, varying, ps).unwrap();
    assert_eq!(compiled.sampled_pairs, 1);
    assert_eq!(compiled.required_uniform_mask, 3);
    let texel = [32, 16, 8, 255];
    let texture = MetalRgba8Texture::upload(
        gpu.device(),
        MetalRgba8TextureDesc::new(1, 1, 1).unwrap(),
        &[Rgba8MipLevel::new_packed(0, 1, 1, &texel)],
    )
    .unwrap();
    let sampler =
        MetalSampler::new(gpu.device(), xenora_texture::SamplerDesc::nearest_clamp()).unwrap();
    let binding = SampledBinding::Whole(
        BoundSampledTexture::bind(texture, sampler, BindingSlot::new(0), BindingSlot::new(0))
            .unwrap(),
    );
    let words = [
        [4, 4, 0, 0, 0, 0, 0, 0],
        [4, 3, 0, 0, 0, 0, 0, 0],
        [4, 2, 0, 0, 0, 0, 0, 0],
        [4, 0, 0, 0, 0, 0, 0, 0],
    ];
    pixel_grid_on(
        &gpu,
        ps,
        &words,
        &[
            [0, 0, 0, 255],
            [32, 16, 8, 255],
            [64, 32, 16, 255],
            [128, 64, 32, 255],
        ],
        vec![binding],
    );
    let dead = include_bytes!("../../../../fixtures/rdna2/x9-loop-break-dead-sample-ps.bin");
    let compiled = compile_graphics_pipeline(Gfx1030, varying, dead).unwrap();
    assert_eq!(compiled.sampled_pairs, 0);
    assert_eq!(compiled.required_uniform_mask, 1);
    // Preserve the varying vertex ABI even though no sampled binding is needed.
    pixel_grid_with_vertex(&gpu, varying, dead, &words, &[[0, 0, 0, 255]; 4], vec![]);
}

#[test]
fn translated_break_vertex_geometry_literal_columns() {
    let gpu = NativeCompute::new().unwrap();
    let compiled = compile_graphics_pipeline(
        Gfx1030,
        include_bytes!("../../../../fixtures/rdna2/x9-loop-break-geometry-vs.bin"),
        include_bytes!("../../../../fixtures/rdna2/x9-constant-ps.bin"),
    )
    .unwrap();
    let positions = [
        [-1., -1., 0.5, 1.],
        [-0.5, -1., 0.5, 1.],
        [-1., 1., 0.5, 1.],
        [-1., 1., 0.5, 1.],
        [-0.5, -1., 0.5, 1.],
        [-0.5, 1., 0.5, 1.],
    ];
    let colors = [[1., 0., 0., 1.]; 6];
    // At width16 each .125 clip-space step shifts exactly one pixel.
    for (n, stop, start) in [(0, 2, 0), (4, 4, 0), (4, 3, 1), (4, 2, 2), (4, 0, 4)] {
        let draws = [TriangleDraw {
            vertices: &positions,
            colors: &colors,
            viewport: [16, 2],
        }];
        let states = [TriangleState::default()];
        let pass = TriangleTargetsPass {
            draws: &draws,
            states: &states,
            extent: [16, 2],
            clears: &[[7, 11, 13, 255]],
            clear_depth: None,
        };
        let output = gpu
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
                    required_mask: compiled.required_uniform_mask,
                    values: Some([n, stop, 0, 0, 0, 0, 0, 0]),
                }],
            )
            .unwrap()
            .wait()
            .unwrap();
        let mut expected = Vec::new();
        for _ in 0..2 {
            for x in 0..16 {
                expected.extend_from_slice(if (start..start + 4).contains(&x) {
                    &[255, 0, 0, 255]
                } else {
                    &[7, 11, 13, 255]
                });
            }
        }
        assert_eq!(output, vec![expected], "count={n}, stop={stop}");
    }
}
