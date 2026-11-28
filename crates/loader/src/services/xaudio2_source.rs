// SPDX-License-Identifier: GPL-3.0-or-later

//! The source voice methods and the mastering voice method.
//!
//! The source voice adds ten methods to the nineteen the voice interfaces
//! share, and the mastering voice adds one. The vtables follow the order of
//! the public interfaces.

use super::xaudio2::{Kind, Method, read_struct};
use crate::cpu::{CpuState, ExitReason};
use crate::imports::Win64Args;
use crate::memory::GuestMemory;
use crate::runtime::Call;
use crate::services::ServicesState;
use xenora_xaudio2::BufferDesc;
use xenora_xaudio2::hresult::{E_NOTIMPL, E_POINTER, S_OK, XAUDIO2_E_INVALID_CALL};

/// The channel mask of a stereo voice: front left and front right.
const SPEAKER_FRONT_LEFT: u32 = 0x1;
/// The channel mask of a mono voice: front center.
const SPEAKER_FRONT_CENTER: u32 = 0x4;

pub(crate) const SOURCE_METHODS: [(&str, Method); 10] = [
    ("IXAudio2SourceVoice.Start", start),
    ("IXAudio2SourceVoice.Stop", stop),
    (
        "IXAudio2SourceVoice.SubmitSourceBuffer",
        submit_source_buffer,
    ),
    (
        "IXAudio2SourceVoice.FlushSourceBuffers",
        flush_source_buffers,
    ),
    ("IXAudio2SourceVoice.Discontinuity", discontinuity),
    ("IXAudio2SourceVoice.ExitLoop", exit_loop),
    ("IXAudio2SourceVoice.GetState", get_state),
    ("IXAudio2SourceVoice.SetFrequencyRatio", set_frequency_ratio),
    ("IXAudio2SourceVoice.GetFrequencyRatio", get_frequency_ratio),
    (
        "IXAudio2SourceVoice.SetSourceSampleRate",
        set_source_sample_rate,
    ),
];

/// The methods the mastering voice adds to the shared voice methods.
pub(crate) const MASTERING_METHODS: [(&str, Method); 1] =
    [("IXAudio2MasteringVoice.GetChannelMask", get_channel_mask)];

/// IXAudio2SourceVoice::Start.
fn start(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    let flags = args.rdx as u32;
    let operation_set = args.r8 as u32;
    let mut xaudio2 = state.xaudio2();
    let Some(voice) = xaudio2.voice_of(args.rcx, Kind::Source) else {
        xaudio2.diagnostics.invalid_call += 1;
        return Ok(XAUDIO2_E_INVALID_CALL as u64);
    };
    match xaudio2.runtime.start(voice, flags, operation_set) {
        Ok(()) => Ok(S_OK as u64),
        Err(error) => Ok(error.hresult() as u64),
    }
}

/// IXAudio2SourceVoice::Stop.
fn stop(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    let flags = args.rdx as u32;
    let operation_set = args.r8 as u32;
    let mut xaudio2 = state.xaudio2();
    let Some(voice) = xaudio2.voice_of(args.rcx, Kind::Source) else {
        xaudio2.diagnostics.invalid_call += 1;
        return Ok(XAUDIO2_E_INVALID_CALL as u64);
    };
    match xaudio2.runtime.stop(voice, flags, operation_set) {
        Ok(()) => Ok(S_OK as u64),
        Err(error) => Ok(error.hresult() as u64),
    }
}

/// IXAudio2SourceVoice::SubmitSourceBuffer.
fn submit_source_buffer(
    state: &ServicesState,
    memory: &mut GuestMemory,
    args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    let buffer = args.rdx;
    let wma = args.r8;
    let mut xaudio2 = state.xaudio2();
    let Some(voice) = xaudio2.voice_of(args.rcx, Kind::Source) else {
        xaudio2.diagnostics.invalid_call += 1;
        return Ok(XAUDIO2_E_INVALID_CALL as u64);
    };
    if wma != 0 {
        xaudio2.diagnostics.not_implemented += 1;
        return Ok(E_NOTIMPL as u64);
    }
    let Some(bytes) = read_struct::<{ BufferDesc::SIZE }>(memory, buffer) else {
        xaudio2.diagnostics.pointer += 1;
        return Ok(E_POINTER as u64);
    };
    let desc = BufferDesc::parse(&bytes);
    // The audio range must be mapped and readable when the buffer is
    // submitted. The bytes themselves are named, never copied.
    if desc.audio_data == 0 || !memory.can_read(desc.audio_data, desc.audio_bytes as usize) {
        xaudio2.diagnostics.pointer += 1;
        return Ok(E_POINTER as u64);
    }
    match xaudio2.runtime.submit_source_buffer(voice, &desc, wma) {
        Ok(()) => Ok(S_OK as u64),
        Err(error) => Ok(error.hresult() as u64),
    }
}

/// IXAudio2SourceVoice::FlushSourceBuffers.
fn flush_source_buffers(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    let mut xaudio2 = state.xaudio2();
    let Some(voice) = xaudio2.voice_of(args.rcx, Kind::Source) else {
        xaudio2.diagnostics.invalid_call += 1;
        return Ok(XAUDIO2_E_INVALID_CALL as u64);
    };
    match xaudio2.runtime.flush_source_buffers(voice) {
        Ok(_) => Ok(S_OK as u64),
        Err(error) => Ok(error.hresult() as u64),
    }
}

/// IXAudio2SourceVoice::Discontinuity.
fn discontinuity(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.not_implemented += 1;
    Ok(E_NOTIMPL as u64)
}

/// IXAudio2SourceVoice::ExitLoop.
fn exit_loop(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.not_implemented += 1;
    Ok(E_NOTIMPL as u64)
}

/// IXAudio2SourceVoice::GetState.
fn get_state(
    state: &ServicesState,
    memory: &mut GuestMemory,
    args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    let out = args.rdx;
    let flags = args.r8 as u32;
    let mut xaudio2 = state.xaudio2();
    let Some(voice) = xaudio2.voice_of(args.rcx, Kind::Source) else {
        xaudio2.diagnostics.silent += 1;
        return Ok(0);
    };
    let Some(engine) = xaudio2.objects.get(&args.rcx).map(|object| object.engine) else {
        xaudio2.diagnostics.silent += 1;
        return Ok(0);
    };
    // The clock of the engine moves one quantum before the state is read.
    let _ = xaudio2.runtime.advance_engine_clock(engine);
    let Ok(voice_state) = xaudio2.runtime.state(voice, flags) else {
        xaudio2.diagnostics.silent += 1;
        return Ok(0);
    };
    if memory.write_bytes(out, &voice_state.to_bytes()).is_err() {
        xaudio2.diagnostics.pointer += 1;
        xaudio2.diagnostics.silent += 1;
    }
    Ok(0)
}

/// IXAudio2SourceVoice::SetFrequencyRatio.
fn set_frequency_ratio(
    state: &ServicesState,
    memory: &mut GuestMemory,
    args: &Win64Args,
    cpu: &CpuState,
) -> Result<u64, ExitReason> {
    // The ratio is the second argument, and the Microsoft x64 convention
    // gives the second argument XMM1.
    let ratio = f32::from_bits(Call::new(args, cpu, memory).f32(1)?.0);
    // The operation set is the third argument, and the Microsoft x64
    // convention gives the third argument R8.
    let operation_set = args.r8 as u32;
    let mut xaudio2 = state.xaudio2();
    let Some(voice) = xaudio2.voice_of(args.rcx, Kind::Source) else {
        xaudio2.diagnostics.invalid_call += 1;
        return Ok(XAUDIO2_E_INVALID_CALL as u64);
    };
    match xaudio2
        .runtime
        .set_frequency_ratio(voice, ratio, operation_set)
    {
        Ok(()) => Ok(S_OK as u64),
        Err(error) => Ok(error.hresult() as u64),
    }
}

/// IXAudio2SourceVoice::GetFrequencyRatio.
fn get_frequency_ratio(
    state: &ServicesState,
    memory: &mut GuestMemory,
    args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    let mut xaudio2 = state.xaudio2();
    let Some(voice) = xaudio2.voice_of(args.rcx, Kind::Source) else {
        xaudio2.diagnostics.silent += 1;
        return Ok(0);
    };
    let Ok(ratio) = xaudio2.runtime.frequency_ratio(voice) else {
        xaudio2.diagnostics.silent += 1;
        return Ok(0);
    };
    if memory.write_bytes(args.rdx, &ratio.to_le_bytes()).is_err() {
        xaudio2.diagnostics.pointer += 1;
        xaudio2.diagnostics.silent += 1;
    }
    Ok(0)
}

/// IXAudio2SourceVoice::SetSourceSampleRate.
fn set_source_sample_rate(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.not_implemented += 1;
    Ok(E_NOTIMPL as u64)
}

/// IXAudio2MasteringVoice::GetChannelMask.
fn get_channel_mask(
    state: &ServicesState,
    memory: &mut GuestMemory,
    args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    let out = args.rdx;
    let mut xaudio2 = state.xaudio2();
    let Some(voice) = xaudio2.voice_of(args.rcx, Kind::Mastering) else {
        xaudio2.diagnostics.invalid_call += 1;
        return Ok(XAUDIO2_E_INVALID_CALL as u64);
    };
    let Ok((channels, _sample_rate)) = xaudio2.runtime.voice_format(voice) else {
        xaudio2.diagnostics.invalid_call += 1;
        return Ok(XAUDIO2_E_INVALID_CALL as u64);
    };
    let mask = match channels {
        1 => SPEAKER_FRONT_CENTER,
        _ => SPEAKER_FRONT_LEFT | 0x2,
    };
    if memory.write_bytes(out, &mask.to_le_bytes()).is_err() {
        xaudio2.diagnostics.pointer += 1;
        return Ok(E_POINTER as u64);
    }
    Ok(S_OK as u64)
}
