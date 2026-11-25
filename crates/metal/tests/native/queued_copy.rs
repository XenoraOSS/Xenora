// SPDX-License-Identifier: GPL-3.0-or-later
//! Original literal byte patterns; downstream inputs remain native produced versions.
use xenora_metal::*;

// Odd length deliberately exercises non-word-aligned whole-buffer copies.
const BUFFER_BYTES: [u8; 37] = [
    0, 255, 128, 1, 17, 83, 149, 231, 2, 254, 127, 64, 3, 9, 27, 81, 243, 7, 11, 13, 19, 23, 29,
    31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 89, 97,
];

#[test]
fn queued_copy_host_buffer_to_produced_buffer_literal_baseline() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let mut host = BUFFER_BYTES.to_vec();
    let producer = cohort
        .submit_copy(RenderQueue::First, QueuedCopyInput::HostBuffer(&host), &[])
        .unwrap();
    let output = match cohort.copy_output(&producer).unwrap() {
        RenderCopyOutput::Buffer(output) => output,
        RenderCopyOutput::Color(_) => panic!("buffer copy returned a color output"),
    };
    assert_eq!(output.byte_len(), 37);
    let consumer = cohort
        .submit_copy(
            RenderQueue::Second,
            QueuedCopyInput::ProducedBuffer(output),
            &[],
        )
        .unwrap();
    host.fill(42);
    drop(host);
    // Collect dependent first; neither returned byte vector supplies another input.
    match cohort.collect_copy(&consumer).unwrap() {
        OwnedCopyOutput::Buffer(bytes) => assert_eq!(bytes, BUFFER_BYTES),
        OwnedCopyOutput::Rgba8 { .. } => panic!("buffer copy returned texture bytes"),
    }
    match cohort.collect_copy(&producer).unwrap() {
        OwnedCopyOutput::Buffer(bytes) => assert_eq!(bytes, BUFFER_BYTES),
        OwnedCopyOutput::Rgba8 { .. } => panic!("buffer copy returned texture bytes"),
    }
}

use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::{MTLCommandBuffer, MTLCommandQueue, MTLDevice, MTLEvent, MTLSharedEvent};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

const TEXELS: [u8; 24] = [
    17, 83, 149, 231, 211, 43, 97, 127, 3, 251, 61, 19, 89, 5, 239, 67, 157, 199, 7, 181, 253, 109,
    31, 255,
];
const POS: [[f32; 4]; 3] = [[-1., -1., 0.5, 1.], [3., -1., 0.5, 1.], [-1., 3., 0.5, 1.]];
const COLORS: [[f32; 4]; 3] = [[0.; 4]; 3];
const SHADER: &str = r"
#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; };
vertex V vs(uint i [[vertex_id]], device const float4 *p [[buffer(0)]]) { return {p[i]}; }
fragment float4 sample_pixels(V v [[stage_in]], texture2d<float> t [[texture(0)]], sampler s [[sampler(0)]]) {
    return t.sample(s, v.position.xy / float2(3.0,2.0));
}
fragment float4 overlay() { return float4(1,0,1,1); }
fragment float4 pattern(V v [[stage_in]]) {
    const float4 p[6] = {float4(17,83,149,231),float4(211,43,97,127),float4(3,251,61,19),float4(89,5,239,67),float4(157,199,7,181),float4(253,109,31,255)};
    return p[uint(v.position.y)*3+uint(v.position.x)] / 255.0;
}
";

fn buffer_output(cohort: &RenderCohort, point: &RenderPoint) -> RenderBufferOutput {
    match cohort.copy_output(point).unwrap() {
        RenderCopyOutput::Buffer(output) => output,
        RenderCopyOutput::Color(_) => panic!("expected buffer output"),
    }
}
fn color_output(cohort: &RenderCohort, point: &RenderPoint) -> RenderColorOutput {
    match cohort.copy_output(point).unwrap() {
        RenderCopyOutput::Color(output) => output,
        RenderCopyOutput::Buffer(_) => panic!("expected RGBA output"),
    }
}
fn check_buffer(output: OwnedCopyOutput, expected: &[u8]) {
    match output {
        OwnedCopyOutput::Buffer(bytes) => assert_eq!(bytes, expected),
        OwnedCopyOutput::Rgba8 { .. } => panic!("expected buffer completion"),
    }
}
fn check_texture(output: OwnedCopyOutput) {
    match output {
        OwnedCopyOutput::Rgba8 { extent, pixels } => {
            assert_eq!(extent, [3, 2]);
            assert_eq!(pixels, TEXELS);
        }
        OwnedCopyOutput::Buffer(_) => panic!("expected texture completion"),
    }
}

// Release before any unwinding drains queued work. The watchdog is rescue only,
// and its release is explicitly a failure, never ordering evidence.
struct Gate {
    event: Retained<ProtocolObject<dyn MTLSharedEvent>>,
    timed_out: Arc<AtomicBool>,
    done: Option<mpsc::Sender<()>>,
    watchdog: Option<std::thread::JoinHandle<()>>,
}
impl Gate {
    fn new(gpu: &NativeCompute, cohort: &RenderCohort, queue: RenderQueue) -> Self {
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
        let command = cohort.queue(queue).commandBuffer().unwrap();
        let event_ref: &ProtocolObject<dyn MTLEvent> = ProtocolObject::from_ref(&*event);
        command.encodeWaitForEvent_value(event_ref, 1);
        command.commit();
        Self {
            event,
            timed_out,
            done: Some(done),
            watchdog: Some(watchdog),
        }
    }
    fn observe_then_release(
        mut self,
        gpu: &NativeCompute,
        cohort: &RenderCohort,
        consumer: RenderQueue,
    ) {
        let committed_held =
            self.event.signaledValue() == 0 && !self.timed_out.load(Ordering::SeqCst);
        let completed = gpu.device().newSharedEvent().unwrap();
        let marker = cohort.queue(consumer).commandBuffer().unwrap();
        let event_ref: &ProtocolObject<dyn MTLEvent> = ProtocolObject::from_ref(&*completed);
        marker.encodeSignalEvent_value(event_ref, 1);
        marker.commit();
        std::thread::sleep(Duration::from_millis(250));
        let early = completed.signaledValue() != 0;
        let observed_held =
            self.event.signaledValue() == 0 && !self.timed_out.load(Ordering::SeqCst);
        self.release();
        assert!(
            committed_held && observed_held && !self.timed_out.load(Ordering::SeqCst),
            "watchdog invalidated gated observation"
        );
        assert!(!early, "consumer GPU marker ran before producer release");
    }
    fn release(&mut self) {
        self.event.setSignaledValue(1);
        if let Some(done) = self.done.take() {
            let _ = done.send(());
        }
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.join().unwrap();
        }
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        self.release();
    }
}

#[test]
fn queued_copy_odd_logical_lengths_and_both_collection_orders() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    for len in [1, 2, 3, 4, 5, 37] {
        for producer_first in [false, true] {
            let expected = if len == 1 {
                &BUFFER_BYTES[1..2]
            } else {
                &BUFFER_BYTES[..len]
            };
            let mut bytes = expected.to_vec();
            let producer = cohort
                .submit_copy(RenderQueue::First, QueuedCopyInput::HostBuffer(&bytes), &[])
                .unwrap();
            let output = buffer_output(&cohort, &producer);
            assert_eq!(output.byte_len(), len);
            let consumer = cohort
                .submit_copy(
                    RenderQueue::Second,
                    QueuedCopyInput::ProducedBuffer(output.clone()),
                    &[],
                )
                .unwrap();
            bytes.fill(42);
            drop(bytes);
            drop(output);
            // Wrong collector must leave the entry available to the right one.
            assert!(cohort.collect(&producer).is_err());
            assert!(cohort.collect(&consumer).is_err());
            if producer_first {
                check_buffer(cohort.collect_copy(&producer).unwrap(), expected);
                check_buffer(cohort.collect_copy(&consumer).unwrap(), expected);
            } else {
                check_buffer(cohort.collect_copy(&consumer).unwrap(), expected);
                check_buffer(cohort.collect_copy(&producer).unwrap(), expected);
            }
        }
    }
}

#[test]
fn queued_copy_gated_buffer_dependencies_and_source_lifetime() {
    let gpu = NativeCompute::new().unwrap();
    for producer_first in [false, true] {
        let mut cohort = gpu.new_render_cohort().unwrap();
        let gate = Gate::new(&gpu, &cohort, RenderQueue::First);
        let host = BUFFER_BYTES.to_vec();
        let producer = cohort
            .submit_copy(RenderQueue::First, QueuedCopyInput::HostBuffer(&host), &[])
            .unwrap();
        let output = buffer_output(&cohort, &producer);
        let consumer = cohort
            .submit_copy(
                RenderQueue::Second,
                QueuedCopyInput::ProducedBuffer(output.clone()),
                &[],
            )
            .unwrap();
        assert!(cohort.collect(&producer).is_err());
        assert!(cohort.collect(&consumer).is_err());
        assert!(
            !gate.timed_out.load(Ordering::SeqCst) && gate.event.signaledValue() == 0,
            "wrong-kind collector drained work before checking entry kind"
        );
        drop(host);
        drop(output);
        gate.observe_then_release(&gpu, &cohort, RenderQueue::Second);
        if producer_first {
            check_buffer(cohort.collect_copy(&producer).unwrap(), &BUFFER_BYTES);
            drop(producer); // consumer must retain the source after ancestor release.
            check_buffer(cohort.collect_copy(&consumer).unwrap(), &BUFFER_BYTES);
        } else {
            check_buffer(cohort.collect_copy(&consumer).unwrap(), &BUFFER_BYTES);
            check_buffer(cohort.collect_copy(&producer).unwrap(), &BUFFER_BYTES);
        }
    }
}

#[test]
fn queued_copy_old_buffer_versions_validation_and_recovery() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    assert!(
        cohort
            .submit_copy(RenderQueue::First, QueuedCopyInput::HostBuffer(&[]), &[])
            .is_err()
    );
    for (extent, pixels) in [
        ([0, 2], TEXELS.as_slice()),
        ([3, 2], &TEXELS[..23]),
        ([4097, 1], TEXELS.as_slice()),
    ] {
        assert!(
            cohort
                .submit_copy(
                    RenderQueue::First,
                    QueuedCopyInput::HostRgba8 { extent, pixels },
                    &[]
                )
                .is_err()
        );
    }
    let first = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostBuffer(&BUFFER_BYTES),
            &[],
        )
        .unwrap();
    let old = buffer_output(&cohort, &first);
    check_buffer(cohort.collect_copy(&first).unwrap(), &BUFFER_BYTES);
    drop(first);
    let newer = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostBuffer(&[201; 37]),
            &[],
        )
        .unwrap();
    let new_output = buffer_output(&cohort, &newer);
    let mut foreign = gpu.new_render_cohort().unwrap();
    assert!(
        foreign
            .submit_copy(
                RenderQueue::First,
                QueuedCopyInput::ProducedBuffer(old.clone()),
                &[]
            )
            .is_err()
    );
    assert!(
        foreign
            .submit_copy(
                RenderQueue::First,
                QueuedCopyInput::HostBuffer(&BUFFER_BYTES),
                &[old.producer()]
            )
            .is_err()
    );
    assert!(
        cohort
            .submit_copy(
                RenderQueue::Second,
                QueuedCopyInput::HostBuffer(&BUFFER_BYTES),
                &[old.producer(), old.producer(), old.producer()]
            )
            .is_err()
    );
    let copied = cohort
        .submit_copy(
            RenderQueue::Second,
            QueuedCopyInput::ProducedBuffer(old.clone()),
            &[],
        )
        .unwrap();
    check_buffer(cohort.collect_copy(&copied).unwrap(), &BUFFER_BYTES);
    check_buffer(cohort.collect_copy(&newer).unwrap(), &[201; 37]);
    let latest = cohort
        .submit_copy(
            RenderQueue::Second,
            QueuedCopyInput::ProducedBuffer(new_output),
            &[],
        )
        .unwrap();
    check_buffer(cohort.collect_copy(&latest).unwrap(), &[201; 37]);
    cohort
        .abort_and_drain("controlled host rejection; not a GPU fault")
        .unwrap();
    assert!(
        cohort
            .submit_copy(
                RenderQueue::First,
                QueuedCopyInput::ProducedBuffer(old.clone()),
                &[]
            )
            .is_err()
    );
    cohort.reinitialize().unwrap();
    assert!(
        cohort
            .submit_copy(
                RenderQueue::First,
                QueuedCopyInput::ProducedBuffer(old),
                &[]
            )
            .is_err()
    );
    let recovery = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostBuffer(&BUFFER_BYTES),
            &[],
        )
        .unwrap();
    check_buffer(cohort.collect_copy(&recovery).unwrap(), &BUFFER_BYTES);
}

#[allow(clippy::too_many_arguments)]
fn render_texture(
    gpu: &NativeCompute,
    cohort: &mut RenderCohort,
    queue: RenderQueue,
    fragment: &str,
    sampled: Option<RenderColorOutput>,
    load: QueuedColorLoad<'_>,
    scissor: Option<[usize; 4]>,
) -> RenderPoint {
    let draws = [TriangleDraw {
        vertices: &POS,
        colors: &COLORS,
        viewport: [3, 2],
    }];
    let states = [TriangleState {
        scissor,
        ..Default::default()
    }];
    let bindings = sampled
        .map(|output| QueuedSampledBinding::Produced {
            output,
            sampler: MetalSampler::new(gpu.device(), xenora_texture::SamplerDesc::nearest_clamp())
                .unwrap(),
            texture_slot: BindingSlot::new(0),
            sampler_slot: BindingSlot::new(0),
        })
        .into_iter()
        .collect();
    let pass = TriangleTargetsPass {
        draws: &draws,
        states: &states,
        extent: [3, 2],
        clears: &[[7, 11, 13, 255]],
        clear_depth: None,
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
            None,
            &[load],
            &[],
            &[DrawUniforms::default()],
        )
        .unwrap()
}

#[test]
fn queued_copy_render_to_odd_texture_then_direct_sampling_and_load() {
    let gpu = NativeCompute::new().unwrap();
    for producer_first in [false, true] {
        let mut cohort = gpu.new_render_cohort().unwrap();
        let gate = Gate::new(&gpu, &cohort, RenderQueue::First);
        let producer = render_texture(
            &gpu,
            &mut cohort,
            RenderQueue::First,
            "pattern",
            None,
            QueuedColorLoad::Clear,
            None,
        );
        let source = cohort.color_output(&producer, 0).unwrap();
        assert!(cohort.copy_output(&producer).is_err());
        assert!(cohort.collect_copy(&producer).is_err());
        let consumer = cohort
            .submit_copy(
                RenderQueue::Second,
                QueuedCopyInput::ProducedColor(source.clone()),
                &[],
            )
            .unwrap();
        let copied = color_output(&cohort, &consumer);
        assert_eq!(copied.extent(), [3, 2]);
        drop(source);
        gate.observe_then_release(&gpu, &cohort, RenderQueue::Second);
        if producer_first {
            assert_eq!(
                cohort.collect(&producer).unwrap().colors,
                vec![TEXELS.to_vec()]
            );
            check_texture(cohort.collect_copy(&consumer).unwrap());
        } else {
            check_texture(cohort.collect_copy(&consumer).unwrap());
            assert_eq!(
                cohort.collect(&producer).unwrap().colors,
                vec![TEXELS.to_vec()]
            );
        }
        drop(producer);
        drop(consumer);
        // New edge from a retained collected native version; no returned pixels
        // enter a texture constructor. This is not three outstanding receipts.
        let sampled = render_texture(
            &gpu,
            &mut cohort,
            RenderQueue::First,
            "sample_pixels",
            Some(copied.clone()),
            QueuedColorLoad::Clear,
            None,
        );
        assert_eq!(
            cohort.collect(&sampled).unwrap().colors,
            vec![TEXELS.to_vec()]
        );
        let overlay = render_texture(
            &gpu,
            &mut cohort,
            RenderQueue::Second,
            "overlay",
            None,
            QueuedColorLoad::Produced(copied.clone()),
            Some([1, 1, 1, 1]),
        );
        assert_eq!(
            cohort.collect(&overlay).unwrap().colors,
            vec![vec![
                17, 83, 149, 231, 211, 43, 97, 127, 3, 251, 61, 19, 89, 5, 239, 67, 255, 0, 255,
                255, 253, 109, 31, 255,
            ]]
        );
        let unchanged = cohort
            .submit_copy(
                RenderQueue::First,
                QueuedCopyInput::ProducedColor(copied),
                &[],
            )
            .unwrap();
        check_texture(cohort.collect_copy(&unchanged).unwrap());
    }
}

#[test]
fn queued_copy_host_texture_to_pending_copy_and_render_edges() {
    let gpu = NativeCompute::new().unwrap();
    for sampling in [false, true] {
        let mut cohort = gpu.new_render_cohort().unwrap();
        let gate = Gate::new(&gpu, &cohort, RenderQueue::First);
        let mut bytes = TEXELS.to_vec();
        let producer = cohort
            .submit_copy(
                RenderQueue::First,
                QueuedCopyInput::HostRgba8 {
                    extent: [3, 2],
                    pixels: &bytes,
                },
                &[],
            )
            .unwrap();
        let output = color_output(&cohort, &producer);
        let consumer = if sampling {
            render_texture(
                &gpu,
                &mut cohort,
                RenderQueue::Second,
                "sample_pixels",
                Some(output.clone()),
                QueuedColorLoad::Clear,
                None,
            )
        } else {
            cohort
                .submit_copy(
                    RenderQueue::Second,
                    QueuedCopyInput::ProducedColor(output.clone()),
                    &[],
                )
                .unwrap()
        };
        drop(output);
        bytes.fill(42);
        drop(bytes);
        gate.observe_then_release(&gpu, &cohort, RenderQueue::Second);
        check_texture(cohort.collect_copy(&producer).unwrap());
        drop(producer);
        if sampling {
            assert_eq!(
                cohort.collect(&consumer).unwrap().colors,
                vec![TEXELS.to_vec()]
            );
        } else {
            check_texture(cohort.collect_copy(&consumer).unwrap());
        }
    }
}

#[test]
fn queued_copy_texture_generation_rejection_preserves_source_and_slot() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let first = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostRgba8 {
                extent: [3, 2],
                pixels: &TEXELS,
            },
            &[],
        )
        .unwrap();
    let output = color_output(&cohort, &first);
    check_texture(cohort.collect_copy(&first).unwrap());
    let mut foreign = gpu.new_render_cohort().unwrap();
    assert!(
        foreign
            .submit_copy(
                RenderQueue::First,
                QueuedCopyInput::ProducedColor(output.clone()),
                &[]
            )
            .is_err()
    );
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
    assert!(
        cohort
            .submit_with_attachments(
                RenderQueue::Second,
                &[TriangleShader {
                    source: SHADER,
                    entries: ["vs", "overlay"]
                }],
                &pass,
                &[vec![]],
                None,
                &[QueuedColorLoad::Produced(output.clone())],
                &[],
                &[DrawUniforms::default()]
            )
            .is_err()
    );
    let recovery = cohort
        .submit_copy(
            RenderQueue::Second,
            QueuedCopyInput::ProducedColor(output.clone()),
            &[],
        )
        .unwrap();
    check_texture(cohort.collect_copy(&recovery).unwrap());
    cohort.reinitialize().unwrap();
    assert!(
        cohort
            .submit_copy(
                RenderQueue::First,
                QueuedCopyInput::ProducedColor(output),
                &[]
            )
            .is_err()
    );
    let fresh = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostRgba8 {
                extent: [3, 2],
                pixels: &TEXELS,
            },
            &[],
        )
        .unwrap();
    check_texture(cohort.collect_copy(&fresh).unwrap());
}

#[test]
fn queued_copy_controlled_rejection_drains_dependency_and_recovers() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let producer = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostBuffer(&BUFFER_BYTES),
            &[],
        )
        .unwrap();
    let source = buffer_output(&cohort, &producer);
    let consumer = cohort
        .submit_copy(
            RenderQueue::Second,
            QueuedCopyInput::ProducedBuffer(source),
            &[],
        )
        .unwrap();
    let rejected_output = buffer_output(&cohort, &consumer);
    check_buffer(cohort.collect_copy(&producer).unwrap(), &BUFFER_BYTES);
    // Host publication rejection is controlled cancellation, not a GPU fault.
    // Require cached successful completion before rejection; do not merely assume it.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if cohort.is_complete(&consumer).unwrap() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "consumer did not complete before cancellation deadline"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    cohort
        .abort_and_drain("controlled ancestor publication rejection")
        .unwrap();
    assert!(cohort.collect_copy(&consumer).is_err());
    cohort.reinitialize().unwrap();
    assert!(
        cohort
            .submit_copy(
                RenderQueue::First,
                QueuedCopyInput::ProducedBuffer(rejected_output),
                &[]
            )
            .is_err()
    );
    let recovery = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostBuffer(&BUFFER_BYTES),
            &[],
        )
        .unwrap();
    check_buffer(cohort.collect_copy(&recovery).unwrap(), &BUFFER_BYTES);
}

#[test]
fn queued_copy_pending_texture_color_load_preserves_untouched_pixels() {
    let gpu = NativeCompute::new().unwrap();
    for producer_first in [false, true] {
        let mut cohort = gpu.new_render_cohort().unwrap();
        let gate = Gate::new(&gpu, &cohort, RenderQueue::First);
        let producer = cohort
            .submit_copy(
                RenderQueue::First,
                QueuedCopyInput::HostRgba8 {
                    extent: [3, 2],
                    pixels: &TEXELS,
                },
                &[],
            )
            .unwrap();
        let output = color_output(&cohort, &producer);
        let consumer = render_texture(
            &gpu,
            &mut cohort,
            RenderQueue::Second,
            "overlay",
            None,
            QueuedColorLoad::Produced(output),
            Some([1, 1, 1, 1]),
        );
        assert!(cohort.collect_copy(&consumer).is_err());
        assert!(
            !gate.timed_out.load(Ordering::SeqCst) && gate.event.signaledValue() == 0,
            "copy collector waited for a render entry before rejecting its kind"
        );
        gate.observe_then_release(&gpu, &cohort, RenderQueue::Second);
        let expected = vec![vec![
            17, 83, 149, 231, 211, 43, 97, 127, 3, 251, 61, 19, 89, 5, 239, 67, 255, 0, 255, 255,
            253, 109, 31, 255,
        ]];
        if producer_first {
            check_texture(cohort.collect_copy(&producer).unwrap());
            drop(producer);
            assert_eq!(cohort.collect(&consumer).unwrap().colors, expected);
        } else {
            assert_eq!(cohort.collect(&consumer).unwrap().colors, expected);
            check_texture(cohort.collect_copy(&producer).unwrap());
        }
    }
}

#[test]
fn queued_buffer_region_host_patch_preserves_full_odd_destination() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let source = [
        91, 73, 55, 37, 255, 2, 129, 4, 17, 83, 149, 231, 13, 29, 61, 127,
    ];
    let destination = [
        101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118,
        119, 120, 121,
    ];
    // Eight bytes from source[4..12] replace destination[8..16]. The final
    // five destination bytes remain literal, including the odd logical tail.
    let expected = [
        101, 102, 103, 104, 105, 106, 107, 108, 255, 2, 129, 4, 17, 83, 149, 231, 117, 118, 119,
        120, 121,
    ];
    let point = cohort
        .submit_buffer_region_copy(
            RenderQueue::First,
            QueuedBufferRegionCopy {
                source: QueuedCopyInput::HostBuffer(&source),
                destination: QueuedCopyInput::HostBuffer(&destination),
                source_offset: 4,
                destination_offset: 8,
                byte_len: 8,
            },
            &[],
        )
        .unwrap();
    assert_eq!(buffer_output(&cohort, &point).byte_len(), 21);
    check_buffer(cohort.collect_copy(&point).unwrap(), &expected);
}

const REGION_SOURCE: [u8; 16] = [
    91, 73, 55, 37, 255, 2, 129, 4, 17, 83, 149, 231, 13, 29, 61, 127,
];
const REGION_DEST: [u8; 21] = [
    101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119,
    120, 121,
];
const REGION_EXPECTED: [u8; 21] = [
    101, 102, 103, 104, 105, 106, 107, 108, 255, 2, 129, 4, 17, 83, 149, 231, 117, 118, 119, 120,
    121,
];

fn region_copy<'a>(
    source: QueuedCopyInput<'a>,
    destination: QueuedCopyInput<'a>,
) -> QueuedBufferRegionCopy<'a> {
    QueuedBufferRegionCopy {
        source,
        destination,
        source_offset: 4,
        destination_offset: 8,
        byte_len: 8,
    }
}

#[test]
fn queued_buffer_region_each_pending_input_waits_and_retires() {
    let gpu = NativeCompute::new().unwrap();
    for pending_source in [false, true] {
        for producer_first in [false, true] {
            let mut cohort = gpu.new_render_cohort().unwrap();
            let gate = Gate::new(&gpu, &cohort, RenderQueue::First);
            let prior = if pending_source {
                REGION_SOURCE.as_slice()
            } else {
                REGION_DEST.as_slice()
            };
            let mut host = prior.to_vec();
            let producer = cohort
                .submit_copy(RenderQueue::First, QueuedCopyInput::HostBuffer(&host), &[])
                .unwrap();
            let output = buffer_output(&cohort, &producer);
            let mut other = if pending_source {
                REGION_DEST.to_vec()
            } else {
                REGION_SOURCE.to_vec()
            };
            let request = if pending_source {
                region_copy(
                    QueuedCopyInput::ProducedBuffer(output.clone()),
                    QueuedCopyInput::HostBuffer(&other),
                )
            } else {
                region_copy(
                    QueuedCopyInput::HostBuffer(&other),
                    QueuedCopyInput::ProducedBuffer(output.clone()),
                )
            };
            let consumer = cohort
                .submit_buffer_region_copy(RenderQueue::Second, request, &[])
                .unwrap();
            host.fill(0);
            other.fill(0);
            drop(host);
            drop(other);
            drop(output);
            gate.observe_then_release(&gpu, &cohort, RenderQueue::Second);
            if producer_first {
                check_buffer(cohort.collect_copy(&producer).unwrap(), prior);
                drop(producer);
                check_buffer(cohort.collect_copy(&consumer).unwrap(), &REGION_EXPECTED);
            } else {
                check_buffer(cohort.collect_copy(&consumer).unwrap(), &REGION_EXPECTED);
                check_buffer(cohort.collect_copy(&producer).unwrap(), prior);
            }
        }
    }
}

#[test]
fn queued_buffer_region_zero_offsets_last_region_and_invalid_logical_ranges() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    for (source_offset, destination_offset, byte_len) in [
        (0, 0, 0),
        (12, 8, 8),
        (4, 20, 4),
        (0, 0, 24),
        (usize::MAX - 3, 0, 8),
        (0, usize::MAX - 3, 8),
        (4, 4, usize::MAX - 3),
    ] {
        let request = QueuedBufferRegionCopy {
            source: QueuedCopyInput::HostBuffer(&REGION_SOURCE),
            destination: QueuedCopyInput::HostBuffer(&REGION_DEST),
            source_offset,
            destination_offset,
            byte_len,
        };
        assert!(
            cohort
                .submit_buffer_region_copy(RenderQueue::First, request, &[])
                .is_err()
        );
    }
    // Source logical padding is inaccessible too: 21 bytes has 24-byte backing.
    assert!(
        cohort
            .submit_buffer_region_copy(
                RenderQueue::First,
                QueuedBufferRegionCopy {
                    source: QueuedCopyInput::HostBuffer(&REGION_DEST),
                    destination: QueuedCopyInput::HostBuffer(&REGION_DEST),
                    source_offset: 20,
                    destination_offset: 0,
                    byte_len: 4,
                },
                &[]
            )
            .is_err()
    );
    for source_is_texture in [false, true] {
        let texture = QueuedCopyInput::HostRgba8 {
            extent: [3, 2],
            pixels: &TEXELS,
        };
        let request = if source_is_texture {
            region_copy(texture, QueuedCopyInput::HostBuffer(&REGION_DEST))
        } else {
            region_copy(QueuedCopyInput::HostBuffer(&REGION_SOURCE), texture)
        };
        assert!(
            cohort
                .submit_buffer_region_copy(RenderQueue::First, request, &[])
                .is_err()
        );
    }
    // Both zero offsets are valid. Only the count must be positive.
    let zero = cohort
        .submit_buffer_region_copy(
            RenderQueue::First,
            QueuedBufferRegionCopy {
                source: QueuedCopyInput::HostBuffer(&REGION_SOURCE),
                destination: QueuedCopyInput::HostBuffer(&REGION_DEST),
                source_offset: 0,
                destination_offset: 0,
                byte_len: 4,
            },
            &[],
        )
        .unwrap();
    check_buffer(
        cohort.collect_copy(&zero).unwrap(),
        &[
            91, 73, 55, 37, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118,
            119, 120, 121,
        ],
    );
    let last = cohort
        .submit_buffer_region_copy(
            RenderQueue::First,
            QueuedBufferRegionCopy {
                source: QueuedCopyInput::HostBuffer(&REGION_SOURCE),
                destination: QueuedCopyInput::HostBuffer(&REGION_DEST),
                source_offset: 12,
                destination_offset: 16,
                byte_len: 4,
            },
            &[],
        )
        .unwrap();
    check_buffer(
        cohort.collect_copy(&last).unwrap(),
        &[
            101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 13, 29,
            61, 127, 121,
        ],
    );
}

#[test]
fn queued_buffer_region_exact_old_versions_and_two_slot_admission() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let a = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostBuffer(&REGION_SOURCE),
            &[],
        )
        .unwrap();
    let old_source = buffer_output(&cohort, &a);
    let b = cohort
        .submit_copy(
            RenderQueue::Second,
            QueuedCopyInput::HostBuffer(&REGION_DEST),
            &[],
        )
        .unwrap();
    let old_destination = buffer_output(&cohort, &b);
    // Independent uncollected producers already consume both slots.
    assert!(
        cohort
            .submit_buffer_region_copy(
                RenderQueue::First,
                region_copy(
                    QueuedCopyInput::ProducedBuffer(old_source.clone()),
                    QueuedCopyInput::ProducedBuffer(old_destination.clone())
                ),
                &[]
            )
            .is_err()
    );
    check_buffer(cohort.collect_copy(&a).unwrap(), &REGION_SOURCE);
    check_buffer(cohort.collect_copy(&b).unwrap(), &REGION_DEST);
    drop(a);
    drop(b);
    let newer = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostBuffer(&[201; 21]),
            &[],
        )
        .unwrap();
    let patched = cohort
        .submit_buffer_region_copy(
            RenderQueue::Second,
            region_copy(
                QueuedCopyInput::ProducedBuffer(old_source.clone()),
                QueuedCopyInput::ProducedBuffer(old_destination.clone()),
            ),
            &[],
        )
        .unwrap();
    check_buffer(cohort.collect_copy(&patched).unwrap(), &REGION_EXPECTED);
    check_buffer(cohort.collect_copy(&newer).unwrap(), &[201; 21]);
    // Neither immutable input may have been patched in place.
    for (output, expected) in [
        (old_source.clone(), REGION_SOURCE.as_slice()),
        (old_destination.clone(), REGION_DEST.as_slice()),
    ] {
        let copy = cohort
            .submit_copy(
                RenderQueue::First,
                QueuedCopyInput::ProducedBuffer(output),
                &[],
            )
            .unwrap();
        check_buffer(cohort.collect_copy(&copy).unwrap(), expected);
    }
    let mut foreign = gpu.new_render_cohort().unwrap();
    for source_side in [false, true] {
        let request = if source_side {
            region_copy(
                QueuedCopyInput::ProducedBuffer(old_source.clone()),
                QueuedCopyInput::HostBuffer(&REGION_DEST),
            )
        } else {
            region_copy(
                QueuedCopyInput::HostBuffer(&REGION_SOURCE),
                QueuedCopyInput::ProducedBuffer(old_destination.clone()),
            )
        };
        assert!(
            foreign
                .submit_buffer_region_copy(RenderQueue::First, request, &[])
                .is_err()
        );
    }
    cohort.reinitialize().unwrap();
    assert!(
        cohort
            .submit_buffer_region_copy(
                RenderQueue::First,
                region_copy(
                    QueuedCopyInput::ProducedBuffer(old_source),
                    QueuedCopyInput::ProducedBuffer(old_destination)
                ),
                &[]
            )
            .is_err()
    );
}

#[test]
fn queued_buffer_region_controlled_failure_rejects_output_and_recovers() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let producer = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostBuffer(&REGION_DEST),
            &[],
        )
        .unwrap();
    let prior = buffer_output(&cohort, &producer);
    let consumer = cohort
        .submit_buffer_region_copy(
            RenderQueue::Second,
            region_copy(
                QueuedCopyInput::HostBuffer(&REGION_SOURCE),
                QueuedCopyInput::ProducedBuffer(prior),
            ),
            &[],
        )
        .unwrap();
    let rejected = buffer_output(&cohort, &consumer);
    check_buffer(cohort.collect_copy(&producer).unwrap(), &REGION_DEST);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !cohort.is_complete(&consumer).unwrap() {
        assert!(
            std::time::Instant::now() < deadline,
            "region completion deadline"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    // Controlled host rejection of completed work, not an actual GPU fault.
    cohort
        .abort_and_drain("controlled region publication rejection")
        .unwrap();
    assert!(cohort.collect_copy(&consumer).is_err());
    cohort.reinitialize().unwrap();
    assert!(
        cohort
            .submit_buffer_region_copy(
                RenderQueue::First,
                region_copy(
                    QueuedCopyInput::HostBuffer(&REGION_SOURCE),
                    QueuedCopyInput::ProducedBuffer(rejected)
                ),
                &[]
            )
            .is_err()
    );
    let recovered = cohort
        .submit_buffer_region_copy(
            RenderQueue::First,
            region_copy(
                QueuedCopyInput::HostBuffer(&REGION_SOURCE),
                QueuedCopyInput::HostBuffer(&REGION_DEST),
            ),
            &[],
        )
        .unwrap();
    check_buffer(cohort.collect_copy(&recovered).unwrap(), &REGION_EXPECTED);
}

#[test]
fn queued_buffer_region_rejects_produced_alias_and_color_without_consuming_slot() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let producer = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostBuffer(&REGION_DEST),
            &[],
        )
        .unwrap();
    let buffer = buffer_output(&cohort, &producer);
    // Distinct Rust clones still name the same immutable native allocation.
    assert!(
        cohort
            .submit_buffer_region_copy(
                RenderQueue::Second,
                region_copy(
                    QueuedCopyInput::ProducedBuffer(buffer.clone()),
                    QueuedCopyInput::ProducedBuffer(buffer.clone())
                ),
                &[],
            )
            .is_err()
    );
    check_buffer(cohort.collect_copy(&producer).unwrap(), &REGION_DEST);
    let texture = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostRgba8 {
                extent: [3, 2],
                pixels: &TEXELS,
            },
            &[],
        )
        .unwrap();
    let color = color_output(&cohort, &texture);
    for source_side in [false, true] {
        let request = if source_side {
            region_copy(
                QueuedCopyInput::ProducedColor(color.clone()),
                QueuedCopyInput::HostBuffer(&REGION_DEST),
            )
        } else {
            region_copy(
                QueuedCopyInput::HostBuffer(&REGION_SOURCE),
                QueuedCopyInput::ProducedColor(color.clone()),
            )
        };
        assert!(
            cohort
                .submit_buffer_region_copy(RenderQueue::Second, request, &[])
                .is_err()
        );
    }
    // All rejected requests leave the second queue available, without damaging
    // either immutable producer. A valid consumer still reads the old buffer.
    let valid = cohort
        .submit_buffer_region_copy(
            RenderQueue::Second,
            region_copy(
                QueuedCopyInput::HostBuffer(&REGION_SOURCE),
                QueuedCopyInput::ProducedBuffer(buffer.clone()),
            ),
            &[],
        )
        .unwrap();
    check_buffer(cohort.collect_copy(&valid).unwrap(), &REGION_EXPECTED);
    check_texture(cohort.collect_copy(&texture).unwrap());
    let unchanged = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::ProducedBuffer(buffer),
            &[],
        )
        .unwrap();
    check_buffer(cohort.collect_copy(&unchanged).unwrap(), &REGION_DEST);
}
