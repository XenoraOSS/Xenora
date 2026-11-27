// SPDX-License-Identifier: GPL-3.0-or-later

//! The public XAudio2 constants the bounded core model uses.
//!
//! Every value here is a number the public XAudio2 documentation states. The
//! volumes and the frequency ratios are single precision floats because the
//! API takes them as float; every other constant is an unsigned integer.

/// Largest buffer size the public documentation accepts, in bytes.
pub const XAUDIO2_MAX_BUFFER_BYTES: u32 = 0x8000_0000;

/// Largest number of buffers a source voice may hold.
pub const XAUDIO2_MAX_QUEUED_BUFFERS: u32 = 64;

/// Largest number of channels the public documentation accepts.
pub const XAUDIO2_MAX_AUDIO_CHANNELS: u32 = 64;

/// Smallest sample rate the public documentation accepts, in hertz.
pub const XAUDIO2_MIN_SAMPLE_RATE: u32 = 1_000;

/// Largest sample rate the public documentation accepts, in hertz.
pub const XAUDIO2_MAX_SAMPLE_RATE: u32 = 200_000;

/// Largest absolute volume level the public documentation accepts.
pub const XAUDIO2_MAX_VOLUME_LEVEL: f32 = 16_777_216.0;

/// Smallest frequency ratio a voice accepts.
pub const XAUDIO2_MIN_FREQ_RATIO: f32 = 1.0 / 1024.0;

/// Largest frequency ratio a voice accepts.
pub const XAUDIO2_MAX_FREQ_RATIO: f32 = 1024.0;

/// The frequency ratio a voice starts with.
pub const XAUDIO2_DEFAULT_FREQ_RATIO: f32 = 2.0;

/// Largest finite loop count; the next value means loop forever.
pub const XAUDIO2_MAX_LOOP_COUNT: u32 = 254;

/// The loop count that asks for an endless loop.
pub const XAUDIO2_LOOP_INFINITE: u32 = 255;

/// The loop region value that asks for no loop region at all.
pub const XAUDIO2_NO_LOOP_REGION: u32 = 0;

/// Buffer flag that ends the stream when the buffer finishes.
pub const XAUDIO2_END_OF_STREAM: u32 = 0x40;

/// Stop flag that lets the tail of the voice finish.
pub const XAUDIO2_PLAY_TAILS: u32 = 0x20;

/// Voice state flag that asks for the sample count to be reported as zero.
pub const XAUDIO2_VOICE_NOSAMPLESPLAYED: u32 = 0x100;

/// Operation set value that applies an operation immediately.
pub const XAUDIO2_COMMIT_NOW: u32 = 0;

/// The default audio processor the engine accepts.
pub const XAUDIO2_DEFAULT_PROCESSOR: u32 = 1;

/// The quantum denominator that a sample rate must be a multiple of.
pub const XAUDIO2_QUANTUM_DENOMINATOR: u32 = 100;
