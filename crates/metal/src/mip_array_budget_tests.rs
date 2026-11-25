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

#[test]
#[cfg_attr(not(feature = "gpu-tests"), ignore = "requires native Metal")]
fn native_array_view_only_owner_retains_full_root_after_collection_reset() {
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    let budget = c.color_bytes.clone();
    let source = [17; 144];
    let prior = [89; 144];
    let point = submit(&mut c, true, host(&source, true), host(&prior, true)).unwrap();
    let RenderCopyOutput::Color(output) = c.copy_output(&point).unwrap() else {
        panic!("array")
    };
    let view = output
        .array_slice_view(crate::ArraySliceViewDesc {
            slice: 1,
            base_mip: 2,
            mip_count: 1,
            format: crate::TextureViewFormat::Rgba8Unorm,
        })
        .unwrap();
    c.collect_copy(&point).unwrap();
    assert!(point.inner.dependencies.borrow().is_empty());
    drop(output);
    drop(point);
    c.reinitialize().unwrap();
    assert_eq!(view.extent(), [1, 1]);
    assert_eq!(view.slice(), Some(1));
    assert_eq!(
        budget.get(),
        576,
        "one selected texel must not shrink the retained full-root/point lease"
    );
    let filler = ColorLease::reserve(&budget, MAX_COLOR_VERSION_BYTES - 576).unwrap();
    assert!(ColorLease::reserve(&budget, 1).is_err());
    drop(filler);
    drop(view);
    assert_eq!(budget.get(), 0);
}

#[test]
#[cfg_attr(not(feature = "gpu-tests"), ignore = "requires native Metal")]
fn native_array_view_controlled_failed_producer_suppresses_completed_descendant() {
    // Controlled terminal-state injection after real commands finish, not a GPU fault.
    struct Release(Retained<ProtocolObject<dyn MTLSharedEvent>>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.setSignaledValue(1);
        }
    }
    let gpu = NativeCompute::new().unwrap();
    let mut c = gpu.new_render_graph_cohort().unwrap();
    let release = Release(gpu.device().newSharedEvent().unwrap());
    let command = c.queue(RenderQueue::First).commandBuffer().unwrap();
    command.encodeWaitForEvent_value(ProtocolObject::from_ref(&*release.0), 1);
    command.commit();
    let source = [17; 144];
    let prior = [89; 144];
    let point = submit(&mut c, true, host(&source, true), host(&prior, true)).unwrap();
    let RenderCopyOutput::Color(output) = c.copy_output(&point).unwrap() else {
        panic!("array")
    };
    let view = output
        .array_slice_view(crate::ArraySliceViewDesc {
            slice: 1,
            base_mip: 2,
            mip_count: 1,
            format: crate::TextureViewFormat::Rgba8Unorm,
        })
        .unwrap();
    let sampler =
        crate::MetalSampler::new(gpu.device(), xenora_texture::SamplerDesc::nearest_clamp())
            .unwrap();
    let shader = "#include <metal_stdlib>\nusing namespace metal;struct V{float4 p [[position]];};vertex V vs(uint i [[vertex_id]],device const float4*p [[buffer(0)]]){return {p[i]};}fragment float4 ps(V v [[stage_in]],texture2d<float>t [[texture(0)]],sampler s [[sampler(0)]]){return t.sample(s,float2(.5));}";
    let positions = [[-1., -1., 0., 1.], [3., -1., 0., 1.], [-1., 3., 0., 1.]];
    let child = c
        .submit_with_attachments(
            RenderQueue::Second,
            &[crate::TriangleShader {
                source: shader,
                entries: ["vs", "ps"],
            }],
            &crate::TriangleTargetsPass {
                draws: &[crate::TriangleDraw {
                    vertices: &positions,
                    colors: &[[0.; 4]; 3],
                    viewport: [1, 1],
                }],
                states: &[crate::TriangleState::default()],
                extent: [1, 1],
                clears: &[[0; 4]],
                clear_depth: None,
            },
            &[vec![crate::QueuedSampledBinding::ProducedView {
                view,
                sampler,
                texture_slot: crate::BindingSlot::new(0),
                sampler_slot: crate::BindingSlot::new(0),
            }]],
            None,
            &[crate::QueuedColorLoad::Clear],
            &[],
            &[crate::DrawUniforms::default()],
        )
        .unwrap();
    drop(release);
    point.inner.command.waitUntilCompleted();
    child.inner.command.waitUntilCompleted();
    point.inner.terminal.set(Some(false));
    let polled = c.is_complete(&child);
    let collected = c.collect(&child);
    assert!(
        collected.is_err(),
        "completed GPU work cannot publish a rejected ancestor"
    );
    assert!(polled.is_err());
    c.abort_and_drain("controlled producer rejection").unwrap();
}
