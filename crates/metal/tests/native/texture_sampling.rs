// SPDX-License-Identifier: GPL-3.0-or-later

use xenora_metal::sampler::MetalSampler;
use xenora_metal::texture_pipeline::{
    BindingSlot, BoundSampledTexture, MetalRgba8Texture, Rgba8MipLevel, Rgba8TextureDesc,
};
use xenora_texture::SamplerDesc;

use super::context;

#[test]
fn native_gpu_rgba8_sampling_matches_cpu_reference() {
    let gpu = context();

    // 1. Prepare a 2x2 texture with four distinct RGBA8 pixels.
    // Pixel layout:
    // (0, 0): Red   [255,   0,   0, 255] -> word: 0xff0000ff
    // (1, 0): Green [  0, 255,   0, 255] -> word: 0xff00ff00
    // (0, 1): Blue  [  0,   0, 255, 255] -> word: 0xffff0000
    // (1, 1): White [255, 255, 255, 255] -> word: 0xffffffff
    let red_word = 0xff00_00ff_u32;
    let green_word = 0xff00_ff00_u32;
    let blue_word = 0xffff_0000_u32;
    let white_word = 0xffff_ffff_u32;

    let texels_level0: [u8; 16] = [
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
    ];

    let tex_desc = Rgba8TextureDesc::new(2, 2, 1).expect("valid 2x2 texture desc");
    let mip0 = Rgba8MipLevel::new_packed(0, 2, 2, &texels_level0);
    let texture = MetalRgba8Texture::upload(gpu.device(), tex_desc, &[mip0])
        .expect("upload RGBA8 2x2 texture to Metal");

    // 2. Create native Metal sampler with nearest filtering and clamp-to-edge addressing.
    let sampler_desc = SamplerDesc::nearest_clamp();
    let sampler =
        MetalSampler::new(gpu.device(), sampler_desc).expect("create native Metal sampler");

    // 3. Verify binding descriptor
    let bound = BoundSampledTexture::bind(
        texture.clone(),
        sampler.clone(),
        BindingSlot::new(0),
        BindingSlot::new(0),
    )
    .expect("valid pipeline binding");
    assert_eq!(bound.texture_slot().index, 0);
    assert_eq!(bound.sampler_slot().index, 0);

    // 4. Test coordinates including interior points, texel boundaries, and edge clamping.
    let uv_coords = [
        // (0, 0) interior -> Red
        [0.25_f32, 0.25],
        // (1, 0) interior -> Green
        [0.75, 0.25],
        // (0, 1) interior -> Blue
        [0.25, 0.75],
        // (1, 1) interior -> White
        [0.75, 0.75],
        // Clamped negative U and V (< 0.0) -> clamps to (0, 0) -> Red
        [-0.5, -0.5],
        // Clamped excess U and V (> 1.0) -> clamps to (1, 1) -> White
        [1.5, 1.5],
        // Clamped mixed: U < 0.0, V > 1.0 -> clamps to (0, 1) -> Blue
        [-0.2, 1.8],
        // Clamped mixed: U > 1.0, V < 0.0 -> clamps to (1, 0) -> Green
        [1.2, -0.3],
    ];

    let expected_words = [
        red_word, green_word, blue_word, white_word, red_word, white_word, blue_word, green_word,
    ];

    // 5. Dispatch on the native Apple Silicon GPU.
    let actual_words = gpu
        .sample_rgba8(&texture, &sampler, &uv_coords)
        .expect("Metal GPU texture sampling dispatch");

    assert_eq!(
        actual_words, expected_words,
        "Native Apple GPU sampling output must match expected pixel words"
    );

    println!(
        "device={} verified {} texture sampling points across interior and clamped coords on Apple Silicon GPU",
        gpu.device_name(),
        uv_coords.len()
    );
}

#[test]
fn malformed_texture_descriptors_fail_before_native_allocation() {
    let gpu = context();
    for desc in [
        Rgba8TextureDesc {
            width: 0,
            height: 1,
            mips: 1,
        },
        Rgba8TextureDesc {
            width: 1,
            height: 1,
            mips: 65,
        },
        Rgba8TextureDesc {
            width: 16385,
            height: 1,
            mips: 1,
        },
        Rgba8TextureDesc {
            width: usize::MAX,
            height: 2,
            mips: 1,
        },
    ] {
        assert!(MetalRgba8Texture::allocate(gpu.device(), desc).is_err());
    }
}
