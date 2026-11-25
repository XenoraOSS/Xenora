// SPDX-License-Identifier: GPL-3.0-or-later
//! Original trusted shaders; cancellation is host validation failure, not a GPU fault.
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCommandBuffer, MTLCommandQueue, MTLDevice, MTLEvent, MTLSharedEvent};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use xenora_metal::*;
use xenora_texture::SamplerDesc;

const SOURCE: &str = r"
#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; };
vertex V vs(uint id [[vertex_id]], device const float4 *p [[buffer(0)]]) { return {p[id]}; }
fragment float4 sample_color(V in [[stage_in]], texture2d<float> t [[texture(0)]], sampler s [[sampler(0)]]) {
    return t.sample(s, float2(0.5));
}
fragment float4 red() { return float4(1,0,0,1); }
";
const POS: [[f32; 4]; 3] = [
    [-1., -1., 0.25, 1.],
    [3., -1., 0.25, 1.],
    [-1., 3., 0.25, 1.],
];
const COLORS: [[f32; 4]; 3] = [[0.; 4]; 3];
fn clear_pass(clears: &[[u8; 4]]) -> TriangleTargetsPass<'_> {
    TriangleTargetsPass {
        draws: &[],
        states: &[],
        extent: [4, 3],
        clears,
        clear_depth: None,
    }
}

#[test]
fn cohort_commits_both_queues_before_gate_and_samples_original_surface() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let sampler = MetalSampler::new(gpu.device(), SamplerDesc::nearest_clamp()).unwrap();
    for expected in [[17, 83, 149, 255], [211, 43, 97, 255]] {
        let event = gpu.device().newSharedEvent().unwrap();
        let rescue = event.clone();
        let timeout = Arc::new(AtomicBool::new(false));
        let timeout_thread = timeout.clone();
        let (done, receive) = mpsc::channel();
        let watchdog = std::thread::spawn(move || {
            if receive.recv_timeout(Duration::from_secs(3)).is_err() {
                timeout_thread.store(true, Ordering::SeqCst);
                rescue.setSignaledValue(1);
            }
        });
        let gate = cohort.queue(RenderQueue::First).commandBuffer().unwrap();
        let wait: &ProtocolObject<dyn MTLEvent> = ProtocolObject::from_ref(&*event);
        gate.encodeWaitForEvent_value(wait, 1);
        gate.commit();
        let producer = cohort
            .submit(
                RenderQueue::First,
                &[],
                &clear_pass(&[expected]),
                &[],
                None,
                &[None],
                &[],
            )
            .unwrap();
        let output = cohort.color_output(&producer, 0).unwrap();
        assert_eq!(output.extent(), [4, 3]);
        let bindings = [vec![QueuedSampledBinding::Produced {
            output,
            sampler: sampler.clone(),
            texture_slot: BindingSlot::new(0),
            sampler_slot: BindingSlot::new(0),
        }]];
        let draws = [TriangleDraw {
            vertices: &POS,
            colors: &COLORS,
            viewport: [4, 3],
        }];
        let states = [TriangleState::default()];
        let pass = TriangleTargetsPass {
            draws: &draws,
            states: &states,
            extent: [4, 3],
            clears: &[[1, 2, 3, 255]],
            clear_depth: None,
        };
        let shaders = [TriangleShader {
            source: SOURCE,
            entries: ["vs", "sample_color"],
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
        drop(bindings);
        // A native signal behind the consumer observes real GPU queue progress,
        // independently of is_complete's ancestor-status bookkeeping.
        let observed = gpu.device().newSharedEvent().unwrap();
        let probe = cohort.queue(RenderQueue::Second).commandBuffer().unwrap();
        let probe_event: &ProtocolObject<dyn MTLEvent> = ProtocolObject::from_ref(&*observed);
        probe.encodeSignalEvent_value(probe_event, 1);
        probe.commit();
        let deadline = std::time::Instant::now() + Duration::from_millis(50);
        while observed.signaledValue() == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let consumer_ran_before_producer = observed.signaledValue() != 0;
        let producer_pending = !cohort.is_complete(&producer).unwrap();
        let consumer_pending = !cohort.is_complete(&consumer).unwrap();
        let committed_before_watchdog = !timeout.load(Ordering::SeqCst);
        event.setSignaledValue(1);
        done.send(()).unwrap();
        watchdog.join().unwrap();
        assert!(committed_before_watchdog && producer_pending && consumer_pending);
        assert!(
            !consumer_ran_before_producer,
            "consumer GPU queue bypassed its producer wait"
        );
        // Exercise reverse collection and cached producer success after collection.
        if expected[0] == 211 {
            assert_eq!(
                cohort.collect(&producer).unwrap().colors,
                vec![expected.repeat(12)]
            );
        }
        let result = cohort.collect(&consumer).unwrap();
        assert_eq!(result.colors, vec![expected.repeat(12)]);
        assert!(result.depth.is_none());
        if expected[0] != 211 {
            assert_eq!(
                cohort.collect(&producer).unwrap().colors,
                vec![expected.repeat(12)]
            );
        }
    }
}

#[test]
fn cohort_setup_rejection_cancel_generation_and_owned_depth() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let clear = [[31, 47, 59, 255]];
    let pass = clear_pass(&clear);
    let point = cohort
        .submit(
            RenderQueue::First,
            &[],
            &pass,
            &[],
            Some(OwnedDepthInput {
                snapshot: None,
                load: DepthLoad::Clear(0.5),
            }),
            &[None],
            &[],
        )
        .unwrap();
    let stale_output = cohort.color_output(&point, 0).unwrap();
    assert!(
        cohort
            .submit(RenderQueue::First, &[], &pass, &[], None, &[None], &[])
            .is_err()
    );
    assert!(cohort.color_output(&point, 1).is_err());
    // Invalid later setup neither consumes its slot nor poisons the producer.
    assert!(
        cohort
            .submit(RenderQueue::Second, &[], &pass, &[], None, &[], &[])
            .is_err()
    );
    let first = cohort.collect(&point).unwrap();
    let snapshot = first.depth.unwrap();
    assert_eq!(snapshot.byte_len(), 4 * 3 * 4);
    let draws = [TriangleDraw {
        vertices: &POS,
        colors: &COLORS,
        viewport: [4, 3],
    }];
    let shaders = [TriangleShader {
        source: SOURCE,
        entries: ["vs", "red"],
    }];
    let states = [TriangleState {
        depth: DepthState {
            compare: DepthCompare::Less,
            write: true,
        },
        ..Default::default()
    }];
    let draw_pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent: [4, 3],
        clears: &clear,
        clear_depth: None,
    };
    let p = cohort
        .submit(
            RenderQueue::Second,
            &shaders,
            &draw_pass,
            &[vec![]],
            Some(OwnedDepthInput {
                snapshot: Some(snapshot),
                load: DepthLoad::Load,
            }),
            &[None],
            &[],
        )
        .unwrap();
    let result = cohort.collect(&p).unwrap();
    assert_eq!(result.colors, vec![[255, 0, 0, 255].repeat(12)]);
    let persisted = result.depth.unwrap();
    let p = cohort
        .submit(
            RenderQueue::First,
            &shaders,
            &draw_pass,
            &[vec![]],
            Some(OwnedDepthInput {
                snapshot: Some(persisted),
                load: DepthLoad::Load,
            }),
            &[None],
            &[],
        )
        .unwrap();
    assert_eq!(
        cohort.collect(&p).unwrap().colors,
        vec![clear[0].repeat(12)]
    );
    // Explicit host rejection after terminal producer work, not an injected GPU fault.
    cohort
        .abort_and_drain("controlled host publication rejection")
        .unwrap();
    assert!(cohort.is_complete(&point).is_err());
    assert!(
        cohort
            .submit(RenderQueue::First, &[], &pass, &[], None, &[None], &[])
            .is_err()
    );
    cohort.reinitialize().unwrap();
    let sampler = MetalSampler::new(gpu.device(), SamplerDesc::nearest_clamp()).unwrap();
    let bindings = [vec![QueuedSampledBinding::Produced {
        output: stale_output,
        sampler,
        texture_slot: BindingSlot::new(0),
        sampler_slot: BindingSlot::new(0),
    }]];
    assert!(
        cohort
            .submit(
                RenderQueue::First,
                &shaders,
                &draw_pass,
                &bindings,
                None,
                &[None],
                &[]
            )
            .is_err()
    );
    assert!(
        cohort
            .submit(RenderQueue::First, &[], &pass, &[], None, &[None], &[point])
            .is_err()
    );
    let mut transient = clear_pass(&clear);
    transient.clear_depth = Some(0.25);
    let p = cohort
        .submit(RenderQueue::First, &[], &transient, &[], None, &[None], &[])
        .unwrap();
    assert!(cohort.collect(&p).unwrap().depth.is_none());
}

#[test]
fn cohort_produced_attachment_load_is_ordered_fresh_and_mixed() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
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
    let gate = cohort.queue(RenderQueue::First).commandBuffer().unwrap();
    let wait: &ProtocolObject<dyn MTLEvent> = ProtocolObject::from_ref(&*event);
    gate.encodeWaitForEvent_value(wait, 1);
    gate.commit();
    let producer = cohort
        .submit(
            RenderQueue::First,
            &[],
            &clear_pass(&[[17, 83, 149, 255]]),
            &[],
            None,
            &[None],
            &[],
        )
        .unwrap();
    let source = cohort.color_output(&producer, 0).unwrap();
    let draws = [TriangleDraw {
        vertices: &POS,
        colors: &COLORS,
        viewport: [4, 3],
    }];
    let states = [TriangleState {
        scissor: Some([1, 1, 2, 1]),
        ..Default::default()
    }];
    let shader = r"
#include <metal_stdlib>
using namespace metal;
struct V { float4 p [[position]]; };
struct O { float4 a [[color(0)]]; float4 b [[color(1)]]; float4 c [[color(2)]]; };
vertex V vs(uint i [[vertex_id]], device const float4 *p [[buffer(0)]]) { return {p[i]}; }
fragment O ps() { return {float4(1,0,0,1),float4(0,1,0,1),float4(0,0,1,1)}; }
";
    let pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent: [4, 3],
        clears: &[[1, 2, 3, 255], [7, 11, 13, 255], [19, 23, 29, 255]],
        clear_depth: None,
    };
    let host = [31, 37, 41, 255].repeat(12);
    // A malformed final slot must reject before reserving/committing queue two.
    assert!(
        cohort
            .submit_with_color_loads(
                RenderQueue::Second,
                &[TriangleShader {
                    source: shader,
                    entries: ["vs", "ps"]
                }],
                &pass,
                &[vec![]],
                None,
                &[
                    QueuedColorLoad::Produced(source.clone()),
                    QueuedColorLoad::Clear,
                    QueuedColorLoad::Host(&[0])
                ],
                &[],
                &[DrawUniforms::default()]
            )
            .is_err()
    );
    let consumer = cohort
        .submit_with_color_loads(
            RenderQueue::Second,
            &[TriangleShader {
                source: shader,
                entries: ["vs", "ps"],
            }],
            &pass,
            &[vec![]],
            None,
            &[
                QueuedColorLoad::Produced(source.clone()),
                QueuedColorLoad::Clear,
                QueuedColorLoad::Host(&host),
            ],
            &[],
            &[DrawUniforms::default()],
        )
        .unwrap();
    let completed = gpu.device().newSharedEvent().unwrap();
    let marker = cohort.queue(RenderQueue::Second).commandBuffer().unwrap();
    let signal: &ProtocolObject<dyn MTLEvent> = ProtocolObject::from_ref(&*completed);
    marker.encodeSignalEvent_value(signal, 1);
    marker.commit();
    let committed_before_release = !timed_out.load(Ordering::SeqCst) && event.signaledValue() == 0;
    // Observe the GPU queue directly, rather than is_complete's ancestor checks:
    // omitting the native wait must not be hidden by software provenance polling.
    std::thread::sleep(Duration::from_millis(250));
    let early_gpu_completion = completed.signaledValue() != 0;
    let pending = !cohort.is_complete(&consumer).unwrap();
    let observed_before_release = !timed_out.load(Ordering::SeqCst) && event.signaledValue() == 0;
    drop(source);
    drop(host);
    event.setSignaledValue(1);
    let _ = done.send(());
    watchdog.join().unwrap();
    assert!(
        committed_before_release,
        "submissions did not commit before watchdog release"
    );
    assert!(
        observed_before_release,
        "GPU marker observation occurred after watchdog release"
    );
    assert!(
        !timed_out.load(Ordering::SeqCst),
        "watchdog released producer gate"
    );
    assert!(
        pending && !early_gpu_completion,
        "consumer GPU progressed before producer release"
    );
    let result = cohort.collect(&consumer).unwrap();
    for (slot, (base, overlay)) in [
        ([17, 83, 149, 255], [255, 0, 0, 255]),
        ([7, 11, 13, 255], [0, 255, 0, 255]),
        ([31, 37, 41, 255], [0, 0, 255, 255]),
    ]
    .into_iter()
    .enumerate()
    {
        for y in 0..3 {
            for x in 0..4 {
                assert_eq!(
                    &result.colors[slot][(y * 4 + x) * 4..(y * 4 + x + 1) * 4],
                    if y == 1 && (1..3).contains(&x) {
                        &overlay
                    } else {
                        &base
                    }
                );
            }
        }
    }
    assert_eq!(
        cohort.collect(&producer).unwrap().colors,
        vec![[17, 83, 149, 255].repeat(12)]
    );
}

#[test]
fn cohort_produced_load_versions_zero_draw_and_rejection() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let first = cohort
        .submit(
            RenderQueue::First,
            &[],
            &clear_pass(&[[17, 83, 149, 255]]),
            &[],
            None,
            &[None],
            &[],
        )
        .unwrap();
    let old = cohort.color_output(&first, 0).unwrap();
    cohort.collect(&first).unwrap();
    let newer = cohort
        .submit(
            RenderQueue::First,
            &[],
            &clear_pass(&[[211, 43, 97, 255]]),
            &[],
            None,
            &[None],
            &[],
        )
        .unwrap();
    let new_output = cohort.color_output(&newer, 0).unwrap();
    let small = TriangleTargetsPass {
        extent: [2, 2],
        ..clear_pass(&[[1, 2, 3, 255]])
    };
    assert!(
        cohort
            .submit_with_color_loads(
                RenderQueue::Second,
                &[],
                &small,
                &[],
                None,
                &[QueuedColorLoad::Produced(old.clone())],
                &[],
                &[]
            )
            .is_err()
    );
    assert!(
        cohort
            .submit_with_color_loads(
                RenderQueue::Second,
                &[],
                &clear_pass(&[[1, 2, 3, 255]]),
                &[],
                None,
                &[],
                &[],
                &[]
            )
            .is_err()
    );
    let mut foreign = gpu.new_render_cohort().unwrap();
    assert!(
        foreign
            .submit_with_color_loads(
                RenderQueue::First,
                &[],
                &clear_pass(&[[1, 2, 3, 255]]),
                &[],
                None,
                &[QueuedColorLoad::Produced(old.clone())],
                &[],
                &[]
            )
            .is_err()
    );
    // Exact versions, not queue's latest output; no draws still performs both blits.
    let copied = cohort
        .submit_with_color_loads(
            RenderQueue::Second,
            &[],
            &clear_pass(&[[1, 2, 3, 255], [4, 5, 6, 255]]),
            &[],
            None,
            &[
                QueuedColorLoad::Produced(old.clone()),
                QueuedColorLoad::Produced(new_output),
            ],
            &[],
            &[],
        )
        .unwrap();
    assert_eq!(
        cohort.collect(&copied).unwrap().colors,
        vec![[17, 83, 149, 255].repeat(12), [211, 43, 97, 255].repeat(12)]
    );
    assert_eq!(
        cohort.collect(&newer).unwrap().colors,
        vec![[211, 43, 97, 255].repeat(12)]
    );
    cohort.reinitialize().unwrap();
    assert!(
        cohort
            .submit_with_color_loads(
                RenderQueue::First,
                &[],
                &clear_pass(&[[1, 2, 3, 255]]),
                &[],
                None,
                &[QueuedColorLoad::Produced(old)],
                &[],
                &[]
            )
            .is_err()
    );
}

fn submit_depth_probe(
    cohort: &mut RenderCohort,
    queue: RenderQueue,
    depth: QueuedDepthInput,
    z: f32,
    write: bool,
    empty: bool,
) -> RenderPoint {
    let positions = POS.map(|[x, y, _, w]| [x, y, z, w]);
    let draws = [TriangleDraw {
        vertices: &positions,
        colors: &COLORS,
        viewport: [4, 3],
    }];
    let states = [TriangleState {
        depth: DepthState {
            compare: DepthCompare::Less,
            write,
        },
        scissor: empty.then_some([0, 0, 0, 0]),
        ..Default::default()
    }];
    let pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent: [4, 3],
        clears: &[[7, 11, 13, 255]],
        clear_depth: None,
    };
    cohort
        .submit_with_attachments(
            queue,
            &[TriangleShader {
                source: SOURCE,
                entries: ["vs", "red"],
            }],
            &pass,
            &[vec![]],
            Some(depth),
            &[QueuedColorLoad::Clear],
            &[],
            &[DrawUniforms::default()],
        )
        .unwrap()
}
fn assert_depth_probe(colors: &[Vec<u8>], right_passes: bool, left_passes: bool) {
    for y in 0..3 {
        for x in 0..4 {
            let passes = if x < 2 { left_passes } else { right_passes };
            assert_eq!(
                &colors[0][(y * 4 + x) * 4..(y * 4 + x + 1) * 4],
                if passes {
                    &[255, 0, 0, 255]
                } else {
                    &[7, 11, 13, 255]
                }
            );
        }
    }
}

#[test]
fn cohort_produced_depth_planes_readonly_write_versions_and_clear() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
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
    let gate = cohort.queue(RenderQueue::First).commandBuffer().unwrap();
    let wait: &ProtocolObject<dyn MTLEvent> = ProtocolObject::from_ref(&*event);
    gate.encodeWaitForEvent_value(wait, 1);
    gate.commit();
    let left = POS.map(|[x, y, _, w]| [x, y, 0.25, w]);
    let right = POS.map(|[x, y, _, w]| [x, y, 0.75, w]);
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
        clears: &[[7, 11, 13, 255]],
        clear_depth: None,
    };
    let producer = cohort
        .submit_with_attachments(
            RenderQueue::First,
            &[TriangleShader {
                source: SOURCE,
                entries: ["vs", "red"],
            }; 2],
            &pass,
            &[vec![], vec![]],
            Some(QueuedDepthInput::Clear(1.)),
            &[QueuedColorLoad::Clear],
            &[],
            &[DrawUniforms::default(); 2],
        )
        .unwrap();
    let original = cohort.depth_output(&producer).unwrap();
    assert_eq!(original.extent(), [4, 3]);
    let consumer = submit_depth_probe(
        &mut cohort,
        RenderQueue::Second,
        QueuedDepthInput::Produced(original.clone()),
        0.5,
        false,
        false,
    );
    let readonly = cohort.depth_output(&consumer).unwrap();
    let completed = gpu.device().newSharedEvent().unwrap();
    let marker = cohort.queue(RenderQueue::Second).commandBuffer().unwrap();
    let signal: &ProtocolObject<dyn MTLEvent> = ProtocolObject::from_ref(&*completed);
    marker.encodeSignalEvent_value(signal, 1);
    marker.commit();
    let committed_held = event.signaledValue() == 0 && !timed_out.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(250));
    let early = completed.signaledValue() != 0;
    let observed_held = event.signaledValue() == 0 && !timed_out.load(Ordering::SeqCst);
    event.setSignaledValue(1);
    let _ = done.send(());
    watchdog.join().unwrap();
    assert!(committed_held && observed_held && !timed_out.load(Ordering::SeqCst));
    assert!(!early, "consumer GPU progressed before producer release");
    let result = cohort.collect(&consumer).unwrap();
    assert_depth_probe(&result.colors, true, false);
    assert_eq!(result.depth.as_ref().unwrap().byte_len(), 48);
    cohort.collect(&producer).unwrap();
    drop(producer);
    drop(consumer);
    // A readonly .5 draw must leave .75 intact: .625 still passes on the right.
    // This distinguishes readonly preservation from accidentally writing .5.
    let readonly_probe = submit_depth_probe(
        &mut cohort,
        RenderQueue::First,
        QueuedDepthInput::Produced(readonly.clone()),
        0.625,
        false,
        false,
    );
    assert_depth_probe(
        &cohort.collect(&readonly_probe).unwrap().colors,
        true,
        false,
    );
    // Empty-scissor Load preserves both planes and remains a usable exact version.
    let empty = submit_depth_probe(
        &mut cohort,
        RenderQueue::First,
        QueuedDepthInput::Produced(readonly),
        0.125,
        true,
        true,
    );
    let empty_output = cohort.depth_output(&empty).unwrap();
    assert_depth_probe(&cohort.collect(&empty).unwrap().colors, false, false);
    let writable = submit_depth_probe(
        &mut cohort,
        RenderQueue::Second,
        QueuedDepthInput::Produced(empty_output),
        0.5,
        true,
        false,
    );
    let written = cohort.depth_output(&writable).unwrap();
    assert_depth_probe(&cohort.collect(&writable).unwrap().colors, true, false);
    let rejected = submit_depth_probe(
        &mut cohort,
        RenderQueue::First,
        QueuedDepthInput::Produced(written),
        0.625,
        false,
        false,
    );
    assert_depth_probe(&cohort.collect(&rejected).unwrap().colors, false, false);
    let old_probe = submit_depth_probe(
        &mut cohort,
        RenderQueue::Second,
        QueuedDepthInput::Produced(original.clone()),
        0.625,
        false,
        false,
    );
    assert_depth_probe(&cohort.collect(&old_probe).unwrap().colors, true, false);
    // Zero-draw Clear is stored, exported, and can initialize a pending Load.
    let cleared = cohort
        .submit_with_attachments(
            RenderQueue::First,
            &[],
            &clear_pass(&[[1, 2, 3, 255]]),
            &[],
            Some(QueuedDepthInput::Clear(0.875)),
            &[QueuedColorLoad::Clear],
            &[],
            &[],
        )
        .unwrap();
    let clear_output = cohort.depth_output(&cleared).unwrap();
    let all = submit_depth_probe(
        &mut cohort,
        RenderQueue::Second,
        QueuedDepthInput::Produced(clear_output),
        0.5,
        false,
        false,
    );
    cohort.collect(&cleared).unwrap(); // Opposite collection order.
    assert_depth_probe(&cohort.collect(&all).unwrap().colors, true, true);
    // Exported host snapshot remains usable through the old/new host path.
    let host = result.depth.unwrap();
    let host_probe = submit_depth_probe(
        &mut cohort,
        RenderQueue::First,
        QueuedDepthInput::Host(host),
        0.5,
        false,
        false,
    );
    assert_depth_probe(&cohort.collect(&host_probe).unwrap().colors, true, false);
    cohort.reinitialize().unwrap();
    assert!(
        cohort
            .submit_with_attachments(
                RenderQueue::First,
                &[],
                &clear_pass(&[[1, 2, 3, 255]]),
                &[],
                Some(QueuedDepthInput::Produced(original)),
                &[QueuedColorLoad::Clear],
                &[],
                &[]
            )
            .is_err()
    );
}

#[test]
fn cohort_produced_depth_rejection_preserves_source_and_transient_is_private() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let pass = clear_pass(&[[7, 11, 13, 255]]);
    let producer = cohort
        .submit_with_attachments(
            RenderQueue::First,
            &[],
            &pass,
            &[],
            Some(QueuedDepthInput::Clear(0.25)),
            &[QueuedColorLoad::Clear],
            &[],
            &[],
        )
        .unwrap();
    let source = cohort.depth_output(&producer).unwrap();
    cohort.collect(&producer).unwrap();
    let wrong_extent = TriangleTargetsPass {
        extent: [2, 2],
        ..pass
    };
    assert!(
        cohort
            .submit_with_attachments(
                RenderQueue::Second,
                &[],
                &wrong_extent,
                &[],
                Some(QueuedDepthInput::Produced(source.clone())),
                &[QueuedColorLoad::Clear],
                &[],
                &[]
            )
            .is_err()
    );
    let mrt = clear_pass(&[[1, 2, 3, 255], [4, 5, 6, 255]]);
    assert!(
        cohort
            .submit_with_attachments(
                RenderQueue::Second,
                &[],
                &mrt,
                &[],
                Some(QueuedDepthInput::Produced(source.clone())),
                &[QueuedColorLoad::Clear, QueuedColorLoad::Host(&[0])],
                &[],
                &[]
            )
            .is_err()
    );
    assert!(
        cohort
            .submit_with_attachments(
                RenderQueue::Second,
                &[],
                &pass,
                &[],
                Some(QueuedDepthInput::Clear(f32::NAN)),
                &[QueuedColorLoad::Clear],
                &[],
                &[]
            )
            .is_err()
    );
    let mut foreign = gpu.new_render_cohort().unwrap();
    assert!(
        foreign
            .submit_with_attachments(
                RenderQueue::First,
                &[],
                &pass,
                &[],
                Some(QueuedDepthInput::Produced(source.clone())),
                &[QueuedColorLoad::Clear],
                &[],
                &[]
            )
            .is_err()
    );
    let zero = cohort
        .submit_with_attachments(
            RenderQueue::Second,
            &[],
            &pass,
            &[],
            Some(QueuedDepthInput::Produced(source)),
            &[QueuedColorLoad::Clear],
            &[],
            &[],
        )
        .unwrap();
    let copied = cohort.depth_output(&zero).unwrap();
    cohort.collect(&zero).unwrap();
    let probe = submit_depth_probe(
        &mut cohort,
        RenderQueue::First,
        QueuedDepthInput::Produced(copied),
        0.5,
        true,
        false,
    );
    assert_depth_probe(&cohort.collect(&probe).unwrap().colors, false, false);
    let transient = TriangleTargetsPass {
        clear_depth: Some(0.875),
        ..pass
    };
    assert!(
        cohort
            .submit_with_attachments(
                RenderQueue::First,
                &[],
                &transient,
                &[],
                Some(QueuedDepthInput::Clear(1.)),
                &[QueuedColorLoad::Clear],
                &[],
                &[]
            )
            .is_err()
    );
    let point = cohort
        .submit_with_attachments(
            RenderQueue::First,
            &[],
            &transient,
            &[],
            None,
            &[QueuedColorLoad::Clear],
            &[],
            &[],
        )
        .unwrap();
    assert!(cohort.depth_output(&point).is_err());
    assert!(cohort.collect(&point).unwrap().depth.is_none());
}
