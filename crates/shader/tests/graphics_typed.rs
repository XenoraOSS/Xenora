// SPDX-License-Identifier: GPL-3.0-or-later
//! Original fixture type checks. Generated-source assertions are not GPU bit evidence.
use xenora_shader::{
    Architecture::Gfx1030, GraphicsExportType, GraphicsPipelineTypes, GraphicsSampleType,
    compile_graphics_pipeline, compile_graphics_pipeline_typed,
};
const VS: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-varying-vs.bin");
const SAMPLE: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-view-sample-ps.bin");
const DIAMOND: &[u8] = include_bytes!("../../../fixtures/rdna2/x9-runtime-sampled-ps.bin");
fn uint_types(both: bool) -> GraphicsPipelineTypes {
    let mut t = GraphicsPipelineTypes::default();
    t.textures[0] = GraphicsSampleType::Uint;
    if both {
        t.textures[1] = GraphicsSampleType::Uint;
    }
    t.colors[0] = GraphicsExportType::Uint;
    t
}
#[test]
fn typed_default_preserves_original_float_pipeline_exactly() {
    for pixel in [
        SAMPLE,
        DIAMOND,
        include_bytes!("../../../fixtures/rdna2/x9-varying-ps.bin").as_slice(),
    ] {
        let old = compile_graphics_pipeline(Gfx1030, VS, pixel).unwrap();
        let explicit =
            compile_graphics_pipeline_typed(Gfx1030, VS, pixel, GraphicsPipelineTypes::default())
                .unwrap();
        assert_eq!(old.shader.msl, explicit.shader.msl);
        assert_eq!(old.shader.ir, explicit.shader.ir);
        assert_eq!(old.shader.disassembly, explicit.shader.disassembly);
        assert_eq!(old.sampled_pairs, explicit.sampled_pairs);
        assert_eq!(old.color_targets, explicit.color_targets);
        assert_eq!(old.required_uniform_mask, explicit.required_uniform_mask);
        assert_eq!(explicit.types, GraphicsPipelineTypes::default());
    }
}
#[test]
fn typed_unused_nondefault_texture_and_export_slots_reject() {
    for texture_type in [GraphicsSampleType::Uint, GraphicsSampleType::Depth] {
        let mut t = GraphicsPipelineTypes::default();
        t.textures[1] = texture_type;
        assert!(compile_graphics_pipeline_typed(Gfx1030, VS, SAMPLE, t).is_err());
    }
    for slot in 1..8 {
        let mut t = GraphicsPipelineTypes::default();
        t.colors[slot] = GraphicsExportType::Uint;
        assert!(compile_graphics_pipeline_typed(Gfx1030, VS, SAMPLE, t).is_err());
    }
}
#[test]
fn typed_uint_transport_survives_runtime_join_but_cannot_export_float() {
    let compiled = compile_graphics_pipeline_typed(Gfx1030, VS, DIAMOND, uint_types(true)).unwrap();
    assert_eq!(compiled.sampled_pairs, 9);
    assert_eq!(compiled.pixel_uniform_mask, 1);
    assert_eq!(compiled.types, uint_types(true));
    assert!(
        compiled
            .shader
            .msl
            .contains("texture2d<uint, access::sample>")
    );
    assert!(compiled.shader.msl.contains("as_type<float>(tex0.sample"));
    assert!(compiled.shader.msl.contains("as_type<float>(tex1.sample"));
    assert!(compiled.shader.msl.contains("as_type<uint4>(float4("));
    for slot in 0..2 {
        let mut t = GraphicsPipelineTypes::default();
        t.textures[slot] = GraphicsSampleType::Uint;
        assert!(
            compile_graphics_pipeline_typed(Gfx1030, VS, DIAMOND, t).is_err(),
            "taint must cross phi for slot{slot}"
        );
    }
}
#[test]
fn typed_uint_float_arithmetic_rejects_direct_and_loop_phi_uses() {
    for (pixel, both) in [
        (
            include_bytes!("../../../fixtures/rdna2/x9-sampled-arithmetic-ps.bin").as_slice(),
            false,
        ),
        (
            include_bytes!("../../../fixtures/rdna2/x9-loop-sampled-accum-ps.bin").as_slice(),
            true,
        ),
    ] {
        assert!(
            compile_graphics_pipeline(Gfx1030, VS, pixel).is_ok(),
            "fixture must be valid float program"
        );
        assert!(compile_graphics_pipeline_typed(Gfx1030, VS, pixel, uint_types(both)).is_err());
    }
}
#[test]
fn typed_depth_is_scalar_with_zero_zero_one_expansion() {
    let mut t = GraphicsPipelineTypes::default();
    t.textures[0] = GraphicsSampleType::Depth;
    let compiled = compile_graphics_pipeline_typed(Gfx1030, VS, SAMPLE, t).unwrap();
    assert_eq!(compiled.types, t);
    assert!(
        compiled
            .shader
            .msl
            .contains("depth2d<float, access::sample>")
    );
    let samples: Vec<_> = compiled
        .shader
        .msl
        .lines()
        .filter(|line| line.contains("tex0.sample("))
        .collect();
    assert_eq!(samples.len(), 1, "only depth R reads the texture");
    assert!(!samples[0].contains(").r"));
    assert!(compiled.shader.msl.contains("= 0.0f;"));
    assert!(compiled.shader.msl.contains("= 1.0f;"));
    assert!(!compiled.shader.msl.contains("as_type<uint4>"));
}

#[test]
fn typed_uint_coordinates_reject_direct_and_phi_merged_sample_bits() {
    for (original, both) in [(SAMPLE, false), (DIAMOND, true)] {
        // Original fixture suffix exports v4..v7. Replace that suffix with an
        // original IMAGE_SAMPLE_LZ v8..v11 using v4:v5 as coordinates, then export.
        // The first sample remains a valid float-coordinate producer.
        assert_eq!(
            &original[original.len() - 12..],
            &[0x0f, 0x18, 0, 0xf8, 4, 5, 6, 7, 0, 0, 0x81, 0xbf]
        );
        let mut pixel = original[..original.len() - 12].to_vec();
        pixel.extend_from_slice(&[0x08, 0x0f, 0x9c, 0xf0, 0x04, 0x08, 0x40, 0x00]);
        pixel.extend_from_slice(&[0x0f, 0x18, 0, 0xf8, 8, 9, 10, 11, 0, 0, 0x81, 0xbf]);
        assert!(
            compile_graphics_pipeline(Gfx1030, VS, &pixel).is_ok(),
            "float dependent sample must remain valid"
        );
        assert!(
            compile_graphics_pipeline_typed(Gfx1030, VS, &pixel, uint_types(both)).is_err(),
            "integer coordinates, phi={both}"
        );
    }
}
