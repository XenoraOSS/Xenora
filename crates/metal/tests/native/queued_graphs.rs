// SPDX-License-Identifier: GPL-3.0-or-later
//! Original complete byte oracles for multiple outstanding native graph nodes.
use xenora_metal::*;

#[test]
fn native_graph_three_pending_nodes_keep_exact_versions() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_graph_cohort().unwrap();
    let a = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostBuffer(&[3, 5, 7, 11]),
            &[],
        )
        .unwrap();
    let av = cohort.copy_output(&a).unwrap();
    let b = cohort
        .submit_copy(
            RenderQueue::Second,
            QueuedCopyInput::HostBuffer(&[13, 17, 19, 23]),
            &[],
        )
        .unwrap();
    let bv = cohort.copy_output(&b).unwrap();
    let (RenderCopyOutput::Buffer(av), RenderCopyOutput::Buffer(bv)) = (av, bv) else {
        panic!("buffer outputs")
    };
    let c = cohort
        .submit_buffer_region_copy(
            RenderQueue::First,
            QueuedBufferRegionCopy {
                source: QueuedCopyInput::ProducedBuffer(av),
                destination: QueuedCopyInput::ProducedBuffer(bv),
                source_offset: 1,
                destination_offset: 2,
                byte_len: 2,
            },
            &[a.clone(), b.clone()],
        )
        .unwrap();
    let OwnedCopyOutput::Buffer(bytes) = cohort.collect_copy(&c).unwrap() else {
        panic!("buffer output")
    };
    assert_eq!(bytes, [13, 17, 5, 7]);
    let OwnedCopyOutput::Buffer(bytes) = cohort.collect_copy(&a).unwrap() else {
        panic!("buffer output")
    };
    assert_eq!(bytes, [3, 5, 7, 11]);
    let OwnedCopyOutput::Buffer(bytes) = cohort.collect_copy(&b).unwrap() else {
        panic!("buffer output")
    };
    assert_eq!(bytes, [13, 17, 19, 23]);
}

#[test]
fn native_graph_diamond_reverse_collection_preserves_all_versions() {
    for reverse_waits in [false, true] {
        let gpu = NativeCompute::new().unwrap();
        let mut cohort = gpu.new_render_graph_cohort().unwrap();
        let a = cohort
            .submit_copy(
                RenderQueue::First,
                QueuedCopyInput::HostBuffer(&[
                    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                ]),
                &[],
            )
            .unwrap();
        let RenderCopyOutput::Buffer(av) = cohort.copy_output(&a).unwrap() else {
            panic!("buffer")
        };
        let b = cohort
            .submit_buffer_region_copy(
                RenderQueue::Second,
                QueuedBufferRegionCopy {
                    source: QueuedCopyInput::HostBuffer(&[201, 202, 203, 204]),
                    destination: QueuedCopyInput::ProducedBuffer(av.clone()),
                    source_offset: 0,
                    destination_offset: 0,
                    byte_len: 4,
                },
                std::slice::from_ref(&a),
            )
            .unwrap();
        let c = cohort
            .submit_buffer_region_copy(
                RenderQueue::First,
                QueuedBufferRegionCopy {
                    source: QueuedCopyInput::HostBuffer(&[211, 212, 213, 214]),
                    destination: QueuedCopyInput::ProducedBuffer(av),
                    source_offset: 0,
                    destination_offset: 17,
                    byte_len: 4,
                },
                std::slice::from_ref(&a),
            )
            .unwrap();
        let RenderCopyOutput::Buffer(bv) = cohort.copy_output(&b).unwrap() else {
            panic!("buffer")
        };
        let RenderCopyOutput::Buffer(cv) = cohort.copy_output(&c).unwrap() else {
            panic!("buffer")
        };
        let waits = if reverse_waits {
            [c.clone(), b.clone()]
        } else {
            [b.clone(), c.clone()]
        };
        let d = cohort
            .submit_buffer_region_copy(
                RenderQueue::Second,
                QueuedBufferRegionCopy {
                    source: QueuedCopyInput::ProducedBuffer(bv),
                    destination: QueuedCopyInput::ProducedBuffer(cv),
                    source_offset: 0,
                    destination_offset: 8,
                    byte_len: 4,
                },
                &waits,
            )
            .unwrap();
        for (point, expected) in [
            (
                &d,
                [
                    1, 2, 3, 4, 5, 6, 7, 8, 201, 202, 203, 204, 13, 14, 15, 16, 17, 211, 212, 213,
                    214,
                ],
            ),
            (
                &c,
                [
                    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 211, 212, 213, 214,
                ],
            ),
            (
                &b,
                [
                    201, 202, 203, 204, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
                    21,
                ],
            ),
            (
                &a,
                [
                    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                ],
            ),
        ] {
            let OwnedCopyOutput::Buffer(bytes) = cohort.collect_copy(point).unwrap() else {
                panic!("buffer")
            };
            assert_eq!(bytes, expected);
        }
    }
}

#[test]
fn native_graph_capacity_and_wrong_kind_collection_are_atomic() {
    let gpu = NativeCompute::new().unwrap();
    let mut cohort = gpu.new_render_graph_cohort().unwrap();
    let mut points = Vec::new();
    for value in 0..16u8 {
        points.push(
            cohort
                .submit_copy(
                    RenderQueue::First,
                    QueuedCopyInput::HostBuffer(&[value, 53, 109, 211]),
                    &[],
                )
                .unwrap(),
        );
    }
    assert!(
        cohort
            .submit_copy(
                RenderQueue::Second,
                QueuedCopyInput::HostBuffer(&[255; 4]),
                &[]
            )
            .is_err()
    );
    assert!(cohort.collect(&points[7]).is_err());
    for (value, point) in points.iter().enumerate().rev() {
        let OwnedCopyOutput::Buffer(bytes) = cohort.collect_copy(point).unwrap() else {
            panic!("buffer")
        };
        assert_eq!(bytes, [value as u8, 53, 109, 211]);
    }
    let last = cohort
        .submit_copy(
            RenderQueue::First,
            QueuedCopyInput::HostBuffer(&[241, 17, 9, 3]),
            &[],
        )
        .unwrap();
    let OwnedCopyOutput::Buffer(bytes) = cohort.collect_copy(&last).unwrap() else {
        panic!("buffer")
    };
    assert_eq!(bytes, [241, 17, 9, 3]);
    cohort.reinitialize().unwrap();
    assert!(cohort.copy_output(&last).is_err());
}

#[test]
fn native_graph_fanin_waits_for_each_independently_gated_producer() {
    for gated_source in [false, true] {
        let gpu = NativeCompute::new().unwrap();
        let mut cohort = gpu.new_render_graph_cohort().unwrap();
        let gated = if gated_source {
            RenderQueue::First
        } else {
            RenderQueue::Second
        };
        let consumer = if gated_source {
            RenderQueue::Second
        } else {
            RenderQueue::First
        };
        let gate = super::unaligned_copy::Gate::new(&gpu, &cohort, gated);
        let a = cohort
            .submit_copy(
                RenderQueue::First,
                QueuedCopyInput::HostBuffer(&[31, 53, 79, 113, 151]),
                &[],
            )
            .unwrap();
        let b = cohort
            .submit_copy(
                RenderQueue::Second,
                QueuedCopyInput::HostBuffer(&[2, 4, 6, 8, 10, 12, 14]),
                &[],
            )
            .unwrap();
        let RenderCopyOutput::Buffer(av) = cohort.copy_output(&a).unwrap() else {
            panic!("buffer")
        };
        let RenderCopyOutput::Buffer(bv) = cohort.copy_output(&b).unwrap() else {
            panic!("buffer")
        };
        let c = cohort
            .submit_buffer_region_copy(
                consumer,
                QueuedBufferRegionCopy {
                    source: QueuedCopyInput::ProducedBuffer(av),
                    destination: QueuedCopyInput::ProducedBuffer(bv),
                    source_offset: 1,
                    destination_offset: 3,
                    byte_len: 3,
                },
                &[a.clone(), b.clone()],
            )
            .unwrap();
        gate.observe_then_release(&gpu, &cohort, consumer);
        for (point, expected) in [
            (&c, &[2, 4, 6, 53, 79, 113, 14][..]),
            (&b, &[2, 4, 6, 8, 10, 12, 14][..]),
            (&a, &[31, 53, 79, 113, 151][..]),
        ] {
            let OwnedCopyOutput::Buffer(bytes) = cohort.collect_copy(point).unwrap() else {
                panic!("buffer")
            };
            assert_eq!(bytes, expected);
        }
    }
}
