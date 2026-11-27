// SPDX-License-Identifier: GPL-3.0-or-later

//! A bounded XInput controller-state surface.
//!
//! Hosts may supply explicit controller snapshots to [`ServicesState`]. This
//! module only serializes those snapshots in the public XInput layout; it does
//! not detect physical controllers or hotplugging by itself.

use super::ServicesState;
use crate::import_table::ImportName;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::GuestMemory;
use std::sync::{Arc, Mutex};

const ERROR_BAD_ARGUMENTS: u32 = 160;
const ERROR_DEVICE_NOT_CONNECTED: u32 = 1167;
const ERROR_EMPTY: u32 = 4306;
const XUSER_MAX_COUNT: u32 = 4;
const XUSER_INDEX_ANY: u32 = 0xff;
const XINPUT_STATE_BYTES: usize = 16;
const XINPUT_KEYSTROKE_BYTES: usize = 8;
const XINPUT_CAPABILITIES_BYTES: usize = 20;
const XINPUT_BATTERY_INFORMATION_BYTES: usize = 2;
const GUID_BYTES: usize = 16;
const XINPUT_FLAG_GAMEPAD: u32 = 0x0001;
const XINPUT_GAMEPAD_BUTTON_MASK: u16 = 0xf3ff;
const XINPUT_GAMEPAD_GUIDE: u16 = 0x0400;
const XINPUT_CAPS_FFB_SUPPORTED: u16 = 0x0001;
const XINPUT_DEVTYPE_GAMEPAD: u8 = 0x01;
const XINPUT_DEVSUBTYPE_GAMEPAD: u8 = 0x01;
const BATTERY_DEVTYPE_GAMEPAD: u32 = 0;
const BATTERY_DEVTYPE_HEADSET: u32 = 1;
const BATTERY_TYPE_DISCONNECTED: u8 = 0;
const BATTERY_TYPE_UNKNOWN: u8 = 0xff;
const BATTERY_LEVEL_EMPTY: u8 = 0;
const BATTERY_LEVEL_LOW: u8 = 1;
const BATTERY_LEVEL_MEDIUM: u8 = 2;
const BATTERY_LEVEL_FULL: u8 = 3;
const MODULES: [&str; 4] = [
    "xinput1_3.dll",
    "xinput1_4.dll",
    "xinput9_1_0.dll",
    "xinputuap.dll",
];

/// One host-supplied controller snapshot in documented XInput units.
///
/// `buttons` uses the public XInput bit assignments, triggers are 0 through
/// 255 and thumb axes are signed values from -32768 through 32767. No deadzone
/// processing is applied here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XInputControllerState {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
}

/// Host evidence about a controller battery. Apple GameController exposes no
/// battery chemistry, so this state is intentionally charge evidence rather
/// than an XInput battery type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum XInputBatteryChargeState {
    #[default]
    Unknown,
    Discharging,
    Charging,
    Full,
}

/// Battery metadata supplied by the host for one controller. `level_percent`
/// is `None` when the host exposes a battery object but no trustworthy level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XInputBatteryState {
    pub charge_state: XInputBatteryChargeState,
    pub level_percent: Option<u8>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Slot {
    connected: bool,
    packet_number: u32,
    state: XInputControllerState,
    vibration_sequence: u32,
    vibration_supported: bool,
    battery: Option<XInputBatteryState>,
    left_motor: u16,
    right_motor: u16,
}

pub(crate) type Slots = [Slot; 4];

impl Slot {
    fn set(
        &mut self,
        next: Option<XInputControllerState>,
        vibration_supported: bool,
        battery: Option<XInputBatteryState>,
    ) {
        match next {
            None => {
                self.connected = false;
                self.vibration_supported = false;
                self.left_motor = 0;
                self.right_motor = 0;
                self.vibration_sequence = 0;
                self.battery = None;
            }
            Some(state) => {
                if !self.connected || self.state != state {
                    self.packet_number = self.packet_number.wrapping_add(1);
                    self.state = state;
                }
                self.connected = true;
                self.vibration_supported = vibration_supported;
                self.battery = battery;
            }
        }
    }

    fn get(&self) -> Option<(u32, XInputControllerState)> {
        self.connected.then_some((self.packet_number, self.state))
    }
}

impl ServicesState {
    /// Read the latest vibration command accepted for a slot. The sequence
    /// increments for every accepted command, including repeated motor values.
    pub fn xinput_vibration(&self, index: u32) -> Option<(u32, u16, u16, bool)> {
        if index >= XUSER_MAX_COUNT {
            return None;
        }
        let slot = &self.xinput.lock().unwrap()[index as usize];
        slot.connected.then_some((
            slot.vibration_sequence,
            slot.left_motor,
            slot.right_motor,
            self.xinput_input_enabled(),
        ))
    }

    /// Replace a user slot's explicit controller snapshot. `None` disconnects
    /// the slot. Returns false for an index outside the documented 0..4 range.
    /// Physical controller discovery is the host's responsibility.
    pub fn set_xinput_controller(&self, index: u32, state: Option<XInputControllerState>) -> bool {
        self.set_xinput_controller_with_vibration(index, state, false)
    }

    /// Replace a user slot and atomically update whether the host reports a
    /// vibration output path for that controller. The support bit is host
    /// capability metadata, not evidence that a physical pulse was observed.
    pub fn set_xinput_controller_with_vibration(
        &self,
        index: u32,
        state: Option<XInputControllerState>,
        vibration_supported: bool,
    ) -> bool {
        if index >= XUSER_MAX_COUNT {
            return false;
        }
        self.set_xinput_controller_with_details(index, state, vibration_supported, None)
    }

    /// Replace a user slot and atomically update vibration capability plus
    /// battery metadata under the same slot lock. `None` battery metadata means
    /// the host exposed no battery object; an object with unknown state or
    /// level remains explicit inside `Some`.
    pub fn set_xinput_controller_with_details(
        &self,
        index: u32,
        state: Option<XInputControllerState>,
        vibration_supported: bool,
        battery: Option<XInputBatteryState>,
    ) -> bool {
        if index >= XUSER_MAX_COUNT {
            return false;
        }
        self.xinput.lock().unwrap()[index as usize].set(state, vibration_supported, battery);
        true
    }
}

fn serialize(packet: u32, state: XInputControllerState) -> [u8; XINPUT_STATE_BYTES] {
    let mut bytes = [0; XINPUT_STATE_BYTES];
    bytes[0..4].copy_from_slice(&packet.to_le_bytes());
    bytes[4..6].copy_from_slice(&state.buttons.to_le_bytes());
    bytes[6] = state.left_trigger;
    bytes[7] = state.right_trigger;
    for (place, value) in bytes[8..16].as_chunks_mut::<2>().0.iter_mut().zip([
        state.left_x,
        state.left_y,
        state.right_x,
        state.right_y,
    ]) {
        *place = value.to_le_bytes();
    }
    bytes
}

/// Serialize the capabilities of Xenora's virtual XInput gamepad surface. The
/// surface accepts every documented standard input field at full resolution.
/// Vibration capability bits and motor resolution are reported only when the
/// host explicitly reports a haptic output path for the slot.
fn serialize_virtual_capabilities(vibration_supported: bool) -> [u8; XINPUT_CAPABILITIES_BYTES] {
    let mut bytes = [0; XINPUT_CAPABILITIES_BYTES];
    bytes[0] = XINPUT_DEVTYPE_GAMEPAD;
    bytes[1] = XINPUT_DEVSUBTYPE_GAMEPAD;
    if vibration_supported {
        bytes[2..4].copy_from_slice(&XINPUT_CAPS_FFB_SUPPORTED.to_le_bytes());
        bytes[16..18].copy_from_slice(&u16::MAX.to_le_bytes());
        bytes[18..20].copy_from_slice(&u16::MAX.to_le_bytes());
    }
    bytes[4..6].copy_from_slice(&XINPUT_GAMEPAD_BUTTON_MASK.to_le_bytes());
    bytes[6] = u8::MAX;
    bytes[7] = u8::MAX;
    for place in bytes[8..16].as_chunks_mut::<2>().0.iter_mut() {
        *place = i16::MAX.to_le_bytes();
    }
    bytes
}

fn get_state(
    slots: &Mutex<Slots>,
    enabled: bool,
    include_guide: bool,
    index: u32,
    output: u64,
    memory: &mut GuestMemory,
) -> u32 {
    if index >= XUSER_MAX_COUNT || output == 0 || !memory.can_write(output, XINPUT_STATE_BYTES) {
        return ERROR_BAD_ARGUMENTS;
    }
    let Some((packet, state)) = slots.lock().unwrap()[index as usize].get() else {
        return ERROR_DEVICE_NOT_CONNECTED;
    };
    // Microsoft documents neutral gamepad data while disabled but does not
    // define the packet number. Xenora chooses zero because the visible state
    // is constant in that mode; this boundary is documented, not Windows
    // comparison evidence.
    let (packet, mut state) = if enabled {
        (packet, state)
    } else {
        (0, XInputControllerState::default())
    };
    if !include_guide {
        state.buttons &= !XINPUT_GAMEPAD_GUIDE;
    }
    if memory
        .write_bytes(output, &serialize(packet, state))
        .is_err()
    {
        return ERROR_BAD_ARGUMENTS;
    }
    0
}

fn set_state(slots: &Mutex<Slots>, index: u32, input: u64, memory: &mut GuestMemory) -> u32 {
    if index >= XUSER_MAX_COUNT || input == 0 {
        return ERROR_BAD_ARGUMENTS;
    }
    let mut slots = slots.lock().unwrap();
    let slot = &mut slots[index as usize];
    if !slot.connected {
        return ERROR_DEVICE_NOT_CONNECTED;
    }
    let mut bytes = [0; 4];
    if memory.read_bytes(input, &mut bytes).is_err() {
        return ERROR_BAD_ARGUMENTS;
    }
    slot.left_motor = u16::from_le_bytes([bytes[0], bytes[1]]);
    slot.right_motor = u16::from_le_bytes([bytes[2], bytes[3]]);
    slot.vibration_sequence = slot.vibration_sequence.wrapping_add(1);
    0
}

fn get_keystroke(
    slots: &Mutex<Slots>,
    index: u32,
    reserved: u32,
    output: u64,
    memory: &mut GuestMemory,
) -> u32 {
    if (index >= XUSER_MAX_COUNT && index != XUSER_INDEX_ANY)
        || reserved != 0
        || output == 0
        || !memory.can_write(output, XINPUT_KEYSTROKE_BYTES)
    {
        return ERROR_BAD_ARGUMENTS;
    }
    let slots = slots.lock().unwrap();
    let connected = if index == XUSER_INDEX_ANY {
        slots.iter().any(|slot| slot.connected)
    } else {
        slots[index as usize].connected
    };
    if !connected {
        return ERROR_DEVICE_NOT_CONNECTED;
    }
    // Xenora has no host keystroke-event source yet. Returning the documented
    // empty result is honest; fabricating a neutral key event would not be.
    ERROR_EMPTY
}

fn audio_id_plan(
    buffer: u64,
    count_pointer: u64,
    memory: &GuestMemory,
) -> Result<Option<(u64, u64, bool)>, ()> {
    if count_pointer == 0 {
        return Ok(None);
    }
    if !memory.can_write(count_pointer, 4) {
        return Err(());
    }
    let count = memory.read_u32(count_pointer).map_err(|_| ())?;
    if count != 0 && (buffer == 0 || !memory.can_write(buffer, 2)) {
        return Err(());
    }
    Ok(Some((count_pointer, buffer, count != 0)))
}

fn get_audio_device_ids(
    slots: &Mutex<Slots>,
    index: u32,
    render_buffer: u64,
    render_count: u64,
    capture_buffer: u64,
    capture_count: u64,
    memory: &mut GuestMemory,
) -> u32 {
    if index >= XUSER_MAX_COUNT {
        return ERROR_BAD_ARGUMENTS;
    }
    let Ok(render) = audio_id_plan(render_buffer, render_count, memory) else {
        return ERROR_BAD_ARGUMENTS;
    };
    let Ok(capture) = audio_id_plan(capture_buffer, capture_count, memory) else {
        return ERROR_BAD_ARGUMENTS;
    };
    if !slots.lock().unwrap()[index as usize].connected {
        return ERROR_DEVICE_NOT_CONNECTED;
    }
    // Xenora has no headset endpoint. The documented empty result is an empty
    // string when the caller supplied capacity and a zero count. Supplying no
    // count pointer leaves the corresponding optional buffer untouched.
    for plan in [render, capture].into_iter().flatten() {
        let (count_pointer, buffer, write_empty_string) = plan;
        if write_empty_string && memory.write_u16(buffer, 0).is_err() {
            return ERROR_BAD_ARGUMENTS;
        }
        if memory.write_u32(count_pointer, 0).is_err() {
            return ERROR_BAD_ARGUMENTS;
        }
    }
    0
}

fn get_dsound_audio_device_guids(
    slots: &Mutex<Slots>,
    index: u32,
    render_output: u64,
    capture_output: u64,
    memory: &mut GuestMemory,
) -> u32 {
    if index >= XUSER_MAX_COUNT
        || render_output == 0
        || capture_output == 0
        || !memory.can_write(render_output, GUID_BYTES)
        || !memory.can_write(capture_output, GUID_BYTES)
    {
        return ERROR_BAD_ARGUMENTS;
    }
    if !slots.lock().unwrap()[index as usize].connected {
        return ERROR_DEVICE_NOT_CONNECTED;
    }
    // Xenora does not expose an XInput headset endpoint. GUID_NULL is the
    // documented no-headset result and is deliberately not a fabricated audio
    // device identifier.
    if memory.write_bytes(render_output, &[0; GUID_BYTES]).is_err()
        || memory
            .write_bytes(capture_output, &[0; GUID_BYTES])
            .is_err()
    {
        return ERROR_BAD_ARGUMENTS;
    }
    0
}

fn get_battery_information(
    slots: &Mutex<Slots>,
    index: u32,
    device_type: u32,
    output: u64,
    memory: &mut GuestMemory,
) -> u32 {
    if index >= XUSER_MAX_COUNT
        || device_type > BATTERY_DEVTYPE_HEADSET
        || output == 0
        || !memory.can_write(output, XINPUT_BATTERY_INFORMATION_BYTES)
    {
        return ERROR_BAD_ARGUMENTS;
    }
    let slot = slots.lock().unwrap()[index as usize];
    let connected = slot.connected;
    // Xenora does not expose an XInput headset and GameController exposes no
    // battery chemistry, so UNKNOWN remains the only honest battery type even
    // when charge evidence exists. Microsoft says the level is valid only for
    // known wireless battery types. Xenora nevertheless publishes this coarse
    // level beside UNKNOWN as an explicit compatibility policy: it is real host
    // evidence, not a claim about Windows behavior or battery chemistry. When
    // the host has no level, EMPTY means "no valid level", not a drained pack.
    let battery_type = if device_type == BATTERY_DEVTYPE_GAMEPAD && connected {
        BATTERY_TYPE_UNKNOWN
    } else {
        BATTERY_TYPE_DISCONNECTED
    };
    let level = if device_type == BATTERY_DEVTYPE_GAMEPAD && connected {
        match slot.battery.and_then(|battery| battery.level_percent) {
            None => BATTERY_LEVEL_EMPTY,
            Some(0..=10) => BATTERY_LEVEL_EMPTY,
            Some(11..=40) => BATTERY_LEVEL_LOW,
            Some(41..=70) => BATTERY_LEVEL_MEDIUM,
            Some(71..=100) => BATTERY_LEVEL_FULL,
            Some(_) => BATTERY_LEVEL_EMPTY,
        }
    } else {
        BATTERY_LEVEL_EMPTY
    };
    if memory.write_bytes(output, &[battery_type, level]).is_err() {
        return ERROR_BAD_ARGUMENTS;
    }
    0
}

fn get_capabilities(
    slots: &Mutex<Slots>,
    index: u32,
    flags: u32,
    output: u64,
    memory: &mut GuestMemory,
) -> u32 {
    if index >= XUSER_MAX_COUNT
        || flags & !XINPUT_FLAG_GAMEPAD != 0
        || output == 0
        || !memory.can_write(output, XINPUT_CAPABILITIES_BYTES)
    {
        return ERROR_BAD_ARGUMENTS;
    }
    let vibration_supported = {
        let slots = slots.lock().unwrap();
        let slot = &slots[index as usize];
        if slot.get().is_none() {
            return ERROR_DEVICE_NOT_CONNECTED;
        }
        slot.vibration_supported
    };
    if memory
        .write_bytes(output, &serialize_virtual_capabilities(vibration_supported))
        .is_err()
    {
        return ERROR_BAD_ARGUMENTS;
    }
    0
}

/// Register the state, capabilities, vibration, keystroke, enable, battery
/// and legacy audio-guid boundaries on the public XInput library names.
/// `XInputGetKeystroke`, `XInputEnable` and `XInputGetBatteryInformation` are
/// intentionally absent from the reduced xinput9_1_0 export set, while
/// `XInputGetDSoundAudioDeviceGuids` is present only on the legacy surfaces.
/// Actual audio endpoints, ordinal-only xinput1_3 functions and
/// physical hotplug discovery remain separate unsupported surfaces.
pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    for module in MODULES {
        let slots = Arc::clone(state);
        registry.register_with_status(
            module,
            "XInputGetState",
            ServiceStatus::Partial,
            move |args, _, memory| {
                Ok(u64::from(get_state(
                    &slots.xinput,
                    slots.xinput_input_enabled(),
                    false,
                    args.rcx as u32,
                    args.rdx,
                    memory,
                )))
            },
        );
        let slots = Arc::clone(state);
        registry.register_with_status(
            module,
            "XInputGetCapabilities",
            ServiceStatus::Partial,
            move |args, _, memory| {
                Ok(u64::from(get_capabilities(
                    &slots.xinput,
                    args.rcx as u32,
                    args.rdx as u32,
                    args.r8,
                    memory,
                )))
            },
        );
        let slots = Arc::clone(state);
        registry.register_with_status(
            module,
            "XInputSetState",
            ServiceStatus::Partial,
            move |args, _, memory| {
                Ok(u64::from(set_state(
                    &slots.xinput,
                    args.rcx as u32,
                    args.rdx,
                    memory,
                )))
            },
        );
        if module == "xinput1_3.dll" || module == "xinput1_4.dll" {
            let slots = Arc::clone(state);
            registry
                .try_register(
                    module,
                    ImportName::Ordinal(100),
                    ServiceStatus::Partial,
                    Box::new(move |args, _, memory| {
                        Ok(u64::from(get_state(
                            &slots.xinput,
                            slots.xinput_input_enabled(),
                            true,
                            args.rcx as u32,
                            args.rdx,
                            memory,
                        )))
                    }),
                )
                .expect("valid XInput ordinal 100 registration");
        }
        if module == "xinput1_3.dll" || module == "xinput9_1_0.dll" {
            let slots = Arc::clone(state);
            registry.register_with_status(
                module,
                "XInputGetDSoundAudioDeviceGuids",
                ServiceStatus::Partial,
                move |args, _, memory| {
                    Ok(u64::from(get_dsound_audio_device_guids(
                        &slots.xinput,
                        args.rcx as u32,
                        args.rdx,
                        args.r8,
                        memory,
                    )))
                },
            );
        }
        if module != "xinput9_1_0.dll" {
            let slots = Arc::clone(state);
            registry.register_with_status(
                module,
                "XInputEnable",
                ServiceStatus::Partial,
                move |args, _, _| {
                    slots.set_xinput_input_enabled(args.rcx != 0);
                    Ok(0)
                },
            );
            if module != "xinput1_3.dll" {
                let slots = Arc::clone(state);
                registry.register_with_status(
                    module,
                    "XInputGetAudioDeviceIds",
                    ServiceStatus::Partial,
                    move |args, _, memory| {
                        let capture_count = args
                            .get(4, memory)
                            .map_err(crate::cpu::ExitReason::MemoryFault)?;
                        Ok(u64::from(get_audio_device_ids(
                            &slots.xinput,
                            args.rcx as u32,
                            args.rdx,
                            args.r8,
                            args.r9,
                            capture_count,
                            memory,
                        )))
                    },
                );
            }
            let slots = Arc::clone(state);
            registry.register_with_status(
                module,
                "XInputGetBatteryInformation",
                ServiceStatus::Partial,
                move |args, _, memory| {
                    Ok(u64::from(get_battery_information(
                        &slots.xinput,
                        args.rcx as u32,
                        args.rdx as u32,
                        args.r8,
                        memory,
                    )))
                },
            );
            let slots = Arc::clone(state);
            registry.register_with_status(
                module,
                "XInputGetKeystroke",
                ServiceStatus::Partial,
                move |args, _, memory| {
                    Ok(u64::from(get_keystroke(
                        &slots.xinput,
                        args.rcx as u32,
                        args.rdx as u32,
                        args.r8,
                        memory,
                    )))
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::CpuState;
    use crate::import_table::ImportName;
    use crate::imports::Win64Args;
    use crate::memory::MemoryPermissions;
    use crate::services::MockClock;

    fn fixture() -> (Arc<ServicesState>, ImportRegistry) {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register(&mut registry, &state);
        (state, registry)
    }

    fn call(
        registry: &ImportRegistry,
        memory: &mut GuestMemory,
        module: &str,
        index: u64,
        output: u64,
    ) -> u64 {
        let mut cpu = CpuState::default();
        cpu.set(crate::cpu::Register::Rcx, index);
        cpu.set(crate::cpu::Register::Rdx, output);
        let descriptor = registry
            .resolve(module, &ImportName::Name("XInputGetState".to_owned()))
            .unwrap();
        (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap()
    }

    fn writable_memory() -> GuestMemory {
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory
    }

    fn call_set_state(
        registry: &ImportRegistry,
        memory: &mut GuestMemory,
        index: u64,
        input: u64,
    ) -> u64 {
        let mut cpu = CpuState::default();
        cpu.set(crate::cpu::Register::Rcx, index);
        cpu.set(crate::cpu::Register::Rdx, input);
        let descriptor = registry
            .resolve(
                "xinput1_4.dll",
                &ImportName::Name("XInputSetState".to_owned()),
            )
            .unwrap();
        (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap()
    }

    fn call_capabilities(
        registry: &ImportRegistry,
        memory: &mut GuestMemory,
        index: u64,
        flags: u64,
        output: u64,
    ) -> u64 {
        let mut cpu = CpuState::default();
        cpu.set(crate::cpu::Register::Rcx, index);
        cpu.set(crate::cpu::Register::Rdx, flags);
        cpu.set(crate::cpu::Register::R8, output);
        let descriptor = registry
            .resolve(
                "xinput1_4.dll",
                &ImportName::Name("XInputGetCapabilities".to_owned()),
            )
            .unwrap();
        (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap()
    }

    fn call_keystroke(
        registry: &ImportRegistry,
        memory: &mut GuestMemory,
        module: &str,
        index: u64,
        reserved: u64,
        output: u64,
    ) -> Option<u64> {
        let mut cpu = CpuState::default();
        cpu.set(crate::cpu::Register::Rcx, index);
        cpu.set(crate::cpu::Register::Rdx, reserved);
        cpu.set(crate::cpu::Register::R8, output);
        let descriptor =
            registry.resolve(module, &ImportName::Name("XInputGetKeystroke".to_owned()))?;
        Some((descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap())
    }

    fn call_state_ex(
        registry: &ImportRegistry,
        memory: &mut GuestMemory,
        module: &str,
        index: u64,
        output: u64,
    ) -> Option<u64> {
        let mut cpu = CpuState::default();
        cpu.set(crate::cpu::Register::Rcx, index);
        cpu.set(crate::cpu::Register::Rdx, output);
        let descriptor = registry.resolve(module, &ImportName::Ordinal(100))?;
        Some((descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap())
    }

    #[test]
    fn ordinal_100_state_exposes_guide_bit_while_named_state_masks_it() {
        let (state, registry) = fixture();
        let mut memory = writable_memory();
        for module in MODULES {
            assert!(
                call_state_ex(&registry, &mut memory, module, 0, 0x1200).is_none()
                    == (module == "xinput9_1_0.dll" || module == "xinputuap.dll"),
                "module={module}"
            );
        }
        assert!(state.set_xinput_controller(
            0,
            Some(XInputControllerState {
                buttons: 0x1000 | XINPUT_GAMEPAD_GUIDE,
                ..XInputControllerState::default()
            }),
        ));
        assert_eq!(call(&registry, &mut memory, "xinput1_4.dll", 0, 0x1200), 0);
        assert_eq!(memory.read_u16(0x1204).unwrap(), 0x1000);
        for module in ["xinput1_3.dll", "xinput1_4.dll"] {
            assert_eq!(
                call_state_ex(&registry, &mut memory, module, 0, 0x1200),
                Some(0)
            );
            assert_eq!(memory.read_u16(0x1204).unwrap(), 0x1400);
        }
        assert_eq!(
            call_state_ex(&registry, &mut memory, "xinput1_4.dll", 4, 0x1200),
            Some(u64::from(ERROR_BAD_ARGUMENTS))
        );
        assert_eq!(
            call_state_ex(&registry, &mut memory, "xinput1_4.dll", 0, 0x1ff1),
            Some(u64::from(ERROR_BAD_ARGUMENTS))
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn call_audio_ids(
        registry: &ImportRegistry,
        memory: &mut GuestMemory,
        module: &str,
        index: u64,
        render_buffer: u64,
        render_count: u64,
        capture_buffer: u64,
        capture_count: u64,
    ) -> Option<u64> {
        let mut cpu = CpuState::default();
        cpu.set(crate::cpu::Register::Rcx, index);
        cpu.set(crate::cpu::Register::Rdx, render_buffer);
        cpu.set(crate::cpu::Register::R8, render_count);
        cpu.set(crate::cpu::Register::R9, capture_buffer);
        cpu.set(crate::cpu::Register::Rsp, 0x3000);
        memory.write_u64(0x3028, capture_count).unwrap();
        let descriptor = registry.resolve(
            module,
            &ImportName::Name("XInputGetAudioDeviceIds".to_owned()),
        )?;
        Some((descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap())
    }

    #[test]
    fn audio_device_ids_report_no_headset_without_fabricating_endpoints() {
        let (state, registry) = fixture();
        let mut memory = writable_memory();
        memory
            .map_region(0x3000, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();
        for module in ["xinput1_3.dll", "xinput9_1_0.dll"] {
            assert!(call_audio_ids(&registry, &mut memory, module, 0, 0, 0, 0, 0).is_none());
        }
        for module in ["xinput1_4.dll", "xinputuap.dll"] {
            assert_eq!(
                call_audio_ids(&registry, &mut memory, module, 0, 0, 0, 0, 0),
                Some(u64::from(ERROR_DEVICE_NOT_CONNECTED))
            );
        }
        assert!(state.set_xinput_controller(0, Some(XInputControllerState::default())));
        memory.write_u32(0x1200, 8).unwrap();
        memory.write_u32(0x1204, 8).unwrap();
        memory.write_bytes(0x1300, &[0xaa; 32]).unwrap();
        assert_eq!(
            call_audio_ids(
                &registry,
                &mut memory,
                "xinput1_4.dll",
                0,
                0x1300,
                0x1200,
                0x1310,
                0x1204
            ),
            Some(0)
        );
        assert_eq!(memory.read_u16(0x1300).unwrap(), 0);
        assert_eq!(memory.read_u16(0x1310).unwrap(), 0);
        assert_eq!(memory.read_u32(0x1200).unwrap(), 0);
        assert_eq!(memory.read_u32(0x1204).unwrap(), 0);

        memory.write_bytes(0x1300, &[0xaa; 32]).unwrap();
        assert_eq!(
            call_audio_ids(&registry, &mut memory, "xinputuap.dll", 0, 0x1300, 0, 0, 0),
            Some(0)
        );
        let mut untouched = [0; 32];
        memory.read_bytes(0x1300, &mut untouched).unwrap();
        assert_eq!(untouched, [0xaa; 32]);

        memory.write_u32(0x1200, 1).unwrap();
        memory.write_u32(0x1204, 1).unwrap();
        memory.write_bytes(0x1300, &[0xaa; 32]).unwrap();
        for (index, render_buffer, render_count, capture_buffer, capture_count) in [
            (4, 0x1300, 0x1200, 0x1310, 0x1204),
            (0, 0, 0x1200, 0x1310, 0x1204),
            (0, 0x1fff, 0x1200, 0x1310, 0x1204),
            (0, 0x1300, 0x1fff, 0x1310, 0x1204),
            (0, 0x1300, 0x1200, 0, 0x1204),
            (0, 0x1300, 0x1200, 0x1310, 0x5000),
        ] {
            assert_eq!(
                call_audio_ids(
                    &registry,
                    &mut memory,
                    "xinput1_4.dll",
                    index,
                    render_buffer,
                    render_count,
                    capture_buffer,
                    capture_count
                ),
                Some(u64::from(ERROR_BAD_ARGUMENTS)),
                "index={index} render_buffer={render_buffer:#x} render_count={render_count:#x} capture_buffer={capture_buffer:#x} capture_count={capture_count:#x}"
            );
        }
        memory.read_bytes(0x1300, &mut untouched).unwrap();
        assert_eq!(untouched, [0xaa; 32]);
        assert_eq!(memory.read_u32(0x1200).unwrap(), 1);
        assert_eq!(memory.read_u32(0x1204).unwrap(), 1);
    }

    fn call_dsound_guids(
        registry: &ImportRegistry,
        memory: &mut GuestMemory,
        module: &str,
        index: u64,
        render_output: u64,
        capture_output: u64,
    ) -> Option<u64> {
        let mut cpu = CpuState::default();
        cpu.set(crate::cpu::Register::Rcx, index);
        cpu.set(crate::cpu::Register::Rdx, render_output);
        cpu.set(crate::cpu::Register::R8, capture_output);
        let descriptor = registry.resolve(
            module,
            &ImportName::Name("XInputGetDSoundAudioDeviceGuids".to_owned()),
        )?;
        Some((descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap())
    }

    #[test]
    fn legacy_dsound_audio_guids_report_no_headset_without_fabricating_devices() {
        let (state, registry) = fixture();
        let mut memory = writable_memory();
        for module in ["xinput1_4.dll", "xinputuap.dll"] {
            assert!(call_dsound_guids(&registry, &mut memory, module, 0, 0x1fc0, 0x1fd0).is_none());
        }
        for module in ["xinput1_3.dll", "xinput9_1_0.dll"] {
            assert_eq!(
                call_dsound_guids(&registry, &mut memory, module, 0, 0x1fc0, 0x1fd0),
                Some(u64::from(ERROR_DEVICE_NOT_CONNECTED))
            );
        }
        assert!(state.set_xinput_controller(2, Some(XInputControllerState::default())));
        assert_eq!(
            call_dsound_guids(&registry, &mut memory, "xinput1_3.dll", 2, 0x1fc0, 0x1fd0),
            Some(0)
        );
        let mut output = [0xaa; 32];
        memory.read_bytes(0x1fc0, &mut output).unwrap();
        assert_eq!(output, [0; 32]);

        memory.write_bytes(0x1fc0, &[0xaa; 32]).unwrap();
        for (index, render, capture) in [
            (4, 0x1fc0, 0x1fd0),
            (2, 0, 0x1fd0),
            (2, 0x1fc0, 0),
            (2, 0x1ff1, 0x1f00),
            (2, 0x1f00, 0x1ff1),
            (2, 0x5000, 0x1fd0),
        ] {
            assert_eq!(
                call_dsound_guids(
                    &registry,
                    &mut memory,
                    "xinput1_3.dll",
                    index,
                    render,
                    capture
                ),
                Some(u64::from(ERROR_BAD_ARGUMENTS)),
                "index={index} render={render:#x} capture={capture:#x}"
            );
        }
        memory.read_bytes(0x1fc0, &mut output).unwrap();
        assert_eq!(output, [0xaa; 32]);
    }

    fn call_battery(
        registry: &ImportRegistry,
        memory: &mut GuestMemory,
        module: &str,
        index: u64,
        device_type: u64,
        output: u64,
    ) -> Option<u64> {
        let mut cpu = CpuState::default();
        cpu.set(crate::cpu::Register::Rcx, index);
        cpu.set(crate::cpu::Register::Rdx, device_type);
        cpu.set(crate::cpu::Register::R8, output);
        let descriptor = registry.resolve(
            module,
            &ImportName::Name("XInputGetBatteryInformation".to_owned()),
        )?;
        Some((descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap())
    }

    #[test]
    fn battery_information_never_invents_power_metadata() {
        let (state, registry) = fixture();
        let mut memory = writable_memory();
        memory.write_bytes(0x1ffe, &[0xaa; 2]).unwrap();
        assert!(call_battery(&registry, &mut memory, "xinput9_1_0.dll", 0, 0, 0x1ffe).is_none());
        for module in ["xinput1_4.dll", "xinputuap.dll"] {
            assert_eq!(
                call_battery(&registry, &mut memory, module, 0, 0, 0x1ffe),
                Some(0)
            );
            let mut output = [0; 2];
            memory.read_bytes(0x1ffe, &mut output).unwrap();
            assert_eq!(output, [BATTERY_TYPE_DISCONNECTED, BATTERY_LEVEL_EMPTY]);
        }

        assert!(state.set_xinput_controller(1, Some(XInputControllerState::default())));
        assert_eq!(
            call_battery(
                &registry,
                &mut memory,
                "xinput1_4.dll",
                1,
                BATTERY_DEVTYPE_GAMEPAD.into(),
                0x1ffe
            ),
            Some(0)
        );
        let mut output = [0; 2];
        memory.read_bytes(0x1ffe, &mut output).unwrap();
        assert_eq!(output, [BATTERY_TYPE_UNKNOWN, BATTERY_LEVEL_EMPTY]);
        assert_eq!(
            call_battery(
                &registry,
                &mut memory,
                "xinputuap.dll",
                1,
                BATTERY_DEVTYPE_HEADSET.into(),
                0x1ffe
            ),
            Some(0)
        );
        memory.read_bytes(0x1ffe, &mut output).unwrap();
        assert_eq!(output, [BATTERY_TYPE_DISCONNECTED, BATTERY_LEVEL_EMPTY]);

        memory.write_bytes(0x1ffe, &[0xaa; 2]).unwrap();
        for (index, device_type, output_address) in [
            (4, 0, 0x1ffe),
            (0, 2, 0x1ffe),
            (0, 0, 0),
            (0, 0, 0x1fff),
            (0, 0, 0x5000),
        ] {
            assert_eq!(
                call_battery(
                    &registry,
                    &mut memory,
                    "xinput1_4.dll",
                    index,
                    device_type,
                    output_address
                ),
                Some(u64::from(ERROR_BAD_ARGUMENTS)),
                "index={index} device_type={device_type} output={output_address:#x}"
            );
        }
        memory.read_bytes(0x1ffe, &mut output).unwrap();
        assert_eq!(output, [0xaa; 2]);
    }

    #[test]
    fn supplied_battery_levels_use_xenoras_documented_coarse_quantization() {
        let (state, registry) = fixture();
        let mut memory = writable_memory();
        for (percent, expected) in [
            (0, BATTERY_LEVEL_EMPTY),
            (10, BATTERY_LEVEL_EMPTY),
            (11, BATTERY_LEVEL_LOW),
            (40, BATTERY_LEVEL_LOW),
            (41, BATTERY_LEVEL_MEDIUM),
            (70, BATTERY_LEVEL_MEDIUM),
            (71, BATTERY_LEVEL_FULL),
            (100, BATTERY_LEVEL_FULL),
        ] {
            assert!(state.set_xinput_controller_with_details(
                1,
                Some(XInputControllerState {
                    buttons: u16::from(percent),
                    ..XInputControllerState::default()
                }),
                false,
                Some(XInputBatteryState {
                    charge_state: XInputBatteryChargeState::Discharging,
                    level_percent: Some(percent),
                })
            ));
            assert_eq!(
                call_battery(
                    &registry,
                    &mut memory,
                    "xinput1_4.dll",
                    1,
                    BATTERY_DEVTYPE_GAMEPAD.into(),
                    0x1ffe
                ),
                Some(0)
            );
            let mut output = [0; 2];
            memory.read_bytes(0x1ffe, &mut output).unwrap();
            assert_eq!(
                output,
                [BATTERY_TYPE_UNKNOWN, expected],
                "percent={percent}"
            );
        }

        assert!(state.set_xinput_controller_with_details(
            1,
            Some(XInputControllerState::default()),
            false,
            Some(XInputBatteryState {
                charge_state: XInputBatteryChargeState::Unknown,
                level_percent: None,
            })
        ));
        assert_eq!(
            call_battery(
                &registry,
                &mut memory,
                "xinput1_4.dll",
                1,
                BATTERY_DEVTYPE_GAMEPAD.into(),
                0x1ffe
            ),
            Some(0)
        );
        let mut output = [0; 2];
        memory.read_bytes(0x1ffe, &mut output).unwrap();
        assert_eq!(output, [BATTERY_TYPE_UNKNOWN, BATTERY_LEVEL_EMPTY]);

        assert!(state.set_xinput_controller(1, None));
        assert_eq!(
            call_battery(
                &registry,
                &mut memory,
                "xinput1_4.dll",
                1,
                BATTERY_DEVTYPE_GAMEPAD.into(),
                0x1ffe
            ),
            Some(0)
        );
        memory.read_bytes(0x1ffe, &mut output).unwrap();
        assert_eq!(output, [BATTERY_TYPE_DISCONNECTED, BATTERY_LEVEL_EMPTY]);
    }

    #[test]
    fn keystroke_boundary_is_empty_but_never_fabricates_events() {
        let (state, registry) = fixture();
        let mut memory = writable_memory();
        memory.write_bytes(0x1ff0, &[0xaa; 8]).unwrap();
        assert!(call_keystroke(&registry, &mut memory, "xinput9_1_0.dll", 0, 0, 0x1ff0).is_none());
        for module in ["xinput1_4.dll", "xinputuap.dll"] {
            assert_eq!(
                call_keystroke(&registry, &mut memory, module, 0, 0, 0x1ff0),
                Some(u64::from(ERROR_DEVICE_NOT_CONNECTED))
            );
        }
        assert!(state.set_xinput_controller(1, Some(XInputControllerState::default())));
        assert_eq!(
            call_keystroke(&registry, &mut memory, "xinput1_4.dll", 1, 0, 0x1ff0),
            Some(u64::from(ERROR_EMPTY))
        );
        assert_eq!(
            call_keystroke(
                &registry,
                &mut memory,
                "xinput1_4.dll",
                XUSER_INDEX_ANY.into(),
                0,
                0x1ff0
            ),
            Some(u64::from(ERROR_EMPTY))
        );
        assert_eq!(
            call_keystroke(&registry, &mut memory, "xinputuap.dll", 0, 0, 0x1ff0),
            Some(u64::from(ERROR_DEVICE_NOT_CONNECTED))
        );
        for (index, reserved, output) in [(4, 0, 0x1ff0), (0, 1, 0x1ff0), (0, 0, 0), (0, 0, 0x1ff9)]
        {
            assert_eq!(
                call_keystroke(
                    &registry,
                    &mut memory,
                    "xinput1_4.dll",
                    index,
                    reserved,
                    output
                ),
                Some(u64::from(ERROR_BAD_ARGUMENTS)),
                "index={index} reserved={reserved} output={output:#x}"
            );
        }
        let mut output = [0; 8];
        memory.read_bytes(0x1ff0, &mut output).unwrap();
        assert_eq!(output, [0xaa; 8]);
    }

    fn call_enable(registry: &ImportRegistry, module: &str, enabled: u64) -> Option<u64> {
        let mut cpu = CpuState::default();
        cpu.set(crate::cpu::Register::Rcx, enabled);
        let mut memory = writable_memory();
        let descriptor = registry.resolve(module, &ImportName::Name("XInputEnable".to_owned()))?;
        Some((descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap())
    }

    #[test]
    fn xinput_enable_neutralizes_input_and_gates_vibration_delivery() {
        let (state, registry) = fixture();
        assert!(call_enable(&registry, "xinput9_1_0.dll", 0).is_none());
        for module in ["xinput1_4.dll", "xinputuap.dll"] {
            assert_eq!(call_enable(&registry, module, 1), Some(0));
        }
        assert!(state.set_xinput_controller(
            0,
            Some(XInputControllerState {
                buttons: 0x1000,
                left_trigger: 200,
                left_x: -32768,
                ..XInputControllerState::default()
            }),
        ));
        let mut memory = writable_memory();
        memory.write_u32(0x1200, 0x8001_4002).unwrap();
        assert_eq!(call_set_state(&registry, &mut memory, 0, 0x1200), 0);
        assert_eq!(state.xinput_vibration(0), Some((1, 0x4002, 0x8001, true)));
        assert_eq!(call(&registry, &mut memory, "xinput1_4.dll", 0, 0x1ff0), 0);
        assert_ne!(memory.read_u32(0x1ff4).unwrap(), 0);

        assert_eq!(call_enable(&registry, "xinput1_4.dll", 0), Some(0));
        assert_eq!(state.xinput_vibration(0), Some((1, 0x4002, 0x8001, false)));
        memory.write_bytes(0x1ff0, &[0xaa; 16]).unwrap();
        assert_eq!(call(&registry, &mut memory, "xinput1_4.dll", 0, 0x1ff0), 0);
        let mut neutral = [0; 16];
        memory.read_bytes(0x1ff0, &mut neutral).unwrap();
        assert_eq!(neutral, [0; 16]);

        assert_eq!(call_set_state(&registry, &mut memory, 0, 0x1200), 0);
        assert_eq!(state.xinput_vibration(0), Some((2, 0x4002, 0x8001, false)));
        assert_eq!(call_enable(&registry, "xinputuap.dll", 1), Some(0));
        assert_eq!(state.xinput_vibration(0), Some((2, 0x4002, 0x8001, true)));
        assert_eq!(call(&registry, &mut memory, "xinput1_4.dll", 0, 0x1ff0), 0);
        assert_eq!(memory.read_u32(0x1ff0).unwrap(), 1);
        assert_eq!(memory.read_u16(0x1ff4).unwrap(), 0x1000);
    }

    #[test]
    fn valid_slots_report_no_connected_controller_without_mutating_output() {
        let (_, registry) = fixture();
        let mut memory = writable_memory();
        memory.write_bytes(0x1ff0, &[0xaa; 16]).unwrap();
        for module in MODULES {
            for index in 0..4 {
                assert_eq!(
                    call(&registry, &mut memory, module, index, 0x1ff0),
                    u64::from(ERROR_DEVICE_NOT_CONNECTED)
                );
            }
        }
        let mut output = [0; 16];
        memory.read_bytes(0x1ff0, &mut output).unwrap();
        assert_eq!(output, [0xaa; 16]);
    }

    #[test]
    fn connected_snapshot_uses_the_exact_public_layout() {
        let (state, registry) = fixture();
        assert!(state.set_xinput_controller(
            2,
            Some(XInputControllerState {
                buttons: 0x9011,
                left_trigger: 17,
                right_trigger: 240,
                left_x: -32768,
                left_y: 32767,
                right_x: -1234,
                right_y: 4321,
            }),
        ));
        let mut memory = writable_memory();
        assert_eq!(call(&registry, &mut memory, "xinput1_4.dll", 2, 0x1200), 0);
        let mut output = [0; 16];
        memory.read_bytes(0x1200, &mut output).unwrap();
        assert_eq!(
            output,
            [
                1, 0, 0, 0, 0x11, 0x90, 17, 240, 0, 0x80, 0xff, 0x7f, 0x2e, 0xfb, 0xe1, 0x10,
            ]
        );
    }

    #[test]
    fn packet_numbers_change_only_for_changed_snapshots() {
        let (state, registry) = fixture();
        let mut memory = writable_memory();
        let one = XInputControllerState {
            buttons: 0x1000,
            ..XInputControllerState::default()
        };
        assert!(state.set_xinput_controller(0, Some(one)));
        assert_eq!(call(&registry, &mut memory, "xinputuap.dll", 0, 0x1200), 0);
        assert_eq!(memory.read_u32(0x1200).unwrap(), 1);
        assert!(state.set_xinput_controller(0, Some(one)));
        assert_eq!(call(&registry, &mut memory, "xinputuap.dll", 0, 0x1200), 0);
        assert_eq!(memory.read_u32(0x1200).unwrap(), 1);
        assert!(state.set_xinput_controller(
            0,
            Some(XInputControllerState {
                buttons: 0x2000,
                ..one
            }),
        ));
        assert_eq!(call(&registry, &mut memory, "xinputuap.dll", 0, 0x1200), 0);
        assert_eq!(memory.read_u32(0x1200).unwrap(), 2);
        assert!(state.set_xinput_controller(0, None));
        assert_eq!(
            call(&registry, &mut memory, "xinputuap.dll", 0, 0x1200),
            u64::from(ERROR_DEVICE_NOT_CONNECTED)
        );
    }

    #[test]
    fn connected_virtual_controller_reports_standard_input_capabilities() {
        let (state, registry) = fixture();
        assert!(state.set_xinput_controller(1, Some(XInputControllerState::default())));
        let mut memory = writable_memory();
        for flags in [0, 1] {
            assert_eq!(
                call_capabilities(&registry, &mut memory, 1, flags, 0x1200),
                0
            );
            let mut output = [0; 20];
            memory.read_bytes(0x1200, &mut output).unwrap();
            assert_eq!(
                output,
                [
                    1, 1, 0, 0, 0xff, 0xf3, 0xff, 0xff, 0xff, 0x7f, 0xff, 0x7f, 0xff, 0x7f, 0xff,
                    0x7f, 0, 0, 0, 0,
                ]
            );
        }
    }

    #[test]
    fn vibration_commands_are_accepted_only_for_connected_slots() {
        let (state, registry) = fixture();
        let mut memory = writable_memory();
        memory.write_u32(0x1200, 0x8001_4002).unwrap();
        assert_eq!(
            call_set_state(&registry, &mut memory, 0, 0x1200),
            u64::from(ERROR_DEVICE_NOT_CONNECTED)
        );
        assert!(state.set_xinput_controller(0, Some(XInputControllerState::default())));
        assert_eq!(call_set_state(&registry, &mut memory, 0, 0x1200), 0);
        assert_eq!(state.xinput_vibration(0), Some((1, 0x4002, 0x8001, true)));
        assert_eq!(call_set_state(&registry, &mut memory, 0, 0x1200), 0);
        assert_eq!(state.xinput_vibration(0), Some((2, 0x4002, 0x8001, true)));
        assert_eq!(
            call_set_state(&registry, &mut memory, 4, 0x1200),
            u64::from(ERROR_BAD_ARGUMENTS)
        );
        assert_eq!(
            call_set_state(&registry, &mut memory, 0, 0),
            u64::from(ERROR_BAD_ARGUMENTS)
        );
        assert_eq!(
            call_set_state(&registry, &mut memory, 0, 0x5000),
            u64::from(ERROR_BAD_ARGUMENTS)
        );
        assert_eq!(state.xinput_vibration(0), Some((2, 0x4002, 0x8001, true)));

        assert!(state.set_xinput_controller(0, None));
        assert_eq!(state.xinput_vibration(0), None);
        assert!(state.set_xinput_controller(0, Some(XInputControllerState::default())));
        assert_eq!(state.xinput_vibration(0), Some((0, 0, 0, true)));
        assert_eq!(call_set_state(&registry, &mut memory, 0, 0x1200), 0);
        assert_eq!(state.xinput_vibration(0), Some((1, 0x4002, 0x8001, true)));
    }

    #[test]
    fn capabilities_reject_bad_input_before_writing_and_disconnected_slots() {
        let (_, registry) = fixture();
        let mut memory = writable_memory();
        memory.write_bytes(0x1fec, &[0xaa; 20]).unwrap();
        for (index, flags, output) in [
            (4, 0, 0x1fec),
            (0, 2, 0x1fec),
            (0, 0, 0),
            (0, 0, 0x1fed),
            (0, 0, 0x5000),
        ] {
            assert_eq!(
                call_capabilities(&registry, &mut memory, index, flags, output),
                u64::from(ERROR_BAD_ARGUMENTS)
            );
        }
        assert_eq!(
            call_capabilities(&registry, &mut memory, 0, 1, 0x1fec),
            u64::from(ERROR_DEVICE_NOT_CONNECTED)
        );
        let mut output = [0; 20];
        memory.read_bytes(0x1fec, &mut output).unwrap();
        assert_eq!(output, [0xaa; 20]);
    }

    #[test]
    fn invalid_indexes_and_output_ranges_are_rejected() {
        let (state, registry) = fixture();
        assert!(!state.set_xinput_controller(4, None));
        let mut memory = writable_memory();
        for (index, output) in [
            (4, 0x1ff0),
            (u64::from(u32::MAX), 0x1ff0),
            (0, 0),
            (0, 0x1ff1),
            (0, 0x5000),
        ] {
            assert_eq!(
                call(&registry, &mut memory, "xinput1_4.dll", index, output),
                u64::from(ERROR_BAD_ARGUMENTS)
            );
        }
    }

    #[test]
    fn read_only_output_is_rejected_before_any_mutation() {
        let (_, registry) = fixture();
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 0x1000, MemoryPermissions::READ_ONLY)
            .unwrap();
        assert_eq!(
            call(&registry, &mut memory, "xinputuap.dll", 0, 0x1ff0),
            u64::from(ERROR_BAD_ARGUMENTS)
        );
    }
}
