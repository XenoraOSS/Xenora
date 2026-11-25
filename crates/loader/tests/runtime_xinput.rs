// SPDX-License-Identifier: GPL-3.0-or-later

//! XInput through the guest import table, fed by simulated controllers. The
//! expected layouts and codes come from the public XInput reference pages for
//! XInputGetState, XInputSetState, XInputGetCapabilities, XInputEnable and the
//! XINPUT_STATE, XINPUT_GAMEPAD, XINPUT_VIBRATION and XINPUT_CAPABILITIES
//! structures. A simulated controller is not physical hardware.

mod support;

use std::sync::Arc;
use support::*;
use xenora_loader::services::xinput::{Gamepad, SimulatedControllers};
use xenora_loader::services::{MockClock, ServicesState};

const IMPORTS: &[(&str, &str)] = &[
    ("xinput1_4.dll", "XInputGetState"),
    ("xinput1_4.dll", "XInputSetState"),
    ("xinput1_4.dll", "XInputGetCapabilities"),
    ("xinput1_4.dll", "XInputEnable"),
    ("kernel32.dll", "GetLastError"),
];

const ERROR_SUCCESS: u64 = 0;
const ERROR_BAD_ARGUMENTS: u64 = 160;
const ERROR_NOACCESS: u64 = 998;
const ERROR_DEVICE_NOT_CONNECTED: u64 = 1167;
const STATE: u64 = SCRATCH;
const VIBRATION: u64 = SCRATCH + 0x40;
const CAPS: u64 = SCRATCH + 0x80;
const UNMAPPED: u64 = 0x10;

fn guest() -> (Guest, Arc<SimulatedControllers>) {
    let pads = Arc::new(SimulatedControllers::default());
    let mut state = ServicesState::new(Arc::new(MockClock::new(0)));
    state.set_controllers(pads.clone());
    (Guest::with_state(IMPORTS, &[], state), pads)
}

fn read_state(guest: &Guest) -> (u32, Gamepad) {
    let at = |offset| STATE + offset;
    let pad = Gamepad {
        buttons: guest.u16_at(at(4)),
        left_trigger: guest.memory.read_u8(at(6)).unwrap(),
        right_trigger: guest.memory.read_u8(at(7)).unwrap(),
        thumb_lx: guest.u16_at(at(8)) as i16,
        thumb_ly: guest.u16_at(at(10)) as i16,
        thumb_rx: guest.u16_at(at(12)) as i16,
        thumb_ry: guest.u16_at(at(14)) as i16,
    };
    (guest.u32_at(STATE), pad)
}

#[test]
fn get_state_reports_readings_and_advances_packets_only_on_change() {
    let (mut guest, pads) = guest();
    assert_eq!(
        guest.call("XInputGetState", &[0, STATE]),
        ERROR_DEVICE_NOT_CONNECTED
    );
    pads.connect(0, true);
    let pad = Gamepad {
        buttons: 0x1000 | 0x0001, // A and DPAD_UP
        left_trigger: 0x40,
        right_trigger: 0xFF,
        thumb_lx: -32768,
        thumb_ly: 32767,
        thumb_rx: -1,
        thumb_ry: 12345,
    };
    pads.set(0, pad);
    assert_eq!(guest.call("XInputGetState", &[0, STATE]), ERROR_SUCCESS);
    let (first, seen) = read_state(&guest);
    assert_eq!(seen, pad);
    assert_eq!(guest.call("XInputGetState", &[0, STATE]), ERROR_SUCCESS);
    assert_eq!(read_state(&guest).0, first, "unchanged reading");
    pads.set(0, Gamepad::default());
    assert_eq!(guest.call("XInputGetState", &[0, STATE]), ERROR_SUCCESS);
    let (second, seen) = read_state(&guest);
    assert_ne!(second, first);
    assert_eq!(seen, Gamepad::default());
    // Other slots stay disconnected.
    assert_eq!(
        guest.call("XInputGetState", &[1, STATE]),
        ERROR_DEVICE_NOT_CONNECTED
    );
    pads.disconnect(0);
    assert_eq!(
        guest.call("XInputGetState", &[0, STATE]),
        ERROR_DEVICE_NOT_CONNECTED
    );
}

#[test]
fn bad_arguments_and_unmapped_buffers_return_codes_without_last_error() {
    let (mut guest, pads) = guest();
    pads.connect(0, true);
    assert_eq!(
        guest.call("XInputGetState", &[4, STATE]),
        ERROR_BAD_ARGUMENTS
    );
    assert_eq!(guest.call("XInputGetState", &[0, 0]), ERROR_BAD_ARGUMENTS);
    assert_eq!(guest.call("XInputGetState", &[0, UNMAPPED]), ERROR_NOACCESS);
    assert_eq!(guest.call("XInputSetState", &[0, 0]), ERROR_BAD_ARGUMENTS);
    assert_eq!(guest.call("XInputSetState", &[0, UNMAPPED]), ERROR_NOACCESS);
    // The last address must not overflow while reading the second motor.
    assert_eq!(guest.call("XInputSetState", &[0, u64::MAX]), ERROR_NOACCESS);
    assert_eq!(
        guest.call("XInputGetCapabilities", &[0, 2, CAPS]),
        ERROR_BAD_ARGUMENTS
    );
    assert_eq!(
        guest.call("XInputGetCapabilities", &[0, 1, UNMAPPED]),
        ERROR_NOACCESS
    );
    assert_eq!(guest.error(), 0, "XInput leaves last-error alone");
}

#[test]
fn set_state_drives_motors_and_enable_pauses_them() {
    let (mut guest, pads) = guest();
    guest.memory.write_u16(VIBRATION, 0x1234).unwrap();
    guest.memory.write_u16(VIBRATION + 2, 0xABCD).unwrap();
    assert_eq!(
        guest.call("XInputSetState", &[2, VIBRATION]),
        ERROR_DEVICE_NOT_CONNECTED
    );
    pads.connect(2, true);
    assert_eq!(guest.call("XInputSetState", &[2, VIBRATION]), ERROR_SUCCESS);
    assert_eq!(pads.motors(2), Some((0x1234, 0xABCD)));

    pads.set(
        2,
        Gamepad {
            buttons: 0x2000,
            ..Gamepad::default()
        },
    );
    guest.call("XInputEnable", &[0]);
    assert_eq!(pads.motors(2), Some((0, 0)), "disabling stops the motors");
    assert_eq!(guest.call("XInputGetState", &[2, STATE]), ERROR_SUCCESS);
    assert_eq!(
        read_state(&guest).1,
        Gamepad::default(),
        "neutral while disabled"
    );
    guest.memory.write_u16(VIBRATION, 0x0101).unwrap();
    assert_eq!(guest.call("XInputSetState", &[2, VIBRATION]), ERROR_SUCCESS);
    assert_eq!(pads.motors(2), Some((0, 0)), "held while disabled");
    guest.call("XInputEnable", &[1]);
    assert_eq!(pads.motors(2), Some((0x0101, 0xABCD)), "resumed on enable");
    assert_eq!(guest.call("XInputGetState", &[2, STATE]), ERROR_SUCCESS);
    assert_eq!(read_state(&guest).1.buttons, 0x2000);
}

#[test]
fn capabilities_describe_a_gamepad_and_whether_it_vibrates() {
    let (mut guest, pads) = guest();
    assert_eq!(
        guest.call("XInputGetCapabilities", &[3, 0, CAPS]),
        ERROR_DEVICE_NOT_CONNECTED
    );
    pads.connect(3, true);
    pads.connect(1, false);
    for (slot, motors, flags) in [(3, 0xFFFF, 1), (1, 0, 0)] {
        assert_eq!(
            guest.call("XInputGetCapabilities", &[slot, 1, CAPS]),
            ERROR_SUCCESS
        );
        assert_eq!(
            guest.memory.read_u8(CAPS).unwrap(),
            1,
            "XINPUT_DEVTYPE_GAMEPAD"
        );
        assert_eq!(
            guest.memory.read_u8(CAPS + 1).unwrap(),
            1,
            "GAMEPAD subtype"
        );
        assert_eq!(guest.u16_at(CAPS + 2), flags, "XINPUT_CAPS_FFB_SUPPORTED");
        assert_eq!(guest.u16_at(CAPS + 4), 0xF3FF, "every defined button");
        assert_eq!(guest.memory.read_u8(CAPS + 6).unwrap(), 0xFF);
        assert_eq!(guest.memory.read_u8(CAPS + 7).unwrap(), 0xFF);
        for axis in 0..4 {
            assert_eq!(guest.u16_at(CAPS + 8 + 2 * axis), 0xFFFF);
        }
        assert_eq!(guest.u16_at(CAPS + 16), motors);
        assert_eq!(guest.u16_at(CAPS + 18), motors);
    }
}

#[test]
fn without_a_source_no_controller_is_connected() {
    let mut guest = Guest::new(IMPORTS, &[]);
    for slot in 0..4 {
        assert_eq!(
            guest.call("XInputGetState", &[slot, STATE]),
            ERROR_DEVICE_NOT_CONNECTED
        );
    }
}

#[test]
fn older_xinput_dll_names_reach_the_same_service() {
    for module in ["xinput1_3.dll", "xinput9_1_0.dll"] {
        let imports: &'static [(&str, &str)] =
            Box::leak(vec![(module, "XInputGetState")].into_boxed_slice());
        let pads = Arc::new(SimulatedControllers::default());
        pads.connect(0, false);
        pads.set(
            0,
            Gamepad {
                buttons: 0x8000,
                ..Gamepad::default()
            },
        );
        let mut state = ServicesState::new(Arc::new(MockClock::new(0)));
        state.set_controllers(pads.clone());
        let mut guest = Guest::with_state(imports, &[], state);
        assert_eq!(
            guest.call("XInputGetState", &[0, STATE]),
            ERROR_SUCCESS,
            "{module}"
        );
        assert_eq!(guest.u16_at(STATE + 4), 0x8000, "{module}");
    }
}
