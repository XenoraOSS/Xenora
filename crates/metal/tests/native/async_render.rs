// SPDX-License-Identifier: GPL-3.0-or-later
//! Native submitted work is held by a real Metal shared event, not a timing guess.
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCommandBuffer, MTLCommandQueue, MTLDevice, MTLEvent, MTLSharedEvent};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use xenora_metal::{
    DepthCompare, DepthLoad, DepthState, NativeCompute, TriangleDraw, TriangleState,
    TriangleTargetsPass,
};
use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};

const ENTRIES: [&str; 2] = [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT];
const POSITIONS: [[f32; 4]; 3] = [
    [-1., -1., 0.25, 1.],
    [3., -1., 0.25, 1.],
    [-1., 3., 0.25, 1.],
];
const CLEAR: [u8; 4] = [7, 11, 19, 255];
fn shader() -> String {
    compile_graphics(
        Architecture::Gfx1030,
        include_bytes!("../../../../fixtures/rdna2/x9-constant-vs.bin"),
        include_bytes!("../../../../fixtures/rdna2/x9-constant-ps.bin"),
    )
    .unwrap()
    .msl
}

#[test]
fn submitted_render_returns_while_gpu_waits_and_owns_its_inputs() {
    let gpu = NativeCompute::new().unwrap();
    let event = gpu
        .device()
        .newSharedEvent()
        .expect("shared event capability");
    let rescue_event = event.clone();
    let timed_out = Arc::new(AtomicBool::new(false));
    let rescue_timeout = timed_out.clone();
    let (done, finished) = mpsc::channel();
    // Also releases the GPU if a regression waits inside submit or an assertion panics.
    let rescue = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(2)).is_err() {
            rescue_timeout.store(true, Ordering::SeqCst);
            rescue_event.setSignaledValue(1);
        }
    });
    let gate = gpu.queue().commandBuffer().unwrap();
    let prerequisite: &ProtocolObject<dyn MTLEvent> = ProtocolObject::from_ref(&*event);
    gate.encodeWaitForEvent_value(prerequisite, 1);
    gate.commit();
    drop(gate);
    let pending = {
        let source = shader();
        let positions = POSITIONS.to_vec();
        let colors = vec![[0.; 4]; 3];
        let prior = [31, 41, 59, 255].repeat(12);
        let draws = [TriangleDraw {
            vertices: &positions,
            colors: &colors,
            viewport: [4, 3],
        }];
        let states = [TriangleState {
            scissor: Some([1, 1, 2, 1]),
            ..Default::default()
        }];
        let pass = TriangleTargetsPass {
            draws: &draws,
            states: &states,
            extent: [4, 3],
            clears: &[CLEAR],
            clear_depth: None,
        };
        gpu.submit_loaded_triangle_targets_rgba8(
            &source,
            ENTRIES,
            &pass,
            None,
            None,
            &[Some(&prior)],
        )
        .unwrap()
    };
    // No input slices, shader source, NativeCompute or caller-owned queue must survive.
    drop(gpu);
    let submitted_before_rescue = !timed_out.load(Ordering::SeqCst);
    let completion_before_signal = pending.is_complete();
    event.setSignaledValue(1);
    assert!(submitted_before_rescue, "submit waited for GPU completion");
    assert!(
        !completion_before_signal.unwrap(),
        "event must hold the render command pending"
    );
    let outputs = pending.wait().unwrap();
    done.send(()).unwrap();
    rescue.join().unwrap();
    assert!(!timed_out.load(Ordering::SeqCst));
    assert_eq!(outputs.len(), 1);
    for (index, pixel) in outputs[0].as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(
            *pixel,
            if index == 5 || index == 6 {
                [255, 0, 0, 255]
            } else {
                [31, 41, 59, 255]
            }
        );
    }
}

#[test]
fn pending_depth_only_publishes_on_successful_collection() {
    let gpu = NativeCompute::new().unwrap();
    let source = shader();
    let mut depth = gpu.create_depth_target([4, 3]).unwrap();
    let empty = TriangleTargetsPass {
        draws: &[],
        states: &[],
        extent: [4, 3],
        clears: &[CLEAR],
        clear_depth: None,
    };
    gpu.render_loaded_triangle_targets_rgba8(
        &source,
        ENTRIES,
        &empty,
        None,
        Some((&mut depth, DepthLoad::Clear(0.5))),
        &[None],
    )
    .unwrap();
    let before = gpu.snapshot_depth(&depth).unwrap();
    {
        let pending = gpu
            .submit_loaded_triangle_targets_rgba8(
                &source,
                ENTRIES,
                &empty,
                None,
                Some((&mut depth, DepthLoad::Clear(0.125))),
                &[None],
            )
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !pending.is_complete().unwrap() {
            assert!(
                std::time::Instant::now() < deadline,
                "native completion timeout"
            );
            std::thread::yield_now();
        }
        // Even completed work stays unpublished until collection; drop discards it.
        drop(pending);
    }
    assert_eq!(gpu.snapshot_depth(&depth).unwrap(), before);
    gpu.submit_loaded_triangle_targets_rgba8(
        &source,
        ENTRIES,
        &empty,
        None,
        Some((&mut depth, DepthLoad::Clear(0.125))),
        &[None],
    )
    .unwrap()
    .wait()
    .unwrap();
    assert_ne!(gpu.snapshot_depth(&depth).unwrap(), before);
    let colors = [[0.; 4]; 3];
    for (probe, expected) in [(0.0625, [255, 0, 0, 255]), (0.25, CLEAR)] {
        let positions = POSITIONS.map(|[x, y, _, w]| [x, y, probe, w]);
        let draws = [TriangleDraw {
            vertices: &positions,
            colors: &colors,
            viewport: [4, 3],
        }];
        let states = [TriangleState {
            depth: DepthState {
                compare: DepthCompare::Less,
                write: false,
            },
            ..Default::default()
        }];
        let pass = TriangleTargetsPass {
            draws: &draws,
            states: &states,
            extent: [4, 3],
            clears: &[CLEAR],
            clear_depth: None,
        };
        let output = gpu
            .submit_loaded_triangle_targets_rgba8(
                &source,
                ENTRIES,
                &pass,
                None,
                Some((&mut depth, DepthLoad::Load)),
                &[None],
            )
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(output[0], expected.repeat(12));
    }
}
