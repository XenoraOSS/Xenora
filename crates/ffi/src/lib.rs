// SPDX-License-Identifier: GPL-3.0-or-later

//! Versioned C interface for the native macOS application (see `include/xenora.h`).
//!
//! Sessions are opaque 64-bit handles: the low 32 bits index a slot and the high
//! 32 bits hold that slot's generation, so a stale handle never reaches a reused slot.
//! Every exported function catches panics and reports them as `XENORA_INTERNAL_ERROR`.

use std::fmt::Write as _;
mod frame_loop;

use frame_loop::FrameLoop;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{ExitReason, ImportRegistry, ParsedPe, ProbeConfig, Register, probe_pe_bytes};
use xenora_metal::{MetalRgba8Texture, MetalRgba8TextureDesc, NativeCompute, Rgba8MipLevel};
use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};

pub const XENORA_ABI_VERSION: u32 = 1;

pub const XENORA_OK: i32 = 0;
pub const XENORA_INVALID_ARGUMENT: i32 = 1;
pub const XENORA_INVALID_HANDLE: i32 = 2;
pub const XENORA_VERSION_MISMATCH: i32 = 3;
pub const XENORA_INTERNAL_ERROR: i32 = 4;
pub const XENORA_PE_ERROR: i32 = 5;
pub const XENORA_LIMIT_EXCEEDED: i32 = 6;
pub const XENORA_BUFFER_TOO_SMALL: i32 = 7;
pub const XENORA_EXECUTION_FAILED: i32 = 8;
pub const XENORA_INVALID_STATE: i32 = 9;
pub const XENORA_FILE_NOT_FOUND: i32 = 10;
pub const XENORA_ACCESS_DENIED: i32 = 11;
pub const XENORA_IO_ERROR: i32 = 12;

pub const XENORA_LOOP_NONE: u32 = 0;
pub const XENORA_LOOP_RUNNING: u32 = frame_loop::RUNNING;
pub const XENORA_LOOP_PAUSED: u32 = frame_loop::PAUSED;
pub const XENORA_LOOP_FINISHED: u32 = frame_loop::FINISHED;
pub const XENORA_LOOP_STOPPED: u32 = frame_loop::STOPPED;
pub const XENORA_LOOP_FAILED: u32 = frame_loop::FAILED;
pub const XENORA_FRAME_LOOP_MAX_FRAMES: u32 = frame_loop::MAX_FRAMES;
pub const XENORA_NO_FRAME: u32 = u32::MAX;

pub const XENORA_INPUT_NONE: u32 = 0;
pub const XENORA_INPUT_RIGHT: u32 = 1 << 0;
pub const XENORA_INPUT_LEFT: u32 = 1 << 1;
pub const XENORA_INPUT_UP: u32 = 1 << 2;
pub const XENORA_INPUT_DOWN: u32 = 1 << 3;
pub const XENORA_INPUT_ACTION_A: u32 = 1 << 4;
pub const XENORA_INPUT_ACTION_B: u32 = 1 << 5;

pub const XENORA_XINPUT_GAMEPAD_DPAD_UP: u16 = 0x0001;
pub const XENORA_XINPUT_GAMEPAD_DPAD_DOWN: u16 = 0x0002;
pub const XENORA_XINPUT_GAMEPAD_DPAD_LEFT: u16 = 0x0004;
pub const XENORA_XINPUT_GAMEPAD_DPAD_RIGHT: u16 = 0x0008;
pub const XENORA_XINPUT_GAMEPAD_START: u16 = 0x0010;
pub const XENORA_XINPUT_GAMEPAD_BACK: u16 = 0x0020;
pub const XENORA_XINPUT_GAMEPAD_LEFT_THUMB: u16 = 0x0040;
pub const XENORA_XINPUT_GAMEPAD_RIGHT_THUMB: u16 = 0x0080;
pub const XENORA_XINPUT_GAMEPAD_LEFT_SHOULDER: u16 = 0x0100;
pub const XENORA_XINPUT_GAMEPAD_RIGHT_SHOULDER: u16 = 0x0200;
pub const XENORA_XINPUT_GAMEPAD_A: u16 = 0x1000;
pub const XENORA_XINPUT_GAMEPAD_B: u16 = 0x2000;
pub const XENORA_XINPUT_GAMEPAD_X: u16 = 0x4000;
pub const XENORA_XINPUT_GAMEPAD_Y: u16 = 0x8000;

pub use frame_loop::ControllerInput as xenora_controller_input;
pub use frame_loop::InputState as xenora_input_state;
pub use frame_loop::XINPUT_BATTERY_LEVEL_UNKNOWN as XENORA_XINPUT_BATTERY_LEVEL_UNKNOWN;
pub use frame_loop::XINPUT_BATTERY_STATE_CHARGING as XENORA_XINPUT_BATTERY_STATE_CHARGING;
pub use frame_loop::XINPUT_BATTERY_STATE_DISCHARGING as XENORA_XINPUT_BATTERY_STATE_DISCHARGING;
pub use frame_loop::XINPUT_BATTERY_STATE_FULL as XENORA_XINPUT_BATTERY_STATE_FULL;
pub use frame_loop::XINPUT_BATTERY_STATE_UNKNOWN as XENORA_XINPUT_BATTERY_STATE_UNKNOWN;
pub use frame_loop::XINPUT_FEATURE_VIBRATION as XENORA_XINPUT_FEATURE_VIBRATION;
pub use frame_loop::XInputBattery as xenora_xinput_battery;
pub use frame_loop::XInputSnapshot as xenora_xinput_state;
pub use frame_loop::XInputVibration as xenora_xinput_vibration;

pub const XENORA_FIXTURE_FRAME_WIDTH: u32 = 4;
pub const XENORA_FIXTURE_FRAME_HEIGHT: u32 = 3;
const FRAME_BYTES: usize = (XENORA_FIXTURE_FRAME_WIDTH * XENORA_FIXTURE_FRAME_HEIGHT * 4) as usize;

/// Upper bound on simultaneously live sessions.
const MAX_SESSIONS: usize = 64;

const FIXTURE: &[u8] = include_bytes!("../../../fixtures/pe/trusted-runner.exe");

pub const XENORA_NO_RESOURCE: u64 = 0;
pub const XENORA_RESOURCE_TYPE_TEXTURE_2D: u32 = 1;
pub const XENORA_RESOURCE_TYPE_BUFFER: u32 = 2;
const MAX_RESOURCES_PER_SESSION: usize = 256;

pub const XENORA_AUDIO_FORMAT_PCM_S16LE: u32 = 1;
pub const XENORA_AUDIO_FORMAT_PCM_F32LE: u32 = 2;

pub struct AudioSubsystem {
    pub sample_rate: u32,
    pub channels: u32,
    pub format: u32,
    pub queue: std::collections::VecDeque<u8>,
    pub max_capacity_bytes: usize,
    pub volume: f32,
    pub muted: bool,
    pub underruns: u32,
    pub total_submitted_bytes: u64,
}

impl Default for AudioSubsystem {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            format: XENORA_AUDIO_FORMAT_PCM_S16LE,
            queue: std::collections::VecDeque::new(),
            max_capacity_bytes: 65536 * 4,
            volume: 1.0,
            muted: false,
            underruns: 0,
            total_submitted_bytes: 0,
        }
    }
}

impl AudioSubsystem {
    fn frame_bytes(&self) -> usize {
        let sample_bytes = if self.format == XENORA_AUDIO_FORMAT_PCM_F32LE {
            4
        } else {
            2
        };
        sample_bytes * self.channels as usize
    }

    pub fn submit(&mut self, bytes: &[u8]) {
        self.total_submitted_bytes = self
            .total_submitted_bytes
            .saturating_add(bytes.len() as u64);
        // Only the newest bytes can survive in the bounded queue. Skip an
        // oversized submission's discarded prefix before copying any samples.
        let retained = &bytes[bytes.len().saturating_sub(self.max_capacity_bytes)..];
        let available_for_old = self.max_capacity_bytes - retained.len();
        let discard = self.queue.len().saturating_sub(available_for_old);
        self.queue.drain(..discard);
        self.queue.extend(retained.iter().copied());
    }

    pub fn read(&mut self, out: &mut [u8]) -> usize {
        let usable = out.len().min(self.queue.len());
        let count = usable - usable % self.frame_bytes();
        let drained = &mut out[..count];
        let (first, second) = self.queue.as_slices();
        let first_count = first.len().min(count);
        drained[..first_count].copy_from_slice(&first[..first_count]);
        drained[first_count..].copy_from_slice(&second[..count - first_count]);
        self.queue.drain(..count);
        self.apply_playback_controls(drained);
        count
    }

    fn apply_playback_controls(&self, pcm: &mut [u8]) {
        if self.muted {
            pcm.fill(0);
            return;
        }
        if self.format == XENORA_AUDIO_FORMAT_PCM_S16LE {
            if self.volume == 1.0 {
                return;
            }
            for chunk in pcm.as_chunks_mut::<2>().0 {
                let sample = i16::from_le_bytes(*chunk);
                let scaled = f32::from(sample) * self.volume;
                *chunk = (scaled.clamp(-32768.0, 32767.0) as i16).to_le_bytes();
            }
        } else {
            for chunk in pcm.as_chunks_mut::<4>().0 {
                let sample = f32::from_le_bytes(*chunk);
                let scaled = if sample.is_finite() {
                    (sample * self.volume).clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                *chunk = scaled.to_le_bytes();
            }
        }
    }

    fn render(&mut self, out: &mut [u8]) -> usize {
        let supplied = self.read(out);
        if supplied < out.len() {
            out[supplied..].fill(0);
            self.underruns = self.underruns.saturating_add(1);
        }
        supplied
    }
}

pub const XENORA_NO_FILE: u64 = 0;
pub const XENORA_SEEK_SET: u32 = 0;
pub const XENORA_SEEK_CUR: u32 = 1;
pub const XENORA_SEEK_END: u32 = 2;
const MAX_VFS_FILES_PER_SESSION: usize = 64;

pub struct VfsFileSlot {
    pub generation: u32,
    pub path: String,
    pub data: Vec<u8>,
    pub position: usize,
    pub flags: u32,
}

pub struct VfsSubsystem {
    pub root: std::path::PathBuf,
    pub files: Vec<VfsFileSlot>,
}

impl Default for VfsSubsystem {
    fn default() -> Self {
        let mut root = std::path::PathBuf::from("fixtures/assets");
        if !root.exists() {
            let candidate = std::env::var("CARGO_MANIFEST_DIR")
                .map(|dir| std::path::PathBuf::from(dir).join("../../fixtures/assets"))
                .unwrap_or_default();
            if candidate.exists() {
                root = candidate;
            }
        }
        if !root.exists() {
            let candidate = std::path::PathBuf::from("../../fixtures/assets");
            if candidate.exists() {
                root = candidate;
            }
        }
        Self {
            root,
            files: Vec::new(),
        }
    }
}

impl VfsSubsystem {
    pub fn sanitize_path(&self, rel_path: &str) -> Result<std::path::PathBuf, i32> {
        if rel_path.is_empty() || rel_path.contains('\0') {
            return Err(XENORA_INVALID_ARGUMENT);
        }
        let p = std::path::Path::new(rel_path);
        if p.is_absolute() {
            return Err(XENORA_INVALID_ARGUMENT);
        }
        for component in p.components() {
            match component {
                std::path::Component::ParentDir => return Err(XENORA_INVALID_ARGUMENT),
                std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                    return Err(XENORA_INVALID_ARGUMENT);
                }
                _ => {}
            }
        }
        let full_path = self.root.join(p);
        Ok(full_path)
    }
}

#[derive(Clone)]
struct SendSyncTexture(MetalRgba8Texture);
// SAFETY: Apple Metal textures are thread-safe native GPU resources.
unsafe impl Send for SendSyncTexture {}
unsafe impl Sync for SendSyncTexture {}

pub struct ResourceSlot {
    pub generation: u32,
    pub resource_type: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    metal_texture: Option<SendSyncTexture>,
    pub in_flight: u32,
    pub retired: bool,
}

struct Slot {
    /// Odd while live, even while free; starts at 0 so the first handle is never 0.
    generation: u32,
    /// The session's persistent frame loop, if one was started.
    frame_loop: Option<Arc<FrameLoop>>,
    /// Persistent GPU resources owned by this session (Gate 5, #279).
    resources: Vec<ResourceSlot>,
    /// Bounded audio subsystem (Gate 6, #284).
    audio: AudioSubsystem,
    /// Virtual filesystem sandboxed subsystem (Gate 6, #284).
    vfs: VfsSubsystem,
    /// Loaded asset bytes staged in the session.
    staged_asset: Vec<u8>,
}

static SESSIONS: Mutex<Vec<Slot>> = Mutex::new(Vec::new());

fn guarded(f: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(XENORA_INTERNAL_ERROR)
}

fn split(handle: u64) -> (usize, u32) {
    ((handle & 0xFFFF_FFFF) as usize, (handle >> 32) as u32)
}

fn is_live(slots: &[Slot], handle: u64) -> bool {
    let (index, generation) = split(handle);
    slots
        .get(index)
        .is_some_and(|slot| slot.generation == generation && generation % 2 == 1)
}

#[unsafe(no_mangle)]
pub extern "C" fn xenora_abi_version() -> u32 {
    XENORA_ABI_VERSION
}

/// # Safety
/// `out_session` must be null or valid for one aligned `u64` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_create(abi_version: u32, out_session: *mut u64) -> i32 {
    guarded(|| {
        if out_session.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        if abi_version != XENORA_ABI_VERSION {
            return XENORA_VERSION_MISMATCH;
        }
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        let index = match slots.iter().position(|slot| slot.generation % 2 == 0) {
            Some(index) => index,
            None if slots.len() < MAX_SESSIONS => {
                slots.push(Slot {
                    generation: 0,
                    frame_loop: None,
                    resources: Vec::new(),
                    audio: AudioSubsystem::default(),
                    vfs: VfsSubsystem::default(),
                    staged_asset: Vec::new(),
                });
                slots.len() - 1
            }
            None => return XENORA_LIMIT_EXCEEDED,
        };
        let slot = &mut slots[index];
        slot.resources.clear();
        slot.audio = AudioSubsystem::default();
        slot.vfs = VfsSubsystem::default();
        slot.staged_asset.clear();
        // ponytail: a slot retires after 2^31 reuses instead of wrapping its generation.
        let Some(generation) = slot.generation.checked_add(1) else {
            return XENORA_LIMIT_EXCEEDED;
        };
        slot.generation = generation;
        // SAFETY: checked non-null above; the caller guarantees validity.
        unsafe { out_session.write((u64::from(generation) << 32) | index as u64) };
        XENORA_OK
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn xenora_session_destroy(session: u64) -> i32 {
    guarded(|| {
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];
        slot.generation += 1;
        let running = slot.frame_loop.take();
        slot.resources.clear();
        slot.audio = AudioSubsystem::default();
        slot.vfs = VfsSubsystem::default();
        slot.staged_asset.clear();
        drop(slots);
        // Stop and join the guest thread without holding the session table.
        if let Some(running) = running {
            running.stop();
        }
        XENORA_OK
    })
}

/// The live session's frame loop, or the status to return.
fn session_loop(session: u64) -> Result<Option<Arc<FrameLoop>>, i32> {
    let slots = SESSIONS.lock().map_err(|_| XENORA_INTERNAL_ERROR)?;
    if !is_live(&slots, session) {
        return Err(XENORA_INVALID_HANDLE);
    }
    Ok(slots[split(session).0].frame_loop.clone())
}

#[unsafe(no_mangle)]
pub extern "C" fn xenora_session_start_frame_loop(session: u64, frame_count: u32) -> i32 {
    guarded(|| {
        if !(1..=XENORA_FRAME_LOOP_MAX_FRAMES).contains(&frame_count) {
            return XENORA_INVALID_ARGUMENT;
        }
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];
        if slot.frame_loop.is_some() {
            return XENORA_INVALID_STATE;
        }
        slot.frame_loop = Some(FrameLoop::start(frame_count, false));
        XENORA_OK
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn xenora_session_start_interactive_loop(session: u64, frame_count: u32) -> i32 {
    guarded(|| {
        if !(1..=XENORA_FRAME_LOOP_MAX_FRAMES).contains(&frame_count) {
            return XENORA_INVALID_ARGUMENT;
        }
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];
        if slot.frame_loop.is_some() {
            return XENORA_INVALID_STATE;
        }
        slot.frame_loop = Some(FrameLoop::start(frame_count, true));
        XENORA_OK
    })
}

/// # Safety
/// `input` must be a valid pointer to an initialized `xenora_input_state`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_send_input(
    session: u64,
    input: *const frame_loop::InputState,
) -> i32 {
    guarded(|| {
        if input.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        let input_val = unsafe { *input };
        match session_loop(session) {
            Ok(Some(running)) => {
                running.send_input(input_val);
                XENORA_OK
            }
            Ok(None) => XENORA_INVALID_STATE,
            Err(status) => status,
        }
    })
}

/// # Safety
/// `input` must be a valid pointer to an initialized `xenora_controller_input`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_send_controller_input(
    session: u64,
    input: *const frame_loop::ControllerInput,
) -> i32 {
    guarded(|| {
        if input.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        let input = unsafe { *input };
        match session_loop(session) {
            Ok(Some(running)) => {
                running.send_controller_input(input);
                XENORA_OK
            }
            Ok(None) => XENORA_INVALID_STATE,
            Err(status) => status,
        }
    })
}

/// # Safety
/// `input` must be a valid pointer to an initialized `xenora_xinput_state`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_send_xinput_state(
    session: u64,
    user_index: u32,
    input: *const frame_loop::XInputSnapshot,
) -> i32 {
    guarded(|| {
        if input.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        let input = unsafe { *input };
        if user_index >= 4 || !input.valid_wire_shape() {
            return XENORA_INVALID_ARGUMENT;
        }
        match session_loop(session) {
            Ok(Some(running)) if running.send_xinput(user_index, input) => XENORA_OK,
            Ok(_) => XENORA_INVALID_STATE,
            Err(status) => status,
        }
    })
}

/// Atomically replace one XInput snapshot and its host battery evidence.
///
/// # Safety
/// `input` and `battery` must be valid pointers to initialized structures.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_send_xinput_state_with_battery(
    session: u64,
    user_index: u32,
    input: *const frame_loop::XInputSnapshot,
    battery: *const frame_loop::XInputBattery,
) -> i32 {
    guarded(|| {
        if input.is_null() || battery.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        let input = unsafe { *input };
        let battery = unsafe { *battery };
        if user_index >= 4 || !input.valid_wire_shape() || !battery.valid_wire_shape() {
            return XENORA_INVALID_ARGUMENT;
        }
        match session_loop(session) {
            Ok(Some(running)) if running.send_xinput_with_battery(user_index, input, battery) => {
                XENORA_OK
            }
            Ok(_) => XENORA_INVALID_STATE,
            Err(status) => status,
        }
    })
}

/// # Safety
/// `output` must be a valid pointer to one writable `xenora_xinput_vibration`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_poll_xinput_vibration(
    session: u64,
    user_index: u32,
    output: *mut frame_loop::XInputVibration,
) -> i32 {
    guarded(|| {
        if output.is_null() || user_index >= 4 {
            return XENORA_INVALID_ARGUMENT;
        }
        match session_loop(session) {
            Ok(Some(running)) => {
                let Some(command) = running.xinput_vibration(user_index) else {
                    return XENORA_INVALID_ARGUMENT;
                };
                unsafe { output.write(command) };
                XENORA_OK
            }
            Ok(None) => XENORA_INVALID_STATE,
            Err(status) => status,
        }
    })
}

/// # Safety
/// `input` must be a valid pointer to an initialized `xenora_input_state`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_queue_input(
    session: u64,
    input: *const frame_loop::InputState,
) -> i32 {
    guarded(|| {
        if input.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        let input_val = unsafe { *input };
        match session_loop(session) {
            Ok(Some(running)) => {
                running.queue_input(input_val);
                XENORA_OK
            }
            Ok(None) => XENORA_INVALID_STATE,
            Err(status) => status,
        }
    })
}

fn set_paused(session: u64, paused: bool) -> i32 {
    guarded(|| match session_loop(session) {
        Ok(Some(running)) if running.set_paused(paused) => XENORA_OK,
        Ok(_) => XENORA_INVALID_STATE,
        Err(status) => status,
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn xenora_session_pause_frame_loop(session: u64) -> i32 {
    set_paused(session, true)
}

#[unsafe(no_mangle)]
pub extern "C" fn xenora_session_resume_frame_loop(session: u64) -> i32 {
    set_paused(session, false)
}

#[unsafe(no_mangle)]
pub extern "C" fn xenora_session_stop_frame_loop(session: u64) -> i32 {
    guarded(|| match session_loop(session) {
        Ok(Some(running)) => {
            running.stop();
            XENORA_OK
        }
        Ok(None) => XENORA_INVALID_STATE,
        Err(status) => status,
    })
}

/// # Safety
/// Each `u32` output must be null or valid for one aligned write; `out_rgba` must be
/// null or valid for `capacity` byte writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_frame_loop_status(
    session: u64,
    out_state: *mut u32,
    out_completed: *mut u32,
    out_frame_id: *mut u32,
    out_rgba: *mut u8,
    capacity: u64,
) -> i32 {
    guarded(|| {
        if out_state.is_null()
            || out_completed.is_null()
            || out_frame_id.is_null()
            || out_rgba.is_null()
        {
            return XENORA_INVALID_ARGUMENT;
        }
        if capacity < frame_loop::FRAME_BYTES as u64 {
            return XENORA_BUFFER_TOO_SMALL;
        }
        let running = match session_loop(session) {
            Ok(running) => running,
            Err(status) => return status,
        };
        let (state, completed, latest) = match running {
            Some(running) => {
                let snapshot = running.snapshot();
                (snapshot.status, snapshot.completed, snapshot.latest)
            }
            None => (XENORA_LOOP_NONE, 0, None),
        };
        // SAFETY: all outputs checked non-null and `out_rgba` at least FRAME_BYTES long.
        unsafe {
            out_state.write(state);
            out_completed.write(completed);
            match latest {
                Some((id, pixels)) => {
                    out_frame_id.write(id);
                    std::ptr::copy_nonoverlapping(
                        pixels.as_ptr(),
                        out_rgba,
                        frame_loop::FRAME_BYTES,
                    );
                }
                None => out_frame_id.write(XENORA_NO_FRAME),
            }
        }
        XENORA_OK
    })
}

/// # Safety
/// `out_frame_id` must be valid for one aligned write; `out_rgba` must be valid
/// for `capacity` byte writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_get_recorded_frame(
    session: u64,
    frame_index: u32,
    out_frame_id: *mut u32,
    out_rgba: *mut u8,
    capacity: u64,
) -> i32 {
    guarded(|| {
        if out_frame_id.is_null() || out_rgba.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        if capacity < frame_loop::FRAME_BYTES as u64 {
            return XENORA_BUFFER_TOO_SMALL;
        }
        let running = match session_loop(session) {
            Ok(Some(running)) => running,
            Ok(None) => return XENORA_INVALID_STATE,
            Err(status) => return status,
        };
        let Some((id, pixels)) = running.get_recorded_frame(frame_index as usize) else {
            return XENORA_INVALID_ARGUMENT;
        };
        // SAFETY: checked non-null and capacity >= FRAME_BYTES
        unsafe {
            out_frame_id.write(id);
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), out_rgba, frame_loop::FRAME_BYTES);
        }
        XENORA_OK
    })
}

/// # Safety
/// `out_required` must be valid for an aligned write. `out_records` may be null
/// only for a zero-capacity query. Otherwise it must be valid for `capacity`
/// byte writes and not overlap `out_required`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_frame_loop_journal(
    session: u64,
    out_records: *mut u8,
    capacity: u64,
    out_required: *mut u64,
) -> i32 {
    guarded(|| {
        if out_required.is_null() || (capacity != 0 && out_records.is_null()) {
            return XENORA_INVALID_ARGUMENT;
        }
        // SAFETY: out_required is non-null and the caller guarantees validity.
        unsafe { out_required.write(0) };
        let running = match session_loop(session) {
            Ok(Some(running)) => running,
            Ok(None) => return XENORA_INVALID_STATE,
            Err(status) => return status,
        };
        let Some(records) = running.verified_journal() else {
            return XENORA_INVALID_STATE;
        };
        let length = records.len();
        // SAFETY: the same output pointer was checked above.
        unsafe { out_required.write(length as u64) };
        if capacity < length as u64 {
            return XENORA_BUFFER_TOO_SMALL;
        }
        if length != 0 {
            // SAFETY: for a nonempty copy capacity is sufficient and out_records is non-null.
            unsafe { std::ptr::copy_nonoverlapping(records.as_ptr(), out_records, length) };
        }
        XENORA_OK
    })
}

/// # Safety
/// Each output pointer must be null or valid for one aligned `u32` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_inspect_trusted_fixture(
    session: u64,
    out_section_count: *mut u32,
    out_entry_rva: *mut u32,
) -> i32 {
    guarded(|| {
        if out_section_count.is_null() || out_entry_rva.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        match SESSIONS.lock() {
            Ok(slots) if is_live(&slots, session) => {}
            Ok(_) => return XENORA_INVALID_HANDLE,
            Err(_) => return XENORA_INTERNAL_ERROR,
        }
        let Ok(pe) = ParsedPe::parse(FIXTURE) else {
            return XENORA_PE_ERROR;
        };
        // SAFETY: both checked non-null above; the caller guarantees validity.
        unsafe {
            out_section_count.write(u32::from(pe.number_of_sections));
            out_entry_rva.write(pe.entry_point_rva);
        }
        XENORA_OK
    })
}

// The trusted fixture's contract, as checked by the CLI runner (crates/cli/src/runner.rs).
// ponytail: duplicated from the runner so the CLI stays untouched; extract a shared
// fixture-run function into a library when a third caller appears.
const IMAGE_BASE: u64 = 0x1_5000_0000;
const TICKET_ADDRESS: u64 = IMAGE_BASE + 0x3000;
const TICKET: u64 = 17;
const DRAW_RESULT: u64 = 0x42;
const ENTRY_STEPS: usize = 100;
/// `x9-constant-ps.s` exports (1.0, 0, 0, 1.0) to mrt0; in an RGBA8 target that is opaque red.
const EXPECTED_PIXEL: [u8; 4] = [255, 0, 0, 255];

fn draw() -> Result<Vec<u8>, String> {
    let shader = compile_graphics(
        Architecture::Gfx1030,
        include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin"),
        include_bytes!("../../../fixtures/rdna2/x9-constant-ps.bin"),
    )
    .map_err(|e| format!("shader translation: {e}"))?;
    let gpu = NativeCompute::new().map_err(|e| format!("Metal device: {e}"))?;
    let pixels = gpu
        .render_rgba8(
            &shader.msl,
            [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
            &[
                [-1.0, -1.0, 0.5, 1.0],
                [3.0, -1.0, 0.5, 1.0],
                [-1.0, 3.0, 0.5, 1.0],
            ],
            &[[0.0; 4]; 3],
            [
                XENORA_FIXTURE_FRAME_WIDTH as usize,
                XENORA_FIXTURE_FRAME_HEIGHT as usize,
            ],
            [19, 43, 93, 251],
        )
        .map_err(|e| format!("Metal render: {e}"))?;
    if pixels != EXPECTED_PIXEL.repeat(FRAME_BYTES / 4) {
        return Err("read-back frame differs from the fixture shader's output".into());
    }
    Ok(pixels)
}

/// Run the embedded fixture and return its verified frame.
fn run_fixture() -> Result<Vec<u8>, String> {
    let frame = Arc::new(Mutex::new(None));
    let draw_frame = Arc::clone(&frame);
    let mut imports = ImportRegistry::new();
    imports.register("xenora.dll", "fixture_draw", move |args, _, memory| {
        let fault = ExitReason::IllegalInstruction(0xff);
        let ticket = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        let mut slot = draw_frame.lock().map_err(|_| fault)?;
        if slot.is_some()
            || ticket != TICKET_ADDRESS
            || memory.read_u64(ticket).map_err(ExitReason::MemoryFault)? != TICKET
        {
            return Err(fault);
        }
        let result = draw();
        let ok = result.is_ok();
        *slot = Some(result);
        if ok { Ok(DRAW_RESULT) } else { Err(fault) }
    });
    let startup = run_synthetic_startup(
        FIXTURE,
        StartupLayout {
            image_base: IMAGE_BASE,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        },
        imports,
        ENTRY_STEPS,
    )
    .map_err(|e| format!("startup: {e}"))?;
    let drawn = frame.lock().map_err(|_| "frame lock poisoned")?.take();
    if let Some(Err(e)) = drawn {
        return Err(e);
    }
    let execution = &startup.execution;
    if execution.exit_reason != ExitReason::Halt
        || startup.relocations.applied_dir64 != 4
        || startup.bound_imports != 1
        || startup.tls_callbacks != 1
        || execution.final_state.get(Register::Rax) != DRAW_RESULT
    {
        return Err(format!(
            "unexpected guest result: {:?}",
            execution.exit_reason
        ));
    }
    drawn.ok_or_else(|| String::from("guest halted without a draw"))?
}

/// # Safety
/// `out_rgba` must be null or valid for `capacity` byte writes; each `u32` output
/// must be null or valid for one aligned write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_run_trusted_fixture(
    session: u64,
    out_rgba: *mut u8,
    capacity: u64,
    out_width: *mut u32,
    out_height: *mut u32,
) -> i32 {
    guarded(|| {
        if out_rgba.is_null() || out_width.is_null() || out_height.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        match SESSIONS.lock() {
            Ok(slots) if is_live(&slots, session) => {}
            Ok(_) => return XENORA_INVALID_HANDLE,
            Err(_) => return XENORA_INTERNAL_ERROR,
        }
        if capacity < FRAME_BYTES as u64 {
            return XENORA_BUFFER_TOO_SMALL;
        }
        let Ok(pixels) = run_fixture() else {
            return XENORA_EXECUTION_FAILED;
        };
        // SAFETY: non-null and at least FRAME_BYTES long, both checked above.
        unsafe {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), out_rgba, FRAME_BYTES);
            out_width.write(XENORA_FIXTURE_FRAME_WIDTH);
            out_height.write(XENORA_FIXTURE_FRAME_HEIGHT);
        }
        XENORA_OK
    })
}

fn get_resource_mut(slot: &mut Slot, resource: u64) -> Result<&mut ResourceSlot, i32> {
    let (index, generation) = split(resource);
    let res = slot.resources.get_mut(index).ok_or(XENORA_INVALID_HANDLE)?;
    if res.generation != generation || generation % 2 == 0 || res.retired {
        return Err(XENORA_INVALID_HANDLE);
    }
    Ok(res)
}

/// # Safety
/// `out_resource` must be non-null and valid for one aligned `u64` write.
/// If `initial_rgba` is non-null, it must point to at least `width * height * 4` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_create_texture(
    session: u64,
    width: u32,
    height: u32,
    initial_rgba: *const u8,
    out_resource: *mut u64,
) -> i32 {
    guarded(|| {
        if out_resource.is_null() || width == 0 || height == 0 || width > 4096 || height > 4096 {
            return XENORA_INVALID_ARGUMENT;
        }
        let total_pixels = match (width as usize).checked_mul(height as usize) {
            Some(pixels) if pixels <= 16384 * 16384 => pixels,
            _ => return XENORA_INVALID_ARGUMENT,
        };
        let total_bytes = total_pixels * 4;

        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];

        let res_idx = match slot
            .resources
            .iter()
            .position(|r| r.generation % 2 == 0 && !r.retired && r.in_flight == 0)
        {
            Some(idx) => idx,
            None if slot.resources.len() < MAX_RESOURCES_PER_SESSION => {
                slot.resources.push(ResourceSlot {
                    generation: 0,
                    resource_type: XENORA_RESOURCE_TYPE_TEXTURE_2D,
                    width: 0,
                    height: 0,
                    pixels: Vec::new(),
                    metal_texture: None,
                    in_flight: 0,
                    retired: false,
                });
                slot.resources.len() - 1
            }
            None => return XENORA_LIMIT_EXCEEDED,
        };

        let res = &mut slot.resources[res_idx];
        let Some(generation) = res.generation.checked_add(1) else {
            return XENORA_LIMIT_EXCEEDED;
        };
        res.generation = generation;
        res.resource_type = XENORA_RESOURCE_TYPE_TEXTURE_2D;
        res.width = width;
        res.height = height;
        res.in_flight = 0;
        res.retired = false;

        let mut pixels = vec![0u8; total_bytes];
        if !initial_rgba.is_null() {
            // SAFETY: Caller guarantees at least width * height * 4 readable bytes when initial_rgba is non-null.
            unsafe {
                std::ptr::copy_nonoverlapping(initial_rgba, pixels.as_mut_ptr(), total_bytes);
            }
        }

        res.metal_texture = if let Ok(gpu) = NativeCompute::new() {
            if let Ok(desc) = MetalRgba8TextureDesc::new(width as usize, height as usize, 1) {
                let mip = Rgba8MipLevel::new_packed(0, width as usize, height as usize, &pixels);
                MetalRgba8Texture::upload(gpu.device(), desc, &[mip])
                    .ok()
                    .map(SendSyncTexture)
            } else {
                None
            }
        } else {
            None
        };
        res.pixels = pixels;

        let handle = (u64::from(generation) << 32) | (res_idx as u64);
        // SAFETY: checked non-null above.
        unsafe { out_resource.write(handle) };
        XENORA_OK
    })
}

/// # Safety
/// `rgba` must be non-null and valid for `byte_count` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_update_texture(
    session: u64,
    resource: u64,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    rgba: *const u8,
    byte_count: u64,
) -> i32 {
    guarded(|| {
        if rgba.is_null() || width == 0 || height == 0 {
            return XENORA_INVALID_ARGUMENT;
        }
        let update_pixels = match (width as usize).checked_mul(height as usize) {
            Some(pixels) => pixels,
            None => return XENORA_INVALID_ARGUMENT,
        };
        let required_bytes = (update_pixels * 4) as u64;
        if byte_count < required_bytes {
            return XENORA_INVALID_ARGUMENT;
        }

        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];
        let res = match get_resource_mut(slot, resource) {
            Ok(r) => r,
            Err(e) => return e,
        };

        if res.resource_type != XENORA_RESOURCE_TYPE_TEXTURE_2D {
            return XENORA_INVALID_ARGUMENT;
        }
        if x.checked_add(width).is_none_or(|end_x| end_x > res.width)
            || y.checked_add(height).is_none_or(|end_y| end_y > res.height)
        {
            return XENORA_INVALID_ARGUMENT;
        }

        // SAFETY: caller guarantees byte_count readable bytes, and required_bytes <= byte_count.
        let src_slice = unsafe { std::slice::from_raw_parts(rgba, required_bytes as usize) };
        for dy in 0..height {
            let src_start = (dy as usize * width as usize) * 4;
            let dst_start = (((y + dy) as usize * res.width as usize) + x as usize) * 4;
            let row_bytes = width as usize * 4;
            res.pixels[dst_start..dst_start + row_bytes]
                .copy_from_slice(&src_slice[src_start..src_start + row_bytes]);
        }

        if let Some(ref tex) = res.metal_texture {
            let mip =
                Rgba8MipLevel::new_packed(0, res.width as usize, res.height as usize, &res.pixels);
            if tex.0.upload_mip(&mip).is_err() {
                return XENORA_INTERNAL_ERROR;
            }
        }

        XENORA_OK
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn xenora_session_destroy_resource(session: u64, resource: u64) -> i32 {
    guarded(|| {
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];
        let res = match get_resource_mut(slot, resource) {
            Ok(r) => r,
            Err(e) => return e,
        };

        if res.in_flight > 0 {
            res.retired = true;
        } else {
            res.generation += 1;
            res.metal_texture = None;
            res.pixels.clear();
            res.retired = false;
        }

        XENORA_OK
    })
}

/// # Safety
/// Each output pointer must be non-null and valid for one aligned `u32` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_resource_info(
    session: u64,
    resource: u64,
    out_type: *mut u32,
    out_width: *mut u32,
    out_height: *mut u32,
    out_in_flight: *mut u32,
) -> i32 {
    guarded(|| {
        if out_type.is_null()
            || out_width.is_null()
            || out_height.is_null()
            || out_in_flight.is_null()
        {
            return XENORA_INVALID_ARGUMENT;
        }
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];
        let res = match get_resource_mut(slot, resource) {
            Ok(r) => r,
            Err(e) => return e,
        };

        // SAFETY: checked non-null above.
        unsafe {
            out_type.write(res.resource_type);
            out_width.write(res.width);
            out_height.write(res.height);
            out_in_flight.write(res.in_flight);
        }
        XENORA_OK
    })
}

const TEXTURE_RENDER_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct VertexOut {
    float4 position [[position]];
    float4 uv [[user(param0)]];
};

vertex VertexOut textured_vs(device const float4 *positions [[buffer(0)]],
                             device const float4 *uvs [[buffer(1)]],
                             uint vid [[vertex_id]]) {
    VertexOut out;
    out.position = positions[vid];
    out.uv = uvs[vid];
    return out;
}

fragment float4 textured_ps(VertexOut in [[stage_in]],
                            texture2d<float, access::sample> tex [[texture(0)]],
                            sampler samp [[sampler(0)]]) {
    return tex.sample(samp, in.uv.xy);
}
"#;

const FULLSCREEN_VERTICES: [[f32; 4]; 3] = [
    [-1.0, -1.0, 0.5, 1.0],
    [3.0, -1.0, 0.5, 1.0],
    [-1.0, 3.0, 0.5, 1.0],
];

const FULLSCREEN_UVS: [[f32; 4]; 3] = [
    [0.0, 1.0, 0.0, 1.0],
    [2.0, 1.0, 0.0, 1.0],
    [0.0, -1.0, 0.0, 1.0],
];

/// # Safety
/// `out_rgba` must be non-null and valid for `capacity` byte writes;
/// `out_width` and `out_height` must be non-null and valid for one aligned `u32` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_render_textured(
    session: u64,
    resource: u64,
    out_rgba: *mut u8,
    capacity: u64,
    out_width: *mut u32,
    out_height: *mut u32,
) -> i32 {
    guarded(|| {
        if out_rgba.is_null() || out_width.is_null() || out_height.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }

        let (width, height, metal_tex) = {
            let Ok(mut slots) = SESSIONS.lock() else {
                return XENORA_INTERNAL_ERROR;
            };
            if !is_live(&slots, session) {
                return XENORA_INVALID_HANDLE;
            }
            let slot = &mut slots[split(session).0];
            let res = match get_resource_mut(slot, resource) {
                Ok(r) => r,
                Err(e) => return e,
            };
            if res.resource_type != XENORA_RESOURCE_TYPE_TEXTURE_2D {
                return XENORA_INVALID_ARGUMENT;
            }
            let required_bytes = (res.width as u64) * (res.height as u64) * 4;
            if capacity < required_bytes {
                return XENORA_BUFFER_TOO_SMALL;
            }
            res.in_flight += 1;
            (res.width, res.height, res.metal_texture.clone())
        };

        // Render on Metal outside the session lock
        let render_result: Result<Vec<u8>, ()> = if let Some(tex) = metal_tex {
            match NativeCompute::new() {
                Ok(gpu) => {
                    match xenora_metal::MetalSampler::new(gpu.device(), Default::default()) {
                        Ok(sampler) => gpu
                            .render_rgba8_sampled(
                                TEXTURE_RENDER_MSL,
                                ["textured_vs", "textured_ps"],
                                &FULLSCREEN_VERTICES,
                                &FULLSCREEN_UVS,
                                [width as usize, height as usize],
                                [0, 0, 0, 0],
                                Some((&tex.0, &sampler)),
                            )
                            .map_err(|_| ()),
                        Err(_) => Err(()),
                    }
                }
                Err(_) => Err(()),
            }
        } else {
            Err(())
        };

        // Re-lock to decrement in_flight and finalize retirement if marked retired
        {
            let Ok(mut slots) = SESSIONS.lock() else {
                return XENORA_INTERNAL_ERROR;
            };
            let (sess_idx, _) = split(session);
            if let Some(slot) = slots.get_mut(sess_idx) {
                let (res_idx, _) = split(resource);
                if let Some(res) = slot.resources.get_mut(res_idx) {
                    res.in_flight = res.in_flight.saturating_sub(1);
                    if res.retired && res.in_flight == 0 {
                        res.generation += 1;
                        res.retired = false;
                        res.metal_texture = None;
                        res.pixels.clear();
                    }
                }
            }
        }

        let pixels = match render_result {
            Ok(p) => p,
            Err(_) => return XENORA_EXECUTION_FAILED,
        };

        // SAFETY: non-null and capacity checked above.
        unsafe {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), out_rgba, pixels.len());
            out_width.write(width);
            out_height.write(height);
        }
        XENORA_OK
    })
}

const TEXTURED_FIXTURE: &[u8] = include_bytes!("../../../fixtures/pe/textured.exe");

const INITIAL_TEXTURE_2X2: [u8; 16] = [
    255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
];

const UPDATED_TEXTURE_2X2: [u8; 16] = [
    255, 255, 0, 255, 0, 255, 255, 255, 255, 0, 255, 255, 64, 64, 64, 255,
];

fn run_textured_fixture(session: u64) -> Result<Vec<u8>, String> {
    let last_frame = Arc::new(Mutex::new(None));
    let frame_sink = Arc::clone(&last_frame);
    let mut imports = ImportRegistry::new();

    imports.register("xenora.dll", "texture_create", move |args, _, memory| {
        let w = args.get(0, memory).map_err(ExitReason::MemoryFault)? as u32;
        let h = args.get(1, memory).map_err(ExitReason::MemoryFault)? as u32;
        if w != 2 || h != 2 {
            return Err(ExitReason::IllegalInstruction(0xfe));
        }
        let mut res_handle: u64 = 0;
        let status = unsafe {
            xenora_session_create_texture(
                session,
                w,
                h,
                INITIAL_TEXTURE_2X2.as_ptr(),
                &mut res_handle,
            )
        };
        if status != XENORA_OK {
            return Err(ExitReason::IllegalInstruction(0xfd));
        }
        Ok(res_handle)
    });

    let sink = Arc::clone(&frame_sink);
    imports.register(
        "xenora.dll",
        "frame_submit_textured",
        move |args, _, memory| {
            let handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
            let mut rendered = vec![0u8; 16];
            let mut rw = 0u32;
            let mut rh = 0u32;
            let status = unsafe {
                xenora_session_render_textured(
                    session,
                    handle,
                    rendered.as_mut_ptr(),
                    16,
                    &mut rw,
                    &mut rh,
                )
            };
            if status != XENORA_OK || rw != 2 || rh != 2 {
                return Err(ExitReason::IllegalInstruction(0xfc));
            }
            if let Ok(mut slot) = sink.lock() {
                *slot = Some(rendered);
            }
            Ok(1)
        },
    );

    imports.register("xenora.dll", "texture_update", move |args, _, memory| {
        let handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        let x = args.get(1, memory).map_err(ExitReason::MemoryFault)? as u32;
        let y = args.get(2, memory).map_err(ExitReason::MemoryFault)? as u32;
        let w = args.get(3, memory).map_err(ExitReason::MemoryFault)? as u32;
        let h = 2u32;
        let status = unsafe {
            xenora_session_update_texture(
                session,
                handle,
                x,
                y,
                w,
                h,
                UPDATED_TEXTURE_2X2.as_ptr(),
                16,
            )
        };
        if status != XENORA_OK {
            return Err(ExitReason::IllegalInstruction(0xfb));
        }
        Ok(1)
    });

    imports.register("xenora.dll", "texture_destroy", move |args, _, memory| {
        let handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        let status = xenora_session_destroy_resource(session, handle);
        if status != XENORA_OK {
            return Err(ExitReason::IllegalInstruction(0xfa));
        }
        Ok(1)
    });

    let startup = run_synthetic_startup(
        TEXTURED_FIXTURE,
        StartupLayout {
            image_base: 0x1_4000_0000,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        },
        imports,
        100,
    )
    .map_err(|e| format!("synthetic startup: {e}"))?;

    if startup.execution.exit_reason != ExitReason::Halt {
        return Err(format!(
            "unexpected exit reason: {:?}",
            startup.execution.exit_reason
        ));
    }

    let final_pixels = last_frame
        .lock()
        .map_err(|_| "frame lock poisoned")?
        .take()
        .ok_or_else(|| "guest halted without rendering a frame".to_string())?;

    if final_pixels != UPDATED_TEXTURE_2X2 {
        return Err("final read-back frame did not match updated texture".into());
    }

    Ok(final_pixels)
}

/// # Safety
/// `out_rgba` must be non-null and valid for `capacity` byte writes;
/// `out_width` and `out_height` must be non-null and valid for one aligned `u32` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_run_textured_fixture(
    session: u64,
    out_rgba: *mut u8,
    capacity: u64,
    out_width: *mut u32,
    out_height: *mut u32,
) -> i32 {
    guarded(|| {
        if out_rgba.is_null() || out_width.is_null() || out_height.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        if capacity < 16 {
            return XENORA_BUFFER_TOO_SMALL;
        }
        match SESSIONS.lock() {
            Ok(slots) if is_live(&slots, session) => {}
            Ok(_) => return XENORA_INVALID_HANDLE,
            Err(_) => return XENORA_INTERNAL_ERROR,
        }
        let Ok(pixels) = run_textured_fixture(session) else {
            return XENORA_EXECUTION_FAILED;
        };
        // SAFETY: non-null and at least 16 bytes, checked above.
        unsafe {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), out_rgba, 16);
            out_width.write(2);
            out_height.write(2);
        }
        XENORA_OK
    })
}

/// Internal test helper for acquiring a resource in-flight reference.
pub fn test_acquire_resource_in_flight(session: u64, resource: u64) -> Result<(), i32> {
    let Ok(mut slots) = SESSIONS.lock() else {
        return Err(XENORA_INTERNAL_ERROR);
    };
    if !is_live(&slots, session) {
        return Err(XENORA_INVALID_HANDLE);
    }
    let slot = &mut slots[split(session).0];
    let res = get_resource_mut(slot, resource)?;
    res.in_flight += 1;
    Ok(())
}

/// Internal test helper for releasing a resource in-flight reference.
pub fn test_release_resource_in_flight(session: u64, resource: u64) -> Result<(), i32> {
    let Ok(mut slots) = SESSIONS.lock() else {
        return Err(XENORA_INTERNAL_ERROR);
    };
    if !is_live(&slots, session) {
        return Err(XENORA_INVALID_HANDLE);
    }
    let slot = &mut slots[split(session).0];
    let (res_idx, res_gen) = split(resource);
    let res = slot
        .resources
        .get_mut(res_idx)
        .ok_or(XENORA_INVALID_HANDLE)?;
    if res.generation != res_gen {
        return Err(XENORA_INVALID_HANDLE);
    }
    res.in_flight = res.in_flight.saturating_sub(1);
    if res.retired && res.in_flight == 0 {
        res.generation += 1;
        res.retired = false;
        res.metal_texture = None;
        res.pixels.clear();
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub extern "C" fn xenora_session_audio_init(
    session: u64,
    sample_rate: u32,
    channels: u32,
    format: u32,
) -> i32 {
    guarded(|| {
        if sample_rate == 0
            || sample_rate > 192000
            || (channels != 1 && channels != 2)
            || (format != XENORA_AUDIO_FORMAT_PCM_S16LE && format != XENORA_AUDIO_FORMAT_PCM_F32LE)
        {
            return XENORA_INVALID_ARGUMENT;
        }
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];
        slot.audio.sample_rate = sample_rate;
        slot.audio.channels = channels;
        slot.audio.format = format;
        slot.audio.queue.clear();
        slot.audio.underruns = 0;
        XENORA_OK
    })
}

/// # Safety
/// `pcm_bytes` must be non-null and valid for `byte_count` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_audio_submit(
    session: u64,
    pcm_bytes: *const u8,
    byte_count: u64,
) -> i32 {
    guarded(|| {
        if pcm_bytes.is_null() || byte_count == 0 || byte_count > 16 * 1024 * 1024 {
            return XENORA_INVALID_ARGUMENT;
        }
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];
        if !(byte_count as usize).is_multiple_of(slot.audio.frame_bytes()) {
            return XENORA_INVALID_ARGUMENT;
        }
        // SAFETY: checked non-null and bounded above.
        let bytes = unsafe { std::slice::from_raw_parts(pcm_bytes, byte_count as usize) };
        slot.audio.submit(bytes);
        XENORA_OK
    })
}

/// # Safety
/// `out_pcm` must be non-null and valid for `capacity` writable bytes;
/// `out_bytes_read` must be non-null and valid for one aligned `u64` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_audio_read(
    session: u64,
    out_pcm: *mut u8,
    capacity: u64,
    out_bytes_read: *mut u64,
) -> i32 {
    guarded(|| {
        if out_pcm.is_null() || out_bytes_read.is_null() || capacity > isize::MAX as u64 {
            return XENORA_INVALID_ARGUMENT;
        }
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];
        // SAFETY: checked non-null above.
        let out_slice = unsafe { std::slice::from_raw_parts_mut(out_pcm, capacity as usize) };
        let count = slot.audio.read(out_slice);
        unsafe { out_bytes_read.write(count as u64) };
        XENORA_OK
    })
}

/// Fill an aligned PCM playback block. Missing frames are replaced with silence.
///
/// # Safety
/// `out_pcm` must point to `byte_count` writable bytes and `out_source_bytes`
/// must be valid for one aligned `u64` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_audio_render(
    session: u64,
    out_pcm: *mut u8,
    byte_count: u64,
    out_source_bytes: *mut u64,
) -> i32 {
    guarded(|| {
        if out_pcm.is_null()
            || out_source_bytes.is_null()
            || !(1..=256 * 1024).contains(&byte_count)
        {
            return XENORA_INVALID_ARGUMENT;
        }
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let audio = &mut slots[split(session).0].audio;
        if !(byte_count as usize).is_multiple_of(audio.frame_bytes()) {
            return XENORA_INVALID_ARGUMENT;
        }
        let output = unsafe { std::slice::from_raw_parts_mut(out_pcm, byte_count as usize) };
        let supplied = audio.render(output);
        unsafe { out_source_bytes.write(supplied as u64) };
        XENORA_OK
    })
}

/// # Safety
/// Each output pointer must be non-null and valid for one aligned write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_audio_status(
    session: u64,
    out_queued_bytes: *mut u32,
    out_underruns: *mut u32,
    out_total_submitted: *mut u64,
) -> i32 {
    guarded(|| {
        if out_queued_bytes.is_null() || out_underruns.is_null() || out_total_submitted.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        let Ok(slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &slots[split(session).0];
        unsafe {
            out_queued_bytes.write(slot.audio.queue.len() as u32);
            out_underruns.write(slot.audio.underruns);
            out_total_submitted.write(slot.audio.total_submitted_bytes);
        }
        XENORA_OK
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn xenora_session_audio_set_volume(session: u64, volume: f32, muted: u32) -> i32 {
    guarded(|| {
        if !(0.0..=1.0).contains(&volume) || volume.is_nan() {
            return XENORA_INVALID_ARGUMENT;
        }
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];
        slot.audio.volume = volume;
        slot.audio.muted = muted != 0;
        XENORA_OK
    })
}

/// # Safety
/// `root_path` must be non-null and point to a valid NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_vfs_set_root(
    session: u64,
    root_path: *const std::ffi::c_char,
) -> i32 {
    guarded(|| {
        if root_path.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        let c_str = unsafe { std::ffi::CStr::from_ptr(root_path) };
        let Ok(path_str) = c_str.to_str() else {
            return XENORA_INVALID_ARGUMENT;
        };
        let path = std::path::PathBuf::from(path_str);
        if !path.exists() {
            return XENORA_INVALID_ARGUMENT;
        }
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];
        slot.vfs.root = path;
        XENORA_OK
    })
}

fn vfs_read_status(kind: std::io::ErrorKind) -> i32 {
    use std::io::ErrorKind;
    match kind {
        ErrorKind::NotFound | ErrorKind::NotADirectory => XENORA_FILE_NOT_FOUND,
        ErrorKind::PermissionDenied => XENORA_ACCESS_DENIED,
        ErrorKind::InvalidInput | ErrorKind::IsADirectory => XENORA_INVALID_ARGUMENT,
        ErrorKind::OutOfMemory => XENORA_LIMIT_EXCEEDED,
        _ => XENORA_IO_ERROR,
    }
}

/// # Safety
/// `rel_path` must be non-null and point to a valid NUL-terminated C string;
/// `out_file` must be non-null and valid for one aligned `u64` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_vfs_open(
    session: u64,
    rel_path: *const std::ffi::c_char,
    flags: u32,
    out_file: *mut u64,
) -> i32 {
    guarded(|| {
        if rel_path.is_null() || out_file.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        let c_str = unsafe { std::ffi::CStr::from_ptr(rel_path) };
        let Ok(path_str) = c_str.to_str() else {
            return XENORA_INVALID_ARGUMENT;
        };

        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];

        let full_path = match slot.vfs.sanitize_path(path_str) {
            Ok(p) => p,
            Err(e) => return e,
        };

        let data = match std::fs::read(&full_path) {
            Ok(bytes) => bytes,
            Err(error) => return vfs_read_status(error.kind()),
        };

        let file_idx = match slot.vfs.files.iter().position(|f| f.generation % 2 == 0) {
            Some(idx) => idx,
            None if slot.vfs.files.len() < MAX_VFS_FILES_PER_SESSION => {
                slot.vfs.files.push(VfsFileSlot {
                    generation: 0,
                    path: String::new(),
                    data: Vec::new(),
                    position: 0,
                    flags: 0,
                });
                slot.vfs.files.len() - 1
            }
            None => return XENORA_LIMIT_EXCEEDED,
        };

        let file_slot = &mut slot.vfs.files[file_idx];
        let Some(next_gen) = file_slot.generation.checked_add(1) else {
            return XENORA_LIMIT_EXCEEDED;
        };
        file_slot.generation = next_gen;
        file_slot.path = path_str.to_string();
        file_slot.data = data;
        file_slot.position = 0;
        file_slot.flags = flags;

        let handle = (u64::from(next_gen) << 32) | (file_idx as u64);
        unsafe { out_file.write(handle) };
        XENORA_OK
    })
}

/// # Safety
/// `out_buffer` must be non-null and valid for `count` writable bytes;
/// `out_bytes_read` must be non-null and valid for one aligned `u64` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_vfs_read(
    session: u64,
    file: u64,
    out_buffer: *mut u8,
    count: u64,
    out_bytes_read: *mut u64,
) -> i32 {
    guarded(|| {
        if out_buffer.is_null() || out_bytes_read.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];

        let (file_idx, file_gen) = split(file);
        let Some(file_slot) = slot.vfs.files.get_mut(file_idx) else {
            return XENORA_INVALID_HANDLE;
        };
        if file_slot.generation != file_gen || file_gen % 2 == 0 {
            return XENORA_INVALID_HANDLE;
        }

        let available = file_slot.data.len().saturating_sub(file_slot.position);
        let to_read = (count as usize).min(available);
        if to_read > 0 {
            let src = &file_slot.data[file_slot.position..file_slot.position + to_read];
            unsafe {
                std::ptr::copy_nonoverlapping(src.as_ptr(), out_buffer, to_read);
            }
            file_slot.position += to_read;
        }
        unsafe { out_bytes_read.write(to_read as u64) };
        XENORA_OK
    })
}

/// # Safety
/// `out_new_offset` must be non-null and valid for one aligned `u64` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_vfs_seek(
    session: u64,
    file: u64,
    offset: i64,
    whence: u32,
    out_new_offset: *mut u64,
) -> i32 {
    guarded(|| {
        if out_new_offset.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];

        let (file_idx, file_gen) = split(file);
        let Some(file_slot) = slot.vfs.files.get_mut(file_idx) else {
            return XENORA_INVALID_HANDLE;
        };
        if file_slot.generation != file_gen || file_gen % 2 == 0 {
            return XENORA_INVALID_HANDLE;
        }

        let base_pos: i64 = match whence {
            XENORA_SEEK_SET => 0,
            XENORA_SEEK_CUR => file_slot.position as i64,
            XENORA_SEEK_END => file_slot.data.len() as i64,
            _ => return XENORA_INVALID_ARGUMENT,
        };

        let Some(new_pos) = base_pos.checked_add(offset) else {
            return XENORA_INVALID_ARGUMENT;
        };
        if new_pos < 0 || (new_pos as usize) > file_slot.data.len() {
            return XENORA_INVALID_ARGUMENT;
        }

        file_slot.position = new_pos as usize;
        unsafe { out_new_offset.write(new_pos as u64) };
        XENORA_OK
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn xenora_session_vfs_close(session: u64, file: u64) -> i32 {
    guarded(|| {
        let Ok(mut slots) = SESSIONS.lock() else {
            return XENORA_INTERNAL_ERROR;
        };
        if !is_live(&slots, session) {
            return XENORA_INVALID_HANDLE;
        }
        let slot = &mut slots[split(session).0];

        let (file_idx, file_gen) = split(file);
        let Some(file_slot) = slot.vfs.files.get_mut(file_idx) else {
            return XENORA_INVALID_HANDLE;
        };
        if file_slot.generation != file_gen || file_gen % 2 == 0 {
            return XENORA_INVALID_HANDLE;
        }

        file_slot.generation += 1;
        file_slot.data.clear();
        file_slot.path.clear();
        XENORA_OK
    })
}

const AUDIO_VFS_FIXTURE: &[u8] = include_bytes!("../../../fixtures/pe/audio_vfs.exe");
const EMBEDDED_ASSET: &[u8] = include_bytes!("../../../fixtures/assets/asset.bin");

fn run_audio_vfs_fixture(session: u64) -> Result<(Vec<u8>, usize), String> {
    let last_frame = Arc::new(Mutex::new(None));
    let frame_sink = Arc::clone(&last_frame);
    let mut imports = ImportRegistry::new();

    imports.register("xenora.dll", "vfs_open_asset", move |_, _, _| Ok(1));

    imports.register("xenora.dll", "vfs_read_asset", move |args, _, memory| {
        let _handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        let count = args.get(1, memory).map_err(ExitReason::MemoryFault)? as usize;
        let Ok(mut slots) = SESSIONS.lock() else {
            return Err(ExitReason::IllegalInstruction(0xfe));
        };
        if !is_live(&slots, session) {
            return Err(ExitReason::IllegalInstruction(0xfd));
        }
        let slot = &mut slots[split(session).0];
        slot.staged_asset = EMBEDDED_ASSET.to_vec();
        Ok(count.min(slot.staged_asset.len()) as u64)
    });

    imports.register("xenora.dll", "vfs_close_asset", move |_, _, _| Ok(1));

    imports.register("xenora.dll", "audio_init", move |args, _, memory| {
        let rate = args.get(0, memory).map_err(ExitReason::MemoryFault)? as u32;
        let ch = args.get(1, memory).map_err(ExitReason::MemoryFault)? as u32;
        let fmt = args.get(2, memory).map_err(ExitReason::MemoryFault)? as u32;
        let status = xenora_session_audio_init(session, rate, ch, fmt);
        if status != XENORA_OK {
            return Err(ExitReason::IllegalInstruction(0xfc));
        }
        Ok(1)
    });

    imports.register("xenora.dll", "audio_submit_pcm", move |args, _, memory| {
        let byte_count = args.get(0, memory).map_err(ExitReason::MemoryFault)? as usize;
        let pcm_data = {
            let Ok(slots) = SESSIONS.lock() else {
                return Err(ExitReason::IllegalInstruction(0xfb));
            };
            if !is_live(&slots, session) {
                return Err(ExitReason::IllegalInstruction(0xfa));
            }
            let slot = &slots[split(session).0];
            if slot.staged_asset.len() < 20 + 16 + byte_count {
                return Err(ExitReason::IllegalInstruction(0xf9));
            }
            slot.staged_asset[36..36 + byte_count].to_vec()
        };
        let status = unsafe {
            xenora_session_audio_submit(session, pcm_data.as_ptr(), pcm_data.len() as u64)
        };
        if status != XENORA_OK {
            return Err(ExitReason::IllegalInstruction(0xf8));
        }
        Ok(1)
    });

    let sink = Arc::clone(&frame_sink);
    imports.register("xenora.dll", "frame_submit_textured", move |_, _, _| {
        let tex_data = {
            let Ok(slots) = SESSIONS.lock() else {
                return Err(ExitReason::IllegalInstruction(0xf7));
            };
            if !is_live(&slots, session) {
                return Err(ExitReason::IllegalInstruction(0xf6));
            }
            let slot = &slots[split(session).0];
            if slot.staged_asset.len() < 36 {
                return Err(ExitReason::IllegalInstruction(0xf5));
            }
            slot.staged_asset[20..36].to_vec()
        };
        let mut res_handle: u64 = 0;
        let status = unsafe {
            xenora_session_create_texture(session, 2, 2, tex_data.as_ptr(), &mut res_handle)
        };
        if status != XENORA_OK {
            return Err(ExitReason::IllegalInstruction(0xf4));
        }
        let mut rendered = vec![0u8; 16];
        let mut rw = 0u32;
        let mut rh = 0u32;
        let r_status = unsafe {
            xenora_session_render_textured(
                session,
                res_handle,
                rendered.as_mut_ptr(),
                16,
                &mut rw,
                &mut rh,
            )
        };
        _ = xenora_session_destroy_resource(session, res_handle);
        if r_status != XENORA_OK || rw != 2 || rh != 2 {
            return Err(ExitReason::IllegalInstruction(0xf3));
        }
        if let Ok(mut slot) = sink.lock() {
            *slot = Some(rendered);
        }
        Ok(1)
    });

    let startup = run_synthetic_startup(
        AUDIO_VFS_FIXTURE,
        StartupLayout {
            image_base: 0x1_4000_0000,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        },
        imports,
        100,
    )
    .map_err(|e| format!("synthetic startup: {e}"))?;

    if startup.execution.exit_reason != ExitReason::Halt {
        return Err(format!(
            "unexpected exit reason: {:?}",
            startup.execution.exit_reason
        ));
    }

    let final_pixels = last_frame
        .lock()
        .map_err(|_| "frame lock poisoned")?
        .take()
        .ok_or_else(|| "guest halted without rendering a frame".to_string())?;

    if final_pixels != EMBEDDED_ASSET[20..36] {
        return Err("read-back frame did not match loaded asset texture".into());
    }

    Ok((final_pixels, 256))
}

/// # Safety
/// `out_rgba` must be non-null and valid for `capacity` byte writes;
/// `out_width`, `out_height`, and `out_audio_samples` must be non-null and valid for one aligned write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_session_run_audio_vfs_fixture(
    session: u64,
    out_rgba: *mut u8,
    capacity: u64,
    out_width: *mut u32,
    out_height: *mut u32,
    out_audio_samples: *mut u32,
) -> i32 {
    guarded(|| {
        if out_rgba.is_null()
            || out_width.is_null()
            || out_height.is_null()
            || out_audio_samples.is_null()
        {
            return XENORA_INVALID_ARGUMENT;
        }
        if capacity < 16 {
            return XENORA_BUFFER_TOO_SMALL;
        }
        match SESSIONS.lock() {
            Ok(slots) if is_live(&slots, session) => {}
            Ok(_) => return XENORA_INVALID_HANDLE,
            Err(_) => return XENORA_INTERNAL_ERROR,
        }
        let Ok((pixels, audio_samples)) = run_audio_vfs_fixture(session) else {
            return XENORA_EXECUTION_FAILED;
        };
        // SAFETY: checked non-null and capacity >= 16.
        unsafe {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), out_rgba, 16);
            out_width.write(2);
            out_height.write(2);
            out_audio_samples.write(audio_samples as u32);
        }
        XENORA_OK
    })
}

pub const XENORA_MAX_INSPECT_BYTES: u64 = 256 * 1024 * 1024;

/// Build the read-only inspection report and its status: `XENORA_OK`, `XENORA_PE_ERROR`
/// for a rejected file, or `XENORA_INTERNAL_ERROR` if Xenora's own probe failed.
fn inspection_report(bytes: &[u8]) -> (i32, String) {
    let mut out = String::from("Xenora read-only inspection: nothing in this file was executed.\n");
    let _ = writeln!(out, "file_size: {} bytes", bytes.len());
    let probed = catch_unwind(AssertUnwindSafe(|| {
        probe_pe_bytes(bytes, &ProbeConfig::default())
    }));
    let report = match probed {
        Ok(Ok(report)) => report,
        Ok(Err(e)) => {
            let _ = writeln!(out, "verdict: not a supported PE32+ (x86-64) image: {e}");
            return (XENORA_PE_ERROR, out);
        }
        Err(_) => {
            out.push_str(
                "verdict: inspection failed inside Xenora's static probe. This is a Xenora \
                 defect, not a finding about the file.\n",
            );
            return (XENORA_INTERNAL_ERROR, out);
        }
    };
    let pe = &report.parsed;
    let _ = writeln!(out, "sha256: {}", report.sha256);
    let _ = writeln!(
        out,
        "machine: {:#06x}  image_base: {:#x}  entry_rva: {:#x}  size_of_image: {:#x}",
        pe.machine, pe.image_base, pe.entry_point_rva, pe.size_of_image
    );
    match ParsedPe::parse(bytes) {
        Ok(_) => out.push_str("loader_validation: accepted\n"),
        Err(e) => {
            let _ = writeln!(out, "loader_validation: rejected: {e}");
        }
    }
    let _ = writeln!(out, "\nsections ({}):", pe.sections.len());
    for s in &pe.sections {
        let p = s.permissions;
        let _ = writeln!(
            out,
            "  {:<8} rva {:#010x}  vsize {:#x}  raw {:#x}  {}{}{}",
            s.name,
            s.virtual_address,
            s.virtual_size,
            s.raw_data_size,
            if p.read { 'r' } else { '-' },
            if p.write { 'w' } else { '-' },
            if p.execute { 'x' } else { '-' },
        );
    }
    let missing: Vec<_> = report.imports.iter().filter(|i| !i.is_supported).collect();
    let _ = writeln!(
        out,
        "\nimports: {} total, {} provided by Xenora's standard runtime services, {} not provided",
        report.imports.len(),
        report.imports.len() - missing.len(),
        missing.len()
    );
    for i in &missing {
        let _ = writeln!(out, "  not provided: {}!{}", i.dll, i.symbol);
    }
    let decoded = report.instruction_records.len();
    let unsupported = report
        .instruction_records
        .iter()
        .filter(|r| !r.is_supported_dbt)
        .count();
    let _ = writeln!(
        out,
        "\nstatic disassembly (bounded): {decoded} instructions decoded, {unsupported} not supported by the ARM64 DBT"
    );
    if let Some(first) = &report.first_unsupported_dbt_insn {
        let hex: String = first.bytes.iter().map(|b| format!("{b:02x}")).collect();
        let _ = writeln!(
            out,
            "  first unsupported: rva {:#x}  {}  [{hex}]",
            first.rva, first.mnemonic
        );
    }
    let _ = writeln!(
        out,
        "\nrelocations: {} entries\ntls callbacks: {}",
        report.relocations.total_entries,
        report.tls.callback_targets.len()
    );
    out.push_str(
        "\nInspection is read-only. The experimental runner can attempt execution separately.\n",
    );
    (XENORA_OK, out)
}

/// # Safety
/// `bytes` must be valid for `length` reads; `out_report` must be null or valid
/// for `capacity` writes; `out_required` must be null or valid for one `u64` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xenora_inspect_executable(
    bytes: *const u8,
    length: u64,
    out_report: *mut std::ffi::c_char,
    capacity: u64,
    out_required: *mut u64,
) -> i32 {
    guarded(|| {
        if bytes.is_null() || out_report.is_null() || out_required.is_null() {
            return XENORA_INVALID_ARGUMENT;
        }
        if length > XENORA_MAX_INSPECT_BYTES {
            return XENORA_LIMIT_EXCEEDED;
        }
        // SAFETY: non-null, and the caller guarantees `length` readable bytes.
        let input = unsafe { std::slice::from_raw_parts(bytes, length as usize) };
        let (status, text) = inspection_report(input);
        let required = text.len() as u64 + 1;
        // SAFETY: non-null, checked above.
        unsafe { out_required.write(required) };
        if capacity < required {
            return XENORA_BUFFER_TOO_SMALL;
        }
        // SAFETY: `capacity >= required`, checked above.
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr(), out_report.cast::<u8>(), text.len());
            out_report.add(text.len()).write(0);
        }
        status
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null_mut;

    /// The session table is process-global, so tests that fill or count it run one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn create() -> u64 {
        let mut handle = 0;
        assert_eq!(
            unsafe { xenora_session_create(XENORA_ABI_VERSION, &mut handle) },
            XENORA_OK
        );
        assert_ne!(handle, 0);
        handle
    }

    fn inspect(handle: u64) -> (i32, u32, u32) {
        let (mut sections, mut entry) = (u32::MAX, u32::MAX);
        let status =
            unsafe { xenora_session_inspect_trusted_fixture(handle, &mut sections, &mut entry) };
        (status, sections, entry)
    }

    #[test]
    fn xinput_boundary_rejects_malformed_input_before_session_state() {
        let input = xenora_xinput_state {
            connected: 1,
            ..xenora_xinput_state::default()
        };
        assert_eq!(
            unsafe { xenora_session_send_xinput_state(0, 0, std::ptr::null()) },
            XENORA_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { xenora_session_send_xinput_state(0, 4, &input) },
            XENORA_INVALID_ARGUMENT
        );
        let bad_connected = xenora_xinput_state {
            connected: 2,
            ..input
        };
        assert_eq!(
            unsafe { xenora_session_send_xinput_state(0, 0, &bad_connected) },
            XENORA_INVALID_ARGUMENT
        );
        let bad_reserved = xenora_xinput_state {
            reserved: [1, 0],
            ..input
        };
        assert_eq!(
            unsafe { xenora_session_send_xinput_state(0, 0, &bad_reserved) },
            XENORA_INVALID_ARGUMENT
        );
        let bad_features = xenora_xinput_state {
            features: 2,
            ..input
        };
        assert_eq!(
            unsafe { xenora_session_send_xinput_state(0, 0, &bad_features) },
            XENORA_INVALID_ARGUMENT
        );
        let battery = xenora_xinput_battery {
            present: 1,
            state: XENORA_XINPUT_BATTERY_STATE_DISCHARGING,
            level: 55,
            reserved: 0,
        };
        for bad in [
            xenora_xinput_battery {
                present: 2,
                ..battery
            },
            xenora_xinput_battery {
                state: 4,
                ..battery
            },
            xenora_xinput_battery {
                level: 101,
                ..battery
            },
            xenora_xinput_battery {
                reserved: 1,
                ..battery
            },
            xenora_xinput_battery {
                present: 0,
                level: 55,
                ..battery
            },
        ] {
            assert_eq!(
                unsafe { xenora_session_send_xinput_state_with_battery(0, 0, &input, &bad) },
                XENORA_INVALID_ARGUMENT
            );
        }
        assert_eq!(
            unsafe {
                xenora_session_send_xinput_state_with_battery(0, 0, &input, std::ptr::null())
            },
            XENORA_INVALID_ARGUMENT
        );
        let mut vibration = xenora_xinput_vibration::default();
        assert_eq!(
            unsafe { xenora_session_poll_xinput_vibration(0, 0, null_mut()) },
            XENORA_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { xenora_session_poll_xinput_vibration(0, 4, &mut vibration) },
            XENORA_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { xenora_session_poll_xinput_vibration(0, 0, &mut vibration) },
            XENORA_INVALID_HANDLE
        );
        assert_eq!(
            unsafe { xenora_session_send_xinput_state(0, 0, &input) },
            XENORA_INVALID_HANDLE
        );
    }

    #[test]
    fn xinput_vibration_polling_requires_a_running_loop() {
        let _serial = serial();
        let handle = create();
        let mut vibration = xenora_xinput_vibration {
            sequence: u32::MAX,
            left_motor: u16::MAX,
            right_motor: u16::MAX,
            connected: 1,
            enabled: 1,
            reserved: [1; 2],
        };
        assert_eq!(
            unsafe { xenora_session_poll_xinput_vibration(handle, 0, &mut vibration) },
            XENORA_INVALID_STATE
        );
        assert_eq!(vibration.sequence, u32::MAX);
        assert_eq!(xenora_session_destroy(handle), XENORA_OK);
    }

    #[test]
    fn host_vfs_read_errors_have_stable_statuses() {
        use std::io::ErrorKind;
        for (kind, expected) in [
            (ErrorKind::NotFound, XENORA_FILE_NOT_FOUND),
            (ErrorKind::NotADirectory, XENORA_FILE_NOT_FOUND),
            (ErrorKind::PermissionDenied, XENORA_ACCESS_DENIED),
            (ErrorKind::IsADirectory, XENORA_INVALID_ARGUMENT),
            (ErrorKind::OutOfMemory, XENORA_LIMIT_EXCEEDED),
            (ErrorKind::UnexpectedEof, XENORA_IO_ERROR),
        ] {
            assert_eq!(vfs_read_status(kind), expected);
        }
    }

    #[test]
    fn header_and_rust_constants_agree() {
        let header = include_str!("../include/xenora.h");
        assert!(header.contains(&format!("#define XENORA_ABI_VERSION {XENORA_ABI_VERSION}u")));
        for (name, value) in [
            ("XENORA_OK", XENORA_OK),
            ("XENORA_INVALID_ARGUMENT", XENORA_INVALID_ARGUMENT),
            ("XENORA_INVALID_HANDLE", XENORA_INVALID_HANDLE),
            ("XENORA_VERSION_MISMATCH", XENORA_VERSION_MISMATCH),
            ("XENORA_INTERNAL_ERROR", XENORA_INTERNAL_ERROR),
            ("XENORA_PE_ERROR", XENORA_PE_ERROR),
            ("XENORA_LIMIT_EXCEEDED", XENORA_LIMIT_EXCEEDED),
            ("XENORA_BUFFER_TOO_SMALL", XENORA_BUFFER_TOO_SMALL),
            ("XENORA_EXECUTION_FAILED", XENORA_EXECUTION_FAILED),
            ("XENORA_INVALID_STATE", XENORA_INVALID_STATE),
            ("XENORA_FILE_NOT_FOUND", XENORA_FILE_NOT_FOUND),
            ("XENORA_ACCESS_DENIED", XENORA_ACCESS_DENIED),
            ("XENORA_IO_ERROR", XENORA_IO_ERROR),
            ("XENORA_LOOP_NONE", XENORA_LOOP_NONE as i32),
            ("XENORA_LOOP_RUNNING", XENORA_LOOP_RUNNING as i32),
            ("XENORA_LOOP_PAUSED", XENORA_LOOP_PAUSED as i32),
            ("XENORA_LOOP_FINISHED", XENORA_LOOP_FINISHED as i32),
            ("XENORA_LOOP_STOPPED", XENORA_LOOP_STOPPED as i32),
            ("XENORA_LOOP_FAILED", XENORA_LOOP_FAILED as i32),
        ] {
            assert!(header.contains(&format!("{name} = {value},")), "{name}");
        }
        for (name, value) in [
            (
                "XENORA_XINPUT_GAMEPAD_DPAD_UP",
                XENORA_XINPUT_GAMEPAD_DPAD_UP,
            ),
            (
                "XENORA_XINPUT_GAMEPAD_DPAD_DOWN",
                XENORA_XINPUT_GAMEPAD_DPAD_DOWN,
            ),
            (
                "XENORA_XINPUT_GAMEPAD_DPAD_LEFT",
                XENORA_XINPUT_GAMEPAD_DPAD_LEFT,
            ),
            (
                "XENORA_XINPUT_GAMEPAD_DPAD_RIGHT",
                XENORA_XINPUT_GAMEPAD_DPAD_RIGHT,
            ),
            ("XENORA_XINPUT_GAMEPAD_START", XENORA_XINPUT_GAMEPAD_START),
            ("XENORA_XINPUT_GAMEPAD_BACK", XENORA_XINPUT_GAMEPAD_BACK),
            (
                "XENORA_XINPUT_GAMEPAD_LEFT_THUMB",
                XENORA_XINPUT_GAMEPAD_LEFT_THUMB,
            ),
            (
                "XENORA_XINPUT_GAMEPAD_RIGHT_THUMB",
                XENORA_XINPUT_GAMEPAD_RIGHT_THUMB,
            ),
            (
                "XENORA_XINPUT_GAMEPAD_LEFT_SHOULDER",
                XENORA_XINPUT_GAMEPAD_LEFT_SHOULDER,
            ),
            (
                "XENORA_XINPUT_GAMEPAD_RIGHT_SHOULDER",
                XENORA_XINPUT_GAMEPAD_RIGHT_SHOULDER,
            ),
            ("XENORA_XINPUT_GAMEPAD_A", XENORA_XINPUT_GAMEPAD_A),
            ("XENORA_XINPUT_GAMEPAD_B", XENORA_XINPUT_GAMEPAD_B),
            ("XENORA_XINPUT_GAMEPAD_X", XENORA_XINPUT_GAMEPAD_X),
            ("XENORA_XINPUT_GAMEPAD_Y", XENORA_XINPUT_GAMEPAD_Y),
        ] {
            assert!(
                header.contains(&format!("#define {name} 0x{value:04x}u")),
                "{name}"
            );
        }
        assert!(header.contains(&format!(
            "XENORA_FIXTURE_FRAME_WIDTH {XENORA_FIXTURE_FRAME_WIDTH}u"
        )));
        assert!(header.contains(&format!(
            "XENORA_FIXTURE_FRAME_HEIGHT {XENORA_FIXTURE_FRAME_HEIGHT}u"
        )));
        assert!(header.contains(&format!(
            "XENORA_MAX_INSPECT_BYTES {XENORA_MAX_INSPECT_BYTES}u"
        )));
        assert!(header.contains(&format!(
            "XENORA_FRAME_LOOP_MAX_FRAMES {XENORA_FRAME_LOOP_MAX_FRAMES}u"
        )));
        assert!(header.contains("XENORA_NO_FRAME 0xFFFFFFFFu"));
        assert!(header.contains("XENORA_INPUT_NONE     0u"));
        assert!(header.contains("XENORA_INPUT_RIGHT    (1u << 0)"));
        assert!(header.contains("XENORA_INPUT_LEFT     (1u << 1)"));
        assert!(header.contains("XENORA_INPUT_UP       (1u << 2)"));
        assert!(header.contains("XENORA_INPUT_DOWN     (1u << 3)"));
        assert!(header.contains("XENORA_INPUT_ACTION_A (1u << 4)"));
        assert!(header.contains("XENORA_INPUT_ACTION_B (1u << 5)"));
    }

    #[test]
    fn session_inspects_the_trusted_fixture() {
        let _serial = serial();
        let handle = create();
        // Independent expectation: read straight from the fixture's COFF and optional headers.
        let lfanew = u32::from_le_bytes(FIXTURE[0x3C..0x40].try_into().unwrap()) as usize;
        let sections = u16::from_le_bytes(FIXTURE[lfanew + 6..lfanew + 8].try_into().unwrap());
        let entry = u32::from_le_bytes(FIXTURE[lfanew + 40..lfanew + 44].try_into().unwrap());
        assert_eq!(inspect(handle), (XENORA_OK, u32::from(sections), entry));
        assert_eq!(xenora_session_destroy(handle), XENORA_OK);
    }

    #[test]
    fn version_mismatch_and_null_outputs_are_rejected() {
        let _serial = serial();
        let mut handle = 7;
        assert_eq!(
            unsafe { xenora_session_create(2, &mut handle) },
            XENORA_VERSION_MISMATCH
        );
        assert_eq!(handle, 7, "output untouched on failure");
        assert_eq!(
            unsafe { xenora_session_create(1, null_mut()) },
            XENORA_INVALID_ARGUMENT
        );
        let live = create();
        let mut x = 0;
        assert_eq!(
            unsafe { xenora_session_inspect_trusted_fixture(live, null_mut(), &mut x) },
            XENORA_INVALID_ARGUMENT
        );
        xenora_session_destroy(live);
    }

    #[test]
    fn invalid_stale_and_double_destroyed_handles_are_rejected() {
        let _serial = serial();
        for bogus in [0, u64::MAX, 0xFFFF_FFFF, 1 << 32] {
            assert_eq!(
                xenora_session_destroy(bogus),
                XENORA_INVALID_HANDLE,
                "{bogus:#x}"
            );
            assert_eq!(inspect(bogus), (XENORA_INVALID_HANDLE, u32::MAX, u32::MAX));
        }
        let handle = create();
        assert_eq!(xenora_session_destroy(handle), XENORA_OK);
        assert_eq!(xenora_session_destroy(handle), XENORA_INVALID_HANDLE);
        assert_eq!(inspect(handle).0, XENORA_INVALID_HANDLE);
        // Reusing the slot must not revive the old handle.
        let reused: Vec<u64> = (0..MAX_SESSIONS).map(|_| create()).collect();
        assert!(!reused.contains(&handle));
        assert_eq!(inspect(handle).0, XENORA_INVALID_HANDLE);
        for h in reused {
            xenora_session_destroy(h);
        }
    }

    #[test]
    fn repeated_and_concurrent_lifecycles_do_not_leak_slots() {
        let _serial = serial();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..500 {
                        let mut handle = 0;
                        let status = unsafe { xenora_session_create(1, &mut handle) };
                        assert_eq!(status, XENORA_OK);
                        assert_eq!(inspect(handle).0, XENORA_OK);
                        assert_eq!(xenora_session_destroy(handle), XENORA_OK);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert!(SESSIONS.lock().unwrap().len() <= MAX_SESSIONS);
    }

    #[test]
    fn panics_do_not_cross_the_boundary() {
        assert_eq!(guarded(|| panic!("injected")), XENORA_INTERNAL_ERROR);
    }

    #[test]
    fn fixture_run_rejects_bad_arguments_before_executing() {
        let _serial = serial();
        let handle = create();
        let (mut buf, mut w, mut h) = ([0u8; 48], 0u32, 0u32);
        let run = |session, buf: *mut u8, cap, w: *mut u32, h: *mut u32| unsafe {
            xenora_session_run_trusted_fixture(session, buf, cap, w, h)
        };
        assert_eq!(
            run(handle, buf.as_mut_ptr(), 47, &mut w, &mut h),
            XENORA_BUFFER_TOO_SMALL
        );
        assert_eq!(
            run(handle, null_mut(), 48, &mut w, &mut h),
            XENORA_INVALID_ARGUMENT
        );
        assert_eq!(
            run(handle, buf.as_mut_ptr(), 48, null_mut(), &mut h),
            XENORA_INVALID_ARGUMENT
        );
        xenora_session_destroy(handle);
        assert_eq!(
            run(handle, buf.as_mut_ptr(), 48, &mut w, &mut h),
            XENORA_INVALID_HANDLE
        );
        assert_eq!((w, h), (0, 0), "outputs untouched on failure");
        assert!(buf.iter().all(|&b| b == 0));
    }

    fn inspect_bytes(bytes: &[u8]) -> (i32, String) {
        let mut required = 0;
        let mut buf = vec![0 as std::ffi::c_char; 1];
        let status = unsafe {
            xenora_inspect_executable(
                bytes.as_ptr(),
                bytes.len() as u64,
                buf.as_mut_ptr(),
                1,
                &mut required,
            )
        };
        assert_eq!(status, XENORA_BUFFER_TOO_SMALL);
        assert_eq!(buf[0], 0, "nothing written when the buffer is too small");
        buf.resize(required as usize, 0);
        let status = unsafe {
            xenora_inspect_executable(
                bytes.as_ptr(),
                bytes.len() as u64,
                buf.as_mut_ptr(),
                required,
                &mut required,
            )
        };
        let text = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(text.len() as u64 + 1, required);
        (status, text)
    }

    #[test]
    fn inspection_reports_independently_read_facts_of_a_fixture() {
        let (status, text) = inspect_bytes(FIXTURE);
        assert_eq!(status, XENORA_OK);
        let lfanew = u32::from_le_bytes(FIXTURE[0x3C..0x40].try_into().unwrap()) as usize;
        let sections = u16::from_le_bytes(FIXTURE[lfanew + 6..lfanew + 8].try_into().unwrap());
        let entry = u32::from_le_bytes(FIXTURE[lfanew + 40..lfanew + 44].try_into().unwrap());
        assert!(text.contains(&format!("file_size: {} bytes", FIXTURE.len())));
        assert!(text.contains(&format!("entry_rva: {entry:#x}")));
        assert!(text.contains(&format!("sections ({sections}):")));
        assert!(text.contains("loader_validation: accepted"));
        assert!(
            text.contains("xenora.dll") || text.contains("1 provided by Xenora"),
            "{text}"
        );
        assert!(text.contains("nothing in this file was executed"));
        assert!(text.contains("experimental runner can attempt execution separately"));
    }

    #[test]
    fn inspection_rejects_malformed_and_oversized_input() {
        for bad in [&b""[..], b"not a pe file", &[0u8; 0x100]] {
            let (status, text) = inspect_bytes(bad);
            assert_eq!(status, XENORA_PE_ERROR, "{text}");
            assert!(text.contains("not a supported PE32+"), "{text}");
        }
        // Every truncation of a valid file yields a report and a deterministic status.
        // Some truncations currently hit a panic inside the loader's static probe
        // (probe.rs, #270); the boundary must turn it into a report.
        for len in (0..FIXTURE.len()).step_by(7) {
            let (status, text) = inspect_bytes(&FIXTURE[..len]);
            assert!(
                matches!(status, XENORA_OK | XENORA_PE_ERROR | XENORA_INTERNAL_ERROR),
                "len {len}"
            );
            assert_eq!(
                inspect_bytes(&FIXTURE[..len]),
                (status, text),
                "len {len} is deterministic"
            );
        }
        let (mut req, mut c) = (0u64, 0 as std::ffi::c_char);
        let one = [0u8];
        assert_eq!(
            unsafe {
                xenora_inspect_executable(
                    one.as_ptr(),
                    XENORA_MAX_INSPECT_BYTES + 1,
                    &mut c,
                    1,
                    &mut req,
                )
            },
            XENORA_LIMIT_EXCEEDED
        );
        assert_eq!(
            unsafe { xenora_inspect_executable(std::ptr::null(), 0, &mut c, 1, &mut req) },
            XENORA_INVALID_ARGUMENT
        );
    }

    #[test]
    fn frame_loop_rejects_bad_arguments_and_states() {
        let _serial = serial();
        let handle = create();
        assert_eq!(
            xenora_session_start_frame_loop(handle, 0),
            XENORA_INVALID_ARGUMENT
        );
        assert_eq!(
            xenora_session_start_interactive_loop(handle, 0),
            XENORA_INVALID_ARGUMENT
        );
        assert_eq!(
            xenora_session_start_frame_loop(handle, XENORA_FRAME_LOOP_MAX_FRAMES + 1),
            XENORA_INVALID_ARGUMENT
        );
        assert_eq!(
            xenora_session_start_interactive_loop(handle, XENORA_FRAME_LOOP_MAX_FRAMES + 1),
            XENORA_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { xenora_session_send_input(handle, std::ptr::null()) },
            XENORA_INVALID_ARGUMENT
        );
        let dummy_input = xenora_input_state::default();
        assert_eq!(
            unsafe { xenora_session_send_input(handle, &dummy_input) },
            XENORA_INVALID_STATE
        );
        assert_eq!(
            unsafe { xenora_session_queue_input(handle, &dummy_input) },
            XENORA_INVALID_STATE
        );
        let mut rec_id = 0u32;
        let mut rec_rgba = [0u8; 48];
        assert_eq!(
            unsafe {
                xenora_session_get_recorded_frame(handle, 0, &mut rec_id, rec_rgba.as_mut_ptr(), 48)
            },
            XENORA_INVALID_STATE
        );
        assert_eq!(
            xenora_session_pause_frame_loop(handle),
            XENORA_INVALID_STATE
        );
        assert_eq!(
            xenora_session_resume_frame_loop(handle),
            XENORA_INVALID_STATE
        );
        assert_eq!(xenora_session_stop_frame_loop(handle), XENORA_INVALID_STATE);
        let (mut state, mut done, mut id, mut rgba) = (9, 9, 9, [0u8; 48]);
        let mut status = |h, cap| unsafe {
            xenora_session_frame_loop_status(
                h,
                &mut state,
                &mut done,
                &mut id,
                rgba.as_mut_ptr(),
                cap,
            )
        };
        assert_eq!(status(handle, 47), XENORA_BUFFER_TOO_SMALL);
        assert_eq!(status(handle, 48), XENORA_OK);
        xenora_session_destroy(handle);
        assert_eq!(status(handle, 48), XENORA_INVALID_HANDLE);
        assert_eq!((state, done, id), (XENORA_LOOP_NONE, 0, XENORA_NO_FRAME));
        assert_eq!(
            xenora_session_start_frame_loop(handle, 1),
            XENORA_INVALID_HANDLE
        );
        assert_eq!(
            xenora_session_pause_frame_loop(handle),
            XENORA_INVALID_HANDLE
        );
    }

    #[test]
    fn test_resource_management_lifecycle_and_validation() {
        let _guard = serial();
        let session = create();

        let mut res = 0u64;
        let initial = [
            255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        assert_eq!(
            unsafe { xenora_session_create_texture(session, 2, 2, initial.as_ptr(), &mut res) },
            XENORA_OK
        );
        assert_ne!(res, XENORA_NO_RESOURCE);

        // Invalid creation parameters
        let mut dummy = 0u64;
        assert_eq!(
            unsafe { xenora_session_create_texture(session, 0, 2, std::ptr::null(), &mut dummy) },
            XENORA_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe {
                xenora_session_create_texture(session, 2, 2, std::ptr::null(), std::ptr::null_mut())
            },
            XENORA_INVALID_ARGUMENT
        );

        // Query info
        let (mut r_type, mut r_w, mut r_h, mut r_flight) = (0u32, 0u32, 0u32, 0u32);
        assert_eq!(
            unsafe {
                xenora_session_resource_info(
                    session,
                    res,
                    &mut r_type,
                    &mut r_w,
                    &mut r_h,
                    &mut r_flight,
                )
            },
            XENORA_OK
        );
        assert_eq!(r_type, XENORA_RESOURCE_TYPE_TEXTURE_2D);
        assert_eq!(r_w, 2);
        assert_eq!(r_h, 2);
        assert_eq!(r_flight, 0);

        // Subregion update
        let update_pixel = [128u8, 128u8, 128u8, 255u8];
        assert_eq!(
            unsafe {
                xenora_session_update_texture(session, res, 1, 1, 1, 1, update_pixel.as_ptr(), 4)
            },
            XENORA_OK
        );

        // Out-of-bounds update
        assert_eq!(
            unsafe {
                xenora_session_update_texture(session, res, 2, 0, 1, 1, update_pixel.as_ptr(), 4)
            },
            XENORA_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe {
                xenora_session_update_texture(session, res, 0, 0, 2, 2, update_pixel.as_ptr(), 8)
            },
            XENORA_INVALID_ARGUMENT
        );

        // Destroy resource
        assert_eq!(xenora_session_destroy_resource(session, res), XENORA_OK);

        // Double destroy rejected
        assert_eq!(
            xenora_session_destroy_resource(session, res),
            XENORA_INVALID_HANDLE
        );
        // Stale handle queries rejected
        assert_eq!(
            unsafe {
                xenora_session_resource_info(
                    session,
                    res,
                    &mut r_type,
                    &mut r_w,
                    &mut r_h,
                    &mut r_flight,
                )
            },
            XENORA_INVALID_HANDLE
        );
        assert_eq!(
            unsafe {
                xenora_session_update_texture(session, res, 0, 0, 1, 1, update_pixel.as_ptr(), 4)
            },
            XENORA_INVALID_HANDLE
        );

        // Next allocation reuses slot with incremented generation
        let mut res2 = 0u64;
        assert_eq!(
            unsafe { xenora_session_create_texture(session, 4, 4, std::ptr::null(), &mut res2) },
            XENORA_OK
        );
        assert_eq!(split(res).0, split(res2).0);
        assert_ne!(split(res).1, split(res2).1);

        xenora_session_destroy(session);
    }

    #[test]
    fn test_deferred_resource_retirement_waits_for_in_flight() {
        let _guard = serial();
        let session = create();

        let mut res = 0u64;
        assert_eq!(
            unsafe { xenora_session_create_texture(session, 2, 2, std::ptr::null(), &mut res) },
            XENORA_OK
        );

        // Acquire an in-flight reference
        assert_eq!(test_acquire_resource_in_flight(session, res), Ok(()));

        let (mut r_type, mut r_w, mut r_h, mut r_flight) = (0u32, 0u32, 0u32, 0u32);
        assert_eq!(
            unsafe {
                xenora_session_resource_info(
                    session,
                    res,
                    &mut r_type,
                    &mut r_w,
                    &mut r_h,
                    &mut r_flight,
                )
            },
            XENORA_OK
        );
        assert_eq!(r_flight, 1);

        // Destroy while in flight -> marked retired
        assert_eq!(xenora_session_destroy_resource(session, res), XENORA_OK);

        // Subsequent user queries see handle as invalid
        assert_eq!(
            unsafe {
                xenora_session_resource_info(
                    session,
                    res,
                    &mut r_type,
                    &mut r_w,
                    &mut r_h,
                    &mut r_flight,
                )
            },
            XENORA_INVALID_HANDLE
        );
        assert_eq!(
            xenora_session_destroy_resource(session, res),
            XENORA_INVALID_HANDLE
        );

        // Release in flight -> slot is freed
        assert_eq!(test_release_resource_in_flight(session, res), Ok(()));

        // Slot can be reused now
        let mut res_reused = 0u64;
        assert_eq!(
            unsafe {
                xenora_session_create_texture(session, 2, 2, std::ptr::null(), &mut res_reused)
            },
            XENORA_OK
        );
        assert_eq!(split(res).0, split(res_reused).0);
        assert!(split(res_reused).1 > split(res).1);

        xenora_session_destroy(session);
    }

    #[test]
    fn pcm_queue_keeps_the_newest_complete_frames_after_wrap_and_overflow() {
        let _guard = serial();
        let session = create();
        assert_eq!(
            xenora_session_audio_init(session, 48_000, 2, XENORA_AUDIO_FORMAT_PCM_S16LE),
            XENORA_OK
        );
        const CAPACITY: usize = 256 * 1024;
        let source: Vec<u8> = (0..66_000_u32)
            .flat_map(|frame| [frame as u16, !(frame as u16)])
            .flat_map(u16::to_le_bytes)
            .collect();
        let (initial, additional) = source.split_at(CAPACITY);

        let submit = |bytes: &[u8]| {
            assert_eq!(
                unsafe { xenora_session_audio_submit(session, bytes.as_ptr(), bytes.len() as u64) },
                XENORA_OK
            );
        };
        let read = |output: &mut [u8]| {
            let mut actual = 0;
            assert_eq!(
                unsafe {
                    xenora_session_audio_read(
                        session,
                        output.as_mut_ptr(),
                        output.len() as u64,
                        &mut actual,
                    )
                },
                XENORA_OK
            );
            assert_eq!(actual, output.len() as u64);
        };

        submit(initial);
        let mut prefix = [0xaa; 8];
        read(&mut prefix);
        assert_eq!(prefix, source[..8]);
        submit(&additional[..16]);
        let mut output = vec![0; CAPACITY];
        read(&mut output);
        assert_eq!(output, source[16..CAPACITY + 16]);

        // A single oversized submission replaces a preexisting frame, and only
        // the source suffix survives. Total submitted counts the discarded bytes.
        submit(&initial[..4]);
        submit(&source);
        read(&mut output);
        assert_eq!(output, source[source.len() - CAPACITY..]);

        let (mut queued, mut underruns, mut total) = (99, 99, 0);
        assert_eq!(
            unsafe {
                xenora_session_audio_status(session, &mut queued, &mut underruns, &mut total)
            },
            XENORA_OK
        );
        assert_eq!(
            (queued, underruns, total),
            (0, 0, (CAPACITY + 20 + source.len()) as u64)
        );
        assert_eq!(xenora_session_destroy(session), XENORA_OK);
    }

    #[test]
    fn queued_s16_pcm_uses_controls_at_the_next_render() {
        let _guard = serial();
        let session = create();
        assert_eq!(
            xenora_session_audio_init(session, 48_000, 1, XENORA_AUDIO_FORMAT_PCM_S16LE),
            XENORA_OK
        );
        let pcm = [0xe8, 0x03, 0x18, 0xfc, 0xd0, 0x07]; // 1000, -1000, 2000
        assert_eq!(
            unsafe { xenora_session_audio_submit(session, pcm.as_ptr(), pcm.len() as u64) },
            XENORA_OK
        );

        let mut output = [0xaa; 2];
        let mut supplied = 0;
        for (gain, muted, expected) in [(1.0, 0, 1000i16), (1.0, 1, 0), (0.5, 0, 1000)] {
            assert_eq!(
                xenora_session_audio_set_volume(session, gain, muted),
                XENORA_OK
            );
            assert_eq!(
                unsafe {
                    xenora_session_audio_render(session, output.as_mut_ptr(), 2, &mut supplied)
                },
                XENORA_OK
            );
            assert_eq!(supplied, 2);
            assert_eq!(output, expected.to_le_bytes());
        }

        let (mut queued, mut underruns, mut total) = (99, 99, 0);
        assert_eq!(
            unsafe {
                xenora_session_audio_status(session, &mut queued, &mut underruns, &mut total)
            },
            XENORA_OK
        );
        assert_eq!((queued, underruns, total), (0, 0, 6));
        assert_eq!(xenora_session_destroy(session), XENORA_OK);
    }

    #[test]
    fn queued_float_pcm_retains_samples_submitted_while_muted() {
        let _guard = serial();
        let session = create();
        assert_eq!(
            xenora_session_audio_init(session, 48_000, 2, XENORA_AUDIO_FORMAT_PCM_F32LE),
            XENORA_OK
        );
        assert_eq!(xenora_session_audio_set_volume(session, 1.0, 1), XENORA_OK);
        let pcm: Vec<u8> = [0.75_f32, -0.5, f32::INFINITY, -4.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        assert_eq!(
            unsafe { xenora_session_audio_submit(session, pcm.as_ptr(), pcm.len() as u64) },
            XENORA_OK
        );
        assert_eq!(xenora_session_audio_set_volume(session, 0.5, 0), XENORA_OK);

        let mut output = [0xaa; 17];
        let mut actual = 0;
        assert_eq!(
            unsafe { xenora_session_audio_read(session, output.as_mut_ptr(), 17, &mut actual) },
            XENORA_OK
        );
        assert_eq!(actual, 16);
        for (sample, expected) in output[..16]
            .as_chunks::<4>()
            .0
            .iter()
            .zip([0.375_f32, -0.25, 0.0, -1.0])
        {
            assert_eq!(*sample, expected.to_le_bytes());
        }
        assert_eq!(output[16], 0xaa);
        assert_eq!(xenora_session_destroy(session), XENORA_OK);
    }

    #[test]
    fn audio_reinit_clears_queue_and_underruns_but_preserves_controls_and_total() {
        let _guard = serial();
        let session = create();
        assert_eq!(
            xenora_session_audio_init(session, 48_000, 1, XENORA_AUDIO_FORMAT_PCM_S16LE),
            XENORA_OK
        );
        assert_eq!(xenora_session_audio_set_volume(session, 0.5, 0), XENORA_OK);
        let pcm = 1000i16.to_le_bytes();
        assert_eq!(
            unsafe { xenora_session_audio_submit(session, pcm.as_ptr(), pcm.len() as u64) },
            XENORA_OK
        );
        let mut output = [0xaa; 2];
        let mut actual = 0;
        assert_eq!(
            unsafe { xenora_session_audio_read(session, output.as_mut_ptr(), 2, &mut actual) },
            XENORA_OK
        );
        assert_eq!(actual, 2);
        assert_eq!(output, 500i16.to_le_bytes());

        let mut padding = [0xaa; 4];
        let mut supplied = 99;
        assert_eq!(
            unsafe { xenora_session_audio_render(session, padding.as_mut_ptr(), 4, &mut supplied) },
            XENORA_OK
        );
        assert_eq!(supplied, 0);
        let (mut queued, mut underruns, mut total) = (99, 99, 0);
        assert_eq!(
            unsafe {
                xenora_session_audio_status(session, &mut queued, &mut underruns, &mut total)
            },
            XENORA_OK
        );
        assert_eq!((queued, underruns, total), (0, 1, 2));

        // Reinitializing changes the frame size and clears queue/underruns; the
        // volume setting and cumulative submitted count survive.
        assert_eq!(
            xenora_session_audio_init(session, 48_000, 1, XENORA_AUDIO_FORMAT_PCM_F32LE),
            XENORA_OK
        );
        assert_eq!(
            unsafe {
                xenora_session_audio_status(session, &mut queued, &mut underruns, &mut total)
            },
            XENORA_OK
        );
        assert_eq!((queued, underruns, total), (0, 0, 2));
        let float = 1.0f32.to_le_bytes();
        assert_eq!(
            unsafe { xenora_session_audio_submit(session, float.as_ptr(), float.len() as u64) },
            XENORA_OK
        );
        let mut float_out = [0xaa; 4];
        assert_eq!(
            unsafe { xenora_session_audio_read(session, float_out.as_mut_ptr(), 4, &mut actual) },
            XENORA_OK
        );
        assert_eq!(actual, 4);
        assert_eq!(float_out, 0.5f32.to_le_bytes());

        // A second reinitialization preserves the mute state too.
        assert_eq!(xenora_session_audio_set_volume(session, 0.5, 1), XENORA_OK);
        assert_eq!(
            xenora_session_audio_init(session, 48_000, 1, XENORA_AUDIO_FORMAT_PCM_S16LE),
            XENORA_OK
        );
        assert_eq!(
            unsafe { xenora_session_audio_submit(session, pcm.as_ptr(), pcm.len() as u64) },
            XENORA_OK
        );
        assert_eq!(
            unsafe { xenora_session_audio_read(session, output.as_mut_ptr(), 2, &mut actual) },
            XENORA_OK
        );
        assert_eq!(actual, 2);
        assert_eq!(output, [0, 0]);
        assert_eq!(
            unsafe {
                xenora_session_audio_status(session, &mut queued, &mut underruns, &mut total)
            },
            XENORA_OK
        );
        assert_eq!((queued, underruns, total), (0, 0, 8));
        assert_eq!(xenora_session_destroy(session), XENORA_OK);
    }

    #[test]
    fn f32_stereo_oversized_submission_keeps_the_newest_whole_frames() {
        let _guard = serial();
        let session = create();
        assert_eq!(
            xenora_session_audio_init(session, 48_000, 2, XENORA_AUDIO_FORMAT_PCM_F32LE),
            XENORA_OK
        );
        const CAPACITY: usize = 256 * 1024;
        const FRAME: usize = 8;
        let source: Vec<u8> = (0..(CAPACITY / FRAME + 1) as u32)
            .flat_map(|frame| {
                let sample = frame as f32 / (CAPACITY / FRAME) as f32;
                [sample, -sample].into_iter().flat_map(f32::to_le_bytes)
            })
            .collect();
        assert_eq!(source.len(), CAPACITY + FRAME);
        assert_eq!(
            unsafe { xenora_session_audio_submit(session, source.as_ptr(), source.len() as u64) },
            XENORA_OK
        );
        let mut output = vec![0xaa; CAPACITY];
        let mut actual = 0;
        assert_eq!(
            unsafe {
                xenora_session_audio_read(
                    session,
                    output.as_mut_ptr(),
                    output.len() as u64,
                    &mut actual,
                )
            },
            XENORA_OK
        );
        assert_eq!(actual, CAPACITY as u64);
        assert_eq!(output, source[FRAME..]);
        let (mut queued, mut underruns, mut total) = (99, 99, 0);
        assert_eq!(
            unsafe {
                xenora_session_audio_status(session, &mut queued, &mut underruns, &mut total)
            },
            XENORA_OK
        );
        assert_eq!((queued, underruns, total), (0, 0, source.len() as u64));
        assert_eq!(xenora_session_destroy(session), XENORA_OK);
    }

    #[test]
    fn audio_render_pads_missing_frames_and_counts_only_render_shortfalls() {
        let _guard = serial();
        let session = create();
        assert_eq!(
            xenora_session_audio_init(session, 48000, 2, XENORA_AUDIO_FORMAT_PCM_S16LE),
            XENORA_OK
        );
        let frame = [1, 2, 3, 4];
        assert_eq!(
            unsafe { xenora_session_audio_submit(session, frame.as_ptr(), 4) },
            XENORA_OK
        );
        let mut output = [0xaa; 8];
        let mut supplied = 99;
        assert_eq!(
            unsafe { xenora_session_audio_render(session, output.as_mut_ptr(), 7, &mut supplied) },
            XENORA_INVALID_ARGUMENT
        );
        assert_eq!(output, [0xaa; 8]);
        assert_eq!(supplied, 99);
        assert_eq!(
            unsafe { xenora_session_audio_render(session, output.as_mut_ptr(), 8, &mut supplied) },
            XENORA_OK
        );
        assert_eq!(supplied, 4);
        assert_eq!(output, [1, 2, 3, 4, 0, 0, 0, 0]);
        assert_eq!(
            unsafe { xenora_session_audio_render(session, output.as_mut_ptr(), 8, &mut supplied) },
            XENORA_OK
        );
        assert_eq!(supplied, 0);
        assert_eq!(output, [0; 8]);
        let overflow = vec![0; 256 * 1024 + 4];
        assert_eq!(
            unsafe {
                xenora_session_audio_submit(session, overflow.as_ptr(), overflow.len() as u64)
            },
            XENORA_OK
        );
        let (mut queued, mut underruns, mut total) = (0, 0, 0);
        assert_eq!(
            unsafe {
                xenora_session_audio_status(session, &mut queued, &mut underruns, &mut total)
            },
            XENORA_OK
        );
        assert_eq!(queued, 256 * 1024);
        assert_eq!(underruns, 2);
        xenora_session_destroy(session);
    }

    #[test]
    fn float_pcm_applies_gain_and_preserves_stereo_frame_boundaries() {
        let _guard = serial();
        let session = create();
        assert_eq!(
            xenora_session_audio_init(session, 48000, 2, XENORA_AUDIO_FORMAT_PCM_F32LE),
            XENORA_OK
        );
        assert_eq!(xenora_session_audio_set_volume(session, 0.5, 0), XENORA_OK);
        let input: Vec<u8> = [1.0_f32, -0.5, f32::NAN, 4.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        assert_eq!(
            unsafe { xenora_session_audio_submit(session, input.as_ptr(), 4) },
            XENORA_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { xenora_session_audio_submit(session, input.as_ptr(), 16) },
            XENORA_OK
        );
        let mut output = [0xaa; 9];
        let mut count = 0;
        for expected in [[0.5_f32, -0.25], [0.0, 1.0]] {
            assert_eq!(
                unsafe { xenora_session_audio_read(session, output.as_mut_ptr(), 9, &mut count) },
                XENORA_OK
            );
            assert_eq!(count, 8);
            assert_eq!(&output[..4], &expected[0].to_le_bytes());
            assert_eq!(&output[4..8], &expected[1].to_le_bytes());
            assert_eq!(output[8], 0xaa);
        }
        xenora_session_destroy(session);
    }

    #[test]
    fn test_audio_subsystem_lifecycle_and_volume() {
        let _guard = serial();
        let session = create();

        assert_eq!(
            xenora_session_audio_init(session, 48000, 1, XENORA_AUDIO_FORMAT_PCM_S16LE),
            XENORA_OK
        );
        assert_eq!(
            xenora_session_audio_init(session, 0, 1, XENORA_AUDIO_FORMAT_PCM_S16LE),
            XENORA_INVALID_ARGUMENT
        );

        // Submit samples: 1000, -1000
        let pcm: [u8; 4] = [232, 3, 24, 252]; // 1000i16, -1000i16 in LE
        assert_eq!(
            unsafe { xenora_session_audio_submit(session, pcm.as_ptr(), 4) },
            XENORA_OK
        );

        let (mut q_bytes, mut underruns, mut total) = (0u32, 0u32, 0u64);
        assert_eq!(
            unsafe {
                xenora_session_audio_status(session, &mut q_bytes, &mut underruns, &mut total)
            },
            XENORA_OK
        );
        assert_eq!(q_bytes, 4);
        assert_eq!(total, 4);
        assert_eq!(underruns, 0);

        let mut read_buf = [0u8; 4];
        let mut read_count = 0u64;
        assert_eq!(
            unsafe {
                xenora_session_audio_read(session, read_buf.as_mut_ptr(), 4, &mut read_count)
            },
            XENORA_OK
        );
        assert_eq!(read_count, 4);
        assert_eq!(read_buf, pcm);

        // Volume attenuation: 50%
        assert_eq!(xenora_session_audio_set_volume(session, 0.5, 0), XENORA_OK);
        assert_eq!(
            unsafe { xenora_session_audio_submit(session, pcm.as_ptr(), 4) },
            XENORA_OK
        );
        assert_eq!(
            unsafe {
                xenora_session_audio_read(session, read_buf.as_mut_ptr(), 4, &mut read_count)
            },
            XENORA_OK
        );
        let s0 = i16::from_le_bytes([read_buf[0], read_buf[1]]);
        let s1 = i16::from_le_bytes([read_buf[2], read_buf[3]]);
        assert_eq!(s0, 500);
        assert_eq!(s1, -500);

        // Mute
        assert_eq!(xenora_session_audio_set_volume(session, 1.0, 1), XENORA_OK);
        assert_eq!(
            unsafe { xenora_session_audio_submit(session, pcm.as_ptr(), 4) },
            XENORA_OK
        );
        assert_eq!(
            unsafe {
                xenora_session_audio_read(session, read_buf.as_mut_ptr(), 4, &mut read_count)
            },
            XENORA_OK
        );
        assert_eq!(read_buf, [0, 0, 0, 0]);

        xenora_session_destroy(session);
    }

    #[test]
    fn test_vfs_sandboxing_traversal_rejection_and_file_ops() {
        let _guard = serial();
        let session = create();

        let mut file = 0u64;
        // Directory traversal rejection
        let bad_path = std::ffi::CString::new("../secret.txt").unwrap();
        assert_eq!(
            unsafe { xenora_session_vfs_open(session, bad_path.as_ptr(), 0, &mut file) },
            XENORA_INVALID_ARGUMENT
        );

        let abs_path = std::ffi::CString::new("/etc/passwd").unwrap();
        assert_eq!(
            unsafe { xenora_session_vfs_open(session, abs_path.as_ptr(), 0, &mut file) },
            XENORA_INVALID_ARGUMENT
        );

        // Open real asset fixture
        let good_path = std::ffi::CString::new("asset.bin").unwrap();
        assert_eq!(
            unsafe { xenora_session_vfs_open(session, good_path.as_ptr(), 0, &mut file) },
            XENORA_OK
        );
        assert_ne!(file, XENORA_NO_FILE);

        // Read header (4B magic 'XAST')
        let mut magic = [0u8; 4];
        let mut read_bytes = 0u64;
        assert_eq!(
            unsafe {
                xenora_session_vfs_read(session, file, magic.as_mut_ptr(), 4, &mut read_bytes)
            },
            XENORA_OK
        );
        assert_eq!(read_bytes, 4);
        assert_eq!(&magic, b"XAST");

        // Seek forward
        let mut new_pos = 0u64;
        assert_eq!(
            unsafe { xenora_session_vfs_seek(session, file, 16, XENORA_SEEK_CUR, &mut new_pos) },
            XENORA_OK
        );
        assert_eq!(new_pos, 20);

        // Close file
        assert_eq!(xenora_session_vfs_close(session, file), XENORA_OK);

        // Double close rejected
        assert_eq!(
            xenora_session_vfs_close(session, file),
            XENORA_INVALID_HANDLE
        );

        // Read on closed file rejected
        assert_eq!(
            unsafe {
                xenora_session_vfs_read(session, file, magic.as_mut_ptr(), 4, &mut read_bytes)
            },
            XENORA_INVALID_HANDLE
        );

        xenora_session_destroy(session);
    }
}
