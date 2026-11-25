// SPDX-License-Identifier: GPL-3.0-or-later
//! Logical allocation admission, not a measurement of driver VRAM or transient RSS.
use super::*;
use crate::{QueuedTypedImageCopy, TypedCopyInput, TypedImageCopyLocation, TypedTextureDesc};
#[test]
#[cfg_attr(not(feature = "gpu-tests"), ignore = "requires native Metal")]
fn native_typed_copy_tall_r8_produced_peak_admission_and_refund() {
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    let budget = c.color_bytes.clone();
    let desc = TypedTextureDesc {
        format: TextureFormat::R8Unorm,
        extent: [1, 4096],
        mip_levels: 1,
        array_layers: None,
    };
    let source = vec![17; 4096];
    let prior = vec![53; 4096];
    let location = TypedImageCopyLocation::Texture {
        mip: 0,
        slice: 0,
        origin: [0, 0],
    };
    let mut outputs = Vec::new();
    for bytes in [&source, &prior] {
        let p = c
            .submit_typed_image_copy(
                RenderQueue::First,
                QueuedTypedImageCopy {
                    source: TypedCopyInput::HostTexture { desc, bytes },
                    destination: TypedCopyInput::HostTexture { desc, bytes },
                    source_location: location,
                    destination_location: location,
                    extent: [1, 4096],
                },
                &[],
            )
            .unwrap();
        let RenderCopyOutput::TypedTexture(output) = c.copy_output(&p).unwrap() else {
            panic!("typed")
        };
        c.collect_copy(&p).unwrap();
        drop(p);
        outputs.push(output);
    }
    let make = || QueuedTypedImageCopy {
        source: TypedCopyInput::ProducedTexture(outputs[0].clone()),
        destination: TypedCopyInput::ProducedTexture(outputs[1].clone()),
        source_location: location,
        destination_location: location,
        extent: [1, 4096],
    };
    // 256-byte padded rows => 1 MiB readback, plus fresh 4096-byte texture
    // and 4096-byte packed result. No host uploads exist in this request.
    const PEAK: usize = 1_048_576 + 4096 + 4096;
    let held = budget.get();
    let filler = ColorLease::reserve(&budget, MAX_COLOR_VERSION_BYTES - held - (PEAK - 1)).unwrap();
    let before = budget.get();
    let sequence = c.sequence;
    assert!(
        c.submit_typed_image_copy(RenderQueue::Second, make(), &[])
            .is_err()
    );
    assert_eq!(budget.get(), before);
    assert_eq!(c.sequence, sequence);
    drop(filler);
    let filler = ColorLease::reserve(&budget, MAX_COLOR_VERSION_BYTES - held - PEAK).unwrap();
    let p = c
        .submit_typed_image_copy(RenderQueue::Second, make(), &[])
        .unwrap();
    assert_eq!(budget.get(), MAX_COLOR_VERSION_BYTES);
    let OwnedCopyOutput::TypedTexture { bytes, .. } = c.collect_copy(&p).unwrap() else {
        panic!("typed")
    };
    assert_eq!(bytes, vec![17; 4096]);
    drop(p);
    drop(outputs);
    drop(filler);
    assert_eq!(budget.get(), 0);
}
