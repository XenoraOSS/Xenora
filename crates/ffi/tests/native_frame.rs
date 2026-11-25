// SPDX-License-Identifier: GPL-3.0-or-later

//! Runs the trusted fixture through the C interface on a native Metal device.
//! Needs `--features gpu-tests` (scripts/test-gpu.sh).
#![cfg(feature = "gpu-tests")]

use xenora_ffi::*;

/// fixtures/rdna2/x9-constant-ps.s: `v_mov_b32 v8, 1.0; v_mov_b32 v9, 0;
/// exp mrt0, v8, v9, v9, v8` writes (1, 0, 0, 1); as RGBA8 unorm that is (255, 0, 0, 255).
const SHADER_OUTPUT: [u8; 4] = [255, 0, 0, 255];

#[test]
fn guest_fixture_frame_reaches_the_caller_through_metal() {
    for run in 0..3 {
        let mut session = 0;
        assert_eq!(
            unsafe { xenora_session_create(XENORA_ABI_VERSION, &mut session) },
            XENORA_OK
        );
        let (mut rgba, mut width, mut height) = ([0u8; 64], 0u32, 0u32);
        let status = unsafe {
            xenora_session_run_trusted_fixture(
                session,
                rgba.as_mut_ptr(),
                64,
                &mut width,
                &mut height,
            )
        };
        assert_eq!(status, XENORA_OK, "run {run}");
        assert_eq!((width, height), (4, 3));
        assert_eq!(&rgba[..48], SHADER_OUTPUT.repeat(12).as_slice());
        assert!(
            rgba[48..].iter().all(|&b| b == 0),
            "no write past the frame"
        );
        assert_eq!(xenora_session_destroy(session), XENORA_OK);
    }
}

#[test]
fn application_renderer_preserves_ordered_state_and_recovers_after_bad_input() {
    use xenora_ffi::graphics_backend::FixtureRenderer;
    use xenora_loader::graphics::{
        GraphicsBlendMode, GraphicsDepthCompare, GraphicsDraw, GraphicsDrawState,
    };
    let mut renderer = FixtureRenderer::new().unwrap();
    let vertices = vec![
        [-1.0, -1.0, 0.25, 1.0],
        [3.0, -1.0, 0.25, 1.0],
        [-1.0, 3.0, 0.25, 1.0],
    ];
    let hidden = GraphicsDraw {
        vertices: vertices.clone(),
        viewport: (4, 3),
        state: GraphicsDrawState {
            depth_compare: Some(GraphicsDepthCompare::Less),
            depth_write: true,
            color_write_mask: 0,
            ..GraphicsDrawState::default()
        },
        ..Default::default()
    };
    let behind = GraphicsDraw {
        vertices: vertices
            .iter()
            .map(|&[x, y, _, w]| [x, y, 0.75, w])
            .collect(),
        viewport: (4, 3),
        state: GraphicsDrawState {
            depth_compare: Some(GraphicsDepthCompare::Less),
            ..GraphicsDrawState::default()
        },
        ..Default::default()
    };
    let clear = [0, 0, 255, 255];
    assert_eq!(
        renderer.render(&[hidden, behind], clear).unwrap().pixels,
        clear.repeat(12)
    );
    let additive = GraphicsDraw {
        vertices,
        viewport: (4, 3),
        state: GraphicsDrawState {
            blend: GraphicsBlendMode::Additive,
            ..GraphicsDrawState::default()
        },
        ..Default::default()
    };
    assert!(renderer.render(&[], clear).is_err());
    let mut invalid = additive.clone();
    invalid.vertices.push([0.0; 4]);
    assert!(renderer.render(&[invalid], clear).is_err());
    // Failures during recording and execution must not poison the next frame.
    for kind in 0..4 {
        let mut invalid = additive.clone();
        match kind {
            0 => invalid.viewport = (0, 3),
            1 => invalid.state.color_write_mask = 16,
            2 => invalid.vertices[2][0] = f32::NAN,
            _ => invalid.vertices = vec![[0.0; 4]; 771],
        }
        assert!(renderer.render(&[invalid], clear).is_err());
        assert_eq!(
            renderer
                .render(std::slice::from_ref(&additive), clear)
                .unwrap()
                .pixels,
            [255, 0, 255, 255].repeat(12)
        );
    }
    let mut enlarged = additive.clone();
    enlarged.viewport = (5, 3);
    assert!(
        renderer
            .render(&[additive.clone(), enlarged], clear)
            .is_err()
    );
    for _ in 0..32 {
        let out = renderer
            .render(std::slice::from_ref(&additive), clear)
            .unwrap();
        assert_eq!(out.pixels, [255, 0, 255, 255].repeat(12));
        assert_eq!(out.draw_count, 1);
    }
}

#[test]
fn application_renderer_maps_offsets_scissors_and_reset() {
    use xenora_ffi::graphics_backend::FixtureRenderer;
    use xenora_loader::graphics::{GraphicsDraw, GraphicsDrawState};
    let mut renderer = FixtureRenderer::new().unwrap();
    let base = GraphicsDraw {
        vertices: vec![
            [-1.0, -1.0, 0.5, 1.0],
            [3.0, -1.0, 0.5, 1.0],
            [-1.0, 3.0, 0.5, 1.0],
        ],
        viewport: (6, 4),
        state: GraphicsDrawState::default(),
        ..Default::default()
    };
    let mut left = base.clone();
    left.state.scissor = Some([0, 0, 2, 4]);
    let mut middle = base.clone();
    middle.viewport = (3, 2);
    middle.state.viewport_origin = [3, 1];
    middle.state.scissor = Some([4, 0, 1, 4]);
    let mut corner = base.clone();
    corner.viewport = (1, 1);
    corner.state.viewport_origin = [5, 3];
    let clear = [0, 0, 255, 255];
    let out = renderer.render(&[left, middle, corner], clear).unwrap();
    assert_eq!((out.width, out.height), (6, 4));
    for (i, &pixel) in out.pixels.as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = (i % 6, i / 6);
        assert_eq!(
            pixel,
            if x < 2 || (x == 4 && (1..3).contains(&y)) || (x == 5 && y == 3) {
                SHADER_OUTPUT
            } else {
                clear
            }
        );
    }
    let mut initial_offset = base.clone();
    initial_offset.viewport = (3, 2);
    initial_offset.state.viewport_origin = [2, 1];
    let out = renderer.render(&[initial_offset], clear).unwrap();
    assert_eq!((out.width, out.height), (5, 3));
    for (i, &pixel) in out.pixels.as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(
            pixel,
            if i % 5 >= 2 && i / 5 >= 1 {
                SHADER_OUTPUT
            } else {
                clear
            }
        );
    }
    let mut invalid = base.clone();
    invalid.state.scissor = Some([usize::MAX, 0, 2, 1]);
    assert!(renderer.render(&[invalid], clear).is_err());
    assert_eq!(
        renderer.render(&[base], clear).unwrap().pixels,
        SHADER_OUTPUT.repeat(24)
    );
}

#[test]
fn application_depth_clear_supports_reversed_depth_and_reset() {
    use xenora_ffi::graphics_backend::FixtureRenderer;
    use xenora_loader::graphics::{
        GraphicsDepthCompare, GraphicsDraw, GraphicsDrawState, GraphicsError,
    };
    let mut renderer = FixtureRenderer::new().unwrap();
    let draw = GraphicsDraw {
        vertices: vec![
            [-1.0, -1.0, 0.25, 1.0],
            [3.0, -1.0, 0.25, 1.0],
            [-1.0, 3.0, 0.25, 1.0],
        ],
        viewport: (4, 3),
        state: GraphicsDrawState {
            depth_compare: Some(GraphicsDepthCompare::Greater),
            depth_write: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let blue = [0, 0, 255, 255];
    assert_eq!(
        renderer
            .render_with_depth_clear(std::slice::from_ref(&draw), blue, 0.0)
            .unwrap()
            .pixels,
        SHADER_OUTPUT.repeat(12)
    );
    assert_eq!(
        renderer
            .render_with_depth_clear(std::slice::from_ref(&draw), blue, 0.5)
            .unwrap()
            .pixels,
        blue.repeat(12)
    );
    for invalid in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
        assert_eq!(
            renderer.render_with_depth_clear(std::slice::from_ref(&draw), blue, invalid),
            Err(GraphicsError::InvalidDescriptor)
        );
    }
    assert_eq!(
        renderer
            .render(std::slice::from_ref(&draw), blue)
            .unwrap()
            .pixels,
        blue.repeat(12)
    );
    assert_eq!(
        renderer
            .render_with_depth_clear(&[draw], blue, 0.0)
            .unwrap()
            .pixels,
        SHADER_OUTPUT.repeat(12)
    );
}
