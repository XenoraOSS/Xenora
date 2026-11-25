// SPDX-License-Identifier: GPL-3.0-or-later
//! Literal byte oracles: no production layout, conversion, or decompression helper.
use super::unaligned_copy::Gate;
use xenora_metal::*;
use xenora_texture::TextureFormat::{self, *};
struct Case {
    format: TextureFormat,
    unit: usize,
    bytes: usize,
    source: usize,
    destination: usize,
    compressed: bool,
    pattern: &'static [u8],
}
fn cases() -> [Case; 10] {
    [
        Case {
            format: Rgba8Unorm,
            unit: 4,
            bytes: 144,
            source: 64,
            destination: 140,
            compressed: false,
            pattern: &[17, 53, 127, 193],
        },
        Case {
            format: Bc1RgbaUnorm,
            unit: 8,
            bytes: 64,
            source: 16,
            destination: 48,
            compressed: true,
            pattern: &[0x00, 0xf8, 0xe0, 0x07, 0xe4, 0x1b, 0xb1, 0x4e],
        },
        Case {
            format: Bc3RgbaUnorm,
            unit: 16,
            bytes: 128,
            source: 32,
            destination: 96,
            compressed: true,
            pattern: &[
                231, 17, 0x88, 0xc6, 0xfa, 0x88, 0xc6, 0xfa, 0x1f, 0, 0, 0xf8, 0xe4, 0x1b, 0xb1,
                0x4e,
            ],
        },
        Case {
            format: R8Unorm,
            unit: 1,
            bytes: 36,
            source: 16,
            destination: 35,
            compressed: false,
            pattern: &[127],
        },
        Case {
            format: Rg8Unorm,
            unit: 2,
            bytes: 72,
            source: 32,
            destination: 70,
            compressed: false,
            pattern: &[17, 193],
        },
        Case {
            format: Bgra8Unorm,
            unit: 4,
            bytes: 144,
            source: 64,
            destination: 140,
            compressed: false,
            pattern: &[17, 53, 127, 193],
        },
        Case {
            format: Rgba16Float,
            unit: 8,
            bytes: 288,
            source: 128,
            destination: 280,
            compressed: false,
            pattern: &[0, 0xbc, 0, 0x40, 0, 0x80, 0x35, 0x7e],
        },
        Case {
            format: R32Float,
            unit: 4,
            bytes: 144,
            source: 64,
            destination: 140,
            compressed: false,
            pattern: &[0x45, 0x23, 0xc1, 0x7f],
        },
        Case {
            format: R32Uint,
            unit: 4,
            bytes: 144,
            source: 64,
            destination: 140,
            compressed: false,
            pattern: &[1, 0, 0, 1],
        },
        Case {
            format: Depth32Float,
            unit: 4,
            bytes: 144,
            source: 64,
            destination: 140,
            compressed: false,
            pattern: &[0x45, 0x23, 0xc1, 0x7f],
        },
    ]
}
fn desc(c: &Case) -> TypedTextureDesc {
    TypedTextureDesc {
        format: c.format,
        extent: [5, 3],
        mip_levels: 3,
        array_layers: Some(2),
    }
}
fn loc(mip: u32, slice: u32, x: usize, y: usize) -> TypedImageCopyLocation {
    TypedImageCopyLocation::Texture {
        mip,
        slice,
        origin: [x, y],
    }
}
fn inputs(c: &Case) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut source = vec![0x39; c.bytes];
    source[c.source..c.source + c.unit].copy_from_slice(c.pattern);
    let prior = vec![0xa5; c.bytes];
    let mut expected = prior.clone();
    expected[c.destination..c.destination + c.unit].copy_from_slice(c.pattern);
    (source, prior, expected)
}
fn request<'a>(
    c: &Case,
    source: TypedCopyInput<'a>,
    destination: TypedCopyInput<'a>,
) -> QueuedTypedImageCopy<'a> {
    QueuedTypedImageCopy {
        source,
        destination,
        source_location: loc(1, 0, usize::from(!c.compressed), 0),
        destination_location: loc(if c.compressed { 1 } else { 2 }, 1, 0, 0),
        extent: if c.compressed { [2, 1] } else { [1, 1] },
    }
}
fn host<'a>(c: &Case, bytes: &'a [u8]) -> TypedCopyInput<'a> {
    TypedCopyInput::HostTexture {
        desc: desc(c),
        bytes,
    }
}
fn check(c: &Case, output: OwnedCopyOutput, expected: &[u8]) {
    let OwnedCopyOutput::TypedTexture {
        desc: actual,
        bytes,
    } = output
    else {
        panic!("typed output")
    };
    assert_eq!(actual, desc(c));
    assert_eq!(bytes, expected, "{:?}", c.format);
}
fn enabled(gpu: &NativeCompute, c: &Case) -> bool {
    !c.compressed || gpu.supports_bc_textures()
}
#[test]
fn native_typed_transfers_all_formats_full_mips_arrays_three_directions() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_graph_cohort().unwrap();
    for c in cases() {
        if !enabled(&gpu, &c) {
            continue;
        }
        let (source, prior, expected) = inputs(&c);
        let p = cohort
            .submit_typed_image_copy(
                RenderQueue::First,
                request(&c, host(&c, &source), host(&c, &prior)),
                &[],
            )
            .unwrap();
        check(&c, cohort.collect_copy(&p).unwrap(), &expected);
        let mut upload = vec![0xee; 2 * c.unit];
        upload[c.unit..].copy_from_slice(c.pattern);
        let mut r = request(&c, TypedCopyInput::HostBuffer(&upload), host(&c, &prior));
        r.source_location = TypedImageCopyLocation::Buffer {
            offset: c.unit,
            bytes_per_row: 3 * c.unit,
        };
        let p = cohort
            .submit_typed_image_copy(RenderQueue::First, r, &[])
            .unwrap();
        check(&c, cohort.collect_copy(&p).unwrap(), &expected);
        let buffer = vec![0xa5; 2 * c.unit + 1];
        let mut r = request(&c, host(&c, &source), TypedCopyInput::HostBuffer(&buffer));
        r.destination_location = TypedImageCopyLocation::Buffer {
            offset: c.unit,
            bytes_per_row: 3 * c.unit,
        };
        let p = cohort
            .submit_typed_image_copy(RenderQueue::First, r, &[])
            .unwrap();
        let OwnedCopyOutput::Buffer(bytes) = cohort.collect_copy(&p).unwrap() else {
            panic!("buffer")
        };
        assert_eq!(&bytes[..c.unit], &buffer[..c.unit]);
        assert_eq!(&bytes[c.unit..2 * c.unit], c.pattern);
        assert_eq!(&bytes[2 * c.unit..], &[0xa5]);
    }
}
fn gated(pending_source: bool) {
    let gpu = NativeCompute::new().unwrap();
    for c in cases() {
        if !enabled(&gpu, &c) {
            continue;
        }
        for producer_first in [false, true] {
            let (mut source, mut prior, expected) = inputs(&c);
            let seed = if pending_source {
                source.clone()
            } else {
                prior.clone()
            };
            let mut cohort = gpu.new_render_graph_cohort().unwrap();
            let gate = Gate::new(&gpu, &cohort, RenderQueue::First);
            // Full-array seed clones all subresources and patches one whole final mip.
            let seed_prior = seed.clone();
            let a = cohort
                .submit_typed_image_copy(
                    RenderQueue::First,
                    QueuedTypedImageCopy {
                        source: host(&c, &seed),
                        destination: host(&c, &seed_prior),
                        source_location: loc(2, 1, 0, 0),
                        destination_location: loc(2, 1, 0, 0),
                        extent: [1, 1],
                    },
                    &[],
                )
                .unwrap();
            let RenderCopyOutput::TypedTexture(output) = cohort.copy_output(&a).unwrap() else {
                panic!("typed")
            };
            assert_eq!(output.desc(), desc(&c));
            let r = if pending_source {
                request(
                    &c,
                    TypedCopyInput::ProducedTexture(output),
                    host(&c, &prior),
                )
            } else {
                request(
                    &c,
                    host(&c, &source),
                    TypedCopyInput::ProducedTexture(output),
                )
            };
            let b = cohort
                .submit_typed_image_copy(RenderQueue::Second, r, &[])
                .unwrap();
            source.fill(0xdd);
            prior.fill(0xcc);
            gate.observe_then_release(&gpu, &cohort, RenderQueue::Second);
            if producer_first {
                cohort.collect_copy(&a).unwrap();
            }
            check(&c, cohort.collect_copy(&b).unwrap(), &expected);
            if !producer_first {
                cohort.collect_copy(&a).unwrap();
            }
        }
    }
}
#[test]
fn native_typed_transfers_gated_source_each_format() {
    gated(true);
}
#[test]
fn native_typed_transfers_gated_destination_each_format() {
    gated(false);
}
#[test]
fn native_typed_transfers_odd_pitches_exact_tail_and_float_uint_bits() {
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    for (format, source, offset, pitch, prior, expected) in [
        (
            R8Unorm,
            vec![0xee, 17, 53, 127, 0xee, 0xee, 193, 29, 71],
            1,
            5,
            vec![0xa5; 6],
            vec![17, 53, 127, 193, 29, 71],
        ),
        (
            Rg8Unorm,
            vec![
                0xee, 0xee, 17, 193, 53, 127, 29, 71, 0xee, 0xee, 0xee, 0xee, 131, 197, 7, 241, 11,
                239,
            ],
            2,
            10,
            vec![0xa5; 12],
            vec![17, 193, 53, 127, 29, 71, 131, 197, 7, 241, 11, 239],
        ),
    ] {
        let d = TypedTextureDesc {
            format,
            extent: [3, 2],
            mip_levels: 1,
            array_layers: None,
        };
        let make = |bytes| QueuedTypedImageCopy {
            source: TypedCopyInput::HostBuffer(bytes),
            destination: TypedCopyInput::HostTexture {
                desc: d,
                bytes: &prior,
            },
            source_location: TypedImageCopyLocation::Buffer {
                offset,
                bytes_per_row: pitch,
            },
            destination_location: loc(0, 0, 0, 0),
            extent: [3, 2],
        };
        assert!(
            c.submit_typed_image_copy(RenderQueue::First, make(&source[..source.len() - 1]), &[])
                .is_err()
        );
        let p = c
            .submit_typed_image_copy(RenderQueue::First, make(&source), &[])
            .unwrap();
        let OwnedCopyOutput::TypedTexture { bytes, .. } = c.collect_copy(&p).unwrap() else {
            panic!("typed")
        };
        assert_eq!(bytes, expected);
    }
    for format in [R32Float, R32Uint, Depth32Float] {
        let bytes = [
            1, 0, 0, 1, 1, 0, 0, 128, 255, 255, 255, 255, 0x45, 0x23, 0xc1, 0x7f, 0, 0, 0x80, 0xbf,
            0, 0, 0, 0x40, 0, 0, 0, 0x80, 0, 0, 0x80, 0x3e, 0, 0, 0x40, 0x3f,
        ];
        let prior = [0xa5; 36];
        let d = TypedTextureDesc {
            format,
            extent: [9, 1],
            mip_levels: 1,
            array_layers: None,
        };
        let p = c
            .submit_typed_image_copy(
                RenderQueue::First,
                QueuedTypedImageCopy {
                    source: TypedCopyInput::HostBuffer(&bytes),
                    destination: TypedCopyInput::HostTexture {
                        desc: d,
                        bytes: &prior,
                    },
                    source_location: TypedImageCopyLocation::Buffer {
                        offset: 0,
                        bytes_per_row: 36,
                    },
                    destination_location: loc(0, 0, 0, 0),
                    extent: [9, 1],
                },
                &[],
            )
            .unwrap();
        let OwnedCopyOutput::TypedTexture { bytes: actual, .. } = c.collect_copy(&p).unwrap()
        else {
            panic!("typed")
        };
        assert_eq!(actual, bytes);
    }
}
#[test]
fn native_typed_transfers_same_size_format_alias_bounds_and_array_one() {
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    let rgba = TypedTextureDesc {
        format: Rgba8Unorm,
        extent: [2, 1],
        mip_levels: 1,
        array_layers: None,
    };
    let source = [17, 53, 127, 193, 29, 71, 131, 197];
    let prior = [0xa5; 8];
    for format in [Bgra8Unorm, R32Float, R32Uint, Depth32Float] {
        assert!(
            c.submit_typed_image_copy(
                RenderQueue::First,
                QueuedTypedImageCopy {
                    source: TypedCopyInput::HostTexture {
                        desc: rgba,
                        bytes: &source
                    },
                    destination: TypedCopyInput::HostTexture {
                        desc: TypedTextureDesc { format, ..rgba },
                        bytes: &prior
                    },
                    source_location: loc(0, 0, 0, 0),
                    destination_location: loc(0, 0, 0, 0),
                    extent: [2, 1]
                },
                &[]
            )
            .is_err()
        );
    }
    let array = TypedTextureDesc {
        array_layers: Some(1),
        ..rgba
    };
    let p = c
        .submit_typed_image_copy(
            RenderQueue::First,
            QueuedTypedImageCopy {
                source: TypedCopyInput::HostTexture {
                    desc: rgba,
                    bytes: &source,
                },
                destination: TypedCopyInput::HostTexture {
                    desc: array,
                    bytes: &prior,
                },
                source_location: loc(0, 0, 0, 0),
                destination_location: loc(0, 0, 0, 0),
                extent: [2, 1],
            },
            &[],
        )
        .unwrap();
    let RenderCopyOutput::TypedTexture(out) = c.copy_output(&p).unwrap() else {
        panic!("typed")
    };
    assert_eq!(out.desc(), array);
    assert!(out.depth_output().is_err());
    assert!(
        c.submit_typed_image_copy(
            RenderQueue::Second,
            QueuedTypedImageCopy {
                source: TypedCopyInput::ProducedTexture(out.clone()),
                destination: TypedCopyInput::ProducedTexture(out.clone()),
                source_location: loc(0, 0, 0, 0),
                destination_location: loc(0, 0, 0, 0),
                extent: [1, 1]
            },
            &[]
        )
        .is_err()
    );
    c.collect_copy(&p).unwrap();
    c.reinitialize().unwrap();
    assert!(
        c.submit_typed_image_copy(
            RenderQueue::First,
            QueuedTypedImageCopy {
                source: TypedCopyInput::ProducedTexture(out),
                destination: TypedCopyInput::HostTexture {
                    desc: rgba,
                    bytes: &prior
                },
                source_location: loc(0, 0, 0, 0),
                destination_location: loc(0, 0, 0, 0),
                extent: [1, 1]
            },
            &[]
        )
        .is_err()
    );
}

#[test]
fn native_typed_transfers_bc_edge_blocks_and_interior_partial_rejection() {
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    for case in cases().into_iter().filter(|c| c.compressed) {
        if !enabled(&gpu, &case) {
            continue;
        }
        let d = TypedTextureDesc {
            format: case.format,
            extent: [5, 3],
            mip_levels: 1,
            array_layers: None,
        };
        let mut source = vec![0x39; 2 * case.unit];
        source[case.unit..].copy_from_slice(case.pattern);
        let prior = vec![0xa5; 2 * case.unit];
        let make = |origin, extent| QueuedTypedImageCopy {
            source: TypedCopyInput::HostTexture {
                desc: d,
                bytes: &source,
            },
            destination: TypedCopyInput::HostTexture {
                desc: d,
                bytes: &prior,
            },
            source_location: loc(0, 0, origin, 0),
            destination_location: loc(0, 0, 4, 0),
            extent,
        };
        for (origin, extent) in [(1, [1, 3]), (0, [1, 3]), (4, [1, 2]), (4, [2, 3])] {
            assert!(
                c.submit_typed_image_copy(RenderQueue::First, make(origin, extent), &[])
                    .is_err()
            );
        }
        let p = c
            .submit_typed_image_copy(RenderQueue::First, make(4, [1, 3]), &[])
            .unwrap();
        let OwnedCopyOutput::TypedTexture { bytes, .. } = c.collect_copy(&p).unwrap() else {
            panic!("typed")
        };
        assert_eq!(&bytes[..case.unit], &prior[..case.unit]);
        assert_eq!(&bytes[case.unit..], case.pattern);
    }
}

#[test]
fn native_typed_transfers_render_depth_copy_then_load_literal_planes() {
    const SHADER: &str = r"#include <metal_stdlib>
using namespace metal;struct V{float4 p [[position]];};vertex V vs(uint i [[vertex_id]],device const float4*p [[buffer(0)]]){return {p[i]};}fragment float4 ps(){return float4(1,0,0,1);}";
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    let gate = Gate::new(&gpu, &c, RenderQueue::First);
    let left = [
        [-1., -1., 0.25, 1.],
        [3., -1., 0.25, 1.],
        [-1., 3., 0.25, 1.],
    ];
    let right = left.map(|[x, y, _, w]| [x, y, 0.75, w]);
    let states = [
        TriangleState {
            depth: DepthState {
                compare: DepthCompare::Less,
                write: true,
            },
            scissor: Some([0, 0, 1, 1]),
            ..Default::default()
        },
        TriangleState {
            depth: DepthState {
                compare: DepthCompare::Less,
                write: true,
            },
            scissor: Some([1, 0, 1, 1]),
            ..Default::default()
        },
    ];
    let producer = c
        .submit_with_attachments(
            RenderQueue::First,
            &[TriangleShader {
                source: SHADER,
                entries: ["vs", "ps"],
            }; 2],
            &TriangleTargetsPass {
                draws: &[
                    TriangleDraw {
                        vertices: &left,
                        colors: &[[0.; 4]; 3],
                        viewport: [2, 1],
                    },
                    TriangleDraw {
                        vertices: &right,
                        colors: &[[0.; 4]; 3],
                        viewport: [2, 1],
                    },
                ],
                states: &states,
                extent: [2, 1],
                clears: &[[0; 4]],
                clear_depth: None,
            },
            &[vec![], vec![]],
            Some(QueuedDepthInput::Clear(1.)),
            &[QueuedColorLoad::Clear],
            &[],
            &[DrawUniforms::default(); 2],
        )
        .unwrap();
    let depth = c.depth_output(&producer).unwrap();
    let d = TypedTextureDesc {
        format: Depth32Float,
        extent: [2, 1],
        mip_levels: 1,
        array_layers: None,
    };
    let copy = c
        .submit_typed_image_copy(
            RenderQueue::Second,
            QueuedTypedImageCopy {
                source: TypedCopyInput::ProducedDepth(depth),
                destination: TypedCopyInput::HostTexture {
                    desc: d,
                    bytes: &[0; 8],
                },
                source_location: loc(0, 0, 0, 0),
                destination_location: loc(0, 0, 0, 0),
                extent: [2, 1],
            },
            &[],
        )
        .unwrap();
    let RenderCopyOutput::TypedTexture(out) = c.copy_output(&copy).unwrap() else {
        panic!("typed depth")
    };
    let copied = out.depth_output().unwrap();
    let probe = left.map(|[x, y, _, w]| [x, y, 0.5, w]);
    let consumer = c
        .submit_with_attachments(
            RenderQueue::Second,
            &[TriangleShader {
                source: SHADER,
                entries: ["vs", "ps"],
            }],
            &TriangleTargetsPass {
                draws: &[TriangleDraw {
                    vertices: &probe,
                    colors: &[[0.; 4]; 3],
                    viewport: [2, 1],
                }],
                states: &[TriangleState {
                    depth: DepthState {
                        compare: DepthCompare::Less,
                        write: false,
                    },
                    ..Default::default()
                }],
                extent: [2, 1],
                clears: &[[7, 11, 13, 255]],
                clear_depth: None,
            },
            &[vec![]],
            Some(QueuedDepthInput::Produced(copied)),
            &[QueuedColorLoad::Clear],
            &[],
            &[DrawUniforms::default()],
        )
        .unwrap();
    drop(out);
    gate.observe_then_release(&gpu, &c, RenderQueue::Second);
    assert_eq!(
        c.collect(&consumer).unwrap().colors,
        vec![vec![7, 11, 13, 255, 255, 0, 0, 255]]
    );
    let OwnedCopyOutput::TypedTexture { bytes, .. } = c.collect_copy(&copy).unwrap() else {
        panic!("depth")
    };
    assert_eq!(bytes, [0, 0, 128, 62, 0, 0, 64, 63]);
    c.collect(&producer).unwrap();
}

#[test]
fn native_typed_transfers_multirow_buffer_edges_and_bc_blocks() {
    let gpu = NativeCompute::new().unwrap();
    for (format, extent, row, rows, offset, pitch) in [
        (R8Unorm, [3, 2], 3, 2, 1, 5),
        (Rg8Unorm, [3, 2], 6, 2, 2, 10),
        (Bc1RgbaUnorm, [5, 5], 16, 2, 8, 24),
        (Bc3RgbaUnorm, [5, 5], 32, 2, 16, 48),
    ] {
        if matches!(format, Bc1RgbaUnorm | Bc3RgbaUnorm) && !gpu.supports_bc_textures() {
            continue;
        }
        let desc = TypedTextureDesc {
            format,
            extent,
            mip_levels: 1,
            array_layers: None,
        };
        // Distinct raw block rows; transfer oracles intentionally do not decode BC.
        let packed = (0..row * rows)
            .map(|i| (17 + i * 3) as u8)
            .collect::<Vec<_>>();
        let end = offset + pitch * (rows - 1) + row;
        let mut pitched = vec![0xa5; end];
        for y in 0..rows {
            pitched[offset + y * pitch..offset + y * pitch + row]
                .copy_from_slice(&packed[y * row..(y + 1) * row]);
        }
        let mut c = gpu.new_render_graph_cohort().unwrap();
        for to_buffer in [false, true] {
            let prior = vec![0x59; if to_buffer { end } else { packed.len() }];
            let make = |short: bool| {
                let buffer = if to_buffer {
                    &prior[..end - usize::from(short)]
                } else {
                    &pitched[..end - usize::from(short)]
                };
                if to_buffer {
                    QueuedTypedImageCopy {
                        source: TypedCopyInput::HostTexture {
                            desc,
                            bytes: &packed,
                        },
                        destination: TypedCopyInput::HostBuffer(buffer),
                        source_location: loc(0, 0, 0, 0),
                        destination_location: TypedImageCopyLocation::Buffer {
                            offset,
                            bytes_per_row: pitch,
                        },
                        extent,
                    }
                } else {
                    QueuedTypedImageCopy {
                        source: TypedCopyInput::HostBuffer(buffer),
                        destination: TypedCopyInput::HostTexture {
                            desc,
                            bytes: &prior,
                        },
                        source_location: TypedImageCopyLocation::Buffer {
                            offset,
                            bytes_per_row: pitch,
                        },
                        destination_location: loc(0, 0, 0, 0),
                        extent,
                    }
                }
            };
            assert!(
                c.submit_typed_image_copy(RenderQueue::First, make(true), &[])
                    .is_err(),
                "{format:?} direction={to_buffer}"
            );
            let p = c
                .submit_typed_image_copy(RenderQueue::First, make(false), &[])
                .unwrap();
            match c.collect_copy(&p).unwrap() {
                OwnedCopyOutput::TypedTexture { bytes, .. } => assert_eq!(bytes, packed),
                OwnedCopyOutput::Buffer(bytes) => {
                    let mut expected = vec![0x59; end];
                    for y in 0..rows {
                        expected[offset + y * pitch..offset + y * pitch + row]
                            .copy_from_slice(&packed[y * row..(y + 1) * row]);
                    }
                    assert_eq!(bytes, expected);
                }
                _ => panic!("wrong typed transfer output"),
            }
        }
    }
}
fn gated_buffer_role(source: bool) {
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    let gate = Gate::new(&gpu, &c, RenderQueue::First);
    let seed = [91, 17, 53, 127, 92, 93, 193, 29, 71];
    let a = c
        .submit_copy(RenderQueue::First, QueuedCopyInput::HostBuffer(&seed), &[])
        .unwrap();
    let RenderCopyOutput::Buffer(buffer) = c.copy_output(&a).unwrap() else {
        panic!("buffer")
    };
    let desc = TypedTextureDesc {
        format: R8Unorm,
        extent: [3, 2],
        mip_levels: 1,
        array_layers: None,
    };
    let pixels = [7, 11, 13, 19, 23, 31];
    let request = if source {
        QueuedTypedImageCopy {
            source: TypedCopyInput::ProducedBuffer(buffer),
            destination: TypedCopyInput::HostTexture {
                desc,
                bytes: &pixels,
            },
            source_location: TypedImageCopyLocation::Buffer {
                offset: 1,
                bytes_per_row: 5,
            },
            destination_location: loc(0, 0, 0, 0),
            extent: [3, 2],
        }
    } else {
        QueuedTypedImageCopy {
            source: TypedCopyInput::HostTexture {
                desc,
                bytes: &pixels,
            },
            destination: TypedCopyInput::ProducedBuffer(buffer),
            source_location: loc(0, 0, 0, 0),
            destination_location: TypedImageCopyLocation::Buffer {
                offset: 1,
                bytes_per_row: 5,
            },
            extent: [3, 2],
        }
    };
    let b = c
        .submit_typed_image_copy(RenderQueue::Second, request, &[])
        .unwrap();
    gate.observe_then_release(&gpu, &c, RenderQueue::Second);
    match c.collect_copy(&b).unwrap() {
        OwnedCopyOutput::TypedTexture { bytes, .. } => {
            assert_eq!(bytes, [17, 53, 127, 193, 29, 71])
        }
        OwnedCopyOutput::Buffer(bytes) => assert_eq!(bytes, [91, 7, 11, 13, 92, 93, 19, 23, 31]),
        _ => panic!("wrong output"),
    }
    c.collect_copy(&a).unwrap();
}
#[test]
fn native_typed_transfers_gated_produced_buffer_source() {
    gated_buffer_role(true);
}
#[test]
fn native_typed_transfers_gated_produced_buffer_prior() {
    gated_buffer_role(false);
}
