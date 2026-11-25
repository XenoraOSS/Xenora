// SPDX-License-Identifier: GPL-3.0-or-later
//! Original trusted MSL: native uniform transport, not guest branch semantics.
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCommandBuffer, MTLCommandQueue, MTLDevice, MTLEvent, MTLSharedEvent};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use xenora_metal::*;

const SOURCE: &str = r"
#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; float4 color; };
vertex V vs(uint i [[vertex_id]], device const float4 *p [[buffer(0)]], constant uint *u [[buffer(2)]]) {
    return {p[i], u[0] == 0 ? float4(1,0,0,1) : float4(0,1,0,1)};
}
fragment float4 ps(V v [[stage_in]], constant uint *u [[buffer(2)]]) {
    return u[1] == 0 ? v.color : float4(v.color.r, v.color.g, 1, 1);
}
vertex V plain_vs(uint i [[vertex_id]], device const float4 *p [[buffer(0)]]) { return {p[i], float4(1)}; }
fragment float4 plain_ps() { return float4(1); }
";
const POS: [[f32; 4]; 3] = [
    [-1., -1., 0.25, 1.],
    [3., -1., 0.25, 1.],
    [-1., 3., 0.25, 1.],
];
const COLORS: [[f32; 4]; 3] = [[0.; 4]; 3];
fn shader() -> TriangleShader<'static> {
    TriangleShader {
        source: SOURCE,
        entries: ["vs", "ps"],
    }
}
fn uniforms(a: u32, b: u32) -> DrawUniforms {
    DrawUniforms {
        required_mask: 3,
        values: Some([a, b, 0, 0, 0, 0, 0, 0]),
    }
}

#[test]
fn runtime_uniforms_both_stages_a_b_a_owned_after_submit() {
    let gpu = NativeCompute::new().unwrap();
    let event = gpu.device().newSharedEvent().unwrap();
    let rescue = event.clone();
    let timed_out = Arc::new(AtomicBool::new(false));
    let timeout_thread = timed_out.clone();
    let (done, receive) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if receive.recv_timeout(Duration::from_secs(10)).is_err() {
            timeout_thread.store(true, Ordering::SeqCst);
            rescue.setSignaledValue(1);
        }
    });
    let gate = gpu.queue().commandBuffer().unwrap();
    let wait: &ProtocolObject<dyn MTLEvent> = ProtocolObject::from_ref(&*event);
    gate.encodeWaitForEvent_value(wait, 1);
    gate.commit();
    let pending = {
        let draws = [TriangleDraw {
            vertices: &POS,
            colors: &COLORS,
            viewport: [4, 1],
        }; 4];
        let states = std::array::from_fn::<_, 4, _>(|i| TriangleState {
            scissor: Some([i, 0, 1, 1]),
            ..Default::default()
        });
        let pass = TriangleTargetsPass {
            draws: &draws,
            states: &states,
            extent: [4, 1],
            clears: &[[7, 11, 13, 255]],
            clear_depth: None,
        };
        let mut values = [
            uniforms(0, 0),
            DrawUniforms::default(),
            uniforms(1, 1),
            uniforms(0, 0),
        ];
        let shaders = [
            shader(),
            TriangleShader {
                source: SOURCE,
                entries: ["plain_vs", "plain_ps"],
            },
            shader(),
            shader(),
        ];
        let ticket = gpu
            .submit_view_binding_triangle_targets_with_uniforms_rgba8(
                &shaders,
                &pass,
                &[vec![], vec![], vec![], vec![]],
                None,
                &[None],
                &values,
            )
            .unwrap();
        values.fill(uniforms(1, 0)); // Submitted bytes cannot alias this array.
        ticket
    };
    let still_pending = !pending.is_complete().unwrap();
    drop(gpu);
    event.setSignaledValue(1);
    done.send(()).unwrap();
    watchdog.join().unwrap();
    assert!(still_pending && !timed_out.load(Ordering::SeqCst));
    assert_eq!(
        pending.wait().unwrap(),
        vec![vec![
            255, 0, 0, 255, 255, 255, 255, 255, 0, 255, 255, 255, 255, 0, 0, 255
        ]]
    );
}

#[test]
fn runtime_uniforms_missing_late_input_and_shader_failure_preserve_depth() {
    let gpu = NativeCompute::new().unwrap();
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
    let mut depth = gpu.create_depth_target([2, 2]).unwrap();
    gpu.submit_view_binding_triangle_targets_with_uniforms_rgba8(
        &[shader(); 2],
        &pass,
        &[vec![], vec![]],
        Some((&mut depth, DepthLoad::Clear(1.))),
        &[None],
        &[uniforms(0, 0); 2],
    )
    .unwrap()
    .wait()
    .unwrap();
    for values in [
        vec![],
        vec![uniforms(0, 0)],
        vec![
            uniforms(0, 0),
            DrawUniforms {
                required_mask: 128,
                values: None,
            },
        ],
    ] {
        assert!(
            gpu.submit_view_binding_triangle_targets_with_uniforms_rgba8(
                &[shader(); 2],
                &pass,
                &[vec![], vec![]],
                Some((&mut depth, DepthLoad::Clear(1.))),
                &[None],
                &values
            )
            .is_err()
        );
    }
    assert!(
        gpu.submit_view_binding_triangle_targets_with_uniforms_rgba8(
            &[
                shader(),
                TriangleShader {
                    source: "invalid original shader",
                    entries: ["vs", "ps"]
                }
            ],
            &pass,
            &[vec![], vec![]],
            Some((&mut depth, DepthLoad::Clear(1.))),
            &[None],
            &[uniforms(1, 1); 2]
        )
        .is_err()
    );
    // Equal .25 must fail Less if rejected requests did not publish their clear.
    let colors = gpu
        .submit_view_binding_triangle_targets_with_uniforms_rgba8(
            &[shader(); 2],
            &pass,
            &[vec![], vec![]],
            Some((&mut depth, DepthLoad::Load)),
            &[None],
            &[uniforms(1, 1); 2],
        )
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(colors, vec![[7, 11, 13, 255].repeat(4)]);
}

#[test]
fn runtime_uniforms_cohort_owns_snapshot_and_depth() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let point = {
        let draws = [TriangleDraw {
            vertices: &POS,
            colors: &COLORS,
            viewport: [2, 2],
        }];
        let states = [TriangleState {
            depth: DepthState {
                compare: DepthCompare::Less,
                write: true,
            },
            ..Default::default()
        }];
        let pass = TriangleTargetsPass {
            draws: &draws,
            states: &states,
            extent: [2, 2],
            clears: &[[7, 11, 13, 255]],
            clear_depth: None,
        };
        assert!(
            cohort
                .submit_with_uniforms(
                    RenderQueue::First,
                    &[shader()],
                    &pass,
                    &[vec![]],
                    None,
                    &[None],
                    &[],
                    &[]
                )
                .is_err()
        );
        cohort
            .submit_with_uniforms(
                RenderQueue::First,
                &[shader()],
                &pass,
                &[vec![]],
                Some(OwnedDepthInput {
                    snapshot: None,
                    load: DepthLoad::Clear(1.),
                }),
                &[None],
                &[],
                &[uniforms(1, 0)],
            )
            .unwrap()
    };
    drop(gpu);
    let output = cohort.collect(&point).unwrap();
    assert_eq!(output.colors, vec![[0, 255, 0, 255].repeat(4)]);
    assert_eq!(output.depth.unwrap().byte_len(), 16);
}
