// SPDX-License-Identifier: GPL-3.0-or-later

//! Persistent guest rendering for one session (Gate 3, #266).
//!
//! The embedded `frame-loop.exe` fixture calls `xenora.dll!frame_submit(index)`
//! with index 0, 1, 2, ... and halts when the call returns 0. The guest runs on
//! its own thread for the session's lifetime. Pause, resume and stop act at the
//! frame-submit boundary: a paused guest waits inside that call with its
//! architectural state untouched, and a stop request makes the call return 0 so
//! the guest leaves its loop and halts.

use std::cell::RefCell;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::graphics_backend::FixtureRenderer;
use xenora_loader::graphics::{GraphicsDraw, GraphicsDrawState};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{ExitReason, ImportRegistry, Register};

const FIXTURE: &[u8] = include_bytes!("../../../fixtures/pe/frame-loop.exe");
const INTERACTIVE_FIXTURE: &[u8] = include_bytes!("../../../fixtures/pe/interactive.exe");
pub const WIDTH: usize = 4;
pub const HEIGHT: usize = 3;
pub const FRAME_BYTES: usize = WIDTH * HEIGHT * 4;
pub const MAX_FRAMES: u32 = 100_000;
/// Guest instructions per loop iteration (mov, call, inc, test, jnz) plus slack.
const STEPS_PER_FRAME: usize = 8;
const STEPS_PER_INTERACTIVE_FRAME: usize = 24;
/// `x9-constant-ps.s` exports (1, 0, 0, 1): opaque red. Everything else is the clear colour.
const RED: [u8; 4] = [255, 0, 0, 255];
const CLEAR: [u8; 4] = [19, 43, 93, 251];

pub const RUNNING: u32 = 1;
pub const PAUSED: u32 = 2;
pub const FINISHED: u32 = 3;
pub const STOPPED: u32 = 4;
pub const FAILED: u32 = 5;

#[derive(Default, Copy, Clone)]
#[repr(C)]
pub struct InputState {
    pub buttons: u32,
    pub stick_x: i16,
    pub stick_y: i16,
    pub mouse_dx: i16,
    pub mouse_dy: i16,
    pub mouse_buttons: u8,
    pub connected: u8,
    pub reserved: u16,
}

#[derive(Default)]
struct State {
    status: u32,
    paused: bool,
    stop: bool,
    interactive: bool,
    limit: u32,
    completed: u32,
    input: InputState,
    input_queue: std::collections::VecDeque<InputState>,
    recorded_frames: Vec<(u32, Vec<u8>)>,
    /// Every `frame_submit` call the guest made, including the one answered with 0.
    calls: u64,
    latest: Option<(u32, Vec<u8>)>,
}

#[derive(Default)]
pub struct FrameLoop {
    state: Mutex<State>,
    wake: Condvar,
    thread: Mutex<Option<JoinHandle<()>>>,
}

pub struct Snapshot {
    pub status: u32,
    pub completed: u32,
    pub latest: Option<(u32, Vec<u8>)>,
}

impl FrameLoop {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn start(limit: u32, interactive: bool) -> Arc<Self> {
        let shared = Arc::new(Self::default());
        {
            let mut state = shared.lock();
            state.status = RUNNING;
            state.limit = limit;
            state.interactive = interactive;
        }
        let guest = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("xenora-guest".into())
            .spawn(move || guest.run())
            .ok();
        if handle.is_none() {
            shared.lock().status = FAILED;
        }
        *shared.thread.lock().unwrap_or_else(|p| p.into_inner()) = handle;
        shared
    }

    /// Pause or resume. Returns false unless the loop is running or paused.
    pub fn set_paused(&self, paused: bool) -> bool {
        let mut state = self.lock();
        if !matches!(state.status, RUNNING | PAUSED) {
            return false;
        }
        state.paused = paused;
        state.status = if paused { PAUSED } else { RUNNING };
        self.wake.notify_all();
        true
    }

    /// Ask the guest to stop at its next frame submission and wait for its thread.
    pub fn stop(&self) {
        {
            let mut state = self.lock();
            state.stop = true;
            self.wake.notify_all();
        }
        if let Some(handle) = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = handle.join();
        }
    }

    pub fn send_input(&self, input: InputState) {
        let mut state = self.lock();
        state.input = input;
    }

    pub fn queue_input(&self, input: InputState) {
        let mut state = self.lock();
        state.input_queue.push_back(input);
    }

    pub fn get_recorded_frame(&self, index: usize) -> Option<(u32, Vec<u8>)> {
        let state = self.lock();
        state.recorded_frames.get(index).cloned()
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.lock();
        Snapshot {
            status: state.status,
            completed: state.completed,
            latest: state.latest.clone(),
        }
    }

    fn run(self: Arc<Self>) {
        let outcome = self.execute();
        let mut state = self.lock();
        state.status = match outcome {
            Err(_) => FAILED,
            Ok(()) if state.stop && state.completed < state.limit => STOPPED,
            Ok(()) => FINISHED,
        };
    }

    fn execute(self: &Arc<Self>) -> Result<(), String> {
        let renderer = FixtureRenderer::new()?;
        // The loader context and Metal adapter live on the guest thread.
        GPU.with(|slot| *slot.borrow_mut() = Some(renderer));

        let shared = Arc::clone(self);
        let mut imports = ImportRegistry::new();
        imports.register("xenora.dll", "frame_submit", move |args, _, memory| {
            let index = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
            shared
                .submit(index)
                .ok_or(ExitReason::IllegalInstruction(0xff))
        });
        if self.lock().interactive {
            let shared_poll = Arc::clone(self);
            imports.register("xenora.dll", "input_poll", move |_args, _, _memory| {
                let mut state = shared_poll.lock();
                let input = state.input_queue.pop_front().unwrap_or(state.input);
                let cmd = if input.buttons & crate::XENORA_INPUT_RIGHT != 0 {
                    1u64
                } else if input.buttons & crate::XENORA_INPUT_LEFT != 0 {
                    2u64
                } else {
                    0u64
                };
                Ok(cmd)
            });
        }
        let limit = self.lock().limit as usize;
        let is_interactive = self.lock().interactive;
        let fixture_bytes = if is_interactive {
            INTERACTIVE_FIXTURE
        } else {
            FIXTURE
        };
        let steps_per_frame = if is_interactive {
            STEPS_PER_INTERACTIVE_FRAME
        } else {
            STEPS_PER_FRAME
        };
        let startup = run_synthetic_startup(
            fixture_bytes,
            StartupLayout {
                image_base: 0x1_4000_0000,
                stack_top: 0x2_0001_0000,
                tls_base: 0x3_0000_0000,
            },
            imports,
            (limit + 1) * steps_per_frame + 32,
        )
        .map_err(|e| format!("startup: {e}"));
        GPU.with(|slot| slot.borrow_mut().take());
        let startup = startup?;
        let calls = self.lock().calls;
        let rbx = startup.execution.final_state.get(Register::Rbx);
        if startup.execution.exit_reason != ExitReason::Halt {
            return Err(format!(
                "guest ended with {:?}",
                startup.execution.exit_reason
            ));
        }
        if !is_interactive && rbx != calls {
            return Err(format!("guest ended with rbx={rbx}, calls={calls}"));
        }
        Ok(())
    }

    /// Handle one `frame_submit`: wait while paused, then render and verify the
    /// frame. `None` fails the guest; `Some(0)` tells it to stop.
    fn submit(&self, index: u64) -> Option<u64> {
        let mut state = self.lock();
        state.calls += 1;
        while state.paused && !state.stop {
            state = self.wake.wait(state).unwrap_or_else(|p| p.into_inner());
        }
        if state.stop || state.completed >= state.limit {
            return Some(0);
        }
        if state.interactive {
            if index >= WIDTH as u64 {
                return None; // column must be inside 0..WIDTH
            }
        } else if index != u64::from(state.completed) {
            return None; // frame identities must arrive in order
        }
        drop(state);
        let pixels = render(index as u32)?;
        let mut state = self.lock();
        state.completed += 1;
        state.latest = Some((index as u32, pixels.clone()));
        if state.interactive {
            state.recorded_frames.push((index as u32, pixels));
        }
        Some(u64::from(state.completed < state.limit && !state.stop))
    }
}

thread_local! {
    static GPU: RefCell<Option<FixtureRenderer>> = const { RefCell::new(None) };
}

/// Frame `index` is a red strip in column `index % 4` on the clear colour.
///
/// The triangle (x0, -10), (x0 + 0.7, 0), (x0, 10) in clip space, with x0 the
/// column's left edge, covers that column's pixel centres in all three rows and
/// none of its neighbours'.
fn render(index: u32) -> Option<Vec<u8>> {
    let column = (index as usize % WIDTH) as f32;
    let x0 = -1.0 + column * (2.0 / WIDTH as f32);
    let pixels = GPU.with(|slot| {
        let mut slot = slot.borrow_mut();
        let renderer = slot.as_mut()?;
        renderer
            .render(
                &[GraphicsDraw {
                    vertices: vec![
                        [x0, -10.0, 0.5, 1.0],
                        [x0 + 0.7, 0.0, 0.5, 1.0],
                        [x0, 10.0, 0.5, 1.0],
                    ],
                    viewport: (WIDTH, HEIGHT),
                    state: GraphicsDrawState::default(),
                    ..Default::default()
                }],
                CLEAR,
            )
            .ok()
            .map(|out| out.pixels)
    })?;
    (pixels == expected(index)).then_some(pixels)
}

/// The expected frame for `index`, from the strip rule alone.
pub fn expected(index: u32) -> Vec<u8> {
    (0..WIDTH * HEIGHT)
        .flat_map(|pixel| {
            if pixel % WIDTH == index as usize % WIDTH {
                RED
            } else {
                CLEAR
            }
        })
        .collect()
}
