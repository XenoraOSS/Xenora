// SPDX-License-Identifier: GPL-3.0-or-later
//! Independent original geometry bytes and literal pixels for queued GPU gathering.
use super::unaligned_copy::Gate;
use objc2_metal::{MTLCommandBuffer, MTLCommandBufferStatus, MTLCommandQueue};
use std::time::{Duration, Instant};
use xenora_metal::*;

const SOURCE: &str = r"
#include <metal_stdlib>
using namespace metal;
struct V { float4 p [[position]]; float4 c; };
struct F { float4 a [[color(0)]]; float4 b [[color(1)]]; };
vertex V vs(uint i [[vertex_id]], device const float4 *p [[buffer(0)]], device const float4 *c [[buffer(1)]]) { return {p[i],c[i]}; }
fragment F color(V v [[stage_in]]) { return {v.c,float4(v.c.b,v.c.r,v.c.g,v.c.a)}; }
fragment F solid(V v [[stage_in]]) { return {float4(1,0,0,1),float4(0,1,0,1)}; }
";
const CLEAR0: [u8; 4] = [7, 11, 13, 255];
const CLEAR1: [u8; 4] = [17, 19, 23, 255];
const TRI: [[f32; 4]; 3] = [
    [-1., -1., 0.25, 1.],
    [3., -1., 0.25, 1.],
    [-1., 3., 0.25, 1.],
];
fn records(offset: usize, stride: usize, values: &[[f32; 4]]) -> Vec<u8> {
    // Logical end is the last float, not a whole final stride.
    let mut bytes = vec![0xa5; offset + stride * (values.len() - 1) + 16];
    for (i, v) in values.iter().enumerate() {
        for (j, x) in v.iter().enumerate() {
            bytes[offset + i * stride + j * 4..offset + i * stride + j * 4 + 4]
                .copy_from_slice(&x.to_bits().to_le_bytes());
        }
    }
    bytes
}
fn geometry<'a>(
    p: GeometryBuffer<'a>,
    c: Option<GeometryBuffer<'a>>,
    indices: Option<GeometryIndices<'a>>,
) -> QueuedGeometry<'a> {
    QueuedGeometry {
        positions: GeometryAttribute {
            buffer: p,
            offset: 0,
            stride: 16,
        },
        colors: c.map(|buffer| GeometryAttribute {
            buffer,
            offset: 0,
            stride: 16,
        }),
        indices,
        start: 0,
        count: 3,
        base_vertex: 0,
    }
}
fn copy(
    cohort: &mut RenderCohort,
    queue: RenderQueue,
    bytes: &[u8],
) -> (RenderPoint, RenderBufferOutput) {
    let point = cohort
        .submit_copy(queue, QueuedCopyInput::HostBuffer(bytes), &[])
        .unwrap();
    let RenderCopyOutput::Buffer(output) = cohort.copy_output(&point).unwrap() else {
        panic!("buffer")
    };
    (point, output)
}
#[allow(clippy::too_many_arguments)]
fn submit(
    cohort: &mut RenderCohort,
    queue: RenderQueue,
    geometry: Vec<QueuedGeometry<'_>>,
    scissor: Option<[usize; 4]>,
    fragment: &str,
    depth: Option<QueuedDepthInput>,
    loads: &[QueuedColorLoad<'_>],
    deps: &[RenderPoint],
) -> Result<RenderPoint, MetalError> {
    let count = geometry.len();
    let draws: Vec<_> = (0..count)
        .map(|_| TriangleDraw {
            vertices: &[],
            colors: &[],
            viewport: [8, 4],
        })
        .collect();
    let states = vec![
        TriangleState {
            scissor,
            depth: if depth.is_some() {
                DepthState {
                    compare: DepthCompare::Less,
                    write: true,
                }
            } else {
                DepthState::DISABLED
            },
            ..Default::default()
        };
        count
    ];
    let shaders: Vec<_> = (0..count)
        .map(|_| TriangleShader {
            source: SOURCE,
            entries: ["vs", fragment],
        })
        .collect();
    cohort.submit_with_geometry(
        queue,
        &shaders,
        &TriangleTargetsPass {
            draws: &draws,
            states: &states,
            extent: [8, 4],
            clears: &[CLEAR0, CLEAR1],
            clear_depth: None,
        },
        &vec![vec![]; count],
        depth,
        loads,
        deps,
        &vec![DrawUniforms::default(); count],
        &geometry.into_iter().map(Some).collect::<Vec<_>>(),
    )
}
fn clears() -> [QueuedColorLoad<'static>; 2] {
    [QueuedColorLoad::Clear, QueuedColorLoad::Clear]
}
fn assert_solid(output: OwnedRenderOutput, a: [u8; 4], b: [u8; 4]) {
    assert_eq!(output.colors, vec![a.repeat(32), b.repeat(32)]);
}
fn wait_gpu_tail(cohort: &RenderCohort, queue: RenderQueue) {
    // A separate queue-tail command proves GPU completion without polling the
    // cohort's validation status or collecting/pruning any ancestor.
    let command = cohort.queue(queue).commandBuffer().unwrap();
    command.commit();
    let deadline = Instant::now() + Duration::from_secs(5);
    while command.status() != MTLCommandBufferStatus::Completed {
        assert_ne!(command.status(), MTLCommandBufferStatus::Error);
        assert!(Instant::now() < deadline, "GPU tail did not complete");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn produced_geometry_host_layouts_indices_and_mathematical_base() {
    for format in [GeometryIndexFormat::R16Uint, GeometryIndexFormat::R32Uint] {
        let gpu = NativeCompute::new().unwrap();
        let mut cohort = gpu.new_render_graph_cohort().unwrap();
        let left = [
            [-1., -1., 0.25, 1.],
            [0., -1., 0.25, 1.],
            [-1., 1., 0.25, 1.],
            [-1., 1., 0.25, 1.],
            [0., -1., 0.25, 1.],
            [0., 1., 0.25, 1.],
        ];
        let right = left.map(|mut v| {
            v[0] += 1.;
            v
        });
        let pl = records(3, 23, &left);
        let pr = records(3, 23, &right);
        let red = records(5, 21, &[[1., 0., 0., 1.]; 6]);
        let green = records(5, 21, &[[0., 1., 0., 1.]; 6]);
        let values = [65535u32, 65535, 1, 2, 3, 4, 5, 6];
        let mut ib = vec![0xa5];
        let width = match format {
            GeometryIndexFormat::R16Uint => 2,
            GeometryIndexFormat::R32Uint => 4,
        };
        for value in values {
            ib.extend_from_slice(&value.to_le_bytes()[..width]);
        }
        let make = |p, c| QueuedGeometry {
            positions: GeometryAttribute {
                buffer: GeometryBuffer::Host(p),
                offset: 3,
                stride: 23,
            },
            colors: Some(GeometryAttribute {
                buffer: GeometryBuffer::Host(c),
                offset: 5,
                stride: 21,
            }),
            indices: Some(GeometryIndices {
                buffer: GeometryBuffer::Host(&ib),
                offset: 1 + width,
                format,
            }),
            start: 1,
            count: 6,
            base_vertex: -1,
        };
        let point = submit(
            &mut cohort,
            RenderQueue::First,
            vec![make(&pl, &red), make(&pr, &green)],
            Some([1, 1, 6, 2]),
            "color",
            None,
            &clears(),
            &[],
        )
        .unwrap();
        let mut a = Vec::new();
        let mut b = Vec::new();
        for y in 0..4 {
            for x in 0..8 {
                let inside = (1..3).contains(&y) && (1..7).contains(&x);
                a.extend_from_slice(if !inside {
                    &CLEAR0
                } else if x < 4 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 255, 0, 255]
                });
                b.extend_from_slice(if !inside {
                    &CLEAR1
                } else if x < 4 {
                    &[0, 255, 0, 255]
                } else {
                    &[0, 0, 255, 255]
                });
            }
        }
        assert_eq!(cohort.collect(&point).unwrap().colors, vec![a, b]);
    }
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_graph_cohort().unwrap();
    let p = records(0, 16, &TRI);
    let c = records(0, 16, &[[1., 0., 0., 1.]; 3]);
    let ib: Vec<_> = [0x80000000u32, 0x80000001, 0x80000002]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    let mut g = geometry(
        GeometryBuffer::Host(&p),
        Some(GeometryBuffer::Host(&c)),
        Some(GeometryIndices {
            buffer: GeometryBuffer::Host(&ib),
            offset: 0,
            format: GeometryIndexFormat::R32Uint,
        }),
    );
    g.base_vertex = i32::MIN;
    let point = submit(
        &mut cohort,
        RenderQueue::First,
        vec![g],
        None,
        "color",
        None,
        &clears(),
        &[],
    )
    .unwrap();
    assert_solid(
        cohort.collect(&point).unwrap(),
        [255, 0, 0, 255],
        [0, 255, 0, 255],
    );
    // Nonindexed start skips a poison/offscreen record.
    let mut shifted = vec![[9., 9., 0.25, 1.]];
    shifted.extend_from_slice(&TRI);
    let p = records(0, 16, &shifted);
    let c = records(0, 16, &[[0., 1., 0., 1.]; 4]);
    let mut g = geometry(
        GeometryBuffer::Host(&p),
        Some(GeometryBuffer::Host(&c)),
        None,
    );
    g.start = 1;
    let point = submit(
        &mut cohort,
        RenderQueue::First,
        vec![g],
        None,
        "color",
        None,
        &clears(),
        &[],
    )
    .unwrap();
    assert_solid(
        cohort.collect(&point).unwrap(),
        [0, 255, 0, 255],
        [0, 0, 255, 255],
    );
    expanded_gather_expanded_literal_bands();
    aliased_geometry_roles_literal_pixels();
}

fn gated_role(role: usize) {
    for format in [GeometryIndexFormat::R16Uint, GeometryIndexFormat::R32Uint] {
        for descendant_first in [false, true] {
            let gpu = NativeCompute::new().unwrap();
            let mut cohort = gpu.new_render_graph_cohort().unwrap();
            let gate = Gate::new(&gpu, &cohort, RenderQueue::First);
            let p = records(0, 16, &TRI);
            let c = records(0, 16, &[[0., 1., 0., 1.]; 3]);
            let width = match format {
                GeometryIndexFormat::R16Uint => 2,
                GeometryIndexFormat::R32Uint => 4,
            };
            let ib: Vec<_> = [3u32, 1, 2]
                .iter()
                .flat_map(|v| v.to_le_bytes()[..width].to_vec())
                .collect();
            let data = match role {
                0 => &p,
                1 => &c,
                _ => &ib,
            };
            let (producer, output) = copy(&mut cohort, RenderQueue::First, data);
            let mut g = geometry(
                if role == 0 {
                    GeometryBuffer::Produced(output.clone())
                } else {
                    GeometryBuffer::Host(&p)
                },
                Some(if role == 1 {
                    GeometryBuffer::Produced(output.clone())
                } else {
                    GeometryBuffer::Host(&c)
                }),
                Some(GeometryIndices {
                    buffer: if role == 2 {
                        GeometryBuffer::Produced(output)
                    } else {
                        GeometryBuffer::Host(&ib)
                    },
                    offset: 0,
                    format,
                }),
            );
            g.base_vertex = -1;
            let consumer = submit(
                &mut cohort,
                RenderQueue::Second,
                vec![g],
                None,
                "color",
                None,
                &clears(),
                &[],
            )
            .unwrap();
            gate.observe_then_release(&gpu, &cohort, RenderQueue::Second);
            if !descendant_first {
                cohort.collect_copy(&producer).unwrap();
            }
            assert_solid(
                cohort.collect(&consumer).unwrap(),
                [0, 255, 0, 255],
                [0, 0, 255, 255],
            );
            if descendant_first {
                cohort.collect_copy(&producer).unwrap();
            }
        }
    }
}
#[test]
fn produced_geometry_pending_positions_wait_before_gather() {
    gated_role(0);
}
#[test]
fn produced_geometry_pending_colors_wait_before_gather() {
    gated_role(1);
}
#[test]
fn produced_geometry_pending_indices_wait_before_gather() {
    gated_role(2);
}

#[test]
fn produced_geometry_fanin_keeps_old_versions_and_owns_sources() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_graph_cohort().unwrap();
    let mut p = records(0, 16, &TRI);
    let mut red = records(0, 16, &[[1., 0., 0., 1.]; 3]);
    let mut ib: Vec<_> = [0u32, 1, 2]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    let (pp, po) = copy(&mut cohort, RenderQueue::First, &p);
    let (cp, co) = copy(&mut cohort, RenderQueue::Second, &red);
    let (ip, io) = copy(&mut cohort, RenderQueue::First, &ib);
    let green = records(0, 16, &[[0., 1., 0., 1.]; 3]);
    let (newp, newo) = copy(&mut cohort, RenderQueue::Second, &green);
    p.fill(0);
    red.fill(0);
    ib.fill(0xff);
    let make = |c| {
        geometry(
            GeometryBuffer::Produced(po.clone()),
            Some(GeometryBuffer::Produced(c)),
            Some(GeometryIndices {
                buffer: GeometryBuffer::Produced(io.clone()),
                offset: 0,
                format: GeometryIndexFormat::R32Uint,
            }),
        )
    };
    let old = submit(
        &mut cohort,
        RenderQueue::First,
        vec![make(co.clone())],
        None,
        "color",
        None,
        &clears(),
        &[],
    )
    .unwrap();
    let new = submit(
        &mut cohort,
        RenderQueue::Second,
        vec![make(newo.clone())],
        None,
        "color",
        None,
        &clears(),
        &[],
    )
    .unwrap();
    drop((po, co, io, newo, p, red, ib, green));
    for point in [&pp, &cp, &ip, &newp] {
        cohort.collect_copy(point).unwrap();
    }
    drop((pp, cp, ip, newp));
    assert_solid(
        cohort.collect(&new).unwrap(),
        [0, 255, 0, 255],
        [0, 0, 255, 255],
    );
    assert_solid(
        cohort.collect(&old).unwrap(),
        [255, 0, 0, 255],
        [0, 255, 0, 255],
    );
}

#[test]
fn produced_geometry_invalid_last_draw_rejects_all_outputs() {
    // Every component is validated even when the fragment ignores vertex colors.
    for colors_bad in [false, true] {
        for component in 0..4 {
            for bits in [0x7fc00001u32, 0x7f800000, 0xff800000] {
                let gpu = NativeCompute::new().unwrap();
                let mut cohort = gpu.new_render_graph_cohort().unwrap();
                let p = records(0, 16, &TRI);
                let c = records(0, 16, &[[1., 0., 0., 1.]; 3]);
                let mut bad = if colors_bad { c.clone() } else { p.clone() };
                bad[32 + component * 4..36 + component * 4].copy_from_slice(&bits.to_le_bytes());
                let (source, out) = copy(&mut cohort, RenderQueue::First, &bad);
                let good = geometry(
                    GeometryBuffer::Host(&p),
                    Some(GeometryBuffer::Host(&c)),
                    None,
                );
                let malformed = geometry(
                    if colors_bad {
                        GeometryBuffer::Host(&p)
                    } else {
                        GeometryBuffer::Produced(out.clone())
                    },
                    Some(if colors_bad {
                        GeometryBuffer::Produced(out)
                    } else {
                        GeometryBuffer::Host(&c)
                    }),
                    None,
                );
                let point = submit(
                    &mut cohort,
                    RenderQueue::Second,
                    vec![good, malformed],
                    None,
                    "solid",
                    Some(QueuedDepthInput::Clear(1.)),
                    &clears(),
                    &[],
                )
                .unwrap();
                assert!(
                    cohort.collect(&point).is_err(),
                    "colors={colors_bad}, component={component}, bits={bits:x}"
                );
                assert!(cohort.color_output(&point, 0).is_err());
                assert!(cohort.depth_output(&point).is_err());
                cohort
                    .abort_and_drain("expected malformed geometry")
                    .unwrap();
                drop(source);
            }
        }
    }
    // Logical bounds, not padded native allocation: final position/color byte,
    // truncated final R16 index, out-of-range last index and negative effective index.
    for case in 0..5 {
        let gpu = NativeCompute::new().unwrap();
        let mut cohort = gpu.new_render_graph_cohort().unwrap();
        let p = records(0, 16, &TRI);
        let c = records(0, 16, &[[1., 0., 0., 1.]; 3]);
        let mut ib = vec![0u8, 0, 1, 0, 2, 0];
        if case == 2 {
            ib.pop();
        }
        if case == 3 {
            ib[4] = 3;
        }
        let pbytes = if case == 0 { &p[..47] } else { &p };
        let cbytes = if case == 1 { &c[..47] } else { &c };
        let mut bad = geometry(
            GeometryBuffer::Host(pbytes),
            Some(GeometryBuffer::Host(cbytes)),
            Some(GeometryIndices {
                buffer: GeometryBuffer::Host(&ib),
                offset: 0,
                format: GeometryIndexFormat::R16Uint,
            }),
        );
        if case == 4 {
            bad.base_vertex = -1;
        }
        let result = submit(
            &mut cohort,
            RenderQueue::First,
            vec![
                geometry(
                    GeometryBuffer::Host(&p),
                    Some(GeometryBuffer::Host(&c)),
                    None,
                ),
                bad,
            ],
            None,
            "color",
            None,
            &clears(),
            &[],
        );
        // Descriptor-visible truncation can reject at admission; payload-dependent
        // invalid indices must reject at completion. Neither may return pixels.
        if let Ok(point) = result {
            assert!(cohort.collect(&point).is_err(), "case={case}");
        }
    }
}

#[test]
fn produced_geometry_invalid_ancestor_gpu_complete_descendant_first_fails() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_graph_cohort().unwrap();
    let gate = Gate::new(&gpu, &cohort, RenderQueue::First);
    let p = records(0, 16, &TRI);
    let c = records(0, 16, &[[1., 0., 0., 1.]; 3]);
    let ib: Vec<_> = [0u32, 1, 999]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    let (producer, index) = copy(&mut cohort, RenderQueue::First, &ib);
    let bad = geometry(
        GeometryBuffer::Host(&p),
        Some(GeometryBuffer::Host(&c)),
        Some(GeometryIndices {
            buffer: GeometryBuffer::Produced(index),
            offset: 0,
            format: GeometryIndexFormat::R32Uint,
        }),
    );
    let ancestor = submit(
        &mut cohort,
        RenderQueue::First,
        vec![bad],
        None,
        "color",
        Some(QueuedDepthInput::Clear(1.)),
        &clears(),
        &[],
    )
    .unwrap();
    let colors = [
        cohort.color_output(&ancestor, 0).unwrap(),
        cohort.color_output(&ancestor, 1).unwrap(),
    ];
    let depth = cohort.depth_output(&ancestor).unwrap();
    // Submit immediately, without observing/pruning the ancestor's status.
    let descendant = submit(
        &mut cohort,
        RenderQueue::Second,
        vec![geometry(
            GeometryBuffer::Host(&p),
            Some(GeometryBuffer::Host(&c)),
            None,
        )],
        Some([0, 0, 4, 4]),
        "color",
        Some(QueuedDepthInput::Produced(depth)),
        &[
            QueuedColorLoad::Produced(colors[0].clone()),
            QueuedColorLoad::Produced(colors[1].clone()),
        ],
        &[],
    )
    .unwrap();
    gate.observe_then_release(&gpu, &cohort, RenderQueue::Second);
    wait_gpu_tail(&cohort, RenderQueue::Second);
    // Exercise poll's terminal-success cache before collection. Collect before
    // asserting the poll result so a bypass mutant exposes returned pixels.
    let polled = cohort.is_complete(&descendant);
    let collected = cohort.collect(&descendant);
    assert!(
        collected.is_err(),
        "GPU-complete polled descendant published invalid ancestor"
    );
    assert!(polled.is_err());
    assert!(cohort.collect(&ancestor).is_err());
    cohort.abort_and_drain("invalid ancestor").unwrap();
    drop(producer);
}

#[test]
fn produced_geometry_admission_provenance_retirement_and_recovery() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_graph_cohort().unwrap();
    let p = records(0, 16, &TRI);
    let c = records(0, 16, &[[1., 0., 0., 1.]; 3]);
    for (offset, stride, start) in [(usize::MAX, 16, 0), (0, 15, 0), (0, usize::MAX, u32::MAX)] {
        let mut g = geometry(
            GeometryBuffer::Host(&p),
            Some(GeometryBuffer::Host(&c)),
            None,
        );
        g.positions.offset = offset;
        g.positions.stride = stride;
        g.start = start;
        assert!(
            submit(
                &mut cohort,
                RenderQueue::First,
                vec![g],
                None,
                "color",
                None,
                &clears(),
                &[]
            )
            .is_err()
        );
    }
    let mut other = gpu.new_render_graph_cohort().unwrap();
    let (foreign, fo) = copy(&mut other, RenderQueue::First, &p);
    assert!(
        submit(
            &mut cohort,
            RenderQueue::First,
            vec![geometry(
                GeometryBuffer::Produced(fo),
                Some(GeometryBuffer::Host(&c)),
                None
            )],
            None,
            "color",
            None,
            &clears(),
            &[]
        )
        .is_err()
    );
    other.collect_copy(&foreign).unwrap();
    let (point, output) = copy(&mut cohort, RenderQueue::First, &p);
    cohort.collect_copy(&point).unwrap();
    cohort.reinitialize().unwrap();
    assert!(
        submit(
            &mut cohort,
            RenderQueue::First,
            vec![geometry(
                GeometryBuffer::Produced(output),
                Some(GeometryBuffer::Host(&c)),
                None
            )],
            None,
            "color",
            None,
            &clears(),
            &[]
        )
        .is_err()
    );
    drop(point);
    for _ in 0..3 {
        let (point, out) = copy(&mut cohort, RenderQueue::First, &p);
        let draw = submit(
            &mut cohort,
            RenderQueue::Second,
            vec![geometry(
                GeometryBuffer::Produced(out),
                Some(GeometryBuffer::Host(&c)),
                None,
            )],
            None,
            "color",
            None,
            &clears(),
            &[],
        )
        .unwrap();
        cohort.collect_copy(&point).unwrap();
        drop(point);
        assert_solid(
            cohort.collect(&draw).unwrap(),
            [255, 0, 0, 255],
            [0, 255, 0, 255],
        );
        drop(draw);
        cohort.reinitialize().unwrap();
    }
}

fn expanded_gather_expanded_literal_bands() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_graph_cohort().unwrap();
    let p = records(0, 16, &TRI);
    let green = records(0, 16, &[[0., 1., 0., 1.]; 3]);
    let red = [[1., 0., 0., 1.]; 3];
    let blue = [[0., 0., 1., 1.]; 3];
    let yellow = [[1., 1., 0., 1.]; 3];
    let draws = [
        TriangleDraw {
            vertices: &TRI,
            colors: &red,
            viewport: [8, 4],
        },
        TriangleDraw {
            vertices: &[],
            colors: &[],
            viewport: [8, 4],
        },
        TriangleDraw {
            vertices: &TRI,
            colors: &yellow,
            viewport: [8, 4],
        },
        TriangleDraw {
            vertices: &[],
            colors: &[],
            viewport: [8, 4],
        },
        TriangleDraw {
            vertices: &TRI,
            colors: &blue,
            viewport: [8, 4],
        },
    ];
    let states = [
        [0, 0, 2, 4],
        [2, 0, 3, 4],
        [8, 0, 0, 4],
        [8, 0, 0, 4],
        [5, 0, 3, 4],
    ]
    .map(|scissor| TriangleState {
        scissor: Some(scissor),
        ..Default::default()
    });
    let shaders: Vec<_> = (0..5)
        .map(|_| TriangleShader {
            source: SOURCE,
            entries: ["vs", "color"],
        })
        .collect();
    let point = cohort
        .submit_with_geometry(
            RenderQueue::First,
            &shaders,
            &TriangleTargetsPass {
                draws: &draws,
                states: &states,
                extent: [8, 4],
                clears: &[CLEAR0, CLEAR1],
                clear_depth: None,
            },
            &[vec![], vec![], vec![], vec![], vec![]],
            None,
            &clears(),
            &[],
            &[DrawUniforms::default(); 5],
            &[
                None,
                Some(geometry(
                    GeometryBuffer::Host(&p),
                    Some(GeometryBuffer::Host(&green)),
                    None,
                )),
                None,
                Some(geometry(
                    GeometryBuffer::Host(&p),
                    Some(GeometryBuffer::Host(&green)),
                    None,
                )),
                None,
            ],
        )
        .unwrap();
    let a = [
        [255, 0, 0, 255],
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 255, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [0, 0, 255, 255],
        [0, 0, 255, 255],
    ];
    let b = [
        [0, 255, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [0, 0, 255, 255],
        [0, 0, 255, 255],
        [255, 0, 0, 255],
        [255, 0, 0, 255],
        [255, 0, 0, 255],
    ];
    assert_eq!(
        cohort.collect(&point).unwrap().colors,
        vec![a.as_flattened().repeat(4), b.as_flattened().repeat(4)]
    );
}

fn aliased_geometry_roles_literal_pixels() {
    // The same logical backing supplies all three roles, with distinct offsets.
    // Produced clones share identity; Host staging may copy each role separately.
    // This is a pixel/lifetime oracle, not a claim about exact budget charges.
    for produced in [false, true] {
        let gpu = NativeCompute::new().unwrap();
        let mut cohort = gpu.new_render_graph_cohort().unwrap();
        let mut bytes = vec![0xa5; 3];
        bytes.extend_from_slice(&records(0, 16, &TRI));
        bytes.extend_from_slice(&records(0, 16, &[[1., 0., 0., 1.]; 3]));
        bytes.extend([0u32, 1, 2].into_iter().flat_map(u32::to_le_bytes));
        let source = produced.then(|| copy(&mut cohort, RenderQueue::First, &bytes));
        let buffer = || match &source {
            Some((_, output)) => GeometryBuffer::Produced(output.clone()),
            None => GeometryBuffer::Host(&bytes),
        };
        let g = QueuedGeometry {
            positions: GeometryAttribute {
                buffer: buffer(),
                offset: 3,
                stride: 16,
            },
            colors: Some(GeometryAttribute {
                buffer: buffer(),
                offset: 51,
                stride: 16,
            }),
            indices: Some(GeometryIndices {
                buffer: buffer(),
                offset: 99,
                format: GeometryIndexFormat::R32Uint,
            }),
            start: 0,
            count: 3,
            base_vertex: 0,
        };
        let point = submit(
            &mut cohort,
            RenderQueue::Second,
            vec![g],
            None,
            "color",
            None,
            &clears(),
            &[],
        )
        .unwrap();
        if let Some((producer, output)) = source {
            cohort.collect_copy(&producer).unwrap();
            drop((producer, output));
        }
        drop(bytes);
        assert_solid(
            cohort.collect(&point).unwrap(),
            [255, 0, 0, 255],
            [0, 255, 0, 255],
        );
    }
}
