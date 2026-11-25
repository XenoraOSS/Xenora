// SPDX-License-Identifier: GPL-3.0-or-later
//! Original literal rectangle and pitched-transfer oracles.
use xenora_metal::*;

#[test]
fn queued_image_copy_rgba_rectangle_preserves_literal_border() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    // Four columns, three rows. Each channel and alpha distinguishes texels.
    let source: [[u8; 4]; 12] = [
        [1, 41, 81, 121],
        [2, 42, 82, 122],
        [3, 43, 83, 123],
        [4, 44, 84, 124],
        [5, 45, 85, 125],
        [6, 46, 86, 126],
        [7, 47, 87, 127],
        [8, 48, 88, 128],
        [9, 49, 89, 129],
        [10, 50, 90, 130],
        [11, 51, 91, 131],
        [12, 52, 92, 132],
    ];
    // Five columns, four rows; destination bytes are disjoint from source bytes.
    let destination: [[u8; 4]; 20] = [
        [151, 181, 211, 241],
        [152, 182, 212, 242],
        [153, 183, 213, 243],
        [154, 184, 214, 244],
        [155, 185, 215, 245],
        [156, 186, 216, 246],
        [157, 187, 217, 247],
        [158, 188, 218, 248],
        [159, 189, 219, 249],
        [160, 190, 220, 250],
        [161, 191, 221, 251],
        [162, 192, 222, 252],
        [163, 193, 223, 253],
        [164, 194, 224, 254],
        [165, 195, 225, 255],
        [166, 196, 226, 236],
        [167, 197, 227, 237],
        [168, 198, 228, 238],
        [169, 199, 229, 239],
        [170, 200, 230, 240],
    ];
    // Copy source (1,1), extent (2,2), to destination (2,1). No production
    // indexing/copy helper constructs this expected result.
    let expected: [[u8; 4]; 20] = [
        [151, 181, 211, 241],
        [152, 182, 212, 242],
        [153, 183, 213, 243],
        [154, 184, 214, 244],
        [155, 185, 215, 245],
        [156, 186, 216, 246],
        [157, 187, 217, 247],
        [6, 46, 86, 126],
        [7, 47, 87, 127],
        [160, 190, 220, 250],
        [161, 191, 221, 251],
        [162, 192, 222, 252],
        [10, 50, 90, 130],
        [11, 51, 91, 131],
        [165, 195, 225, 255],
        [166, 196, 226, 236],
        [167, 197, 227, 237],
        [168, 198, 228, 238],
        [169, 199, 229, 239],
        [170, 200, 230, 240],
    ];
    let point = cohort
        .submit_image_copy(
            RenderQueue::First,
            QueuedImageCopy {
                source: QueuedCopyInput::HostRgba8 {
                    extent: [4, 3],
                    pixels: source.as_flattened(),
                },
                destination: QueuedCopyInput::HostRgba8 {
                    extent: [5, 4],
                    pixels: destination.as_flattened(),
                },
                source_location: ImageCopyLocation::Rgba8 { origin: [1, 1] },
                destination_location: ImageCopyLocation::Rgba8 { origin: [2, 1] },
                extent: [2, 2],
            },
            &[],
        )
        .unwrap();
    match cohort.collect_copy(&point).unwrap() {
        OwnedCopyOutput::Rgba8 { extent, pixels } => {
            assert_eq!(extent, [5, 4]);
            assert_eq!(pixels, expected.as_flattened());
        }
        OwnedCopyOutput::Buffer(_) => panic!("texture rectangle returned buffer output"),
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

const PIXELS: [u8; 16] = [
    1, 41, 81, 121, 2, 42, 82, 122, 3, 43, 83, 123, 4, 44, 84, 124,
];
// Pitch 12, exactly 20 logical bytes: no padding after the last row.
const PITCHED: [u8; 20] = [
    1, 41, 81, 121, 2, 42, 82, 122, 251, 252, 253, 254, 3, 43, 83, 123, 4, 44, 84, 124,
];
const PRIOR: [u8; 16] = [
    101, 111, 121, 131, 102, 112, 122, 132, 103, 113, 123, 133, 104, 114, 124, 134,
];
const BUFFER_PRIOR: [u8; 33] = [
    101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119,
    120, 121, 122, 123, 124, 125, 126, 127, 128, 129, 130, 131, 132, 133,
];
const BUFFER_EXPECTED: [u8; 33] = [
    101, 102, 103, 104, 1, 41, 81, 121, 2, 42, 82, 122, 113, 114, 115, 116, 117, 118, 119, 120,
    121, 122, 123, 124, 3, 43, 83, 123, 4, 44, 84, 124, 133,
];

fn image_source(direction: u8) -> QueuedCopyInput<'static> {
    if direction == 1 {
        QueuedCopyInput::HostBuffer(&PITCHED)
    } else {
        QueuedCopyInput::HostRgba8 {
            extent: [2, 2],
            pixels: &PIXELS,
        }
    }
}
fn image_destination(direction: u8) -> QueuedCopyInput<'static> {
    if direction == 2 {
        QueuedCopyInput::HostBuffer(&BUFFER_PRIOR)
    } else {
        QueuedCopyInput::HostRgba8 {
            extent: [2, 2],
            pixels: &PRIOR,
        }
    }
}
fn image_request<'a>(
    direction: u8,
    source: QueuedCopyInput<'a>,
    destination: QueuedCopyInput<'a>,
) -> QueuedImageCopy<'a> {
    QueuedImageCopy {
        source,
        destination,
        source_location: if direction == 1 {
            ImageCopyLocation::Buffer {
                offset: 0,
                bytes_per_row: 12,
            }
        } else {
            ImageCopyLocation::Rgba8 { origin: [0, 0] }
        },
        destination_location: if direction == 2 {
            ImageCopyLocation::Buffer {
                offset: 4,
                bytes_per_row: 20,
            }
        } else {
            ImageCopyLocation::Rgba8 { origin: [0, 0] }
        },
        extent: [2, 2],
    }
}
fn produced_input(cohort: &RenderCohort, point: &RenderPoint) -> QueuedCopyInput<'static> {
    match cohort.copy_output(point).unwrap() {
        RenderCopyOutput::Buffer(b) => QueuedCopyInput::ProducedBuffer(b),
        RenderCopyOutput::Color(c) => QueuedCopyInput::ProducedColor(c),
    }
}
fn check_image(output: OwnedCopyOutput, direction: u8) {
    match output {
        OwnedCopyOutput::Buffer(bytes) => {
            assert_eq!(direction, 2);
            assert_eq!(bytes, BUFFER_EXPECTED);
        }
        OwnedCopyOutput::Rgba8 { extent, pixels } => {
            assert_ne!(direction, 2);
            assert_eq!(extent, [2, 2]);
            assert_eq!(pixels, PIXELS);
        }
    }
}

#[test]
fn queued_image_copy_pitches_exact_last_row_and_odd_destination_tail() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    for direction in [0, 1, 2] {
        let point = cohort
            .submit_image_copy(
                RenderQueue::First,
                image_request(
                    direction,
                    image_source(direction),
                    image_destination(direction),
                ),
                &[],
            )
            .unwrap();
        check_image(cohort.collect_copy(&point).unwrap(), direction);
    }
    // The exact final addressed byte is sufficient for texture-to-buffer too.
    let point = cohort
        .submit_image_copy(
            RenderQueue::First,
            image_request(
                2,
                image_source(2),
                QueuedCopyInput::HostBuffer(&BUFFER_PRIOR[..32]),
            ),
            &[],
        )
        .unwrap();
    match cohort.collect_copy(&point).unwrap() {
        OwnedCopyOutput::Buffer(bytes) => assert_eq!(bytes, &BUFFER_EXPECTED[..32]),
        _ => panic!("expected buffer"),
    }
}

#[test]
fn queued_image_copy_gated_texture_to_texture_source() {
    image_copy_pending_role(0, true);
}

#[test]
fn queued_image_copy_gated_texture_to_texture_destination() {
    image_copy_pending_role(0, false);
}

#[test]
fn queued_image_copy_gated_buffer_to_texture_source() {
    image_copy_pending_role(1, true);
}

#[test]
fn queued_image_copy_gated_buffer_to_texture_destination() {
    image_copy_pending_role(1, false);
}

#[test]
fn queued_image_copy_gated_texture_to_buffer_source() {
    image_copy_pending_role(2, true);
}

#[test]
fn queued_image_copy_gated_texture_to_buffer_destination() {
    image_copy_pending_role(2, false);
}

fn image_copy_pending_role(direction: u8, pending_source: bool) {
    let gpu = NativeCompute::new().unwrap();
    for producer_first in [false, true] {
        let mut cohort = gpu.new_render_cohort().unwrap();
        let gate = Gate::new(&gpu, &cohort, RenderQueue::First);
        let producer = cohort
            .submit_copy(
                RenderQueue::First,
                if pending_source {
                    image_source(direction)
                } else {
                    image_destination(direction)
                },
                &[],
            )
            .unwrap();
        let input = produced_input(&cohort, &producer);
        let request = if pending_source {
            image_request(direction, input, image_destination(direction))
        } else {
            image_request(direction, image_source(direction), input)
        };
        let consumer = cohort
            .submit_image_copy(RenderQueue::Second, request, &[])
            .unwrap();
        gate.observe_then_release(&gpu, &cohort, RenderQueue::Second);
        if producer_first {
            cohort.collect_copy(&producer).unwrap();
            drop(producer);
            check_image(cohort.collect_copy(&consumer).unwrap(), direction);
        } else {
            check_image(cohort.collect_copy(&consumer).unwrap(), direction);
            cohort.collect_copy(&producer).unwrap();
        }
    }
}

#[test]
fn queued_image_copy_rejects_pitch_bounds_kind_and_preserves_admission() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    for (offset, pitch, extent) in [
        (0, 4, [2, 2]),
        (0, 10, [2, 2]),
        (1, 12, [2, 2]),
        (0, 16388, [2, 2]),
        (0, 12, [0, 2]),
        (0, 12, [2, 0]),
        (0, 12, [3, 2]),
        (usize::MAX - 3, 12, [2, 2]),
        (0, usize::MAX - 3, [2, 2]),
    ] {
        let mut request = image_request(1, image_source(1), image_destination(1));
        request.source_location = ImageCopyLocation::Buffer {
            offset,
            bytes_per_row: pitch,
        };
        request.extent = extent;
        assert!(
            cohort
                .submit_image_copy(RenderQueue::First, request, &[])
                .is_err()
        );
    }
    for source_side in [false, true] {
        let mut request = image_request(0, image_source(0), image_destination(0));
        if source_side {
            request.source_location = ImageCopyLocation::Rgba8 { origin: [1, 0] };
        } else {
            request.destination_location = ImageCopyLocation::Rgba8 {
                origin: [usize::MAX, 0],
            };
        }
        assert!(
            cohort
                .submit_image_copy(RenderQueue::First, request, &[])
                .is_err()
        );
        let mut request = image_request(0, image_source(0), image_destination(0));
        if source_side {
            request.source_location = ImageCopyLocation::Buffer {
                offset: 0,
                bytes_per_row: 8,
            };
        } else {
            request.destination_location = ImageCopyLocation::Buffer {
                offset: 0,
                bytes_per_row: 8,
            };
        }
        assert!(
            cohort
                .submit_image_copy(RenderQueue::First, request, &[])
                .is_err()
        );
    }
    assert!(
        cohort
            .submit_image_copy(
                RenderQueue::First,
                image_request(
                    1,
                    QueuedCopyInput::HostBuffer(&PITCHED[..19]),
                    image_destination(1)
                ),
                &[]
            )
            .is_err()
    );
    assert!(
        cohort
            .submit_image_copy(
                RenderQueue::First,
                image_request(
                    2,
                    image_source(2),
                    QueuedCopyInput::HostBuffer(&BUFFER_PRIOR[..31])
                ),
                &[]
            )
            .is_err()
    );
    let mut buffers = image_request(
        1,
        image_source(1),
        QueuedCopyInput::HostBuffer(&BUFFER_PRIOR),
    );
    buffers.destination_location = ImageCopyLocation::Buffer {
        offset: 4,
        bytes_per_row: 20,
    };
    assert!(
        cohort
            .submit_image_copy(RenderQueue::First, buffers, &[])
            .is_err()
    );
    let valid = cohort
        .submit_image_copy(
            RenderQueue::First,
            image_request(1, image_source(1), image_destination(1)),
            &[],
        )
        .unwrap();
    check_image(cohort.collect_copy(&valid).unwrap(), 1);
}

#[test]
fn queued_image_copy_alias_foreign_capacity_and_controlled_failure() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let a = cohort
        .submit_copy(RenderQueue::First, image_source(0), &[])
        .unwrap();
    assert!(
        cohort
            .submit_image_copy(
                RenderQueue::Second,
                image_request(0, produced_input(&cohort, &a), produced_input(&cohort, &a)),
                &[]
            )
            .is_err()
    );
    let mut foreign = gpu.new_render_cohort().unwrap();
    for source_side in [false, true] {
        let request = if source_side {
            image_request(0, produced_input(&cohort, &a), image_destination(0))
        } else {
            image_request(0, image_source(0), produced_input(&cohort, &a))
        };
        assert!(
            foreign
                .submit_image_copy(RenderQueue::First, request, &[])
                .is_err()
        );
    }
    let b = cohort
        .submit_copy(RenderQueue::Second, image_destination(0), &[])
        .unwrap();
    // Two independent uncollected producers leave no third submission slot.
    assert!(
        cohort
            .submit_image_copy(
                RenderQueue::First,
                image_request(0, produced_input(&cohort, &a), produced_input(&cohort, &b)),
                &[]
            )
            .is_err()
    );
    let old = produced_input(&cohort, &a);
    cohort.collect_copy(&a).unwrap();
    cohort.collect_copy(&b).unwrap();
    let consumer = cohort
        .submit_image_copy(
            RenderQueue::First,
            image_request(0, old, image_destination(0)),
            &[],
        )
        .unwrap();
    let rejected = produced_input(&cohort, &consumer);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !cohort.is_complete(&consumer).unwrap() {
        assert!(
            std::time::Instant::now() < deadline,
            "image copy completion deadline"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    cohort
        .abort_and_drain("controlled image publication rejection, not GPU fault")
        .unwrap();
    assert!(cohort.collect_copy(&consumer).is_err());
    cohort.reinitialize().unwrap();
    assert!(
        cohort
            .submit_image_copy(
                RenderQueue::First,
                image_request(0, rejected, image_destination(0)),
                &[]
            )
            .is_err()
    );
    let recovery = cohort
        .submit_image_copy(
            RenderQueue::First,
            image_request(0, image_source(0), image_destination(0)),
            &[],
        )
        .unwrap();
    check_image(cohort.collect_copy(&recovery).unwrap(), 0);
}

#[test]
fn queued_image_copy_pending_output_direct_sampling_and_color_load() {
    const SHADER: &str = r"
#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; };
vertex V vs(uint i [[vertex_id]], device const float4 *p [[buffer(0)]]) { return {p[i]}; }
fragment float4 sampled(V v [[stage_in]], texture2d<float> t [[texture(0)]], sampler s [[sampler(0)]]) {
    return t.sample(s, v.position.xy / float2(2.0,2.0));
}
fragment float4 overlay() { return float4(1,0,1,1); }
";
    let gpu = NativeCompute::new().unwrap();
    for load in [false, true] {
        let mut cohort = gpu.new_render_cohort().unwrap();
        let gate = Gate::new(&gpu, &cohort, RenderQueue::First);
        let producer = cohort
            .submit_image_copy(
                RenderQueue::First,
                image_request(1, image_source(1), image_destination(1)),
                &[],
            )
            .unwrap();
        let output = match cohort.copy_output(&producer).unwrap() {
            RenderCopyOutput::Color(output) => output,
            _ => panic!("expected color output"),
        };
        let vertices = [[-1., -1., 0.5, 1.], [3., -1., 0.5, 1.], [-1., 3., 0.5, 1.]];
        let colors = [[0.; 4]; 3];
        let draws = [TriangleDraw {
            vertices: &vertices,
            colors: &colors,
            viewport: [2, 2],
        }];
        let states = [TriangleState {
            scissor: load.then_some([1, 0, 1, 1]),
            ..Default::default()
        }];
        let bindings = if load {
            vec![]
        } else {
            vec![QueuedSampledBinding::Produced {
                output: output.clone(),
                sampler: MetalSampler::new(
                    gpu.device(),
                    xenora_texture::SamplerDesc::nearest_clamp(),
                )
                .unwrap(),
                texture_slot: BindingSlot::new(0),
                sampler_slot: BindingSlot::new(0),
            }]
        };
        let pass = TriangleTargetsPass {
            draws: &draws,
            states: &states,
            extent: [2, 2],
            clears: &[[7, 11, 13, 255]],
            clear_depth: None,
        };
        let consumer = cohort
            .submit_with_attachments(
                RenderQueue::Second,
                &[TriangleShader {
                    source: SHADER,
                    entries: ["vs", if load { "overlay" } else { "sampled" }],
                }],
                &pass,
                &[bindings],
                None,
                &[if load {
                    QueuedColorLoad::Produced(output.clone())
                } else {
                    QueuedColorLoad::Clear
                }],
                &[],
                &[DrawUniforms::default()],
            )
            .unwrap();
        drop(output);
        gate.observe_then_release(&gpu, &cohort, RenderQueue::Second);
        let expected = if load {
            [
                1, 41, 81, 121, 255, 0, 255, 255, 3, 43, 83, 123, 4, 44, 84, 124,
            ]
        } else {
            PIXELS
        };
        assert_eq!(
            cohort.collect(&consumer).unwrap().colors,
            vec![expected.to_vec()]
        );
        check_image(cohort.collect_copy(&producer).unwrap(), 1);
    }
}

#[test]
fn queued_image_copy_host_snapshots_old_versions_and_stale_each_role() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    let gate = Gate::new(&gpu, &cohort, RenderQueue::First);
    let mut source = PITCHED.to_vec();
    let mut destination = PRIOR.to_vec();
    let first = cohort
        .submit_image_copy(
            RenderQueue::First,
            image_request(
                1,
                QueuedCopyInput::HostBuffer(&source),
                QueuedCopyInput::HostRgba8 {
                    extent: [2, 2],
                    pixels: &destination,
                },
            ),
            &[],
        )
        .unwrap();
    let old = match cohort.copy_output(&first).unwrap() {
        RenderCopyOutput::Color(output) => output,
        _ => panic!("expected color output"),
    };
    source.fill(199);
    destination.fill(201);
    drop(source);
    drop(destination);
    drop(gate); // Release intentionally; this case tests owned snapshots, not wait ordering.
    check_image(cohort.collect_copy(&first).unwrap(), 1);
    drop(first);
    let newer = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostRgba8 {
                extent: [2, 2],
                pixels: &PRIOR,
            },
            &[],
        )
        .unwrap();
    cohort.collect_copy(&newer).unwrap();
    // Old output remains the source despite creation/collection of a newer one.
    let copied = cohort
        .submit_image_copy(
            RenderQueue::Second,
            image_request(
                0,
                QueuedCopyInput::ProducedColor(old.clone()),
                image_destination(0),
            ),
            &[],
        )
        .unwrap();
    check_image(cohort.collect_copy(&copied).unwrap(), 0);
    // Old output also supplies untouched destination pixels in a partial update.
    let mut patch = image_request(
        0,
        image_destination(0),
        QueuedCopyInput::ProducedColor(old.clone()),
    );
    patch.extent = [1, 1];
    patch.destination_location = ImageCopyLocation::Rgba8 { origin: [1, 1] };
    let patched = cohort
        .submit_image_copy(RenderQueue::First, patch, &[])
        .unwrap();
    match cohort.collect_copy(&patched).unwrap() {
        OwnedCopyOutput::Rgba8 { extent, pixels } => {
            assert_eq!(extent, [2, 2]);
            assert_eq!(
                pixels,
                [
                    1, 41, 81, 121, 2, 42, 82, 122, 3, 43, 83, 123, 101, 111, 121, 131
                ]
            );
        }
        _ => panic!("expected color completion"),
    }
    let unchanged = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::ProducedColor(old.clone()),
            &[],
        )
        .unwrap();
    check_image(cohort.collect_copy(&unchanged).unwrap(), 0);
    cohort.reinitialize().unwrap();
    for source_side in [false, true] {
        let request = if source_side {
            image_request(
                0,
                QueuedCopyInput::ProducedColor(old.clone()),
                image_destination(0),
            )
        } else {
            image_request(
                0,
                image_source(0),
                QueuedCopyInput::ProducedColor(old.clone()),
            )
        };
        assert!(
            cohort
                .submit_image_copy(RenderQueue::First, request, &[])
                .is_err()
        );
    }
    let recovery = cohort
        .submit_image_copy(
            RenderQueue::First,
            image_request(0, image_source(0), image_destination(0)),
            &[],
        )
        .unwrap();
    check_image(cohort.collect_copy(&recovery).unwrap(), 0);
}

#[test]
fn queued_image_copy_two_row_buffer_source_nonzero_offset() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_cohort().unwrap();
    // Four-byte prefix, 12-byte row pitch, exact last-row end at byte 24.
    let bytes = [
        241, 242, 243, 244, 1, 41, 81, 121, 2, 42, 82, 122, 251, 252, 253, 254, 3, 43, 83, 123, 4,
        44, 84, 124,
    ];
    let mut request = image_request(1, QueuedCopyInput::HostBuffer(&bytes), image_destination(1));
    request.source_location = ImageCopyLocation::Buffer {
        offset: 4,
        bytes_per_row: 12,
    };
    let point = cohort
        .submit_image_copy(RenderQueue::First, request, &[])
        .unwrap();
    check_image(cohort.collect_copy(&point).unwrap(), 1);
}
