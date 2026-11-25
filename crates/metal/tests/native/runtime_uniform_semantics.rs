// SPDX-License-Identifier: GPL-3.0-or-later
//! Original assembled shaders; literal truth tables do not use decoder comparison helpers.
use xenora_metal::*;
use xenora_shader::{
    Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics_pipeline,
};
const VS: &[u8] = include_bytes!("../../../../fixtures/rdna2/x9-constant-vs.bin");
const POS: [[f32; 4]; 3] = [[-1., -1., 0.5, 1.], [3., -1., 0.5, 1.], [-1., 3., 0.5, 1.]];
const COLORS: [[f32; 4]; 3] = [[0.; 4]; 3];
const RELATIONS: [&[u8]; 12] = [
    include_bytes!("../../../../fixtures/rdna2/x9-runtime-eq-i32-ps.bin").as_slice(),
    include_bytes!("../../../../fixtures/rdna2/x9-runtime-lg-i32-ps.bin").as_slice(),
    include_bytes!("../../../../fixtures/rdna2/x9-runtime-gt-i32-ps.bin").as_slice(),
    include_bytes!("../../../../fixtures/rdna2/x9-runtime-ge-i32-ps.bin").as_slice(),
    include_bytes!("../../../../fixtures/rdna2/x9-runtime-lt-i32-ps.bin").as_slice(),
    include_bytes!("../../../../fixtures/rdna2/x9-runtime-le-i32-ps.bin").as_slice(),
    include_bytes!("../../../../fixtures/rdna2/x9-runtime-eq-u32-ps.bin").as_slice(),
    include_bytes!("../../../../fixtures/rdna2/x9-runtime-lg-u32-ps.bin").as_slice(),
    include_bytes!("../../../../fixtures/rdna2/x9-runtime-gt-u32-ps.bin").as_slice(),
    include_bytes!("../../../../fixtures/rdna2/x9-runtime-ge-u32-ps.bin").as_slice(),
    include_bytes!("../../../../fixtures/rdna2/x9-runtime-lt-u32-ps.bin").as_slice(),
    include_bytes!("../../../../fixtures/rdna2/x9-runtime-le-u32-ps.bin").as_slice(),
];

#[test]
fn translated_runtime_all_twelve_integer_relations_literal_grid() {
    // Columns within each signed/unsigned group: ==, !=, >, >=, <, <=.
    // Final two inputs are integer words, despite their float NaN/Infinity encodings.
    let cases = [
        (
            0xffffffff,
            1,
            [false, true, false, false, true, true],
            [false, true, true, true, false, false],
        ),
        (
            1,
            0xffffffff,
            [false, true, true, true, false, false],
            [false, true, false, false, true, true],
        ),
        (
            0x80000000,
            0x7fffffff,
            [false, true, false, false, true, true],
            [false, true, true, true, false, false],
        ),
        (
            0x7fffffff,
            0x80000000,
            [false, true, true, true, false, false],
            [false, true, false, false, true, true],
        ),
        (
            0x80000000,
            0x80000000,
            [true, false, false, true, false, true],
            [true, false, false, true, false, true],
        ),
        (
            0,
            0,
            [true, false, false, true, false, true],
            [true, false, false, true, false, true],
        ),
        (
            0x7fc00000,
            0x7f800000,
            [false, true, true, true, false, false],
            [false, true, true, true, false, false],
        ),
        (
            0x7f800000,
            0x7fc00000,
            [false, true, false, false, true, true],
            [false, true, false, false, true, true],
        ),
    ];
    let pipelines: Vec<_> = RELATIONS
        .iter()
        .map(|ps| compile_graphics_pipeline(Architecture::Gfx1030, VS, ps).unwrap())
        .collect();
    let mut draws = Vec::new();
    let mut states = Vec::new();
    let mut shaders = Vec::new();
    let mut uniforms = Vec::new();
    let mut expected = Vec::new();
    for (y, (a, b, signed, unsigned)) in cases.iter().enumerate() {
        for (x, pipeline) in pipelines.iter().enumerate() {
            assert_eq!(pipeline.required_uniform_mask, 3);
            draws.push(TriangleDraw {
                vertices: &POS,
                colors: &COLORS,
                viewport: [12, 8],
            });
            states.push(TriangleState {
                scissor: Some([x, y, 1, 1]),
                ..Default::default()
            });
            shaders.push(TriangleShader {
                source: &pipeline.shader.msl,
                entries: [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
            });
            uniforms.push(DrawUniforms {
                required_mask: pipeline.required_uniform_mask,
                values: Some([*a, *b, 0, 0, 0, 0, 0, 0]),
            });
            let truth = if x < 6 { signed[x] } else { unsigned[x - 6] };
            expected.extend_from_slice(if truth {
                &[255, 0, 0, 255]
            } else {
                &[0, 255, 0, 255]
            });
        }
    }
    let gpu = NativeCompute::new().unwrap();
    let pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent: [12, 8],
        clears: &[[7, 11, 13, 255]],
        clear_depth: None,
    };
    let output = gpu
        .submit_view_binding_triangle_targets_with_uniforms_rgba8(
            &shaders,
            &pass,
            &vec![vec![]; 96],
            None,
            &[None],
            &uniforms,
        )
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(output, vec![expected]);
}

#[test]
fn translated_runtime_nested_scc_and_vgpr_joins_export_two_targets() {
    let pipeline = compile_graphics_pipeline(
        Architecture::Gfx1030,
        VS,
        include_bytes!("../../../../fixtures/rdna2/x9-runtime-nested-mrt-ps.bin"),
    )
    .unwrap();
    assert_eq!(pipeline.required_uniform_mask, 3);
    let draws = [TriangleDraw {
        vertices: &POS,
        colors: &COLORS,
        viewport: [4, 1],
    }; 4];
    let states = std::array::from_fn::<_, 4, _>(|x| TriangleState {
        scissor: Some([x, 0, 1, 1]),
        ..Default::default()
    });
    let uniforms = [(0, 0), (0, 1), (1, 0), (1, 1)].map(|(a, b)| DrawUniforms {
        required_mask: 3,
        values: Some([a, b, 0, 0, 0, 0, 0, 0]),
    });
    let shaders = [TriangleShader {
        source: &pipeline.shader.msl,
        entries: [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
    }; 4];
    let pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent: [4, 1],
        clears: &[[7, 11, 13, 255], [17, 19, 23, 255]],
        clear_depth: None,
    };
    let gpu = NativeCompute::new().unwrap();
    let output = gpu
        .submit_view_binding_triangle_targets_with_uniforms_rgba8(
            &shaders,
            &pass,
            &[vec![], vec![], vec![], vec![]],
            None,
            &[None, None],
            &uniforms,
        )
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(
        output,
        vec![
            vec![
                255, 255, 255, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 255, 255
            ],
            vec![
                255, 255, 255, 255, 0, 0, 255, 255, 255, 0, 0, 255, 255, 0, 0, 255
            ],
        ]
    );
}

#[test]
fn translated_runtime_conditional_sampling_selects_both_pairs_per_draw() {
    let pipeline = compile_graphics_pipeline(
        Architecture::Gfx1030,
        include_bytes!("../../../../fixtures/rdna2/x9-varying-vs.bin"),
        include_bytes!("../../../../fixtures/rdna2/x9-runtime-sampled-ps.bin"),
    )
    .unwrap();
    assert_eq!(pipeline.required_uniform_mask, 1);
    assert_eq!(pipeline.sampled_pairs, 0b1001);
    let gpu = NativeCompute::new().unwrap();
    // Four different literal mip colors detect either texture or sampler reuse.
    // Pair0 samples A mip0; pair1 samples B mip1. Opposite mips are poison colors.
    let a0 = [255, 0, 0, 255].repeat(4);
    let a1 = [0, 0, 255, 255];
    let b0 = [255, 255, 0, 255].repeat(4);
    let b1 = [0, 255, 0, 255];
    let texture_a = MetalRgba8Texture::upload(
        gpu.device(),
        MetalRgba8TextureDesc::new(2, 2, 2).unwrap(),
        &[
            Rgba8MipLevel::new_packed(0, 2, 2, &a0),
            Rgba8MipLevel::new_packed(1, 1, 1, &a1),
        ],
    )
    .unwrap();
    let texture_b = MetalRgba8Texture::upload(
        gpu.device(),
        MetalRgba8TextureDesc::new(2, 2, 2).unwrap(),
        &[
            Rgba8MipLevel::new_packed(0, 2, 2, &b0),
            Rgba8MipLevel::new_packed(1, 1, 1, &b1),
        ],
    )
    .unwrap();
    let pair0 = BoundSampledTexture::bind(
        texture_a,
        MetalSampler::new(
            gpu.device(),
            xenora_texture::SamplerDesc::nearest_clamp().with_explicit_mip(0),
        )
        .unwrap(),
        BindingSlot::new(0),
        BindingSlot::new(0),
    )
    .unwrap();
    let pair1 = BoundSampledTexture::bind(
        texture_b,
        MetalSampler::new(
            gpu.device(),
            xenora_texture::SamplerDesc::nearest_clamp().with_explicit_mip(1),
        )
        .unwrap(),
        BindingSlot::new(1),
        BindingSlot::new(1),
    )
    .unwrap();
    let uv = [[0.25, 0.25, 0.0, 1.0]; 3];
    let draws = [TriangleDraw {
        vertices: &POS,
        colors: &uv,
        viewport: [4, 2],
    }; 4];
    let states = std::array::from_fn::<_, 4, _>(|x| TriangleState {
        scissor: Some([x, 0, 1, 2]),
        ..Default::default()
    });
    let shaders = [TriangleShader {
        source: &pipeline.shader.msl,
        entries: [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
    }; 4];
    let uniforms = [0, 1, 0, 0x80000000].map(|condition| DrawUniforms {
        required_mask: pipeline.required_uniform_mask,
        values: Some([condition, 0, 0, 0, 0, 0, 0, 0]),
    });
    // Both feasible pairs are supplied on every draw. Native trusted-MSL calls
    // do not enforce the compiler's sampled-pair requirements; loader does that.
    let bindings = vec![vec![SampledBinding::Whole(pair0), SampledBinding::Whole(pair1)]; 4];
    let pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent: [4, 2],
        clears: &[[7, 11, 13, 255]],
        clear_depth: None,
    };
    let pending = gpu
        .submit_view_binding_triangle_targets_with_uniforms_rgba8(
            &shaders,
            &pass,
            &bindings,
            None,
            &[None],
            &uniforms,
        )
        .unwrap();
    drop(bindings);
    assert_eq!(
        pending.wait().unwrap(),
        vec![
            [
                255, 0, 0, 255, 0, 255, 0, 255, 255, 0, 0, 255, 0, 255, 0, 255
            ]
            .repeat(2)
        ]
    );
}
