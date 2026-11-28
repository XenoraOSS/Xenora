// SPDX-License-Identifier: GPL-3.0-or-later

//! The XAudio2 services, called the way a guest calls them.
//!
//! These tests reach the calls the acceptance program cannot: a pointer that
//! is not mapped, an audio range that leaves the mapped memory, the details
//! of a voice and a clock the guest stopped.

use std::sync::Arc;
use xenora_loader::cpu::CpuState;
use xenora_loader::import_table::ImportName;
use xenora_loader::imports::{ImportRegistry, Win64Args};
use xenora_loader::memory::{GuestMemory, MemoryPermissions};
use xenora_loader::services::xaudio2::MODULE;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};

/// The module the guest imports the engine factory from.
const DLL: &str = "xaudio2_9.dll";
/// The mapped region the tests keep their data in.
const SCRATCH: u64 = 0x5_0000;
/// The stack the tests pass their arguments on.
const STACK: u64 = 0x8_0000;
/// The end of the mapped region the audio bytes live in.
const AUDIO: u64 = 0x9_0000;

const S_OK: u32 = 0;
const E_POINTER: u32 = 0x8000_4003;
const E_INVALIDARG: u32 = 0x8007_0057;

/// The test environment: a registry, guest memory and a CPU.
struct Guest {
    registry: ImportRegistry,
    state: Arc<ServicesState>,
    memory: GuestMemory,
    cpu: CpuState,
}

impl Guest {
    fn new() -> Self {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, Arc::clone(&state));
        let mut memory = GuestMemory::new();
        for (address, size) in [(SCRATCH, 0x1000), (STACK, 0x1000), (AUDIO, 0x1_0000)] {
            memory
                .map_region(address, size, MemoryPermissions::READ_WRITE)
                .unwrap();
        }
        let cpu = CpuState::new(0, STACK);
        Self {
            registry,
            state,
            memory,
            cpu,
        }
    }

    /// Calls a service with the four register arguments.
    fn call(&mut self, module: &str, symbol: &str, args: Win64Args) -> u32 {
        let descriptor = self
            .registry
            .resolve(module, &ImportName::Name(symbol.to_owned()))
            .expect("the service");
        let result =
            (descriptor.handler)(&args, &mut self.cpu, &mut self.memory).expect("the service call");
        result as u32
    }

    /// Writes one stack argument of a call.
    fn push(&mut self, index: usize, value: u64) {
        let at = STACK + 40 + ((index - 4) as u64) * 8;
        self.memory.write_u64(at, value).unwrap();
    }

    /// Reads bytes from the guest.
    fn read(&self, address: u64, len: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; len];
        self.memory.read_bytes(address, &mut bytes).unwrap();
        bytes
    }

    /// The four register arguments of a call.
    fn args(rcx: u64, rdx: u64, r8: u64, r9: u64) -> Win64Args {
        Win64Args {
            rcx,
            rdx,
            r8,
            r9,
            rsp: STACK,
        }
    }

    /// Creates an engine and returns the guest pointer of it.
    fn engine(&mut self) -> u64 {
        self.memory.write_u64(SCRATCH, 0).unwrap();
        let result = self.call(DLL, "XAudio2Create", Guest::args(SCRATCH, 0, 1, 0));
        assert_eq!(result, S_OK);
        let engine = self.memory.read_u64(SCRATCH).unwrap();
        assert_ne!(engine, 0);
        engine
    }

    /// Creates a mastering voice and a source voice on an engine.
    fn voices(&mut self, engine: u64) -> (u64, u64) {
        for (index, value) in [(4, 0), (5, 0), (6, 0), (7, 6)] {
            self.push(index, value);
        }
        let result = self.call(
            MODULE,
            "IXAudio2.CreateMasteringVoice",
            Guest::args(engine, SCRATCH + 8, 2, 48_000),
        );
        assert_eq!(result, S_OK);
        let mastering = self.memory.read_u64(SCRATCH + 8).unwrap();
        assert_ne!(mastering, 0);
        // A stereo PCM16 format at 48000 hertz.
        let format = SCRATCH + 32;
        let mut bytes = [0u8; 18];
        bytes[0..2].copy_from_slice(&1u16.to_le_bytes());
        bytes[2..4].copy_from_slice(&2u16.to_le_bytes());
        bytes[4..8].copy_from_slice(&48_000u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&192_000u32.to_le_bytes());
        bytes[12..14].copy_from_slice(&4u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&16u16.to_le_bytes());
        self.memory.write_bytes(format, &bytes).unwrap();
        for (index, value) in [(4, 2.0f32.to_bits() as u64), (5, 0), (6, 0), (7, 0)] {
            self.push(index, value);
        }
        let result = self.call(
            MODULE,
            "IXAudio2.CreateSourceVoice",
            Guest::args(engine, SCRATCH + 16, format, 0),
        );
        assert_eq!(result, S_OK);
        let voice = self.memory.read_u64(SCRATCH + 16).unwrap();
        assert_ne!(voice, 0);
        (mastering, voice)
    }

    /// Writes an XAUDIO2_BUFFER that names the given audio range.
    fn buffer(&mut self, audio: u64, bytes: u32) {
        let mut buffer = [0u8; 44]; // packed XAUDIO2_BUFFER
        buffer[4..8].copy_from_slice(&bytes.to_le_bytes());
        buffer[8..16].copy_from_slice(&audio.to_le_bytes());
        self.memory.write_bytes(SCRATCH + 64, &buffer).unwrap();
    }
}

/// An out pointer the guest cannot write is a pointer error, not a fault.
#[test]
fn an_unwritable_out_pointer_is_a_pointer_error() {
    let mut guest = Guest::new();
    // 0x10 is not mapped, so the factory cannot null the out pointer.
    let result = guest.call(DLL, "XAudio2Create", Guest::args(0x10, 0, 1, 0));
    assert_eq!(result, E_POINTER);
    assert_eq!(guest.state.xaudio2_diagnostics().engines, 0);
}

/// An audio range that leaves the mapped memory is a pointer error.
#[test]
fn an_audio_range_that_leaves_the_mapped_memory_is_a_pointer_error() {
    let mut guest = Guest::new();
    let engine = guest.engine();
    let (_mastering, voice) = guest.voices(engine);
    // The range starts inside the mapped region and runs past its end.
    guest.buffer(AUDIO + 0xFFF8, 64);
    let result = guest.call(
        MODULE,
        "IXAudio2SourceVoice.SubmitSourceBuffer",
        Guest::args(voice, SCRATCH + 64, 0, 0),
    );
    assert_eq!(result, E_POINTER);
    // A range that stays inside the mapped memory is accepted.
    guest.buffer(AUDIO, 64);
    let result = guest.call(
        MODULE,
        "IXAudio2SourceVoice.SubmitSourceBuffer",
        Guest::args(voice, SCRATCH + 64, 0, 0),
    );
    assert_eq!(result, S_OK);
    let diagnostics = guest.state.xaudio2_diagnostics();
    assert_eq!(diagnostics.queued_buffers, 1);
    assert_eq!(diagnostics.queued_bytes, 64);
}

/// A buffer that is not a whole number of frames is refused.
#[test]
fn a_buffer_that_is_not_a_whole_frame_is_refused() {
    let mut guest = Guest::new();
    let engine = guest.engine();
    let (_mastering, voice) = guest.voices(engine);
    guest.buffer(AUDIO, 6);
    let result = guest.call(
        MODULE,
        "IXAudio2SourceVoice.SubmitSourceBuffer",
        Guest::args(voice, SCRATCH + 64, 0, 0),
    );
    assert_eq!(result, E_INVALIDARG);
}

/// The details of a voice and the channel mask of a mastering voice.
#[test]
fn the_voice_details_and_the_channel_mask_follow_the_voice() {
    let mut guest = Guest::new();
    let engine = guest.engine();
    let (mastering, _voice) = guest.voices(engine);
    let result = guest.call(
        MODULE,
        "IXAudio2Voice.GetVoiceDetails",
        Guest::args(mastering, SCRATCH + 128, 0, 0),
    );
    assert_eq!(result, 0);
    let details = guest.read(SCRATCH + 128, 16);
    assert_eq!(u32::from_le_bytes(details[8..12].try_into().unwrap()), 2);
    assert_eq!(
        u32::from_le_bytes(details[12..16].try_into().unwrap()),
        48_000
    );
    let result = guest.call(
        MODULE,
        "IXAudio2MasteringVoice.GetChannelMask",
        Guest::args(mastering, SCRATCH + 160, 0, 0),
    );
    assert_eq!(result, S_OK);
    let mask = guest.memory.read_u32(SCRATCH + 160).unwrap();
    assert_eq!(mask, 0x3, "front left and front right");
}

/// A stopped engine clock leaves the voices where they are.
#[test]
fn a_stopped_engine_clock_does_not_move_the_voice() {
    let mut guest = Guest::new();
    let engine = guest.engine();
    let (_mastering, voice) = guest.voices(engine);
    guest.buffer(AUDIO, 4800 * 4);
    let result = guest.call(
        MODULE,
        "IXAudio2SourceVoice.SubmitSourceBuffer",
        Guest::args(voice, SCRATCH + 64, 0, 0),
    );
    assert_eq!(result, S_OK);
    let result = guest.call(
        MODULE,
        "IXAudio2SourceVoice.Start",
        Guest::args(voice, 0, 0, 0),
    );
    assert_eq!(result, S_OK);
    let result = guest.call(MODULE, "IXAudio2.StopEngine", Guest::args(engine, 0, 0, 0));
    assert_eq!(result, 0);
    let result = guest.call(
        MODULE,
        "IXAudio2SourceVoice.GetState",
        Guest::args(voice, SCRATCH + 192, 0, 0),
    );
    assert_eq!(result, 0);
    let state = guest.read(SCRATCH + 192, 20);
    assert_eq!(u64::from_le_bytes(state[12..20].try_into().unwrap()), 0);
    // The clock runs again once the guest starts the engine.
    let result = guest.call(MODULE, "IXAudio2.StartEngine", Guest::args(engine, 0, 0, 0));
    assert_eq!(result, S_OK);
    let result = guest.call(
        MODULE,
        "IXAudio2SourceVoice.GetState",
        Guest::args(voice, SCRATCH + 192, 0, 0),
    );
    assert_eq!(result, 0);
    let state = guest.read(SCRATCH + 192, 20);
    assert_eq!(u64::from_le_bytes(state[12..20].try_into().unwrap()), 480);
}

/// An interface identifier the guest cannot read is a pointer error.
#[test]
fn an_unreadable_interface_identifier_is_a_pointer_error() {
    let mut guest = Guest::new();
    let engine = guest.engine();
    let result = guest.call(
        MODULE,
        "IXAudio2.QueryInterface",
        Guest::args(engine, 0x10, SCRATCH + 200, 0),
    );
    assert_eq!(result, E_POINTER);
    assert_eq!(guest.memory.read_u64(SCRATCH + 200).unwrap(), 0);
}
