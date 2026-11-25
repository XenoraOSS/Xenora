// SPDX-License-Identifier: GPL-3.0-or-later
//! Original full-chain bytes: no expected result uses production layout/copy code.
use super::unaligned_copy::Gate;
use xenora_metal::*;
const SOURCE: [u8; 144] = [
    1, 41, 81, 121, 2, 42, 82, 122, 3, 43, 83, 123, 4, 44, 84, 124, 5, 45, 85, 125, 6, 46, 86, 126,
    7, 47, 87, 127, 8, 48, 88, 128, 9, 49, 89, 129, 10, 50, 90, 130, 11, 51, 91, 131, 12, 52, 92,
    132, 13, 53, 93, 133, 14, 54, 94, 134, 15, 55, 95, 135, 71, 81, 91, 101, 72, 82, 92, 102, 201,
    211, 221, 231, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39,
    39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39,
    39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 39, 17, 53, 127, 193, 29, 71, 131,
    197, 203, 213, 223, 233,
];
const PRIOR: [u8; 144] = [
    101, 1, 21, 241, 102, 2, 22, 242, 103, 3, 23, 243, 104, 4, 24, 244, 105, 5, 25, 245, 106, 6,
    26, 246, 107, 7, 27, 247, 108, 8, 28, 248, 109, 9, 29, 249, 110, 10, 30, 250, 111, 11, 31, 251,
    112, 12, 32, 252, 113, 13, 33, 253, 114, 14, 34, 254, 115, 15, 35, 255, 31, 41, 51, 61, 32, 42,
    52, 62, 33, 43, 53, 63, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89,
    89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89,
    89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89,
    89, 89, 89, 89, 89, 89,
];
const TEXTURE_EXPECTED: [u8; 144] = [
    101, 1, 21, 241, 102, 2, 22, 242, 103, 3, 23, 243, 104, 4, 24, 244, 105, 5, 25, 245, 106, 6,
    26, 246, 107, 7, 27, 247, 108, 8, 28, 248, 109, 9, 29, 249, 110, 10, 30, 250, 111, 11, 31, 251,
    112, 12, 32, 252, 113, 13, 33, 253, 114, 14, 34, 254, 115, 15, 35, 255, 31, 41, 51, 61, 32, 42,
    52, 62, 33, 43, 53, 63, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89,
    89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89,
    89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89,
    89, 89, 29, 71, 131, 197,
];
const UPLOAD: [u8; 12] = [231, 232, 233, 234, 9, 19, 29, 39, 10, 20, 30, 40];
const UPLOAD_EXPECTED: [u8; 144] = [
    101, 1, 21, 241, 102, 2, 22, 242, 103, 3, 23, 243, 104, 4, 24, 244, 105, 5, 25, 245, 106, 6,
    26, 246, 107, 7, 27, 247, 108, 8, 28, 248, 109, 9, 29, 249, 110, 10, 30, 250, 111, 11, 31, 251,
    112, 12, 32, 252, 113, 13, 33, 253, 114, 14, 34, 254, 115, 15, 35, 255, 31, 41, 51, 61, 32, 42,
    52, 62, 33, 43, 53, 63, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89,
    89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89,
    89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 89, 9, 19, 29, 39, 10, 20,
    30, 40, 89, 89, 89, 89,
];
const BUFFER_PRIOR: [u8; 13] = [
    101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113,
];
const BUFFER_EXPECTED: [u8; 13] = [101, 102, 103, 104, 17, 53, 127, 193, 29, 71, 131, 197, 113];
fn chain(bytes: &[u8]) -> QueuedCopyInput<'_> {
    QueuedCopyInput::HostRgba8Array {
        extent: [5, 3],
        mip_levels: 3,
        array_layers: 2,
        pixels: bytes,
    }
}
fn source(d: u8) -> QueuedCopyInput<'static> {
    if d == 1 {
        QueuedCopyInput::HostBuffer(&UPLOAD)
    } else {
        chain(&SOURCE)
    }
}
fn destination(d: u8) -> QueuedCopyInput<'static> {
    if d == 2 {
        QueuedCopyInput::HostBuffer(&BUFFER_PRIOR)
    } else {
        chain(&PRIOR)
    }
}
fn mip(level: u32, x: usize, y: usize) -> ImageCopyLocation {
    ImageCopyLocation::Rgba8Mip {
        mip: level,
        slice: 1,
        origin: [x, y],
    }
}
fn request<'a>(
    d: u8,
    source: QueuedCopyInput<'a>,
    destination: QueuedCopyInput<'a>,
) -> QueuedImageCopy<'a> {
    QueuedImageCopy {
        source,
        destination,
        source_location: if d == 1 {
            ImageCopyLocation::Buffer {
                offset: 4,
                bytes_per_row: 12,
            }
        } else {
            mip(1, usize::from(d == 0), 0)
        },
        destination_location: if d == 2 {
            ImageCopyLocation::Buffer {
                offset: 4,
                bytes_per_row: 20,
            }
        } else {
            mip(if d == 0 { 2 } else { 1 }, 0, 0)
        },
        extent: if d == 0 { [1, 1] } else { [2, 1] },
    }
}
fn produced(c: &RenderCohort, p: &RenderPoint) -> QueuedCopyInput<'static> {
    match c.copy_output(p).unwrap() {
        RenderCopyOutput::Color(v) => QueuedCopyInput::ProducedColor(v),
        RenderCopyOutput::Buffer(v) => QueuedCopyInput::ProducedBuffer(v),
    }
}
fn check(output: OwnedCopyOutput, d: u8) {
    match output {
        OwnedCopyOutput::Rgba8Array {
            extent,
            mip_levels,
            array_layers,
            pixels,
        } => {
            assert_ne!(d, 2);
            assert_eq!(extent, [5, 3]);
            assert_eq!(mip_levels, 3);
            assert_eq!(array_layers, 2);
            assert_eq!(
                pixels,
                if d == 0 {
                    TEXTURE_EXPECTED.to_vec()
                } else {
                    UPLOAD_EXPECTED.to_vec()
                }
            );
        }
        OwnedCopyOutput::Buffer(bytes) => {
            assert_eq!(d, 2);
            assert_eq!(bytes, BUFFER_EXPECTED);
        }
        _ => panic!("wrong output layout"),
    }
}
#[test]
fn native_array_transfers_literal_full_arrays_and_cross_slice_mip() {
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    for d in 0..3 {
        let p = c
            .submit_array_image_copy(
                RenderQueue::First,
                request(d, source(d), destination(d)),
                &[],
            )
            .unwrap();
        check(c.collect_copy(&p).unwrap(), d);
    }
    let p = c
        .submit_array_image_copy(
            RenderQueue::First,
            QueuedImageCopy {
                source: chain(&SOURCE),
                destination: chain(&PRIOR),
                source_location: ImageCopyLocation::Rgba8Mip {
                    mip: 1,
                    slice: 0,
                    origin: [1, 0],
                },
                destination_location: mip(0, 3, 2),
                extent: [1, 1],
            },
            &[],
        )
        .unwrap();
    let OwnedCopyOutput::Rgba8Array { pixels, .. } = c.collect_copy(&p).unwrap() else {
        panic!("array")
    };
    assert_eq!(&pixels[..124], &PRIOR[..124]);
    assert_eq!(&pixels[124..128], &[72, 82, 92, 102]);
    assert_eq!(&pixels[128..], &PRIOR[128..]);
    let padded = [
        231, 232, 233, 234, 9, 19, 29, 39, 10, 20, 30, 40, 221, 222, 223, 224, 11, 21, 31, 41, 12,
        22, 32, 42,
    ];
    let make = |bytes| QueuedImageCopy {
        source: QueuedCopyInput::HostBuffer(bytes),
        destination: chain(&PRIOR),
        source_location: ImageCopyLocation::Buffer {
            offset: 4,
            bytes_per_row: 12,
        },
        destination_location: mip(0, 1, 1),
        extent: [2, 2],
    };
    assert!(
        c.submit_array_image_copy(RenderQueue::First, make(&padded[..23]), &[])
            .is_err()
    );
    let p = c
        .submit_array_image_copy(RenderQueue::First, make(&padded), &[])
        .unwrap();
    let OwnedCopyOutput::Rgba8Array { pixels, .. } = c.collect_copy(&p).unwrap() else {
        panic!("array")
    };
    assert_eq!(&pixels[..96], &PRIOR[..96]);
    assert_eq!(&pixels[96..104], &[9, 19, 29, 39, 10, 20, 30, 40]);
    assert_eq!(&pixels[104..116], &PRIOR[104..116]);
    assert_eq!(&pixels[116..124], &[11, 21, 31, 41, 12, 22, 32, 42]);
    assert_eq!(&pixels[124..], &PRIOR[124..]);
}
// Legacy whole-copy intentionally rejects mip chains. Seed through an explicit
// mip transfer with separate host backings; the untouched full chain is cloned.
fn seed(c: &mut RenderCohort, input: QueuedCopyInput<'_>) -> RenderPoint {
    match input {
        QueuedCopyInput::HostRgba8Array {
            extent,
            mip_levels,
            array_layers,
            pixels,
        } => {
            let prior = pixels.to_vec();
            c.submit_array_image_copy(
                RenderQueue::First,
                QueuedImageCopy {
                    source: QueuedCopyInput::HostRgba8Array {
                        extent,
                        mip_levels,
                        array_layers,
                        pixels,
                    },
                    destination: QueuedCopyInput::HostRgba8Array {
                        extent,
                        mip_levels,
                        array_layers,
                        pixels: &prior,
                    },
                    source_location: mip(1, 0, 0),
                    destination_location: mip(1, 0, 0),
                    extent: [2, 1],
                },
                &[],
            )
            .unwrap()
        }
        other => c.submit_copy(RenderQueue::First, other, &[]).unwrap(),
    }
}
fn gated(d: u8, pending_source: bool) {
    for producer_first in [false, true] {
        let gpu = NativeCompute::new().unwrap();
        let mut c = gpu.new_render_graph_cohort().unwrap();
        let gate = Gate::new(&gpu, &c, RenderQueue::First);
        let a = seed(
            &mut c,
            if pending_source {
                source(d)
            } else {
                destination(d)
            },
        );
        let input = produced(&c, &a);
        let req = if pending_source {
            request(d, input, destination(d))
        } else {
            request(d, source(d), input)
        };
        let b = c
            .submit_array_image_copy(RenderQueue::Second, req, &[])
            .unwrap();
        gate.observe_then_release(&gpu, &c, RenderQueue::Second);
        if producer_first {
            c.collect_copy(&a).unwrap();
        }
        check(c.collect_copy(&b).unwrap(), d);
        if !producer_first {
            c.collect_copy(&a).unwrap();
        }
    }
}
#[test]
fn native_array_transfers_gated_texture_to_texture_source() {
    gated(0, true);
}
#[test]
fn native_array_transfers_gated_texture_to_texture_destination() {
    gated(0, false);
}
#[test]
fn native_array_transfers_gated_buffer_to_texture_source() {
    gated(1, true);
}
#[test]
fn native_array_transfers_gated_buffer_to_texture_destination() {
    gated(1, false);
}
#[test]
fn native_array_transfers_gated_texture_to_buffer_source() {
    gated(2, true);
}
#[test]
fn native_array_transfers_gated_texture_to_buffer_destination() {
    gated(2, false);
}
#[test]
fn native_array_transfers_selected_slice_to_produced_2d_sample() {
    const SHADER: &str = r"#include <metal_stdlib>
 using namespace metal; struct V{float4 p [[position]];};
 vertex V vs(uint i [[vertex_id]],device const float4*p [[buffer(0)]]){return {p[i]};}
 fragment float4 ps(V v [[stage_in]],texture2d<float>t [[texture(0)]],sampler s [[sampler(0)]]){return t.sample(s,v.p.xy/float2(2.,1.));}";
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    let gate = Gate::new(&gpu, &c, RenderQueue::First);
    let a = c
        .submit_array_image_copy(
            RenderQueue::First,
            request(1, source(1), destination(1)),
            &[],
        )
        .unwrap();
    let RenderCopyOutput::Color(old) = c.copy_output(&a).unwrap() else {
        panic!("color")
    };
    assert!(old.is_array());
    assert_eq!(old.array_layers(), 2);
    assert_eq!(old.mip_levels(), 3);
    assert!(
        old.view(TextureViewDesc {
            base_mip: 1,
            mip_count: 1,
            format: TextureViewFormat::Rgba8Unorm
        })
        .is_err()
    );
    let prior2d = [0x55; 72];
    let b = c
        .submit_array_image_copy(
            RenderQueue::First,
            QueuedImageCopy {
                source: QueuedCopyInput::ProducedColor(old.clone()),
                destination: QueuedCopyInput::HostRgba8MipChain {
                    extent: [5, 3],
                    mip_levels: 3,
                    pixels: &prior2d,
                },
                source_location: mip(1, 0, 0),
                destination_location: ImageCopyLocation::Rgba8Mip {
                    mip: 1,
                    slice: 0,
                    origin: [0, 0],
                },
                extent: [2, 1],
            },
            &[],
        )
        .unwrap();
    let RenderCopyOutput::Color(two_d) = c.copy_output(&b).unwrap() else {
        panic!("2D")
    };
    assert!(!two_d.is_array());
    assert_eq!(two_d.array_layers(), 1);
    let view = two_d
        .view(TextureViewDesc {
            base_mip: 1,
            mip_count: 2,
            format: TextureViewFormat::Rgba8Unorm,
        })
        .unwrap();
    let sampler =
        MetalSampler::new(gpu.device(), xenora_texture::SamplerDesc::nearest_clamp()).unwrap();
    let vertices = [[-1., -1., 0., 1.], [3., -1., 0., 1.], [-1., 3., 0., 1.]];
    let render = c
        .submit_with_attachments(
            RenderQueue::Second,
            &[TriangleShader {
                source: SHADER,
                entries: ["vs", "ps"],
            }],
            &TriangleTargetsPass {
                draws: &[TriangleDraw {
                    vertices: &vertices,
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
        .unwrap();
    drop(old);
    gate.observe_then_release(&gpu, &c, RenderQueue::Second);
    assert_eq!(
        c.collect(&render).unwrap().colors,
        vec![vec![9, 19, 29, 39, 10, 20, 30, 40]]
    );
    let OwnedCopyOutput::Rgba8MipChain { pixels, .. } = c.collect_copy(&b).unwrap() else {
        panic!("2D chain")
    };
    assert_eq!(&pixels[..60], &[0x55; 60]);
    assert_eq!(&pixels[60..68], &[9, 19, 29, 39, 10, 20, 30, 40]);
    assert_eq!(&pixels[68..], &[0x55; 4]);
    check(c.collect_copy(&a).unwrap(), 1);
}

#[test]
fn native_array_transfers_invalid_metadata_legacy_alias_and_stale() {
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    for (levels, layers, len) in [
        (3, 0, 144),
        (3, 257, 144),
        (4, 2, 144),
        (3, 2, 143),
        (3, 2, 145),
    ] {
        let bytes = vec![1; len];
        let input = QueuedCopyInput::HostRgba8Array {
            extent: [5, 3],
            mip_levels: levels,
            array_layers: layers,
            pixels: &bytes,
        };
        assert!(
            c.submit_array_image_copy(RenderQueue::First, request(0, input, destination(0)), &[])
                .is_err()
        );
    }
    for location in [
        ImageCopyLocation::Rgba8Mip {
            mip: 1,
            slice: 2,
            origin: [0, 0],
        },
        mip(3, 0, 0),
        mip(1, 2, 0),
        mip(1, 0, 1),
    ] {
        let mut r = request(0, source(0), destination(0));
        r.source_location = location;
        assert!(
            c.submit_array_image_copy(RenderQueue::First, r, &[])
                .is_err()
        );
    }
    let one = [0x77; 72];
    let single = || QueuedCopyInput::HostRgba8Array {
        extent: [5, 3],
        mip_levels: 3,
        array_layers: 1,
        pixels: &one,
    };
    assert!(c.submit_copy(RenderQueue::First, single(), &[]).is_err());
    let mut r = request(0, single(), destination(0));
    r.source_location = ImageCopyLocation::Rgba8Mip {
        mip: 1,
        slice: 0,
        origin: [1, 0],
    };
    assert!(c.submit_mip_image_copy(RenderQueue::First, r, &[]).is_err());
    let point = seed(&mut c, chain(&SOURCE));
    let old = produced(&c, &point);
    let RenderCopyOutput::Color(out) = c.copy_output(&point).unwrap() else {
        panic!("array")
    };
    assert!(
        c.submit_array_image_copy(
            RenderQueue::Second,
            request(
                0,
                QueuedCopyInput::ProducedColor(out.clone()),
                QueuedCopyInput::ProducedColor(out)
            ),
            &[]
        )
        .is_err()
    );
    c.collect_copy(&point).unwrap();
    c.reinitialize().unwrap();
    assert!(
        c.submit_array_image_copy(RenderQueue::First, request(0, old, destination(0)), &[])
            .is_err()
    );
    let p = c
        .submit_array_image_copy(
            RenderQueue::First,
            request(0, source(0), destination(0)),
            &[],
        )
        .unwrap();
    check(c.collect_copy(&p).unwrap(), 0);
}

#[test]
fn native_array_transfers_one_layer_output_stays_array_and_rejects_view() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_graph_cohort().unwrap();
    let source = [17, 53, 127, 193];
    let prior = [89; 72];
    let point = cohort
        .submit_array_image_copy(
            RenderQueue::First,
            QueuedImageCopy {
                source: QueuedCopyInput::HostBuffer(&source),
                destination: QueuedCopyInput::HostRgba8Array {
                    extent: [5, 3],
                    mip_levels: 3,
                    array_layers: 1,
                    pixels: &prior,
                },
                source_location: ImageCopyLocation::Buffer {
                    offset: 0,
                    bytes_per_row: 4,
                },
                destination_location: ImageCopyLocation::Rgba8Mip {
                    mip: 2,
                    slice: 0,
                    origin: [0, 0],
                },
                extent: [1, 1],
            },
            &[],
        )
        .unwrap();
    let RenderCopyOutput::Color(output) = cohort.copy_output(&point).unwrap() else {
        panic!("color output")
    };
    assert!(output.is_array());
    assert_eq!(output.array_layers(), 1);
    assert_eq!(output.mip_levels(), 3);
    assert_eq!(output.extent(), [5, 3]);
    assert!(
        output
            .view(TextureViewDesc {
                base_mip: 0,
                mip_count: 3,
                format: TextureViewFormat::Rgba8Unorm
            })
            .is_err()
    );
    let OwnedCopyOutput::Rgba8Array {
        extent,
        mip_levels,
        array_layers,
        pixels,
    } = cohort.collect_copy(&point).unwrap()
    else {
        panic!("one-layer arrays must retain array output kind")
    };
    assert_eq!(extent, [5, 3]);
    assert_eq!(mip_levels, 3);
    assert_eq!(array_layers, 1);
    assert_eq!(&pixels[..68], &[89; 68]);
    assert_eq!(&pixels[68..], &[17, 53, 127, 193]);
}
