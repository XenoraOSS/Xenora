// SPDX-License-Identifier: GPL-3.0-or-later
//! Original BC blocks sampled persistently by original trusted graphics shaders.
use objc2_metal::MTLDevice;
use xenora_metal::*;
use xenora_texture::{SamplerDesc, TextureFormat};
const SOURCE: &str = r"
#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; };
vertex V vs(uint i [[vertex_id]], device const float4 *p [[buffer(0)]]) { return {p[i]}; }
fragment float4 ps(V v [[stage_in]], texture2d<float> t [[texture(0)]], sampler s [[sampler(0)]]) {
 return t.sample(s, v.position.xy / float2(t.get_width(),t.get_height()));
}
";
const POS: [[f32; 4]; 3] = [
    [-1., -1., 0.25, 1.],
    [3., -1., 0.25, 1.],
    [-1., 3., 0.25, 1.],
];
const COLORS: [[f32; 4]; 3] = [[0.; 4]; 3];
fn view_desc(base_mip: u32, mip_count: u32, format: TextureViewFormat) -> TextureViewDesc {
    TextureViewDesc {
        base_mip,
        mip_count,
        format,
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
    let draws = [TriangleDraw {
        vertices: &POS,
        colors: &COLORS,
        viewport: extent,
    }];
    let states = [TriangleState::default()];
    let pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent,
        clears: &[[7, 11, 13, 255]],
        clear_depth: None,
    };
    gpu.submit_view_binding_triangle_targets_rgba8(
        &[TriangleShader {
            source: SOURCE,
            entries: ["vs", "ps"],
        }],
        &pass,
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
fn block(color: u16) -> [u8; 8] {
    let [lo, hi] = color.to_le_bytes();
    [lo, hi, 0, 0, 0, 0, 0, 0]
}
#[test]
fn native_persistent_bc1_odd_pitched_mips_views_and_owner_drop() {
    let gpu = NativeCompute::new().unwrap();
    assert!(
        gpu.device().supportsBCTextureCompression(),
        "BC hardware required; no decompression fallback"
    );
    let colors = [0xf800, 0x07e0, 0x001f, 0xffff];
    let mut level0 = vec![0xdd; 40 * 3];
    for y in 0..3 {
        for x in 0..4 {
            level0[y * 40 + x * 8..y * 40 + x * 8 + 8].copy_from_slice(&block(colors[(x + y) % 4]));
        }
    }
    let mut level1 = vec![0xdd; 24];
    level1[..8].copy_from_slice(&block(0x07e0));
    level1[8..16].copy_from_slice(&block(0x07e0));
    let mut level2 = vec![0xdd; 16];
    level2[..8].copy_from_slice(&block(0x001f));
    let mut level3 = vec![0xdd; 16];
    level3[..8].copy_from_slice(&block(0xffff));
    let owner = MetalBcTexture::upload(
        gpu.device(),
        BcTextureDesc {
            width: 13,
            height: 9,
            mips: 4,
            format: TextureFormat::Bc1RgbaUnorm,
        },
        &[
            BcMipUpload {
                level: 0,
                row_pitch: 40,
                bytes: &level0,
            },
            BcMipUpload {
                level: 1,
                row_pitch: 24,
                bytes: &level1,
            },
            BcMipUpload {
                level: 2,
                row_pitch: 16,
                bytes: &level2,
            },
            BcMipUpload {
                level: 3,
                row_pitch: 16,
                bytes: &level3,
            },
        ],
    )
    .unwrap();
    assert!(
        owner
            .view(view_desc(0, 1, TextureViewFormat::Bc3RgbaUnorm))
            .is_err()
    );
    assert!(
        owner
            .view(view_desc(0, 1, TextureViewFormat::Rgba8Unorm))
            .is_err()
    );
    assert!(
        owner
            .view(view_desc(3, 2, TextureViewFormat::Bc1RgbaUnorm))
            .is_err()
    );
    let view = owner
        .view(view_desc(1, 3, TextureViewFormat::Bc1RgbaUnorm))
        .unwrap();
    assert_eq!(view.extent(), [6, 4]);
    assert!(submit(&gpu, view.clone(), 3).is_err());
    for (mip, color) in [
        (0, [0, 255, 0, 255]),
        (1, [0, 0, 255, 255]),
        (2, [255, 255, 255, 255]),
    ] {
        assert_eq!(
            submit(&gpu, view.clone(), mip).unwrap().wait().unwrap(),
            vec![color.repeat(24)]
        );
    }
    let full = owner
        .view(view_desc(0, 4, TextureViewFormat::Bc1RgbaUnorm))
        .unwrap();
    let pending = submit(&gpu, full, 0).unwrap();
    drop(view);
    drop(owner);
    drop(gpu);
    let result = pending.wait().unwrap();
    let palette = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 255, 255],
    ];
    for (i, pixel) in result[0].as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(
            *pixel,
            palette[((i % 13) / 4 + (i / 13) / 4) % 4],
            "pixel {i}"
        );
    }
}
#[test]
fn native_persistent_bc_transparency_alpha_orders_and_srgb() {
    let gpu = NativeCompute::new().unwrap();
    assert!(gpu.device().supportsBCTextureCompression());
    let transparent = [0, 0, 255, 255, 255, 255, 255, 255];
    let bc1 = MetalBcTexture::upload(
        gpu.device(),
        BcTextureDesc {
            width: 1,
            height: 1,
            mips: 1,
            format: TextureFormat::Bc1RgbaUnorm,
        },
        &[BcMipUpload {
            level: 0,
            row_pitch: 8,
            bytes: &transparent,
        }],
    )
    .unwrap();
    assert_eq!(
        submit(
            &gpu,
            bc1.view(view_desc(0, 1, TextureViewFormat::Bc1RgbaUnorm))
                .unwrap(),
            0
        )
        .unwrap()
        .wait()
        .unwrap(),
        vec![vec![0, 0, 0, 0]]
    );
    for (endpoints, expected) in [
        ([240, 30], [240, 30, 210, 180, 150, 120, 90, 60]),
        ([0, 250], [0, 250, 50, 100, 150, 200, 0, 255]),
    ] {
        let mut bytes = [0u8; 16];
        bytes[..2].copy_from_slice(&endpoints);
        let mut selectors = 0u64;
        for i in 0..16 {
            selectors |= ((i % 8) as u64) << (i * 3);
        }
        bytes[2..8].copy_from_slice(&selectors.to_le_bytes()[..6]);
        bytes[8..].copy_from_slice(&block(0xf800));
        let bc3 = MetalBcTexture::upload(
            gpu.device(),
            BcTextureDesc {
                width: 4,
                height: 4,
                mips: 1,
                format: TextureFormat::Bc3RgbaUnorm,
            },
            &[BcMipUpload {
                level: 0,
                row_pitch: 16,
                bytes: &bytes,
            }],
        )
        .unwrap();
        let pixels = submit(
            &gpu,
            bc3.view(view_desc(0, 1, TextureViewFormat::Bc3RgbaUnorm))
                .unwrap(),
            0,
        )
        .unwrap()
        .wait()
        .unwrap();
        for (i, pixel) in pixels[0].as_chunks::<4>().0.iter().enumerate() {
            let selector = i % 8;
            assert_eq!(pixel[..3], [255, 0, 0]);
            if selector < 2 || (endpoints[0] <= endpoints[1] && selector >= 6) {
                // Nonextreme endpoint exactness is a stronger local regression
                // expectation; decoded 0/1 must be exact under the reference spec.
                assert_eq!(pixel[3], expected[selector]);
            } else {
                // D3D11.3 §§19.5.2,19.5.8 and3.2.3.6: BC3 alpha error in
                // byte units is strictly <1 + .03*endpoint_distance + .6.
                // For these integral reference palettes that means <=7 or <=9.
                // This is a spec-derived compatibility envelope, not a Metal
                // precision guarantee or an observed-value-specific tolerance.
                // https://microsoft.github.io/DirectX-Specs/d3d/archive/D3D11_3_FunctionalSpec.htm
                let bound = if endpoints[0] > endpoints[1] { 7 } else { 9 };
                assert!(
                    pixel[3].abs_diff(expected[selector]) <= bound,
                    "alpha selector{selector}: got {}, reference {}, bound{bound}",
                    pixel[3],
                    expected[selector]
                );
            }
        }
    }
    // White/black color endpoints, selector 3 => 85 in four-color mode.
    // The independently evaluated sRGB transfer yields 23.1646/255 => byte 23.
    let grey = [255, 255, 0, 0, 255, 255, 255, 255];
    for format in [TextureFormat::Bc1RgbaUnorm, TextureFormat::Bc3RgbaUnorm] {
        let mut bytes = Vec::new();
        if format == TextureFormat::Bc3RgbaUnorm {
            bytes.extend_from_slice(&[128, 0, 0, 0, 0, 0, 0, 0]);
        }
        bytes.extend_from_slice(&grey);
        let owner = MetalBcTexture::upload(
            gpu.device(),
            BcTextureDesc {
                width: 4,
                height: 4,
                mips: 1,
                format,
            },
            &[BcMipUpload {
                level: 0,
                row_pitch: bytes.len(),
                bytes: &bytes,
            }],
        )
        .unwrap();
        let (linear, srgb, alpha) = if format == TextureFormat::Bc1RgbaUnorm {
            (
                TextureViewFormat::Bc1RgbaUnorm,
                TextureViewFormat::Bc1RgbaUnormSrgb,
                255,
            )
        } else {
            (
                TextureViewFormat::Bc3RgbaUnorm,
                TextureViewFormat::Bc3RgbaUnormSrgb,
                128,
            )
        };
        // Reference RGB85/23 is unchanged. Compose the primary-spec BC error,
        // optional linearization-input precision and final UNORM conversion:
        // linear:85 +/- (1 + .03*255 + .6) => integer76..94;
        // sRGB:decode((85 +/- 10.15)/255)*255 +/- .6 => integer18..29.
        // Independently reviewed; these disjoint envelopes still catch a lost
        // sRGB interpretation. They do not assert a Metal precision guarantee.
        for (view_format, reference, range) in [(linear, 85, 76..=94), (srgb, 23, 18..=29)] {
            let pixels = submit(&gpu, owner.view(view_desc(0, 1, view_format)).unwrap(), 0)
                .unwrap()
                .wait()
                .unwrap();
            assert_eq!(pixels.len(), 1);
            assert_eq!(pixels[0].len(), 64);
            for pixel in pixels[0].as_chunks::<4>().0 {
                assert_eq!(pixel[3], alpha);
                for value in &pixel[..3] {
                    assert!(
                        range.contains(value),
                        "got {value}, reference{reference}, accepted{range:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn native_queued_bc_view_and_produced_rgba_keep_real_dependency() {
    use objc2::runtime::ProtocolObject;
    use objc2_metal::{MTLCommandBuffer, MTLCommandQueue, MTLEvent, MTLSharedEvent};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::Duration;
    let gpu = NativeCompute::new().unwrap();
    assert!(gpu.device().supportsBCTextureCompression());
    let mut cohort = gpu.new_render_cohort().unwrap();
    let compressed = block(0x07e0);
    let owner = MetalBcTexture::upload(
        gpu.device(),
        BcTextureDesc {
            width: 4,
            height: 4,
            mips: 1,
            format: TextureFormat::Bc1RgbaUnorm,
        },
        &[BcMipUpload {
            level: 0,
            row_pitch: 8,
            bytes: &compressed,
        }],
    )
    .unwrap();
    let view = owner
        .view(view_desc(0, 1, TextureViewFormat::Bc1RgbaUnorm))
        .unwrap();
    let event = gpu.device().newSharedEvent().unwrap();
    let rescue = event.clone();
    let timeout = Arc::new(AtomicBool::new(false));
    let thread_timeout = timeout.clone();
    let (done, receiver) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if receiver.recv_timeout(Duration::from_secs(3)).is_err() {
            thread_timeout.store(true, Ordering::SeqCst);
            rescue.setSignaledValue(1);
        }
    });
    let gate = cohort.queue(RenderQueue::First).commandBuffer().unwrap();
    let wait: &ProtocolObject<dyn MTLEvent> = ProtocolObject::from_ref(&*event);
    gate.encodeWaitForEvent_value(wait, 1);
    gate.commit();
    let producer_pass = TriangleTargetsPass {
        draws: &[],
        states: &[],
        extent: [4, 4],
        clears: &[[200, 40, 60, 255]],
        clear_depth: None,
    };
    let producer = cohort
        .submit(
            RenderQueue::First,
            &[],
            &producer_pass,
            &[],
            None,
            &[None],
            &[],
        )
        .unwrap();
    let output = cohort.color_output(&producer, 0).unwrap();
    assert!(
        output
            .view(view_desc(0, 1, TextureViewFormat::Bc1RgbaUnorm))
            .is_err()
    );
    let sampler = MetalSampler::new(gpu.device(), SamplerDesc::nearest_clamp()).unwrap();
    let pairs = [vec![
        QueuedSampledBinding::Viewed {
            view,
            sampler: sampler.clone(),
            texture_slot: BindingSlot::new(0),
            sampler_slot: BindingSlot::new(0),
        },
        QueuedSampledBinding::Produced {
            output,
            sampler,
            texture_slot: BindingSlot::new(1),
            sampler_slot: BindingSlot::new(0),
        },
    ]];
    let source = format!(
        "{SOURCE}\nfragment float4 mixed(texture2d<float> a [[texture(0)]],texture2d<float> b [[texture(1)]],sampler s [[sampler(0)]]) {{ float4 x=a.sample(s,float2(.5)); float4 y=b.sample(s,float2(.5)); return float4(y.r,x.g,y.b,1); }}"
    );
    let draws = [TriangleDraw {
        vertices: &POS,
        colors: &COLORS,
        viewport: [4, 4],
    }];
    let states = [TriangleState::default()];
    let pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent: [4, 4],
        clears: &[[7, 11, 13, 255]],
        clear_depth: None,
    };
    let consumer = cohort
        .submit(
            RenderQueue::Second,
            &[TriangleShader {
                source: &source,
                entries: ["vs", "mixed"],
            }],
            &pass,
            &pairs,
            None,
            &[None],
            &[],
        )
        .unwrap();
    drop(pairs);
    drop(owner);
    drop(gpu);
    let pending = !cohort.is_complete(&consumer).unwrap();
    let immediate = !timeout.load(Ordering::SeqCst);
    event.setSignaledValue(1);
    done.send(()).unwrap();
    watchdog.join().unwrap();
    assert!(pending && immediate);
    assert_eq!(
        cohort.collect(&consumer).unwrap().colors,
        vec![[200, 255, 60, 255].repeat(16)]
    );
    cohort.collect(&producer).unwrap();
}

#[test]
fn native_bc_late_binding_failure_preserves_depth() {
    let gpu = NativeCompute::new().unwrap();
    assert!(gpu.device().supportsBCTextureCompression());
    let mut depth = gpu.create_depth_target([4, 4]).unwrap();
    let empty = TriangleTargetsPass {
        draws: &[],
        states: &[],
        extent: [4, 4],
        clears: &[[7, 11, 13, 255]],
        clear_depth: None,
    };
    gpu.render_loaded_triangle_targets_rgba8(
        SOURCE,
        ["vs", "ps"],
        &empty,
        None,
        Some((&mut depth, DepthLoad::Clear(0.5))),
        &[None],
    )
    .unwrap();
    let bytes = block(0xf800);
    let owner = MetalBcTexture::upload(
        gpu.device(),
        BcTextureDesc {
            width: 4,
            height: 4,
            mips: 1,
            format: TextureFormat::Bc1RgbaUnorm,
        },
        &[BcMipUpload {
            level: 0,
            row_pitch: 8,
            bytes: &bytes,
        }],
    )
    .unwrap();
    let view = owner
        .view(view_desc(0, 1, TextureViewFormat::Bc1RgbaUnorm))
        .unwrap();
    let pairs = |mip| {
        vec![SampledBinding::View {
            view: view.clone(),
            sampler: MetalSampler::new(
                gpu.device(),
                SamplerDesc::nearest_clamp().with_explicit_mip(mip),
            )
            .unwrap(),
            texture_slot: BindingSlot::new(0),
            sampler_slot: BindingSlot::new(0),
        }]
    };
    let draws = [TriangleDraw {
        vertices: &POS,
        colors: &COLORS,
        viewport: [4, 4],
    }; 2];
    let states = [TriangleState {
        depth: DepthState {
            compare: DepthCompare::Less,
            write: true,
        },
        ..Default::default()
    }; 2];
    let pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent: [4, 4],
        clears: &[[7, 11, 13, 255]],
        clear_depth: None,
    };
    let shaders = [TriangleShader {
        source: SOURCE,
        entries: ["vs", "ps"],
    }; 2];
    assert!(
        gpu.submit_view_binding_triangle_targets_rgba8(
            &shaders,
            &pass,
            &[pairs(0), pairs(1)],
            Some((&mut depth, DepthLoad::Clear(0.0))),
            &[None]
        )
        .is_err()
    );
    let probe = TriangleTargetsPass {
        draws: &draws[..1],
        states: &states[..1],
        extent: [4, 4],
        clears: &[[7, 11, 13, 255]],
        clear_depth: None,
    };
    assert_eq!(
        gpu.submit_view_binding_triangle_targets_rgba8(
            &shaders[..1],
            &probe,
            &[pairs(0)],
            Some((&mut depth, DepthLoad::Load)),
            &[None]
        )
        .unwrap()
        .wait()
        .unwrap(),
        vec![[255, 0, 0, 255].repeat(16)]
    );
}
