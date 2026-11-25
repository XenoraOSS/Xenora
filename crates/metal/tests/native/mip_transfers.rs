// SPDX-License-Identifier: GPL-3.0-or-later
//! Original full-chain bytes: no expected result uses production layout/copy code.
use super::unaligned_copy::Gate;
use xenora_metal::*;
const SOURCE: [u8; 72] = [
    1, 41, 81, 121, 2, 42, 82, 122, 3, 43, 83, 123, 4, 44, 84, 124, 5, 45, 85, 125, 6, 46, 86, 126,
    7, 47, 87, 127, 8, 48, 88, 128, 9, 49, 89, 129, 10, 50, 90, 130, 11, 51, 91, 131, 12, 52, 92,
    132, 13, 53, 93, 133, 14, 54, 94, 134, 15, 55, 95, 135, 71, 81, 91, 101, 72, 82, 92, 102, 201,
    211, 221, 231,
];
const PRIOR: [u8; 72] = [
    101, 1, 21, 241, 102, 2, 22, 242, 103, 3, 23, 243, 104, 4, 24, 244, 105, 5, 25, 245, 106, 6,
    26, 246, 107, 7, 27, 247, 108, 8, 28, 248, 109, 9, 29, 249, 110, 10, 30, 250, 111, 11, 31, 251,
    112, 12, 32, 252, 113, 13, 33, 253, 114, 14, 34, 254, 115, 15, 35, 255, 31, 41, 51, 61, 32, 42,
    52, 62, 33, 43, 53, 63,
];
const TEXTURE_EXPECTED: [u8; 72] = [
    101, 1, 21, 241, 102, 2, 22, 242, 103, 3, 23, 243, 104, 4, 24, 244, 105, 5, 25, 245, 106, 6,
    26, 246, 107, 7, 27, 247, 108, 8, 28, 248, 109, 9, 29, 249, 110, 10, 30, 250, 111, 11, 31, 251,
    112, 12, 32, 252, 113, 13, 33, 253, 114, 14, 34, 254, 115, 15, 35, 255, 31, 41, 51, 61, 32, 42,
    52, 62, 72, 82, 92, 102,
];
const UPLOAD: [u8; 12] = [231, 232, 233, 234, 9, 19, 29, 39, 10, 20, 30, 40];
const UPLOAD_EXPECTED: [u8; 72] = [
    101, 1, 21, 241, 102, 2, 22, 242, 103, 3, 23, 243, 104, 4, 24, 244, 105, 5, 25, 245, 106, 6,
    26, 246, 107, 7, 27, 247, 108, 8, 28, 248, 109, 9, 29, 249, 110, 10, 30, 250, 111, 11, 31, 251,
    112, 12, 32, 252, 113, 13, 33, 253, 114, 14, 34, 254, 115, 15, 35, 255, 9, 19, 29, 39, 10, 20,
    30, 40, 33, 43, 53, 63,
];
const BUFFER_PRIOR: [u8; 13] = [
    101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113,
];
const BUFFER_EXPECTED: [u8; 13] = [101, 102, 103, 104, 71, 81, 91, 101, 72, 82, 92, 102, 113];
fn chain(bytes: &[u8]) -> QueuedCopyInput<'_> {
    QueuedCopyInput::HostRgba8MipChain {
        extent: [5, 3],
        mip_levels: 3,
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
        slice: 0,
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
        OwnedCopyOutput::Rgba8MipChain {
            extent,
            mip_levels,
            pixels,
        } => {
            assert_ne!(d, 2);
            assert_eq!(extent, [5, 3]);
            assert_eq!(mip_levels, 3);
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
fn native_mip_transfers_literal_three_directions_and_full_chain() {
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    for d in 0..3 {
        let p = c
            .submit_mip_image_copy(
                RenderQueue::First,
                request(d, source(d), destination(d)),
                &[],
            )
            .unwrap();
        check(c.collect_copy(&p).unwrap(), d);
    }
    // Two rows with a non256 pitch, exact final row end, and untouched root border.
    let bytes = [
        231, 232, 233, 234, 9, 19, 29, 39, 10, 20, 30, 40, 221, 222, 223, 224, 11, 21, 31, 41, 12,
        22, 32, 42,
    ];
    let p = c
        .submit_mip_image_copy(
            RenderQueue::First,
            QueuedImageCopy {
                source: QueuedCopyInput::HostBuffer(&bytes),
                destination: chain(&PRIOR),
                source_location: ImageCopyLocation::Buffer {
                    offset: 4,
                    bytes_per_row: 12,
                },
                destination_location: mip(0, 1, 1),
                extent: [2, 2],
            },
            &[],
        )
        .unwrap();
    let OwnedCopyOutput::Rgba8MipChain { pixels, .. } = c.collect_copy(&p).unwrap() else {
        panic!("chain")
    };
    assert_eq!(
        pixels,
        [
            101, 1, 21, 241, 102, 2, 22, 242, 103, 3, 23, 243, 104, 4, 24, 244, 105, 5, 25, 245,
            106, 6, 26, 246, 9, 19, 29, 39, 10, 20, 30, 40, 109, 9, 29, 249, 110, 10, 30, 250, 111,
            11, 31, 251, 11, 21, 31, 41, 12, 22, 32, 42, 114, 14, 34, 254, 115, 15, 35, 255, 31,
            41, 51, 61, 32, 42, 52, 62, 33, 43, 53, 63
        ]
    );
    let mut distinctive = SOURCE;
    distinctive[64..68].copy_from_slice(&[17, 53, 127, 193]);
    let p = c
        .submit_mip_image_copy(
            RenderQueue::First,
            QueuedImageCopy {
                source: chain(&distinctive),
                destination: chain(&PRIOR),
                source_location: mip(1, 1, 0),
                destination_location: mip(0, 3, 2),
                extent: [1, 1],
            },
            &[],
        )
        .unwrap();
    let OwnedCopyOutput::Rgba8MipChain { pixels, .. } = c.collect_copy(&p).unwrap() else {
        panic!("chain")
    };
    assert_eq!(&pixels[..52], &PRIOR[..52]);
    assert_eq!(&pixels[52..56], &[17, 53, 127, 193]);
    assert_eq!(&pixels[56..], &PRIOR[56..]);
    assert!(
        c.submit_mip_image_copy(
            RenderQueue::First,
            QueuedImageCopy {
                source: QueuedCopyInput::HostBuffer(&bytes[..23]),
                destination: chain(&PRIOR),
                source_location: ImageCopyLocation::Buffer {
                    offset: 4,
                    bytes_per_row: 12
                },
                destination_location: mip(0, 1, 1),
                extent: [2, 2]
            },
            &[]
        )
        .is_err()
    );
}
// Legacy whole-copy intentionally rejects mip chains. Seed through an explicit
// mip transfer with separate host backings; the untouched full chain is cloned.
fn seed(c: &mut RenderCohort, input: QueuedCopyInput<'_>) -> RenderPoint {
    match input {
        QueuedCopyInput::HostRgba8MipChain {
            extent,
            mip_levels,
            pixels,
        } => {
            let prior = pixels.to_vec();
            c.submit_mip_image_copy(
                RenderQueue::First,
                QueuedImageCopy {
                    source: QueuedCopyInput::HostRgba8MipChain {
                        extent,
                        mip_levels,
                        pixels,
                    },
                    destination: QueuedCopyInput::HostRgba8MipChain {
                        extent,
                        mip_levels,
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
            .submit_mip_image_copy(RenderQueue::Second, req, &[])
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
fn native_mip_transfers_gated_texture_to_texture_source() {
    gated(0, true);
}
#[test]
fn native_mip_transfers_gated_texture_to_texture_destination() {
    gated(0, false);
}
#[test]
fn native_mip_transfers_gated_buffer_to_texture_source() {
    gated(1, true);
}
#[test]
fn native_mip_transfers_gated_buffer_to_texture_destination() {
    gated(1, false);
}
#[test]
fn native_mip_transfers_gated_texture_to_buffer_source() {
    gated(2, true);
}
#[test]
fn native_mip_transfers_gated_texture_to_buffer_destination() {
    gated(2, false);
}
#[test]
fn native_mip_transfers_produced_nonzero_view_and_exact_old_version() {
    const SHADER: &str = r"#include <metal_stdlib>
 using namespace metal; struct V{float4 p [[position]];};
 vertex V vs(uint i [[vertex_id]],device const float4*p [[buffer(0)]]){return {p[i]};}
 fragment float4 ps(V v [[stage_in]],texture2d<float>t [[texture(0)]],sampler s [[sampler(0)]]){return t.sample(s,v.p.xy/float2(2.,1.));}";
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    let gate = Gate::new(&gpu, &c, RenderQueue::First);
    let a = c
        .submit_mip_image_copy(
            RenderQueue::First,
            request(1, source(1), destination(1)),
            &[],
        )
        .unwrap();
    let RenderCopyOutput::Color(old) = c.copy_output(&a).unwrap() else {
        panic!("color")
    };
    let view = old
        .view(TextureViewDesc {
            base_mip: 1,
            mip_count: 2,
            format: TextureViewFormat::Rgba8Unorm,
        })
        .unwrap();
    let replacement = [171, 181, 191, 201];
    let b = c
        .submit_mip_image_copy(
            RenderQueue::First,
            QueuedImageCopy {
                source: QueuedCopyInput::HostBuffer(&replacement),
                destination: QueuedCopyInput::ProducedColor(old.clone()),
                source_location: ImageCopyLocation::Buffer {
                    offset: 0,
                    bytes_per_row: 4,
                },
                destination_location: mip(1, 0, 0),
                extent: [1, 1],
            },
            &[],
        )
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
        panic!("chain")
    };
    assert_eq!(
        &pixels[60..],
        [171, 181, 191, 201, 10, 20, 30, 40, 33, 43, 53, 63]
    );
    assert_eq!(&pixels[..60], &PRIOR[..60]);
    check(c.collect_copy(&a).unwrap(), 1);
}
#[test]
fn native_mip_transfers_reject_layout_bounds_slice_and_stale_inputs() {
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    for (location, extent) in [
        (mip(3, 0, 0), [1, 1]),
        (mip(1, 2, 0), [1, 1]),
        (mip(1, 1, 0), [2, 1]),
        (mip(1, 0, 1), [1, 1]),
        (
            ImageCopyLocation::Rgba8Mip {
                mip: 1,
                slice: 1,
                origin: [0, 0],
            },
            [1, 1],
        ),
        (mip(1, usize::MAX, 0), [1, 1]),
    ] {
        let mut r = request(0, source(0), destination(0));
        r.source_location = location;
        r.extent = extent;
        assert!(c.submit_mip_image_copy(RenderQueue::First, r, &[]).is_err());
    }
    for bytes in [&SOURCE[..71], &[0u8; 73][..]] {
        assert!(
            c.submit_mip_image_copy(
                RenderQueue::First,
                request(0, chain(bytes), destination(0)),
                &[]
            )
            .is_err()
        );
    }
    for (offset, pitch) in [(4, 4), (4, 6), (usize::MAX, 12), (5, 12)] {
        let mut r = request(1, source(1), destination(1));
        r.source_location = ImageCopyLocation::Buffer {
            offset,
            bytes_per_row: pitch,
        };
        assert!(c.submit_mip_image_copy(RenderQueue::First, r, &[]).is_err());
    }
    let point = seed(&mut c, chain(&SOURCE));
    let old = produced(&c, &point);
    c.collect_copy(&point).unwrap();
    c.reinitialize().unwrap();
    assert!(
        c.submit_mip_image_copy(RenderQueue::First, request(0, old, destination(0)), &[])
            .is_err()
    );
    let p = c
        .submit_mip_image_copy(
            RenderQueue::First,
            request(0, source(0), destination(0)),
            &[],
        )
        .unwrap();
    check(c.collect_copy(&p).unwrap(), 0);
}
