// SPDX-License-Identifier: GPL-3.0-or-later
//! Original render-only graph: literal pixels and depth occlusion across five pending nodes.
use super::unaligned_copy::Gate;
use xenora_metal::*;

const SHADER: &str = r"
#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; };
vertex V vs(uint id [[vertex_id]], device const float4 *p [[buffer(0)]]) { return {p[id]}; }
fragment float4 red() { return float4(1,0,0,1); }
fragment float4 green() { return float4(0,1,0,1); }
fragment float4 blue() { return float4(0,0,1,1); }
fragment float4 yellow() { return float4(1,1,0,1); }
fragment float4 sampled(V v [[stage_in]], texture2d<float> t [[texture(0)]], sampler s [[sampler(0)]]) {
    return t.sample(s, v.position.xy / float2(4.0,3.0));
}
";
const COLORS: [[f32; 4]; 3] = [[0.; 4]; 3];
const CLEAR: [u8; 4] = [7, 11, 13, 255];
fn positions(z: f32) -> [[f32; 4]; 3] {
    [[-1., -1., z, 1.], [3., -1., z, 1.], [-1., 3., z, 1.]]
}

#[allow(clippy::too_many_arguments)]
fn submit_descendant(
    gpu: &NativeCompute,
    cohort: &mut RenderCohort,
    queue: RenderQueue,
    fragment: &str,
    z: f32,
    write: bool,
    depth: RenderDepthOutput,
    color: RenderColorOutput,
    sample: bool,
) -> RenderPoint {
    let pos = positions(z);
    let draws = [TriangleDraw {
        vertices: &pos,
        colors: &COLORS,
        viewport: [4, 3],
    }];
    let states = [TriangleState {
        depth: DepthState {
            compare: DepthCompare::Less,
            write,
        },
        ..Default::default()
    }];
    let pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent: [4, 3],
        clears: &[CLEAR],
        clear_depth: None,
    };
    let bindings = if sample {
        vec![QueuedSampledBinding::Produced {
            output: color.clone(),
            sampler: MetalSampler::new(gpu.device(), xenora_texture::SamplerDesc::nearest_clamp())
                .unwrap(),
            texture_slot: BindingSlot::new(0),
            sampler_slot: BindingSlot::new(0),
        }]
    } else {
        vec![]
    };
    cohort
        .submit_with_attachments(
            queue,
            &[TriangleShader {
                source: SHADER,
                entries: ["vs", fragment],
            }],
            &pass,
            &[bindings],
            Some(QueuedDepthInput::Produced(depth)),
            &[if sample {
                QueuedColorLoad::Clear
            } else {
                QueuedColorLoad::Produced(color)
            }],
            &[],
            &[DrawUniforms::default()],
        )
        .unwrap()
}
fn check_planes(output: OwnedRenderOutput, left: [u8; 4], right: [u8; 4]) {
    let row = [left, left, right, right];
    assert_eq!(output.colors, vec![row.as_flattened().repeat(3)]);
    assert_eq!(
        output
            .depth
            .as_ref()
            .expect("owned depth snapshot")
            .byte_len(),
        48
    );
}

#[test]
fn native_render_graph_five_pending_color_depth_versions_collect_descendant_first() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_graph_cohort().unwrap();
    let gate = Gate::new(&gpu, &cohort, RenderQueue::First);
    let left = positions(0.25);
    let right = positions(0.75);
    let draws = [
        TriangleDraw {
            vertices: &left,
            colors: &COLORS,
            viewport: [4, 3],
        },
        TriangleDraw {
            vertices: &right,
            colors: &COLORS,
            viewport: [4, 3],
        },
    ];
    let states = [0, 2].map(|x| TriangleState {
        depth: DepthState {
            compare: DepthCompare::Less,
            write: true,
        },
        scissor: Some([x, 0, 2, 3]),
        ..Default::default()
    });
    let pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent: [4, 3],
        clears: &[CLEAR],
        clear_depth: None,
    };
    let a = cohort
        .submit_with_attachments(
            RenderQueue::First,
            &[
                TriangleShader {
                    source: SHADER,
                    entries: ["vs", "red"],
                },
                TriangleShader {
                    source: SHADER,
                    entries: ["vs", "green"],
                },
            ],
            &pass,
            &[vec![], vec![]],
            Some(QueuedDepthInput::Clear(1.)),
            &[QueuedColorLoad::Clear],
            &[],
            &[DrawUniforms::default(); 2],
        )
        .unwrap();
    let ac = cohort.color_output(&a, 0).unwrap();
    let ad = cohort.depth_output(&a).unwrap();
    // B samples A. .5 passes only on the .75 right plane, without depth writes.
    let b = submit_descendant(
        &gpu,
        &mut cohort,
        RenderQueue::Second,
        "sampled",
        0.5,
        false,
        ad.clone(),
        ac.clone(),
        true,
    );
    let bc = cohort.color_output(&b, 0).unwrap();
    let bd = cohort.depth_output(&b).unwrap();
    // C uses B's produced color AND depth. .625 must still pass on the right:
    // writing .5 in the readonly B pass would incorrectly reject this blue draw.
    let c = submit_descendant(
        &gpu,
        &mut cohort,
        RenderQueue::First,
        "blue",
        0.625,
        false,
        bd,
        bc,
        false,
    );
    let cc = cohort.color_output(&c, 0).unwrap();
    let cd = cohort.depth_output(&c).unwrap();
    // D writes .5 on the right. E's farther .625 red draw must reject there,
    // preserving D's yellow color through another produced-color Load.
    let d = submit_descendant(
        &gpu,
        &mut cohort,
        RenderQueue::Second,
        "yellow",
        0.5,
        true,
        cd,
        cc,
        false,
    );
    let dc = cohort.color_output(&d, 0).unwrap();
    let dd = cohort.depth_output(&d).unwrap();
    let e = submit_descendant(
        &gpu,
        &mut cohort,
        RenderQueue::First,
        "red",
        0.625,
        false,
        dd,
        dc,
        false,
    );
    // Exact identity lookup must still find the oldest submission on queue First.
    assert_eq!(cohort.color_output(&a, 0).unwrap().extent(), [4, 3]);
    assert_eq!(cohort.depth_output(&a).unwrap().extent(), [4, 3]);
    assert!(cohort.collect_copy(&a).is_err());
    assert!(cohort.collect_copy(&e).is_err());
    for point in [&a, &b, &c, &d, &e] {
        assert!(!cohort.is_complete(point).unwrap());
    }
    gate.observe_then_release(&gpu, &cohort, RenderQueue::First);
    check_planes(cohort.collect(&e).unwrap(), CLEAR, [255, 255, 0, 255]);
    check_planes(cohort.collect(&d).unwrap(), CLEAR, [255, 255, 0, 255]);
    check_planes(cohort.collect(&c).unwrap(), CLEAR, [0, 0, 255, 255]);
    check_planes(cohort.collect(&b).unwrap(), CLEAR, [0, 255, 0, 255]);
    check_planes(
        cohort.collect(&a).unwrap(),
        [255, 0, 0, 255],
        [0, 255, 0, 255],
    );
    drop(a);
    drop(b);
    drop(c);
    drop(d);
    drop(e);
    // Original immutable A remains usable after all descendants were collected.
    // A still has .75 on the right: .625 must pass, despite D's later .5 version.
    let old = submit_descendant(
        &gpu,
        &mut cohort,
        RenderQueue::Second,
        "blue",
        0.625,
        false,
        ad,
        ac,
        false,
    );
    check_planes(
        cohort.collect(&old).unwrap(),
        [255, 0, 0, 255],
        [0, 0, 255, 255],
    );
}
