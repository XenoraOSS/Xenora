// SPDX-License-Identifier: GPL-3.0-or-later

//! XInput controller services backed by a host-supplied [`ControllerSource`].
//!
//! Structure layouts, button masks and return codes follow the public XInput
//! reference (`XINPUT_STATE`, `XINPUT_GAMEPAD`, `XINPUT_VIBRATION`,
//! `XINPUT_CAPABILITIES`, `XInputGetState`, `XInputSetState`,
//! `XInputGetCapabilities` and `XInputEnable`). The functions return their
//! error code directly and leave the thread's last-error value alone.
//!
//! A source decides what is connected. [`SimulatedControllers`] is a
//! deterministic source for tests; it is not physical hardware. Without a
//! source every slot reports `ERROR_DEVICE_NOT_CONNECTED`.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use std::sync::{Arc, Mutex};

const MODULE: &str = "xinput1_4.dll";
const ALIASES: [&str; 2] = ["xinput1_3.dll", "xinput9_1_0.dll"];
/// Number of controller slots XInput exposes.
pub const XUSER_MAX_COUNT: u32 = 4;
const ERROR_SUCCESS: u64 = 0;
const ERROR_BAD_ARGUMENTS: u64 = 160;
const ERROR_NOACCESS: u64 = 998;
const ERROR_DEVICE_NOT_CONNECTED: u64 = 1167;
const XINPUT_FLAG_GAMEPAD: u32 = 1;
const XINPUT_DEVTYPE_GAMEPAD: u8 = 1;
const XINPUT_DEVSUBTYPE_GAMEPAD: u8 = 1;
const XINPUT_CAPS_FFB_SUPPORTED: u16 = 1;
/// Every button bit `XINPUT_GAMEPAD` defines.
pub const ALL_BUTTONS: u16 = 0xF3FF;

/// One `XINPUT_GAMEPAD` reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gamepad {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub thumb_lx: i16,
    pub thumb_ly: i16,
    pub thumb_rx: i16,
    pub thumb_ry: i16,
}

impl Gamepad {
    fn bytes(self) -> [u8; 12] {
        let mut bytes = [0; 12];
        bytes[..2].copy_from_slice(&self.buttons.to_le_bytes());
        bytes[2] = self.left_trigger;
        bytes[3] = self.right_trigger;
        for (index, axis) in [self.thumb_lx, self.thumb_ly, self.thumb_rx, self.thumb_ry]
            .into_iter()
            .enumerate()
        {
            bytes[4 + 2 * index..6 + 2 * index].copy_from_slice(&axis.to_le_bytes());
        }
        bytes
    }
}

/// Where controller readings come from.
pub trait ControllerSource: Send + Sync {
    /// The current reading of slot `index` (below [`XUSER_MAX_COUNT`]), or
    /// `None` when nothing is connected there.
    fn read(&self, index: u32) -> Option<Gamepad>;
    /// Whether the controller in slot `index` can vibrate.
    fn supports_vibration(&self, index: u32) -> bool;
    /// Set the motor speeds of slot `index`; `false` when not connected.
    fn vibrate(&self, index: u32, left: u16, right: u16) -> bool;
}

/// A source with nothing connected.
pub struct NoControllers;

impl ControllerSource for NoControllers {
    fn read(&self, _: u32) -> Option<Gamepad> {
        None
    }
    fn supports_vibration(&self, _: u32) -> bool {
        false
    }
    fn vibrate(&self, _: u32, _: u16, _: u16) -> bool {
        false
    }
}

#[derive(Clone, Copy, Default)]
struct SimulatedSlot {
    pad: Gamepad,
    rumble: bool,
    motors: (u16, u16),
}

/// Deterministic controllers for tests, changed through its methods.
#[derive(Default)]
pub struct SimulatedControllers {
    slots: Mutex<[Option<SimulatedSlot>; XUSER_MAX_COUNT as usize]>,
}

impl SimulatedControllers {
    /// Connect a neutral controller to `index`, with or without motors.
    pub fn connect(&self, index: u32, rumble: bool) {
        self.slots.lock().expect("simulated controllers")[index as usize] = Some(SimulatedSlot {
            rumble,
            ..SimulatedSlot::default()
        });
    }

    pub fn disconnect(&self, index: u32) {
        self.slots.lock().expect("simulated controllers")[index as usize] = None;
    }

    /// Replace the reading of a connected controller.
    pub fn set(&self, index: u32, pad: Gamepad) {
        if let Some(slot) = &mut self.slots.lock().expect("simulated controllers")[index as usize] {
            slot.pad = pad;
        }
    }

    /// The motor speeds last applied to slot `index`.
    pub fn motors(&self, index: u32) -> Option<(u16, u16)> {
        self.slots.lock().expect("simulated controllers")[index as usize].map(|slot| slot.motors)
    }
}

impl ControllerSource for SimulatedControllers {
    fn read(&self, index: u32) -> Option<Gamepad> {
        self.slots.lock().expect("simulated controllers")[index as usize].map(|slot| slot.pad)
    }
    fn supports_vibration(&self, index: u32) -> bool {
        self.slots.lock().expect("simulated controllers")[index as usize]
            .is_some_and(|slot| slot.rumble)
    }
    fn vibrate(&self, index: u32, left: u16, right: u16) -> bool {
        match &mut self.slots.lock().expect("simulated controllers")[index as usize] {
            Some(slot) => {
                if slot.rumble {
                    slot.motors = (left, right);
                }
                true
            }
            None => false,
        }
    }
}

/// Guest-visible XInput state: packet numbers, the enable switch and the
/// vibration each slot last asked for.
pub(super) struct XInput {
    enabled: bool,
    last: [Option<Gamepad>; XUSER_MAX_COUNT as usize],
    packets: [u32; XUSER_MAX_COUNT as usize],
    requested: [(u16, u16); XUSER_MAX_COUNT as usize],
}

impl Default for XInput {
    fn default() -> Self {
        Self {
            enabled: true,
            last: [None; XUSER_MAX_COUNT as usize],
            packets: [0; XUSER_MAX_COUNT as usize],
            requested: [(0, 0); XUSER_MAX_COUNT as usize],
        }
    }
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let get = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XInputGetState",
        ServiceStatus::Partial,
        move |args, _, memory| {
            let index = args.rcx as u32;
            if index >= XUSER_MAX_COUNT || args.rdx == 0 {
                return Ok(ERROR_BAD_ARGUMENTS);
            }
            let Some(pad) = get.controllers.read(index) else {
                get.xinput.lock().expect("xinput state").last[index as usize] = None;
                return Ok(ERROR_DEVICE_NOT_CONNECTED);
            };
            let mut xinput = get.xinput.lock().expect("xinput state");
            // While disabled, applications see a neutral controller.
            let pad = if xinput.enabled {
                pad
            } else {
                Gamepad::default()
            };
            let slot = index as usize;
            if xinput.last[slot] != Some(pad) {
                xinput.last[slot] = Some(pad);
                xinput.packets[slot] = xinput.packets[slot].wrapping_add(1);
            }
            let mut bytes = [0; 16];
            bytes[..4].copy_from_slice(&xinput.packets[slot].to_le_bytes());
            bytes[4..].copy_from_slice(&pad.bytes());
            if memory.write_bytes(args.rdx, &bytes).is_err() {
                return Ok(ERROR_NOACCESS);
            }
            Ok(ERROR_SUCCESS)
        },
    );

    let set = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XInputSetState",
        ServiceStatus::Partial,
        move |args, _, memory| {
            let index = args.rcx as u32;
            if index >= XUSER_MAX_COUNT || args.rdx == 0 {
                return Ok(ERROR_BAD_ARGUMENTS);
            }
            // XINPUT_VIBRATION is two consecutive WORDs, read as one DWORD so
            // a guest pointer at the top of the address space cannot overflow.
            let Ok(motors) = memory.read_u32(args.rdx) else {
                return Ok(ERROR_NOACCESS);
            };
            let (left, right) = (motors as u16, (motors >> 16) as u16);
            if set.controllers.read(index).is_none() {
                return Ok(ERROR_DEVICE_NOT_CONNECTED);
            }
            let mut xinput = set.xinput.lock().expect("xinput state");
            xinput.requested[index as usize] = (left, right);
            // A disabled XInput keeps the request until it is enabled again.
            if xinput.enabled && !set.controllers.vibrate(index, left, right) {
                return Ok(ERROR_DEVICE_NOT_CONNECTED);
            }
            Ok(ERROR_SUCCESS)
        },
    );

    let caps = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XInputGetCapabilities",
        ServiceStatus::Partial,
        move |args, _, memory| {
            let index = args.rcx as u32;
            let flags = args.rdx as u32;
            if index >= XUSER_MAX_COUNT || flags & !XINPUT_FLAG_GAMEPAD != 0 || args.r8 == 0 {
                return Ok(ERROR_BAD_ARGUMENTS);
            }
            if caps.controllers.read(index).is_none() {
                return Ok(ERROR_DEVICE_NOT_CONNECTED);
            }
            let rumble = caps.controllers.supports_vibration(index);
            // Each field reports the resolution the source provides: every
            // button, 8-bit triggers and 16-bit sticks.
            let resolution = Gamepad {
                buttons: ALL_BUTTONS,
                left_trigger: 0xFF,
                right_trigger: 0xFF,
                thumb_lx: -1,
                thumb_ly: -1,
                thumb_rx: -1,
                thumb_ry: -1,
            };
            let motors: u16 = if rumble { 0xFFFF } else { 0 };
            let mut bytes = [0; 20];
            bytes[0] = XINPUT_DEVTYPE_GAMEPAD;
            bytes[1] = XINPUT_DEVSUBTYPE_GAMEPAD;
            let flags = if rumble { XINPUT_CAPS_FFB_SUPPORTED } else { 0 };
            bytes[2..4].copy_from_slice(&flags.to_le_bytes());
            bytes[4..16].copy_from_slice(&resolution.bytes());
            bytes[16..18].copy_from_slice(&motors.to_le_bytes());
            bytes[18..20].copy_from_slice(&motors.to_le_bytes());
            if memory.write_bytes(args.r8, &bytes).is_err() {
                return Ok(ERROR_NOACCESS);
            }
            Ok(ERROR_SUCCESS)
        },
    );

    let enable = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XInputEnable",
        ServiceStatus::Partial,
        move |args, _, _| -> Result<u64, ExitReason> {
            let enabled = args.rcx as u32 != 0;
            let mut xinput = enable.xinput.lock().expect("xinput state");
            xinput.enabled = enabled;
            for index in 0..XUSER_MAX_COUNT {
                let (left, right) = if enabled {
                    xinput.requested[index as usize]
                } else {
                    (0, 0)
                };
                enable.controllers.vibrate(index, left, right);
            }
            Ok(0)
        },
    );

    for module in ALIASES {
        for symbol in ["XInputGetState", "XInputSetState", "XInputGetCapabilities"] {
            registry.alias(module, MODULE, symbol);
        }
    }
    registry.alias("xinput1_3.dll", MODULE, "XInputEnable");
}
