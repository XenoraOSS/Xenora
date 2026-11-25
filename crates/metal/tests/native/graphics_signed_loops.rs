// SPDX-License-Identifier: GPL-3.0-or-later
//! Original signed-positive countdowns with literal pixel oracles, independent of scalar evaluators.
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

// Each incorrect expectation changes just one output channel. In particular,
// 256 trips versus an expected255 cannot disappear through UNORM saturation.
fn checked_cases(cases: &[[u32; 8]], fields: &[(usize, usize)]) -> (Vec<[u32; 8]>, Vec<[u8; 4]>) {
    let mut words = Vec::new();
    let mut pixels = Vec::new();
    for case in cases {
        words.push(*case);
        pixels.push([255, 255, 255, 255]);
        for &(field, channel) in fields {
            let mut wrong = *case;
            wrong[field] = if field == 1 && wrong[field] == 256 {
                255
            } else {
                wrong[field] ^ 1
            };
            words.push(wrong);
            let mut pixel = [255, 255, 255, 255];
            pixel[channel] = 0;
            pixels.push(pixel);
        }
    }
    (words, pixels)
}

#[test]
fn translated_signed_count_exact_256_and_reversed_operands() {
    let cases = [
        [0xffffffff, 0, 0xffffffff, 0, 0, 0, 0, 0],
        [0x80000000, 0, 0x80000000, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0],
        [1, 1, 0, 0, 0, 0, 0, 0],
        [256, 256, 0, 0, 0, 0, 0, 0],
        [0x7fffffff, 256, 0, 0, 0, 0, 0, 0],
    ];
    let (words, expected) = checked_cases(&cases, &[(1, 0), (2, 1), (3, 2), (4, 2)]);
    for ps in [
        include_bytes!("../../../../fixtures/rdna2/x9-loop-signed-count-ps.bin").as_slice(),
        include_bytes!("../../../../fixtures/rdna2/x9-loop-signed-reversed-ps.bin").as_slice(),
    ] {
        let compiled = compile_graphics_pipeline(Gfx1030, VS, ps).unwrap();
        assert_eq!(compiled.required_uniform_mask, 0x1f);
        pixel_grid(ps, &words, &expected, vec![]);
    }
}

#[test]
fn translated_signed_negative_cap_preserves_counter_and_integer_liveout() {
    let cases = [
        [0x80000000, 0, 0x80000000, 0, 0, 0x7fc00001, 0, 0],
        [0xfffffffe, 0, 0xfffffffe, 0, 0, 0x7fc00001, 0, 0],
        [0xffffffff, 0, 0xffffffff, 0, 0, 0x7fc00001, 0, 0],
        [0, 0, 0xffffffff, 0, 0, 0x7fc00001, 0, 0],
        [0x7fffffff, 0, 0xffffffff, 0, 0, 0x7fc00001, 0, 0],
    ];
    let (words, expected) = checked_cases(&cases, &[(1, 0), (2, 1), (3, 2), (4, 2), (5, 2)]);
    pixel_grid(
        include_bytes!("../../../../fixtures/rdna2/x9-loop-signed-negative-ps.bin"),
        &words,
        &expected,
        vec![],
    );
}

#[test]
fn translated_signed_decrement_scc_and_true_break_liveouts() {
    // [input, trips, final bits, exit SCC, decrement SCC sum, stop].
    // Guarded positive SUB_I32 has no overflow. Save its SCC immediately;
    // the later comparison supplies the true SCC of a taken early exit.
    let cases = [
        [0xffffffff, 0, 0xffffffff, 0, 0, 0, 0, 0],
        [0x80000000, 0, 0x80000000, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0],
        [1, 1, 0, 1, 0, 0, 0, 0],
        [4, 2, 2, 1, 0, 2, 0, 0],
        [4, 4, 0, 0, 0, 9, 0, 0],
        [0x7fffffff, 1, 3, 1, 0, 3, 0, 0],
    ];
    let (words, expected) = checked_cases(&cases, &[(1, 0), (2, 1), (3, 2), (4, 2)]);
    pixel_grid(
        include_bytes!("../../../../fixtures/rdna2/x9-loop-signed-break-ps.bin"),
        &words,
        &expected,
        vec![],
    );
}

#[test]
fn translated_signed_nested_negative_child_and_reset_counts() {
    // [outer, child, trips, final child bits, final outer bits].
    let cases = [
        [0xffffffff, 3, 0, 3, 0xffffffff, 0, 0, 0],
        [0x80000000, 0xffffffff, 0, 0xffffffff, 0x80000000, 0, 0, 0],
        [0, 4, 0, 4, 0, 0, 0, 0],
        [3, 0xffffffff, 0, 0xffffffff, 0, 0, 0, 0],
        [4, 0x80000000, 0, 0x80000000, 0, 0, 0, 0],
        [3, 0, 0, 0, 0, 0, 0, 0],
        [1, 1, 1, 0, 0, 0, 0, 0],
        [2, 3, 6, 0, 0, 0, 0, 0],
        [3, 2, 6, 0, 0, 0, 0, 0],
        [0x7fffffff, 0x7fffffff, 16, 0, 0, 0, 0, 0],
    ];
    let (words, expected) = checked_cases(&cases, &[(2, 0), (3, 1), (4, 2)]);
    pixel_grid(
        include_bytes!("../../../../fixtures/rdna2/x9-loop-signed-nested-ps.bin"),
        &words,
        &expected,
        vec![],
    );
}

#[test]
fn translated_signed_sampling_runtime_union_and_negative_constant_pruning() {
    let gpu = NativeCompute::new().unwrap();
    let varying = include_bytes!("../../../../fixtures/rdna2/x9-varying-vs.bin");
    let ps = include_bytes!("../../../../fixtures/rdna2/x9-loop-signed-sampled-ps.bin");
    let compiled = compile_graphics_pipeline(Gfx1030, varying, ps).unwrap();
    assert_eq!(compiled.required_uniform_mask, 1);
    assert_eq!(compiled.sampled_pairs, 1);
    let color = [32, 16, 8, 255];
    let texture = MetalRgba8Texture::upload(
        gpu.device(),
        MetalRgba8TextureDesc::new(1, 1, 1).unwrap(),
        &[Rgba8MipLevel::new_packed(0, 1, 1, &color)],
    )
    .unwrap();
    let sampler =
        MetalSampler::new(gpu.device(), xenora_texture::SamplerDesc::nearest_clamp()).unwrap();
    let binding = SampledBinding::Whole(
        BoundSampledTexture::bind(texture, sampler, BindingSlot::new(0), BindingSlot::new(0))
            .unwrap(),
    );
    let words = [0xffffffff, 0x80000000, 0, 1, 2, 4, 0x7fffffff].map(|n| [n, 0, 0, 0, 0, 0, 0, 0]);
    pixel_grid_on(
        &gpu,
        ps,
        &words,
        &[
            [0, 0, 0, 255],
            [0, 0, 0, 255],
            [0, 0, 0, 255],
            [32, 16, 8, 255],
            [64, 32, 16, 255],
            [128, 64, 32, 255],
            [128, 64, 32, 255],
        ],
        vec![binding],
    );
    let dead = include_bytes!("../../../../fixtures/rdna2/x9-loop-signed-dead-sample-ps.bin");
    let compiled = compile_graphics_pipeline(Gfx1030, varying, dead).unwrap();
    assert_eq!(compiled.required_uniform_mask, 0);
    assert_eq!(compiled.sampled_pairs, 0);
    pixel_grid_with_vertex(&gpu, varying, dead, &words, &[[0, 0, 0, 255]; 7], vec![]);
}
