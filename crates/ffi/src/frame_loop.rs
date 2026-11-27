// SPDX-License-Identifier: GPL-3.0-or-later

//! Persistent guest rendering for one session (Gate 3, #266).
//!
//! The embedded guest records graphics commands once and submits one list per
//! frame. It waits at the frame boundary while paused and releases its graphics
//! objects before exiting. Only a checked guest readback reaches the app.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;

use xenora_loader::graphics::{FramePresentation, GraphicsBackendId, GraphicsError};
use xenora_loader::services::{
    XInputBatteryChargeState, XInputBatteryState, XInputControllerState,
};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{
    ExitReason, ImportRegistry, MockClock, ServicesState, register_standard_services,
};
use xenora_metal::NativeCompute;
use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};

const FIXTURE: &[u8] = include_bytes!("../../../fixtures/pe/app-graphics-loop.exe");
const FRAME_JOURNAL: &str = "C:\\xenora-app-frames.bin";
pub const WIDTH: usize = 4;
pub const HEIGHT: usize = 3;
pub const FRAME_BYTES: usize = WIDTH * HEIGHT * 4;
pub const MAX_FRAMES: u32 = 100_000;
/// Budget for startup, four command recordings, the final gate and teardown.
const SETUP_AND_EXIT_STEPS: usize = 2048;
/// Budget per guest frame, including graphics and journal write/readback.
const STEPS_PER_FRAME: usize = 96;
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

#[derive(Default, Copy, Clone)]
#[repr(C)]
pub struct ControllerInput {
    pub state: InputState,
    pub left_trigger: u16,
    pub right_trigger: u16,
}

impl ControllerInput {
    fn guest_bytes(self) -> [u8; 20] {
        let mut bytes = [0; 20];
        bytes[..4].copy_from_slice(&self.state.buttons.to_le_bytes());
        for (slot, value) in bytes[4..12].as_chunks_mut::<2>().0.iter_mut().zip([
            self.state.stick_x,
            self.state.stick_y,
            self.state.mouse_dx,
            self.state.mouse_dy,
        ]) {
            *slot = value.to_le_bytes();
        }
        bytes[12] = self.state.mouse_buttons;
        bytes[13] = self.state.connected;
        bytes[16..18].copy_from_slice(&self.left_trigger.to_le_bytes());
        bytes[18..20].copy_from_slice(&self.right_trigger.to_le_bytes());
        bytes
    }
}

/// A full controller snapshot in XInput units. Unlike the legacy fixture input
/// above, triggers already use the public XInput range 0 through 255 and both
/// thumbsticks are represented. `connected` must be 0 or 1, only defined
/// feature bits are accepted and reserved bytes must be zero.
#[derive(Default, Copy, Clone)]
#[repr(C)]
pub struct XInputSnapshot {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
    pub connected: u8,
    pub features: u8,
    pub reserved: [u8; 2],
}

/// Latest vibration command accepted by the guest for one XInput slot. When
/// `connected` is zero the sequence and motor values are zero. `enabled` is
/// the XInput reporting state: when zero the host must stop output even though
/// the stored motor values remain registered for a later re-enable. Reserved
/// bytes are always zero in output written by this library.
#[derive(Default, Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct XInputVibration {
    pub sequence: u32,
    pub left_motor: u16,
    pub right_motor: u16,
    pub connected: u8,
    pub enabled: u8,
    pub reserved: [u8; 2],
}

pub const XINPUT_FEATURE_VIBRATION: u8 = 0x01;

pub const XINPUT_BATTERY_STATE_UNKNOWN: u8 = 0;
pub const XINPUT_BATTERY_STATE_DISCHARGING: u8 = 1;
pub const XINPUT_BATTERY_STATE_CHARGING: u8 = 2;
pub const XINPUT_BATTERY_STATE_FULL: u8 = 3;
pub const XINPUT_BATTERY_LEVEL_UNKNOWN: u8 = u8::MAX;

/// Host battery evidence for one XInput controller. `present=0` means the host
/// exposed no battery object and requires the canonical zero state/level. For
/// a present object, level is 0 through 100 or XINPUT_BATTERY_LEVEL_UNKNOWN.
/// Charging state is evidence for host/internal use only; it is never treated
/// as battery chemistry.
#[derive(Default, Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct XInputBattery {
    pub present: u8,
    pub state: u8,
    pub level: u8,
    pub reserved: u8,
}

impl XInputBattery {
    pub(crate) fn valid_wire_shape(self) -> bool {
        self.present <= 1
            && self.state <= XINPUT_BATTERY_STATE_FULL
            && self.reserved == 0
            && (self.level <= 100 || self.level == XINPUT_BATTERY_LEVEL_UNKNOWN)
            && (self.present != 0 || (self.state == 0 && self.level == 0))
    }

    fn battery_state(self) -> Option<XInputBatteryState> {
        if self.present == 0 {
            return None;
        }
        let charge_state = match self.state {
            XINPUT_BATTERY_STATE_DISCHARGING => XInputBatteryChargeState::Discharging,
            XINPUT_BATTERY_STATE_CHARGING => XInputBatteryChargeState::Charging,
            XINPUT_BATTERY_STATE_FULL => XInputBatteryChargeState::Full,
            _ => XInputBatteryChargeState::Unknown,
        };
        Some(XInputBatteryState {
            charge_state,
            level_percent: (self.level != XINPUT_BATTERY_LEVEL_UNKNOWN).then_some(self.level),
        })
    }
}

impl XInputSnapshot {
    fn controller_state(self) -> Option<XInputControllerState> {
        (self.connected != 0).then_some(XInputControllerState {
            buttons: self.buttons,
            left_trigger: self.left_trigger,
            right_trigger: self.right_trigger,
            left_x: self.left_x,
            left_y: self.left_y,
            right_x: self.right_x,
            right_y: self.right_y,
        })
    }

    pub(crate) fn valid_wire_shape(self) -> bool {
        self.connected <= 1
            && self.features & !XINPUT_FEATURE_VIBRATION == 0
            && self.reserved == [0; 2]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
enum FrameCommand {
    Neutral = 0,
    Right = 1,
    Left = 2,
}

#[derive(Default)]
struct State {
    status: u32,
    paused: bool,
    stop: bool,
    interactive: bool,
    limit: u32,
    completed: u32,
    input: ControllerInput,
    input_queue: std::collections::VecDeque<InputState>,
    recorded_frames: Vec<(u32, Vec<u8>)>,
    admitted_record: Option<u8>,
    frame_input: Option<FrameCommand>,
    expected_column: u32,
    end_seen: bool,
    finish_seen: bool,
    expected_journal: Vec<u8>,
    verified_journal: Option<Arc<[u8]>>,
    latest: Option<(u32, Vec<u8>)>,
}

pub struct FrameLoop {
    state: Mutex<State>,
    wake: Condvar,
    thread: Mutex<Option<JoinHandle<()>>>,
    services: Arc<ServicesState>,
}

impl Default for FrameLoop {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            wake: Condvar::new(),
            thread: Mutex::default(),
            services: Arc::new(ServicesState::new(Arc::new(MockClock::new(0)))),
        }
    }
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

    /// Ask the guest to stop at its next frame boundary and wait for its thread.
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
        self.send_controller_input(ControllerInput {
            state: input,
            ..ControllerInput::default()
        });
    }

    pub fn send_controller_input(&self, input: ControllerInput) {
        let mut state = self.lock();
        state.input = input;
    }

    /// Install or remove one explicit XInput controller snapshot in the
    /// session's runtime services. Returns false for a malformed wire shape or
    /// a user index outside the documented XInput range.
    pub fn send_xinput(&self, user_index: u32, input: XInputSnapshot) -> bool {
        let vibration_supported =
            input.connected != 0 && input.features & XINPUT_FEATURE_VIBRATION != 0;
        input.valid_wire_shape()
            && self.services.set_xinput_controller_with_vibration(
                user_index,
                input.controller_state(),
                vibration_supported,
            )
    }

    /// Atomically install a controller snapshot and its latest battery
    /// evidence. The two inputs are validated before the slot lock is taken.
    pub fn send_xinput_with_battery(
        &self,
        user_index: u32,
        input: XInputSnapshot,
        battery: XInputBattery,
    ) -> bool {
        let vibration_supported =
            input.connected != 0 && input.features & XINPUT_FEATURE_VIBRATION != 0;
        input.valid_wire_shape()
            && battery.valid_wire_shape()
            && self.services.set_xinput_controller_with_details(
                user_index,
                input.controller_state(),
                vibration_supported,
                battery.battery_state(),
            )
    }

    /// Read the latest vibration command without consuming it. Disconnected
    /// slots report a neutral zeroed command so hosts can stop actuators.
    pub fn xinput_vibration(&self, user_index: u32) -> Option<XInputVibration> {
        if user_index >= 4 {
            return None;
        }
        let output = match self.services.xinput_vibration(user_index) {
            Some((sequence, left_motor, right_motor, enabled)) => XInputVibration {
                sequence,
                left_motor,
                right_motor,
                connected: 1,
                enabled: u8::from(enabled),
                reserved: [0; 2],
            },
            None => XInputVibration::default(),
        };
        Some(output)
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

    pub fn verified_journal(&self) -> Option<Arc<[u8]>> {
        self.lock().verified_journal.clone()
    }

    fn run(self: Arc<Self>) {
        self.finish_run(self.execute());
    }

    fn finish_run(&self, outcome: Result<Arc<[u8]>, String>) {
        let mut state = self.lock();
        state.status = match outcome {
            Err(_) => FAILED,
            Ok(journal) => {
                state.verified_journal = Some(journal);
                if state.stop && state.completed < state.limit {
                    STOPPED
                } else {
                    FINISHED
                }
            }
        };
    }

    fn execute(self: &Arc<Self>) -> Result<Arc<[u8]>, String> {
        let msl = compile_graphics(
            Architecture::Gfx1030,
            include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin"),
            include_bytes!("../../../fixtures/rdna2/x9-constant-ps.bin"),
        )
        .map_err(|e| format!("shader translation: {e}"))?
        .msl;
        let gpu = shared_gpu()?;
        let services = Arc::clone(&self.services);
        let native_draws = Arc::new(AtomicU32::new(0));
        let draws = Arc::clone(&native_draws);
        services
            .graphics
            .lock()
            .map_err(|_| "guest graphics state is poisoned")?
            .set_metal_backend(move |vertices, (width, height), clear| {
                let pixels = gpu
                    .render_rgba8(
                        &msl,
                        [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
                        vertices,
                        &[[0.0; 4]; 3],
                        [width, height],
                        clear,
                    )
                    .map_err(|e| GraphicsError::BackendError(format!("Metal draw: {e}")))?;
                draws.fetch_add(1, Ordering::Relaxed);
                Ok(pixels)
            });
        self.execute_guest(GraphicsBackendId::MetalNative, &native_draws)
    }

    fn execute_guest(
        self: &Arc<Self>,
        backend: GraphicsBackendId,
        native_draws: &Arc<AtomicU32>,
    ) -> Result<Arc<[u8]>, String> {
        let services = &self.services;
        let last_presentation = services
            .graphics
            .lock()
            .map_err(|_| "guest graphics state is poisoned")?
            .last_presentation
            .clone();
        let shared = Arc::clone(self);
        let mut imports = ImportRegistry::new();
        register_standard_services(&mut imports, Arc::clone(services));
        imports.register("xenora.dll", "frame_begin", move |args, _, _| {
            shared.begin_frame(args.rcx as u32)
        });
        let shared = Arc::clone(self);
        let draws = Arc::clone(native_draws);
        let presentation = Arc::clone(&last_presentation);
        imports.register("xenora.dll", "frame_present", move |args, _, memory| {
            let mut readback = [0; FRAME_BYTES];
            memory
                .read_bytes(args.r8, &mut readback)
                .map_err(ExitReason::MemoryFault)?;
            let frame = presentation
                .lock()
                .map_err(|_| ExitReason::ServiceAbiViolation)?;
            shared.publish_frame(
                args.rcx as u32,
                args.rdx as u32,
                &readback,
                &frame,
                backend,
                draws.load(Ordering::Relaxed),
            )
        });
        let shared_poll = Arc::clone(self);
        imports.register("xenora.dll", "input_poll", move |_, _, _| {
            shared_poll
                .lock()
                .frame_input
                .take()
                .map(|command| command as u64)
                .ok_or(ExitReason::ServiceAbiViolation)
        });
        let shared_finish = Arc::clone(self);
        let finish_services = Arc::clone(services);
        imports.register("xenora.dll", "frame_finish", move |args, _, _| {
            shared_finish.frame_finish(args.rcx as u32, finish_services.open_handle_count())
        });
        let snapshot = Arc::clone(self);
        imports.register(
            "xenora.dll",
            "input_poll_snapshot",
            move |args, _, memory| {
                let input = snapshot.lock().input;
                memory
                    .write_bytes(args.rcx, &input.guest_bytes())
                    .map_err(ExitReason::MemoryFault)?;
                Ok(1)
            },
        );
        let limit = self.lock().limit as usize;
        let startup = run_synthetic_startup(
            FIXTURE,
            StartupLayout {
                image_base: 0x1_4000_0000,
                stack_top: 0x2_0001_0000,
                tls_base: 0x3_0000_0000,
            },
            imports,
            limit * STEPS_PER_FRAME + SETUP_AND_EXIT_STEPS,
        )
        .map_err(|e| format!("startup: {e}"))?;
        if startup.execution.exit_reason != ExitReason::ProcessExit(0) {
            return Err(format!(
                "guest ended with {:?}",
                startup.execution.exit_reason
            ));
        }
        let rendered = last_presentation
            .lock()
            .map_err(|_| "guest graphics presentation is poisoned")?
            .frame_index;
        let state = self.lock();
        if !state.end_seen
            || state.admitted_record.is_some()
            || !state.finish_seen
            || services.exit_status() != Some(0)
        {
            return Err("guest exited before completing the frame protocol".to_owned());
        }
        if rendered != u64::from(state.completed) {
            return Err("guest rendered a frame without publishing it".to_owned());
        }
        if backend == GraphicsBackendId::MetalNative
            && native_draws.load(Ordering::Relaxed) != state.completed
        {
            return Err(
                "guest native draw count differs from the completed frame count".to_owned(),
            );
        }
        let journal = services
            .file_contents(FRAME_JOURNAL)
            .ok_or("guest did not create its frame journal")?;
        if journal != state.expected_journal {
            return Err("guest frame journal differs from accepted frames".to_owned());
        }
        Ok(journal.into())
    }

    fn begin_frame(&self, sequence: u32) -> Result<u64, ExitReason> {
        let mut state = self.lock();
        if state.admitted_record.is_some() || state.end_seen || sequence != state.completed {
            return Err(ExitReason::ServiceAbiViolation);
        }
        // The interactive loop samples the next direction before waiting on a pause.
        let input =
            (state.interactive && !state.stop && state.completed < state.limit).then(|| {
                let input = state.input_queue.pop_front().unwrap_or(state.input.state);
                if input.buttons & crate::XENORA_INPUT_RIGHT != 0 {
                    FrameCommand::Right
                } else if input.buttons & crate::XENORA_INPUT_LEFT != 0 {
                    FrameCommand::Left
                } else {
                    FrameCommand::Neutral
                }
            });
        while state.paused && !state.stop && state.completed < state.limit {
            state = self.wake.wait(state).unwrap_or_else(|p| p.into_inner());
        }
        if state.stop || state.completed >= state.limit {
            state.end_seen = true;
            return Ok(0);
        }
        let record = if let Some(command) = input {
            let previous = state.latest.as_ref().map_or(0, |(column, _)| *column);
            let width = WIDTH as u32;
            state.expected_column = match command {
                FrameCommand::Neutral => previous,
                FrameCommand::Right => (previous + 1) % width,
                FrameCommand::Left => (previous + width - 1) % width,
            };
            0x20 | ((command as u8) << 2) | state.expected_column as u8
        } else {
            0x10 | (sequence % WIDTH as u32) as u8
        };
        state.admitted_record = Some(record);
        state.frame_input = input;
        Ok(if state.interactive { 2 } else { 1 })
    }

    fn publish_frame(
        &self,
        sequence: u32,
        column: u32,
        readback: &[u8; FRAME_BYTES],
        frame: &FramePresentation,
        backend: GraphicsBackendId,
        native_draws: u32,
    ) -> Result<u64, ExitReason> {
        let mut state = self.lock();
        let record = state
            .admitted_record
            .ok_or(ExitReason::ServiceAbiViolation)?;
        let id = if state.interactive {
            state.expected_column
        } else {
            sequence
        };
        if state.frame_input.is_some()
            || sequence != state.completed
            || column >= WIDTH as u32
            || column != id % WIDTH as u32
            || frame.frame_index != u64::from(sequence) + 1
            || (frame.width, frame.height) != (WIDTH, HEIGHT)
            || frame.backend != Some(backend)
            || frame.draw_count != 1
            || (backend == GraphicsBackendId::MetalNative && native_draws != sequence + 1)
            || frame.pixels != readback.as_slice()
            || expected(id) != readback.as_slice()
        {
            return Err(ExitReason::ServiceAbiViolation);
        }
        state.admitted_record = None;
        state.expected_journal.push(record);
        state.completed += 1;
        let pixels = readback.to_vec();
        if state.interactive {
            state.recorded_frames.push((id, pixels.clone()));
        }
        state.latest = Some((id, pixels));
        Ok(1)
    }

    fn frame_finish(&self, count: u32, open_handles: usize) -> Result<u64, ExitReason> {
        let mut state = self.lock();
        if !state.end_seen
            || state.admitted_record.is_some()
            || state.frame_input.is_some()
            || state.finish_seen
            || count != state.completed
            || open_handles != 0
        {
            return Err(ExitReason::ServiceAbiViolation);
        }
        state.finish_seen = true;
        Ok(1)
    }
}

/// One Metal device and command queue shared by every frame-loop session (#290).
///
/// The first command buffer submitted on a queue makes the GPU driver allocate a
/// working set for it (about 45 MiB on an Apple M5 with Metal validation), and the
/// driver frees that memory asynchronously, 10–50 ms after the queue is released.
/// A queue per session therefore allocated and freed it once per session, and the
/// process footprint depended on how far that release had progressed. One queue
/// for the process allocates the working set once and keeps it: bounded caching,
/// not per-session churn. `NativeCompute` is `Send + Sync`, and rendering takes
/// `&self`, so concurrent sessions can share it. A device that cannot be opened
/// is reported to every session that asks.
fn shared_gpu() -> Result<&'static NativeCompute, String> {
    static SHARED: OnceLock<Result<NativeCompute, String>> = OnceLock::new();
    SHARED
        .get_or_init(|| NativeCompute::new().map_err(|e| format!("Metal device: {e}")))
        .as_ref()
        .map_err(Clone::clone)
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

#[cfg(test)]
#[test]
fn embedded_guest_renders_standard_and_queued_interactive_frames() {
    let red = [255, 0, 0, 255];
    let clear = [19, 43, 93, 251];
    let check_pixels = |column: usize, pixels: Vec<u8>| {
        let mut row = [clear; 4];
        row[column] = red;
        assert_eq!(pixels, row.concat().repeat(3));
    };
    for interactive in [false, true] {
        let session = Arc::new(FrameLoop::default());
        session.lock().limit = 5;
        session.lock().interactive = interactive;
        if interactive {
            let left = crate::XENORA_INPUT_LEFT;
            let right = crate::XENORA_INPUT_RIGHT;
            for buttons in [left, left | right, 0, right] {
                session.queue_input(InputState {
                    buttons,
                    ..InputState::default()
                });
            }
            session.send_input(InputState {
                buttons: left,
                ..InputState::default()
            });
        }
        let journal = session
            .execute_guest(
                GraphicsBackendId::SoftwareReference,
                &Arc::new(AtomicU32::new(0)),
            )
            .unwrap();
        let expected_journal: &[u8] = if interactive {
            &[0x2b, 0x24, 0x20, 0x25, 0x28]
        } else {
            &[0x10, 0x11, 0x12, 0x13, 0x10]
        };
        assert_eq!(journal.as_ref(), expected_journal);
        assert!(session.verified_journal().is_none());
        session.finish_run(Ok(journal));
        assert_eq!(
            session.verified_journal().as_deref(),
            Some(expected_journal)
        );

        let snapshot = session.snapshot();
        assert_eq!(snapshot.status, FINISHED);
        assert_eq!(snapshot.completed, 5);
        let (id, pixels) = snapshot.latest.unwrap();
        assert_eq!(id, if interactive { 0 } else { 4 });
        check_pixels(0, pixels);
        if interactive {
            for (index, column) in [3, 0, 0, 1, 0].into_iter().enumerate() {
                let (id, pixels) = session.get_recorded_frame(index).unwrap();
                assert_eq!(id, column);
                check_pixels(column as usize, pixels);
            }
        }
        assert_eq!(session.services.open_handle_count(), 0);
    }
}

#[cfg(test)]
#[test]
fn guest_journal_crosses_readback_chunk_boundary_and_isolates_sessions() {
    let run = |limit| {
        let session = Arc::new(FrameLoop::default());
        session.lock().limit = limit;
        let journal = session
            .execute_guest(
                GraphicsBackendId::SoftwareReference,
                &Arc::new(AtomicU32::new(0)),
            )
            .unwrap();
        session.finish_run(Ok(journal));
        session
    };

    let first = run(257);
    let second = run(1);
    let cycle: Vec<_> = [0x10, 0x11, 0x12, 0x13]
        .into_iter()
        .cycle()
        .take(257)
        .collect();
    assert_eq!(first.verified_journal().as_deref(), Some(cycle.as_slice()));
    assert_eq!(
        second.verified_journal().as_deref(),
        Some([0x10].as_slice())
    );
}

#[cfg(test)]
#[test]
fn embedded_guest_releases_resources_on_stop_before_first_draw() {
    use std::time::{Duration, Instant};

    let session = Arc::new(FrameLoop::default());
    {
        let mut state = session.lock();
        state.limit = 5;
        state.interactive = true;
        state.paused = true;
        state.status = PAUSED;
    }
    session.queue_input(InputState::default());
    let worker_session = Arc::clone(&session);
    let worker = std::thread::spawn(move || {
        worker_session.execute_guest(
            GraphicsBackendId::SoftwareReference,
            &Arc::new(AtomicU32::new(0)),
        )
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    // The queued command is sampled at the first gate before the guest waits.
    while !session.lock().input_queue.is_empty()
        && !worker.is_finished()
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(1));
    }
    let reached_gate = session.lock().input_queue.is_empty();
    let open_handles = session.services.open_handle_count();
    let file = session.services.file_contents(FRAME_JOURNAL);
    let exposed = session.verified_journal();
    session.stop();
    let journal = worker.join().unwrap().unwrap();
    assert!(reached_gate);
    assert_eq!(open_handles, 1);
    assert_eq!(file.as_deref(), Some(&[][..]));
    assert!(exposed.is_none());
    assert!(journal.is_empty());
    session.finish_run(Ok(journal));
    assert_eq!(session.snapshot().status, STOPPED);
    assert_eq!(session.snapshot().completed, 0);
    assert!(session.snapshot().latest.is_none());
    assert_eq!(session.verified_journal().as_deref(), Some(&[][..]));
    assert_eq!(session.services.open_handle_count(), 0);
}

#[cfg(test)]
#[test]
fn paused_interactive_frame_keeps_its_sampled_command() {
    use std::time::{Duration, Instant};

    let session = Arc::new(FrameLoop::default());
    {
        let mut state = session.lock();
        state.status = PAUSED;
        state.paused = true;
        state.limit = 1;
        state.interactive = true;
    }
    session.queue_input(InputState {
        buttons: crate::XENORA_INPUT_LEFT,
        ..InputState::default()
    });
    let guest = Arc::clone(&session);
    let gate = std::thread::spawn(move || guest.begin_frame(0));
    let until = Instant::now() + Duration::from_secs(2);
    let sampled = loop {
        if session.lock().input_queue.is_empty() {
            break true;
        }
        if Instant::now() >= until {
            break false;
        }
        std::thread::yield_now();
    };
    session.send_input(InputState {
        buttons: crate::XENORA_INPUT_RIGHT,
        ..InputState::default()
    });
    assert!(session.set_paused(false));
    assert!(matches!(gate.join().unwrap(), Ok(2)));
    assert!(sampled);
    let state = session.lock();
    assert_eq!(state.frame_input, Some(FrameCommand::Left));
    assert_eq!(state.expected_column, 3);
    assert_eq!(state.admitted_record, Some(0x2b));
}

#[cfg(test)]
#[test]
fn interactive_publication_rejects_a_coherent_frame_for_the_wrong_column() {
    let session = FrameLoop::default();
    {
        let mut state = session.lock();
        state.limit = 2;
        state.interactive = true;
    }
    session.send_input(InputState {
        buttons: crate::XENORA_INPUT_RIGHT,
        ..InputState::default()
    });
    let backend = GraphicsBackendId::SoftwareReference;
    let publish = |sequence, column| {
        let readback: [u8; FRAME_BYTES] = expected(column).try_into().unwrap();
        let frame = FramePresentation {
            width: WIDTH,
            height: HEIGHT,
            pixels: readback.to_vec(),
            frame_index: u64::from(sequence) + 1,
            draw_count: 1,
            backend: Some(backend),
        };
        session.publish_frame(sequence, column, &readback, &frame, backend, 0)
    };

    assert!(matches!(session.begin_frame(0), Ok(2)));
    assert_eq!(session.lock().frame_input.take(), Some(FrameCommand::Right));
    assert!(matches!(publish(0, 1), Ok(1)));

    assert!(matches!(session.begin_frame(1), Ok(2)));
    assert_eq!(session.lock().frame_input.take(), Some(FrameCommand::Right));
    assert!(matches!(
        publish(1, 3),
        Err(ExitReason::ServiceAbiViolation)
    ));
    let snapshot = session.snapshot();
    assert_eq!(snapshot.completed, 1);
    assert_eq!(snapshot.latest, Some((1, expected(1))));
    assert!(session.get_recorded_frame(1).is_none());
    assert_eq!(session.lock().expected_journal, [0x25]);
    assert!(session.verified_journal().is_none());

    assert!(matches!(publish(1, 2), Ok(1)));
    assert_eq!(session.get_recorded_frame(1), Some((2, expected(2))));
    assert_eq!(session.lock().expected_journal, [0x25, 0x26]);
}

#[cfg(test)]
#[test]
fn frame_finish_requires_the_end_gate_and_explicit_cleanup() {
    let session = FrameLoop::default();
    assert!(session.frame_finish(0, 0).is_err());
    assert!(matches!(session.begin_frame(0), Ok(0)));
    assert!(session.frame_finish(1, 0).is_err());
    assert!(session.frame_finish(0, 1).is_err());
    assert!(matches!(session.frame_finish(0, 0), Ok(1)));
    assert!(session.frame_finish(0, 0).is_err());
    assert!(session.begin_frame(0).is_err());
    assert!(session.verified_journal().is_none());
}

#[cfg(test)]
#[test]
fn publication_requires_fresh_presentation_and_actual_native_draw() {
    let session = FrameLoop::default();
    session.lock().limit = 2;
    let readback: [u8; FRAME_BYTES] = expected(0).try_into().unwrap();
    let backend = GraphicsBackendId::MetalNative;
    let mut frame = FramePresentation {
        width: WIDTH,
        height: HEIGHT,
        pixels: readback.to_vec(),
        frame_index: 1,
        draw_count: 1,
        backend: Some(backend),
    };
    let publish = |frame: &FramePresentation, pixels: &[u8; FRAME_BYTES], draws| {
        session.publish_frame(0, 0, pixels, frame, backend, draws)
    };
    assert!(publish(&frame, &readback, 1).is_err());
    assert!(matches!(session.begin_frame(0), Ok(1)));
    frame.frame_index = 0;
    assert!(publish(&frame, &readback, 1).is_err());
    frame.frame_index = 1;
    assert!(publish(&frame, &readback, 0).is_err());
    assert!(publish(&frame, &[0; FRAME_BYTES], 1).is_err());
    assert_eq!(session.snapshot().completed, 0);
    assert!(session.snapshot().latest.is_none());

    assert!(matches!(publish(&frame, &readback, 1), Ok(1)));
    assert!(publish(&frame, &readback, 1).is_err());
    assert_eq!(session.snapshot().completed, 1);
}

#[cfg(test)]
#[test]
fn xinput_snapshots_reach_the_runtime_state_service() {
    let session = FrameLoop::default();
    let snapshot = XInputSnapshot {
        buttons: 0x9011,
        left_trigger: 17,
        right_trigger: 240,
        left_x: -32768,
        left_y: 32767,
        right_x: -1234,
        right_y: 4321,
        connected: 1,
        features: XINPUT_FEATURE_VIBRATION,
        reserved: [0; 2],
    };
    assert_eq!(std::mem::size_of::<XInputSnapshot>(), 16);
    assert!(session.send_xinput(2, snapshot));

    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&session.services));
    let mut memory = xenora_loader::GuestMemory::new();
    memory
        .map_region(0x1000, 0x1000, xenora_loader::MemoryPermissions::READ_WRITE)
        .unwrap();
    let mut cpu = xenora_loader::CpuState::default();
    cpu.set(xenora_loader::Register::Rcx, 2);
    cpu.set(xenora_loader::Register::Rdx, 0x1200);
    let descriptor = registry
        .resolve(
            "xinput1_4.dll",
            &xenora_loader::import_table::ImportName::Name("XInputGetState".to_owned()),
        )
        .unwrap();
    let result =
        (descriptor.handler)(&xenora_loader::Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap();
    assert_eq!(result, 0);
    let mut output = [0; 16];
    memory.read_bytes(0x1200, &mut output).unwrap();
    assert_eq!(
        output,
        [
            1, 0, 0, 0, 0x11, 0x90, 17, 240, 0, 0x80, 0xff, 0x7f, 0x2e, 0xfb, 0xe1, 0x10,
        ]
    );

    let mut cpu = xenora_loader::CpuState::default();
    cpu.set(xenora_loader::Register::Rcx, 2);
    cpu.set(xenora_loader::Register::Rdx, 0);
    cpu.set(xenora_loader::Register::R8, 0x1220);
    let descriptor = registry
        .resolve(
            "xinput1_4.dll",
            &xenora_loader::import_table::ImportName::Name("XInputGetCapabilities".to_owned()),
        )
        .unwrap();
    assert_eq!(
        (descriptor.handler)(&xenora_loader::Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap(),
        0
    );
    let mut capabilities = [0; 20];
    memory.read_bytes(0x1220, &mut capabilities).unwrap();
    assert_eq!(capabilities[2..4], [1, 0]);
    assert_eq!(capabilities[16..20], [0xff; 4]);

    assert_eq!(
        session.xinput_vibration(2),
        Some(XInputVibration {
            connected: 1,
            enabled: 1,
            ..XInputVibration::default()
        })
    );
    memory.write_u32(0x1210, 0x8001_4002).unwrap();
    let mut cpu = xenora_loader::CpuState::default();
    cpu.set(xenora_loader::Register::Rcx, 2);
    cpu.set(xenora_loader::Register::Rdx, 0x1210);
    let descriptor = registry
        .resolve(
            "xinput1_4.dll",
            &xenora_loader::import_table::ImportName::Name("XInputSetState".to_owned()),
        )
        .unwrap();
    assert_eq!(
        (descriptor.handler)(&xenora_loader::Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap(),
        0
    );
    assert_eq!(
        session.xinput_vibration(2),
        Some(XInputVibration {
            sequence: 1,
            left_motor: 0x4002,
            right_motor: 0x8001,
            connected: 1,
            enabled: 1,
            reserved: [0; 2],
        })
    );
    let mut cpu = xenora_loader::CpuState::default();
    cpu.set(xenora_loader::Register::Rcx, 0);
    let descriptor = registry
        .resolve(
            "xinput1_4.dll",
            &xenora_loader::import_table::ImportName::Name("XInputEnable".to_owned()),
        )
        .unwrap();
    assert_eq!(
        (descriptor.handler)(&xenora_loader::Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap(),
        0
    );
    assert_eq!(
        session.xinput_vibration(2),
        Some(XInputVibration {
            sequence: 1,
            left_motor: 0x4002,
            right_motor: 0x8001,
            connected: 1,
            enabled: 0,
            reserved: [0; 2],
        })
    );
    cpu.set(xenora_loader::Register::Rcx, 1);
    assert_eq!(
        (descriptor.handler)(&xenora_loader::Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap(),
        0
    );
    assert_eq!(
        session.xinput_vibration(2).map(|command| command.enabled),
        Some(1)
    );

    assert_eq!(std::mem::size_of::<XInputVibration>(), 12);
    assert!(session.send_xinput(
        2,
        XInputSnapshot {
            connected: 0,
            ..XInputSnapshot::default()
        }
    ));
    assert_eq!(
        session.xinput_vibration(2),
        Some(XInputVibration::default())
    );
    assert!(session.xinput_vibration(4).is_none());

    assert!(!session.send_xinput(4, snapshot));
    assert!(!session.send_xinput(
        0,
        XInputSnapshot {
            connected: 2,
            ..snapshot
        },
    ));
    assert!(!session.send_xinput(
        0,
        XInputSnapshot {
            reserved: [1, 0],
            ..snapshot
        },
    ));
}

#[cfg(test)]
#[test]
fn extended_controller_snapshot_retains_trigger_bytes_and_legacy_input_resets_them() {
    let running = FrameLoop::default();
    running.send_controller_input(ControllerInput {
        state: InputState {
            buttons: 0x10,
            stick_x: -32767,
            stick_y: 9,
            connected: 1,
            reserved: u16::MAX,
            ..InputState::default()
        },
        left_trigger: 0x1234,
        right_trigger: u16::MAX,
    });
    let bytes = running.lock().input.guest_bytes();
    assert_eq!(std::mem::size_of::<ControllerInput>(), 20);
    assert_eq!(
        bytes,
        [
            16, 0, 0, 0, 1, 128, 9, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0x34, 0x12, 255, 255
        ]
    );
    running.send_input(InputState::default());
    assert_eq!(running.lock().input.guest_bytes(), [0; 20]);
}
