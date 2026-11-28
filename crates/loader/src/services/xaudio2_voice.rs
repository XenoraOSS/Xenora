// SPDX-License-Identifier: GPL-3.0-or-later

//! The voice methods of the guest-visible XAudio2 objects.
//!
//! The voice interfaces of the public API share nineteen methods, and the
//! source voice and the mastering voice add one block each. The vtables are
//! in the order of the public interfaces, and the loader writes them into
//! guest memory when it creates an object.

use super::xaudio2::Method;
use crate::cpu::{CpuState, ExitReason};
use crate::imports::Win64Args;
use crate::memory::GuestMemory;
use crate::runtime::Call;
use crate::services::ServicesState;
use xenora_xaudio2::hresult::{E_NOTIMPL, S_OK, XAUDIO2_E_INVALID_CALL};

/// The voice methods the source voice and the mastering voice share.
pub(crate) const VOICE_METHODS: [(&str, Method); 19] = [
    ("IXAudio2Voice.GetVoiceDetails", get_voice_details),
    ("IXAudio2Voice.SetOutputVoices", set_output_voices),
    ("IXAudio2Voice.SetEffectChain", set_effect_chain),
    ("IXAudio2Voice.EnableEffect", enable_effect),
    ("IXAudio2Voice.DisableEffect", disable_effect),
    ("IXAudio2Voice.GetEffectState", get_effect_state),
    ("IXAudio2Voice.SetEffectParameters", set_effect_parameters),
    ("IXAudio2Voice.GetEffectParameters", get_effect_parameters),
    ("IXAudio2Voice.SetFilterParameters", set_filter_parameters),
    ("IXAudio2Voice.GetFilterParameters", get_filter_parameters),
    (
        "IXAudio2Voice.SetOutputFilterParameters",
        set_output_filter_parameters,
    ),
    (
        "IXAudio2Voice.GetOutputFilterParameters",
        get_output_filter_parameters,
    ),
    ("IXAudio2Voice.SetVolume", set_volume),
    ("IXAudio2Voice.GetVolume", get_volume),
    ("IXAudio2Voice.SetChannelVolumes", set_channel_volumes),
    ("IXAudio2Voice.GetChannelVolumes", get_channel_volumes),
    ("IXAudio2Voice.SetOutputMatrix", set_output_matrix),
    ("IXAudio2Voice.GetOutputMatrix", get_output_matrix),
    ("IXAudio2Voice.DestroyVoice", destroy_voice),
];

/// IXAudio2Voice::GetVoiceDetails.
fn get_voice_details(
    state: &ServicesState,
    memory: &mut GuestMemory,
    args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    let mut xaudio2 = state.xaudio2();
    let Some(voice) = xaudio2.any_voice_of(args.rcx) else {
        xaudio2.diagnostics.silent += 1;
        return Ok(0);
    };
    let Ok((channels, sample_rate)) = xaudio2.runtime.voice_format(voice) else {
        xaudio2.diagnostics.silent += 1;
        return Ok(0);
    };
    // XAUDIO2_VOICE_DETAILS: the creation flags, the active flags, the
    // channel count and the sample rate of the voice.
    let mut bytes = [0u8; 16];
    bytes[8..12].copy_from_slice(&channels.to_le_bytes());
    bytes[12..16].copy_from_slice(&sample_rate.to_le_bytes());
    if memory.write_bytes(args.rdx, &bytes).is_err() {
        xaudio2.diagnostics.pointer += 1;
        xaudio2.diagnostics.silent += 1;
    }
    Ok(0)
}

/// IXAudio2Voice::SetOutputVoices.
fn set_output_voices(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.not_implemented += 1;
    Ok(E_NOTIMPL as u64)
}

/// IXAudio2Voice::SetEffectChain.
fn set_effect_chain(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.not_implemented += 1;
    Ok(E_NOTIMPL as u64)
}

/// IXAudio2Voice::EnableEffect.
fn enable_effect(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.not_implemented += 1;
    Ok(E_NOTIMPL as u64)
}

/// IXAudio2Voice::DisableEffect.
fn disable_effect(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.not_implemented += 1;
    Ok(E_NOTIMPL as u64)
}

/// IXAudio2Voice::GetEffectState.
fn get_effect_state(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.silent += 1;
    Ok(0)
}

/// IXAudio2Voice::SetEffectParameters.
fn set_effect_parameters(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.not_implemented += 1;
    Ok(E_NOTIMPL as u64)
}

/// IXAudio2Voice::GetEffectParameters.
fn get_effect_parameters(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.not_implemented += 1;
    Ok(E_NOTIMPL as u64)
}

/// IXAudio2Voice::SetFilterParameters.
fn set_filter_parameters(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.not_implemented += 1;
    Ok(E_NOTIMPL as u64)
}

/// IXAudio2Voice::GetFilterParameters.
fn get_filter_parameters(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.silent += 1;
    Ok(0)
}

/// IXAudio2Voice::SetOutputFilterParameters.
fn set_output_filter_parameters(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.not_implemented += 1;
    Ok(E_NOTIMPL as u64)
}

/// IXAudio2Voice::GetOutputFilterParameters.
fn get_output_filter_parameters(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.silent += 1;
    Ok(0)
}

/// IXAudio2Voice::SetVolume.
fn set_volume(
    state: &ServicesState,
    memory: &mut GuestMemory,
    args: &Win64Args,
    cpu: &CpuState,
) -> Result<u64, ExitReason> {
    // The volume is the second argument, and the Microsoft x64 convention
    // gives the second argument XMM1.
    let volume = f32::from_bits(Call::new(args, cpu, memory).f32(1)?.0);
    // The operation set is the third argument, and the Microsoft x64
    // convention gives the third argument R8.
    let operation_set = args.r8 as u32;
    let mut xaudio2 = state.xaudio2();
    let Some(voice) = xaudio2.any_voice_of(args.rcx) else {
        xaudio2.diagnostics.invalid_call += 1;
        return Ok(XAUDIO2_E_INVALID_CALL as u64);
    };
    match xaudio2.runtime.set_volume(voice, volume, operation_set) {
        Ok(()) => Ok(S_OK as u64),
        Err(error) => Ok(error.hresult() as u64),
    }
}

/// IXAudio2Voice::GetVolume.
fn get_volume(
    state: &ServicesState,
    memory: &mut GuestMemory,
    args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    let mut xaudio2 = state.xaudio2();
    let Some(voice) = xaudio2.any_voice_of(args.rcx) else {
        xaudio2.diagnostics.silent += 1;
        return Ok(0);
    };
    let Ok(volume) = xaudio2.runtime.volume(voice) else {
        xaudio2.diagnostics.silent += 1;
        return Ok(0);
    };
    if memory.write_bytes(args.rdx, &volume.to_le_bytes()).is_err() {
        xaudio2.diagnostics.pointer += 1;
        xaudio2.diagnostics.silent += 1;
    }
    Ok(0)
}

/// IXAudio2Voice::SetChannelVolumes.
fn set_channel_volumes(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.not_implemented += 1;
    Ok(E_NOTIMPL as u64)
}

/// IXAudio2Voice::GetChannelVolumes.
fn get_channel_volumes(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.silent += 1;
    Ok(0)
}

/// IXAudio2Voice::SetOutputMatrix.
fn set_output_matrix(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.not_implemented += 1;
    Ok(E_NOTIMPL as u64)
}

/// IXAudio2Voice::GetOutputMatrix.
fn get_output_matrix(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    _args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    state.xaudio2().diagnostics.silent += 1;
    Ok(0)
}

/// IXAudio2Voice::DestroyVoice.
fn destroy_voice(
    state: &ServicesState,
    _memory: &mut GuestMemory,
    args: &Win64Args,
    _cpu: &CpuState,
) -> Result<u64, ExitReason> {
    let mut xaudio2 = state.xaudio2();
    let Some(voice) = xaudio2.any_voice_of(args.rcx) else {
        xaudio2.diagnostics.invalid_call += 1;
        return Ok(0);
    };
    if xaudio2.runtime.destroy_voice(voice).is_err() {
        // A mastering voice cannot go while the engine still has source
        // voices. The call writes nothing and records the refusal.
        xaudio2.diagnostics.invalid_call += 1;
        return Ok(0);
    }
    // The guest object stays allocated as a tombstone, so a later call
    // through its vtable is refused instead of touching freed memory.
    if let Some(object) = xaudio2.objects.get_mut(&args.rcx) {
        object.destroyed = true;
        object.voice = None;
    }
    Ok(0)
}
