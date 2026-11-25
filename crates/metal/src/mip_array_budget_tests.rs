// SPDX-License-Identifier: GPL-3.0-or-later
//! Independent logical-payload accounting; not a physical VRAM measurement.
use super::*;
use crate::{ImageCopyLocation, QueuedImageCopy};
fn host(bytes: &[u8], array: bool) -> QueuedCopyInput<'_> {
    if array {
        QueuedCopyInput::HostRgba8Array {
            extent: [5, 3],
            mip_levels: 3,
            array_layers: 2,
            pixels: bytes,
        }
    } else {
        QueuedCopyInput::HostRgba8MipChain {
            extent: [5, 3],
            mip_levels: 3,
            pixels: bytes,
        }
    }
}
fn loc(array: bool) -> ImageCopyLocation {
    ImageCopyLocation::Rgba8Mip {
        mip: 2,
        slice: u32::from(array),
        origin: [0, 0],
    }
}
fn submit(
    c: &mut RenderCohort,
    array: bool,
    source: QueuedCopyInput<'_>,
    destination: QueuedCopyInput<'_>,
) -> Result<RenderPoint, MetalError> {
    let request = QueuedImageCopy {
        source,
        destination,
        source_location: loc(array),
        destination_location: loc(array),
        extent: [1, 1],
    };
    if array {
        c.submit_array_image_copy(RenderQueue::First, request, &[])
    } else {
        c.submit_mip_image_copy(RenderQueue::First, request, &[])
    }
}
#[test]
#[cfg_attr(not(feature = "gpu-tests"), ignore = "requires native Metal")]
fn native_mip_array_full_payload_admission_atomic_refund() {
    for array in [false, true] {
        let size = if array { 144 } else { 72 };
        let source = vec![0x17; size];
        let destination = vec![0x59; size];
        let gpu = NativeCompute::new().unwrap();
        let mut c = gpu.new_render_graph_cohort().unwrap();
        let budget = c.color_bytes.clone();
        // One byte below the exact four-full-version charge. Base-only or selected-
        // mip charging would admit this request incorrectly.
        let filler =
            ColorLease::reserve(&budget, MAX_COLOR_VERSION_BYTES - (4 * size - 1)).unwrap();
        let before = budget.get();
        let sequence = c.sequence;
        assert!(
            submit(
                &mut c,
                array,
                host(&source, array),
                host(&destination, array)
            )
            .is_err()
        );
        assert_eq!(budget.get(), before);
        assert_eq!(c.sequence, sequence);
        assert!(c.entries.iter().all(Option::is_none));
        drop(filler);
        assert_eq!(budget.get(), 0);
        let point = submit(
            &mut c,
            array,
            host(&source, array),
            host(&destination, array),
        )
        .unwrap();
        assert_eq!(budget.get(), 4 * size);
        let output = c.copy_output(&point).unwrap();
        c.collect_copy(&point).unwrap();
        c.reinitialize().unwrap();
        assert!(Rc::ptr_eq(&budget, &c.color_bytes));
        drop(point);
        assert_eq!(budget.get(), 4 * size);
        drop(output);
        assert_eq!(budget.get(), 0);
    }
}
#[test]
#[cfg_attr(not(feature = "gpu-tests"), ignore = "requires native Metal")]
fn native_mip_array_both_produced_leases_survive_pruned_edges() {
    for array in [false, true] {
        let size = if array { 144 } else { 72 };
        let source = vec![0x17; size];
        let prior = vec![0x59; size];
        let gpu = NativeCompute::new().unwrap();
        let mut c = gpu.new_render_graph_cohort().unwrap();
        let budget = c.color_bytes.clone();
        let a = submit(&mut c, array, host(&source, array), host(&prior, array)).unwrap();
        let RenderCopyOutput::Color(src) = c.copy_output(&a).unwrap() else {
            panic!("color")
        };
        c.collect_copy(&a).unwrap();
        let b = submit(&mut c, array, host(&prior, array), host(&source, array)).unwrap();
        let RenderCopyOutput::Color(dst) = c.copy_output(&b).unwrap() else {
            panic!("color")
        };
        c.collect_copy(&b).unwrap();
        assert_eq!(budget.get(), 8 * size);
        let consumer = submit(
            &mut c,
            array,
            QueuedCopyInput::ProducedColor(src.clone()),
            QueuedCopyInput::ProducedColor(dst.clone()),
        )
        .unwrap();
        let output = c.copy_output(&consumer).unwrap();
        assert_eq!(budget.get(), 10 * size);
        c.collect_copy(&consumer).unwrap();
        assert!(consumer.inner.dependencies.borrow().is_empty());
        drop(a);
        drop(b);
        drop(src);
        drop(dst);
        assert_eq!(
            budget.get(),
            4 * size,
            "two destination payloads plus direct source and prior texture leases"
        );
        c.reinitialize().unwrap();
        drop(consumer);
        assert_eq!(budget.get(), 4 * size);
        drop(output);
        assert_eq!(budget.get(), 0);
    }
}
