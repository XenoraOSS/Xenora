// SPDX-License-Identifier: GPL-3.0-or-later
//! Original literal texels, selected by real shared-storage Metal views.
use super::unaligned_copy::Gate;
use xenora_metal::*;
const SHADER: &str = r"#include <metal_stdlib>
using namespace metal; struct V {float4 p [[position]];};
vertex V vs(uint i [[vertex_id]],device const float4*p [[buffer(0)]]){return {p[i]};}
fragment float4 ps(V v [[stage_in]],texture2d<float>t [[texture(0)]],sampler s [[sampler(0)]]){return t.sample(s,v.p.xy/float2(2.,1.));}";
const POS: [[f32; 4]; 3] = [[-1., -1., 0., 1.], [3., -1., 0., 1.], [-1., 3., 0., 1.]];
fn desc(slice: u32, base: u32, count: u32, srgb: bool) -> ArraySliceViewDesc {
    ArraySliceViewDesc {
        slice,
        base_mip: base,
        mip_count: count,
        format: if srgb {
            TextureViewFormat::Rgba8UnormSrgb
        } else {
            TextureViewFormat::Rgba8Unorm
        },
    }
}
fn host_render(gpu: &NativeCompute, view: MetalTextureView, mip: u32) -> PendingRender<'static> {
    let sampler = MetalSampler::new(
        gpu.device(),
        xenora_texture::SamplerDesc::nearest_clamp().with_explicit_mip(mip),
    )
    .unwrap();
    gpu.submit_view_binding_triangle_targets_rgba8(
        &[TriangleShader {
            source: SHADER,
            entries: ["vs", "ps"],
        }],
        &TriangleTargetsPass {
            draws: &[TriangleDraw {
                vertices: &POS,
                colors: &[[0.; 4]; 3],
                viewport: [2, 1],
            }],
            states: &[TriangleState::default()],
            extent: [2, 1],
            clears: &[[0; 4]],
            clear_depth: None,
        },
        &[vec![SampledBinding::View {
            view,
            sampler,
            texture_slot: BindingSlot::new(0),
            sampler_slot: BindingSlot::new(0),
        }]],
        None,
        &[None],
    )
    .unwrap()
}
fn queued_render(gpu: &NativeCompute, c: &mut RenderCohort, view: RenderColorView) -> RenderPoint {
    let sampler =
        MetalSampler::new(gpu.device(), xenora_texture::SamplerDesc::nearest_clamp()).unwrap();
    c.submit_with_attachments(
        RenderQueue::Second,
        &[TriangleShader {
            source: SHADER,
            entries: ["vs", "ps"],
        }],
        &TriangleTargetsPass {
            draws: &[TriangleDraw {
                vertices: &POS,
                colors: &[[0.; 4]; 3],
                viewport: [2, 1],
            }],
            states: &[TriangleState::default()],
            extent: [2, 1],
            clears: &[[0; 4]],
            clear_depth: None,
        },
        &[vec![QueuedSampledBinding::ProducedView {
            view,
            sampler,
            texture_slot: BindingSlot::new(0),
            sampler_slot: BindingSlot::new(0),
        }]],
        None,
        &[QueuedColorLoad::Clear],
        &[],
        &[DrawUniforms::default()],
    )
    .unwrap()
}
#[test]
fn native_array_views_host_slices_mips_srgb_and_owner_drop() {
    let gpu = NativeCompute::new().unwrap();
    for layers in [1, 2] {
        let root0 = [211, 7, 13, 255].repeat(15);
        let root1 = [5, 223, 19, 255].repeat(15);
        let uploads = [
            Rgba8ArrayMipUpload {
                slice: 0,
                level: 0,
                row_pitch: 20,
                bytes: &root0,
            },
            Rgba8ArrayMipUpload {
                slice: 0,
                level: 1,
                row_pitch: 8,
                bytes: &[17, 53, 127, 193, 29, 71, 131, 197],
            },
            Rgba8ArrayMipUpload {
                slice: 0,
                level: 2,
                row_pitch: 4,
                bytes: &[3, 7, 11, 31],
            },
            Rgba8ArrayMipUpload {
                slice: 1,
                level: 0,
                row_pitch: 20,
                bytes: &root1,
            },
            Rgba8ArrayMipUpload {
                slice: 1,
                level: 1,
                row_pitch: 8,
                bytes: &[128, 64, 192, 128, 64, 128, 32, 192],
            },
            Rgba8ArrayMipUpload {
                slice: 1,
                level: 2,
                row_pitch: 4,
                bytes: &[9, 19, 29, 39],
            },
        ];
        let owner = MetalRgba8ArrayTexture::upload(
            gpu.device(),
            Rgba8ArrayTextureDesc {
                width: 5,
                height: 3,
                mips: 3,
                array_layers: layers,
            },
            &uploads[..layers as usize * 3],
        )
        .unwrap();
        let view0 = owner.array_slice_view(desc(0, 1, 2, false)).unwrap();
        assert_eq!(view0.slice(), Some(0));
        assert_eq!(view0.extent(), [2, 1]);
        let selected = owner
            .array_slice_view(desc(layers - 1, 1, 2, false))
            .unwrap();
        let tail = owner
            .array_slice_view(desc(layers - 1, 2, 1, false))
            .unwrap();
        let srgb = if layers == 2 {
            Some(owner.array_slice_view(desc(1, 1, 1, true)).unwrap())
        } else {
            None
        };
        drop(owner);
        assert_eq!(
            host_render(&gpu, view0, 0).wait().unwrap(),
            vec![vec![17, 53, 127, 193, 29, 71, 131, 197]]
        );
        let expected = if layers == 2 {
            vec![128, 64, 192, 128, 64, 128, 32, 192]
        } else {
            vec![17, 53, 127, 193, 29, 71, 131, 197]
        };
        assert_eq!(
            host_render(&gpu, selected.clone(), 0).wait().unwrap(),
            vec![expected]
        );
        let expected_tail = if layers == 2 {
            [9, 19, 29, 39]
        } else {
            [3, 7, 11, 31]
        };
        assert_eq!(
            host_render(&gpu, selected, 1).wait().unwrap(),
            vec![expected_tail.repeat(2)]
        );
        assert_eq!(
            host_render(&gpu, tail, 0).wait().unwrap(),
            vec![expected_tail.repeat(2)]
        );
        if let Some(view) = srgb {
            assert_eq!(
                host_render(&gpu, view, 0).wait().unwrap(),
                vec![vec![55, 13, 134, 128, 13, 55, 4, 192]]
            );
        }
    }
}
#[test]
fn native_array_views_pending_exact_versions_and_descendant_first() {
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    let gate = Gate::new(&gpu, &c, RenderQueue::First);
    let prior = [0x55; 144];
    let pixels = [17, 53, 127, 193, 29, 71, 131, 197];
    let a = c
        .submit_array_image_copy(
            RenderQueue::First,
            QueuedImageCopy {
                source: QueuedCopyInput::HostBuffer(&pixels),
                destination: QueuedCopyInput::HostRgba8Array {
                    extent: [5, 3],
                    mip_levels: 3,
                    array_layers: 2,
                    pixels: &prior,
                },
                source_location: ImageCopyLocation::Buffer {
                    offset: 0,
                    bytes_per_row: 8,
                },
                destination_location: ImageCopyLocation::Rgba8Mip {
                    mip: 1,
                    slice: 1,
                    origin: [0, 0],
                },
                extent: [2, 1],
            },
            &[],
        )
        .unwrap();
    let RenderCopyOutput::Color(old) = c.copy_output(&a).unwrap() else {
        panic!("array")
    };
    let old_view = old.array_slice_view(desc(1, 1, 1, false)).unwrap();
    assert_eq!(old_view.slice(), Some(1));
    let changed = [101, 103, 107, 109, 127, 131, 137, 139];
    let b = c
        .submit_array_image_copy(
            RenderQueue::First,
            QueuedImageCopy {
                source: QueuedCopyInput::HostBuffer(&changed),
                destination: QueuedCopyInput::ProducedColor(old.clone()),
                source_location: ImageCopyLocation::Buffer {
                    offset: 0,
                    bytes_per_row: 8,
                },
                destination_location: ImageCopyLocation::Rgba8Mip {
                    mip: 1,
                    slice: 1,
                    origin: [0, 0],
                },
                extent: [2, 1],
            },
            &[],
        )
        .unwrap();
    let RenderCopyOutput::Color(new) = c.copy_output(&b).unwrap() else {
        panic!("array")
    };
    let new_view = new.array_slice_view(desc(1, 1, 1, false)).unwrap();
    let r_old = queued_render(&gpu, &mut c, old_view);
    let r_new = queued_render(&gpu, &mut c, new_view);
    drop(old);
    drop(new);
    gate.observe_then_release(&gpu, &c, RenderQueue::Second);
    assert_eq!(c.collect(&r_new).unwrap().colors, vec![changed.to_vec()]);
    assert_eq!(c.collect(&r_old).unwrap().colors, vec![pixels.to_vec()]);
    c.collect_copy(&b).unwrap();
    c.collect_copy(&a).unwrap();
}
#[test]
fn native_array_views_invalid_slice_range_format_and_uploads() {
    let gpu = NativeCompute::new().unwrap();
    let root = [0; 60];
    let levels = [
        Rgba8ArrayMipUpload {
            slice: 0,
            level: 0,
            row_pitch: 20,
            bytes: &root,
        },
        Rgba8ArrayMipUpload {
            slice: 0,
            level: 1,
            row_pitch: 8,
            bytes: &[17; 8],
        },
        Rgba8ArrayMipUpload {
            slice: 0,
            level: 2,
            row_pitch: 4,
            bytes: &[31; 4],
        },
    ];
    let d = Rgba8ArrayTextureDesc {
        width: 5,
        height: 3,
        mips: 3,
        array_layers: 1,
    };
    assert!(MetalRgba8ArrayTexture::upload(gpu.device(), d, &levels[..2]).is_err());
    // Complete record count is not enough: duplicates imply a missing mip.
    let duplicate = [
        Rgba8ArrayMipUpload {
            slice: 0,
            level: 0,
            row_pitch: 20,
            bytes: &root,
        },
        Rgba8ArrayMipUpload {
            slice: 0,
            level: 0,
            row_pitch: 20,
            bytes: &root,
        },
        Rgba8ArrayMipUpload {
            slice: 0,
            level: 2,
            row_pitch: 4,
            bytes: &[31; 4],
        },
    ];
    assert!(MetalRgba8ArrayTexture::upload(gpu.device(), d, &duplicate).is_err());
    for (slice, level, pitch, bytes) in [
        (1, 2, 4, &root[..4]),
        (0, 3, 4, &root[..4]),
        (0, 2, 3, &root[..4]),
        (0, 2, 4, &root[..3]),
    ] {
        let bad = [
            Rgba8ArrayMipUpload {
                slice: 0,
                level: 0,
                row_pitch: 20,
                bytes: &root,
            },
            Rgba8ArrayMipUpload {
                slice: 0,
                level: 1,
                row_pitch: 8,
                bytes: &[17; 8],
            },
            Rgba8ArrayMipUpload {
                slice,
                level,
                row_pitch: pitch,
                bytes,
            },
        ];
        assert!(MetalRgba8ArrayTexture::upload(gpu.device(), d, &bad).is_err());
    }
    let owner = MetalRgba8ArrayTexture::upload(gpu.device(), d, &levels).unwrap();
    for bad in [
        desc(1, 0, 1, false),
        desc(0, 3, 1, false),
        desc(0, 1, 3, false),
        desc(0, 0, 0, false),
        ArraySliceViewDesc {
            format: TextureViewFormat::R8Unorm,
            ..desc(0, 0, 1, false)
        },
        ArraySliceViewDesc {
            format: TextureViewFormat::Bc1RgbaUnorm,
            ..desc(0, 0, 1, false)
        },
    ] {
        assert!(owner.array_slice_view(bad).is_err());
    }
    assert_eq!(
        owner
            .array_slice_view(desc(0, 2, 1, false))
            .unwrap()
            .slice(),
        Some(0)
    );
}

#[test]
fn native_array_views_ordinary_output_rejects_array_selection() {
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    let p = c
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostRgba8 {
                extent: [2, 1],
                pixels: &[17, 53, 127, 193, 29, 71, 131, 197],
            },
            &[],
        )
        .unwrap();
    let RenderCopyOutput::Color(output) = c.copy_output(&p).unwrap() else {
        panic!("color")
    };
    assert!(output.array_slice_view(desc(0, 0, 1, false)).is_err());
    assert_eq!(
        output
            .view(TextureViewDesc {
                base_mip: 0,
                mip_count: 1,
                format: TextureViewFormat::Rgba8Unorm
            })
            .unwrap()
            .slice(),
        None
    );
    c.collect_copy(&p).unwrap();
}
