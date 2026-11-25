// SPDX-License-Identifier: GPL-3.0-or-later
//! Original trusted shaders and literal expectations, with actual native views.
use xenora_metal::*;
use xenora_texture::SamplerDesc;

const SOURCE: &str = r"
#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; };
vertex V vs(uint i [[vertex_id]], device const float4 *p [[buffer(0)]]) { return {p[i]}; }
fragment float4 sample_view(V v [[stage_in]], texture2d<float> t [[texture(0)]], sampler s [[sampler(0)]]) {
    return t.sample(s, v.position.xy / float2(2.0));
}
fragment float4 dimensions(texture2d<float> t [[texture(0)]]) {
    return float4(float(t.get_width())/4.0, float(t.get_height())/4.0, float(t.get_num_mip_levels())/4.0, 1);
}
";
const POS: [[f32; 4]; 3] = [
    [-1., -1., 0.25, 1.],
    [3., -1., 0.25, 1.],
    [-1., 3., 0.25, 1.],
];
const COLORS: [[f32; 4]; 3] = [[0.; 4]; 3];
fn desc(base: u32, count: u32, format: TextureViewFormat) -> TextureViewDesc {
    TextureViewDesc {
        base_mip: base,
        mip_count: count,
        format,
    }
}
fn submit(
    gpu: &NativeCompute,
    view: MetalTextureView,
    mip: u32,
    fragment: &str,
) -> Result<PendingRender<'static>, MetalError> {
    let sampler = MetalSampler::new(
        gpu.device(),
        SamplerDesc::nearest_clamp().with_explicit_mip(mip),
    )?;
    let draws = [TriangleDraw {
        vertices: &POS,
        colors: &COLORS,
        viewport: [2, 2],
    }];
    let states = [TriangleState::default()];
    let pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent: [2, 2],
        clears: &[[9, 13, 17, 255]],
        clear_depth: None,
    };
    gpu.submit_view_binding_triangle_targets_rgba8(
        &[TriangleShader {
            source: SOURCE,
            entries: ["vs", fragment],
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
#[test]
fn native_views_rebase_mips_dimensions_and_preserve_spatial_pixels() {
    let gpu = NativeCompute::new().unwrap();
    let base = [255, 0, 0, 255].repeat(16);
    let spatial = [
        0, 255, 0, 255, 255, 255, 0, 255, 0, 255, 255, 255, 255, 0, 255, 255,
    ];
    let last = [0, 0, 255, 255];
    let texture = MetalRgba8Texture::upload(
        gpu.device(),
        MetalRgba8TextureDesc::new(4, 4, 3).unwrap(),
        &[
            Rgba8MipLevel::new_packed(0, 4, 4, &base),
            Rgba8MipLevel::new_packed(1, 2, 2, &spatial),
            Rgba8MipLevel::new_packed(2, 1, 1, &last),
        ],
    )
    .unwrap();
    let view = texture
        .view(desc(1, 2, TextureViewFormat::Rgba8Unorm))
        .unwrap();
    assert_eq!(view.extent(), [2, 2]);
    assert_eq!(view.desc().mip_count, 2);
    assert!(submit(&gpu, view.clone(), 2, "sample_view").is_err());
    for invalid in [
        desc(0, 0, TextureViewFormat::Rgba8Unorm),
        desc(2, 2, TextureViewFormat::Rgba8Unorm),
        desc(u32::MAX, 2, TextureViewFormat::Rgba8Unorm),
    ] {
        assert!(texture.view(invalid).is_err());
    }
    assert_eq!(
        submit(&gpu, view.clone(), 0, "dimensions")
            .unwrap()
            .wait()
            .unwrap(),
        vec![[128, 128, 128, 255].repeat(4)]
    );
    assert_eq!(
        submit(&gpu, view.clone(), 1, "sample_view")
            .unwrap()
            .wait()
            .unwrap(),
        vec![last.repeat(4)]
    );
    let pending = submit(&gpu, view, 0, "sample_view").unwrap();
    drop(texture);
    drop(gpu);
    assert_eq!(pending.wait().unwrap(), vec![spatial.to_vec()]);
}
#[test]
fn native_views_linear_srgb_decode_rgb_but_preserve_alpha() {
    let gpu = NativeCompute::new().unwrap();
    let bytes = [128, 64, 192, 128].repeat(4);
    let texture = MetalRgba8Texture::upload(
        gpu.device(),
        MetalRgba8TextureDesc::new(2, 2, 1).unwrap(),
        &[Rgba8MipLevel::new_packed(0, 2, 2, &bytes)],
    )
    .unwrap();
    for (format, expected) in [
        (TextureViewFormat::Rgba8Unorm, [128, 64, 192, 128]),
        (TextureViewFormat::Rgba8UnormSrgb, [55, 13, 134, 128]),
    ] {
        let view = texture.view(desc(0, 1, format)).unwrap();
        assert_eq!(
            submit(&gpu, view, 0, "sample_view")
                .unwrap()
                .wait()
                .unwrap(),
            vec![expected.repeat(4)]
        );
    }
    // Serialized upload changes the shared allocation: an existing view observes it.
    let view = texture
        .view(desc(0, 1, TextureViewFormat::Rgba8Unorm))
        .unwrap();
    let changed = [31, 47, 59, 71].repeat(4);
    texture
        .upload_mip(&Rgba8MipLevel::new_packed(0, 2, 2, &changed))
        .unwrap();
    assert_eq!(
        submit(&gpu, view, 0, "sample_view")
            .unwrap()
            .wait()
            .unwrap(),
        vec![changed]
    );
}

#[test]
fn native_produced_srgb_view_keeps_gate_dependency_and_rejects_stale_generation() {
    use objc2::runtime::ProtocolObject;
    use objc2_metal::{MTLCommandBuffer, MTLCommandQueue, MTLDevice, MTLEvent, MTLSharedEvent};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::Duration;
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let event = gpu.device().newSharedEvent().unwrap();
    let rescue = event.clone();
    let timed_out = Arc::new(AtomicBool::new(false));
    let timeout = timed_out.clone();
    let (done, receiver) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if receiver.recv_timeout(Duration::from_secs(3)).is_err() {
            timeout.store(true, Ordering::SeqCst);
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
        extent: [2, 2],
        clears: &[[128, 64, 192, 128]],
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
            .view(desc(0, 2, TextureViewFormat::Rgba8Unorm))
            .is_err()
    );
    let view = output
        .view(desc(0, 1, TextureViewFormat::Rgba8UnormSrgb))
        .unwrap();
    let sampler = MetalSampler::new(gpu.device(), SamplerDesc::nearest_clamp()).unwrap();
    let bindings = [vec![QueuedSampledBinding::ProducedView {
        view: view.clone(),
        sampler,
        texture_slot: BindingSlot::new(0),
        sampler_slot: BindingSlot::new(0),
    }]];
    let draws = [TriangleDraw {
        vertices: &POS,
        colors: &COLORS,
        viewport: [2, 2],
    }];
    let states = [TriangleState::default()];
    let pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent: [2, 2],
        clears: &[[7, 11, 13, 255]],
        clear_depth: None,
    };
    let shaders = [TriangleShader {
        source: SOURCE,
        entries: ["vs", "sample_view"],
    }];
    let consumer = cohort
        .submit(
            RenderQueue::Second,
            &shaders,
            &pass,
            &bindings,
            None,
            &[None],
            &[],
        )
        .unwrap();
    let pending = !cohort.is_complete(&consumer).unwrap();
    let immediate = !timed_out.load(Ordering::SeqCst);
    drop(output);
    event.setSignaledValue(1);
    done.send(()).unwrap();
    watchdog.join().unwrap();
    assert!(pending && immediate);
    assert_eq!(
        cohort.collect(&consumer).unwrap().colors,
        vec![[55, 13, 134, 128].repeat(4)]
    );
    cohort.collect(&producer).unwrap();
    cohort.abort_and_drain("controlled host rejection").unwrap();
    assert!(
        cohort
            .submit(
                RenderQueue::First,
                &shaders,
                &pass,
                &bindings,
                None,
                &[None],
                &[]
            )
            .is_err()
    );
    cohort.reinitialize().unwrap();
    assert!(
        cohort
            .submit(
                RenderQueue::First,
                &shaders,
                &pass,
                &bindings,
                None,
                &[None],
                &[]
            )
            .is_err()
    );
}

#[test]
fn native_view_late_invalid_relative_lod_preserves_retained_depth() {
    let gpu = NativeCompute::new().unwrap();
    let mut depth = gpu.create_depth_target([2, 2]).unwrap();
    let empty = TriangleTargetsPass {
        draws: &[],
        states: &[],
        extent: [2, 2],
        clears: &[[7, 11, 13, 255]],
        clear_depth: None,
    };
    gpu.render_loaded_triangle_targets_rgba8(
        SOURCE,
        ["vs", "sample_view"],
        &empty,
        None,
        Some((&mut depth, DepthLoad::Clear(0.5))),
        &[None],
    )
    .unwrap();
    let texture =
        MetalRgba8Texture::allocate(gpu.device(), MetalRgba8TextureDesc::new(4, 4, 3).unwrap())
            .unwrap();
    let view = texture
        .view(desc(1, 2, TextureViewFormat::Rgba8Unorm))
        .unwrap();
    let valid = MetalSampler::new(
        gpu.device(),
        SamplerDesc::nearest_clamp().with_explicit_mip(0),
    )
    .unwrap();
    let invalid = MetalSampler::new(
        gpu.device(),
        SamplerDesc::nearest_clamp().with_explicit_mip(2),
    )
    .unwrap();
    let binding = |sampler| {
        vec![SampledBinding::View {
            view: view.clone(),
            sampler,
            texture_slot: BindingSlot::new(0),
            sampler_slot: BindingSlot::new(0),
        }]
    };
    let draws = [TriangleDraw {
        vertices: &POS,
        colors: &COLORS,
        viewport: [2, 2],
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
        extent: [2, 2],
        clears: &[[7, 11, 13, 255]],
        clear_depth: None,
    };
    let shaders = [TriangleShader {
        source: SOURCE,
        entries: ["vs", "sample_view"],
    }; 2];
    assert!(
        gpu.submit_view_binding_triangle_targets_rgba8(
            &shaders,
            &pass,
            &[binding(valid), binding(invalid)],
            Some((&mut depth, DepthLoad::Clear(0.0))),
            &[None]
        )
        .is_err()
    );
    // The failed clear-to-zero would reject .25. Preserved .5 must accept it.
    let red_source = format!("{SOURCE}\nfragment float4 red() {{ return float4(1,0,0,1); }}");
    let probe = TriangleTargetsPass {
        draws: &draws[..1],
        states: &states[..1],
        extent: [2, 2],
        clears: &[[7, 11, 13, 255]],
        clear_depth: None,
    };
    assert_eq!(
        gpu.render_loaded_triangle_targets_rgba8(
            &red_source,
            ["vs", "red"],
            &probe,
            None,
            Some((&mut depth, DepthLoad::Load)),
            &[None]
        )
        .unwrap(),
        vec![[255, 0, 0, 255].repeat(4)]
    );
}

#[test]
fn native_views_odd_nonsquare_chain_rebases_dimensions_and_spatial_sampling() {
    let gpu = NativeCompute::new().unwrap();
    let base = [200, 10, 20, 255].repeat(13 * 9);
    let middle = [30, 210, 50, 255].repeat(6 * 4);
    let spatial = [
        [11, 31, 51, 71],
        [91, 111, 131, 151],
        [171, 191, 211, 231],
        [12, 32, 52, 72],
        [92, 112, 132, 152],
        [172, 192, 212, 232],
    ];
    let pixels: Vec<u8> = spatial.into_iter().flatten().collect();
    let last = [7, 11, 19, 23];
    let texture = MetalRgba8Texture::upload(
        gpu.device(),
        MetalRgba8TextureDesc::new(13, 9, 4).unwrap(),
        &[
            Rgba8MipLevel::new_packed(0, 13, 9, &base),
            Rgba8MipLevel::new_packed(1, 6, 4, &middle),
            Rgba8MipLevel::new_packed(2, 3, 2, &pixels),
            Rgba8MipLevel::new_packed(3, 1, 1, &last),
        ],
    )
    .unwrap();
    let view = texture
        .view(desc(2, 2, TextureViewFormat::Rgba8Unorm))
        .unwrap();
    assert_eq!(view.extent(), [3, 2]);
    assert_eq!(
        submit(&gpu, view.clone(), 0, "dimensions")
            .unwrap()
            .wait()
            .unwrap(),
        vec![[191, 128, 128, 255].repeat(4)]
    );
    assert_eq!(
        submit(&gpu, view.clone(), 1, "sample_view")
            .unwrap()
            .wait()
            .unwrap(),
        vec![last.repeat(4)]
    );
    let pending = submit(&gpu, view, 0, "sample_view").unwrap();
    drop(texture);
    assert_eq!(
        pending.wait().unwrap(),
        vec![
            [
                [11, 31, 51, 71],
                [171, 191, 211, 231],
                [12, 32, 52, 72],
                [172, 192, 212, 232]
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
        ]
    );
}
