// SPDX-License-Identifier: GPL-3.0-or-later
//! Original single-channel texels; expected RGBA bytes never use format helpers.
use xenora_metal::*;
use xenora_texture::SamplerDesc;
const SOURCE: &str = r"
#include <metal_stdlib>
using namespace metal;
struct V { float4 p [[position]]; };
vertex V vs(uint i [[vertex_id]], device const float4 *p [[buffer(0)]]) { return {p[i]}; }
fragment float4 ps(V v [[stage_in]], texture2d<float> t [[texture(0)]], sampler s [[sampler(0)]]) {
 return t.sample(s,v.p.xy/float2(t.get_width(),t.get_height()));
}
";
const POS: [[f32; 4]; 3] = [[-1., -1., 0., 1.], [3., -1., 0., 1.], [-1., 3., 0., 1.]];
fn desc(base_mip: u32, mip_count: u32) -> TextureViewDesc {
    TextureViewDesc {
        base_mip,
        mip_count,
        format: TextureViewFormat::R8Unorm,
    }
}
fn submit(
    gpu: &NativeCompute,
    view: MetalTextureView,
    mip: u32,
) -> Result<PendingRender<'static>, MetalError> {
    let extent = view.extent();
    let sampler = MetalSampler::new(
        gpu.device(),
        SamplerDesc::nearest_clamp().with_explicit_mip(mip),
    )?;
    gpu.submit_view_binding_triangle_targets_rgba8(
        &[TriangleShader {
            source: SOURCE,
            entries: ["vs", "ps"],
        }],
        &TriangleTargetsPass {
            draws: &[TriangleDraw {
                vertices: &POS,
                colors: &[[0.; 4]; 3],
                viewport: extent,
            }],
            states: &[TriangleState::default()],
            extent,
            clears: &[[7, 11, 13, 19]],
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
}
fn expected(red: &[u8]) -> Vec<Vec<u8>> {
    vec![red.iter().flat_map(|r| [*r, 0, 0, 255]).collect()]
}
fn original_texture(gpu: &NativeCompute) -> MetalR8Texture {
    let rows = [
        0, 17, 128, 255, 71, 0xdd, 0xdd, 0xdd, 89, 107, 127, 149, 173, 0xdd, 0xdd, 0xdd, 191, 211,
        229, 241, 253, 0xdd, 0xdd, 0xdd,
    ];
    MetalR8Texture::upload(
        gpu.device(),
        R8TextureDesc {
            width: 5,
            height: 3,
            mips: 3,
        },
        &[
            R8MipUpload {
                level: 0,
                row_pitch: 8,
                bytes: &rows,
            },
            R8MipUpload {
                level: 1,
                row_pitch: 5,
                bytes: &[43, 197, 0xdd, 0xdd, 0xdd],
            },
            R8MipUpload {
                level: 2,
                row_pitch: 3,
                bytes: &[101, 0xdd, 0xdd],
            },
        ],
    )
    .unwrap()
}
#[test]
fn native_r8_odd_nonsquare_padded_rows_and_missing_components() {
    let gpu = NativeCompute::new().unwrap();
    let texture = original_texture(&gpu);
    let output = submit(&gpu, texture.view(desc(0, 3)).unwrap(), 0)
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(
        output,
        expected(&[
            0, 17, 128, 255, 71, 89, 107, 127, 149, 173, 191, 211, 229, 241, 253
        ])
    );
    assert_eq!(
        submit(&gpu, texture.view(desc(1, 2)).unwrap(), 0)
            .unwrap()
            .wait()
            .unwrap(),
        expected(&[43, 197])
    );
    assert_eq!(
        submit(&gpu, texture.view(desc(2, 1)).unwrap(), 0)
            .unwrap()
            .wait()
            .unwrap(),
        expected(&[101])
    );
}
#[test]
fn native_r8_relative_lod_distinct_storage_and_owner_retirement() {
    let gpu = NativeCompute::new().unwrap();
    let first = original_texture(&gpu);
    let second = MetalR8Texture::upload(
        gpu.device(),
        R8TextureDesc {
            width: 5,
            height: 3,
            mips: 1,
        },
        &[R8MipUpload {
            level: 0,
            row_pitch: 5,
            bytes: &[7, 13, 19, 29, 31, 41, 47, 59, 61, 73, 79, 83, 97, 103, 109],
        }],
    )
    .unwrap();
    let relative = first.view(desc(1, 2)).unwrap();
    assert_eq!(relative.extent(), [2, 1]);
    let a = submit(&gpu, relative, 1).unwrap();
    let b = submit(&gpu, second.view(desc(0, 1)).unwrap(), 0).unwrap();
    let c = submit(&gpu, first.view(desc(0, 3)).unwrap(), 1).unwrap();
    drop((first, second));
    assert_eq!(
        b.wait().unwrap(),
        expected(&[7, 13, 19, 29, 31, 41, 47, 59, 61, 73, 79, 83, 97, 103, 109])
    );
    assert_eq!(a.wait().unwrap(), expected(&[101, 101]));
    assert_eq!(
        c.wait().unwrap(),
        expected(&[
            43, 43, 197, 197, 197, 43, 43, 197, 197, 197, 43, 43, 197, 197, 197
        ])
    );
}
#[test]
fn native_r8_rejects_malformed_full_spans_mips_and_view_formats() {
    let gpu = NativeCompute::new().unwrap();
    // Xenora requires full row_pitch*height span including final padding;
    // this is stricter than the Metal minimum, with no256-byte alignment rule.
    for (pitch, bytes) in [
        (4, vec![1; 12]),
        (8, vec![1; 23]),
        (8, vec![1; 24 + 1]),
        (usize::MAX, vec![1; 15]),
    ] {
        assert!(
            MetalR8Texture::upload(
                gpu.device(),
                R8TextureDesc {
                    width: 5,
                    height: 3,
                    mips: 1
                },
                &[R8MipUpload {
                    level: 0,
                    row_pitch: pitch,
                    bytes: &bytes
                }]
            )
            .is_err()
        );
    }
    assert!(
        MetalR8Texture::upload(
            gpu.device(),
            R8TextureDesc {
                width: 5,
                height: 3,
                mips: 2
            },
            &[R8MipUpload {
                level: 0,
                row_pitch: 5,
                bytes: &[1; 15]
            }]
        )
        .is_err()
    );
    assert!(
        MetalR8Texture::upload(
            gpu.device(),
            R8TextureDesc {
                width: 5,
                height: 3,
                mips: 2
            },
            &[
                R8MipUpload {
                    level: 0,
                    row_pitch: 5,
                    bytes: &[1; 15]
                },
                R8MipUpload {
                    level: 0,
                    row_pitch: 2,
                    bytes: &[2; 2]
                }
            ]
        )
        .is_err()
    );
    assert!(
        MetalR8Texture::upload(
            gpu.device(),
            R8TextureDesc {
                width: 0,
                height: 3,
                mips: 1
            },
            &[]
        )
        .is_err()
    );
    let texture = original_texture(&gpu);
    for format in [
        TextureViewFormat::Rgba8Unorm,
        TextureViewFormat::Rgba8UnormSrgb,
        TextureViewFormat::Bc1RgbaUnorm,
    ] {
        assert!(
            texture
                .view(TextureViewDesc {
                    base_mip: 0,
                    mip_count: 1,
                    format
                })
                .is_err()
        );
    }
    assert!(texture.view(desc(2, 2)).is_err());
    assert!(texture.view(desc(0, 0)).is_err());
    assert!(submit(&gpu, texture.view(desc(1, 1)).unwrap(), 1).is_err());
}
