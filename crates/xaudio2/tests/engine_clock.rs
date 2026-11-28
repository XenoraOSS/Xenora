// SPDX-License-Identifier: GPL-3.0-or-later
//! The engine clock moves the voices of one engine by one quantum.
//!
//! The clock is a stand-in for the mixer of issue #306: it moves the sample
//! count of the started source voices of an engine, and nothing else.

use xenora_xaudio2::{BufferDesc, Runtime, VoiceId, WaveFormat};

/// A PCM16 WAVEFORMATEX with the given channel count and sample rate.
fn pcm16(channels: u16, rate: u32) -> WaveFormat {
    let align = u32::from(channels) * 2;
    let mut b = [0u8; 18];
    b[0..2].copy_from_slice(&1u16.to_le_bytes());
    b[2..4].copy_from_slice(&channels.to_le_bytes());
    b[4..8].copy_from_slice(&rate.to_le_bytes());
    b[8..12].copy_from_slice(&(rate * align).to_le_bytes());
    b[12..14].copy_from_slice(&(align as u16).to_le_bytes());
    b[14..16].copy_from_slice(&16u16.to_le_bytes());
    WaveFormat::parse(&b).unwrap()
}

/// One buffer of the given number of frames.
fn buffer(frames: u32) -> BufferDesc {
    BufferDesc {
        flags: 0,
        audio_bytes: frames * 4,
        audio_data: 0x4000_0000,
        play_begin: 0,
        play_length: 0,
        loop_begin: 0,
        loop_length: 0,
        loop_count: 0,
        context: 1,
    }
}

/// An engine with a mastering voice at the given rate and a source voice.
fn engine_with_voice(rate: u32, source_rate: u32) -> (Runtime, xenora_xaudio2::EngineId, VoiceId) {
    let mut rt = Runtime::new();
    let engine = rt.create_engine(0, 1).unwrap();
    rt.create_mastering_voice(engine, 2, rate, 0, 0, 0, 6)
        .unwrap();
    let voice = rt
        .create_source_voice(engine, &pcm16(2, source_rate), 0, 2.0, 0, 0, 0)
        .unwrap();
    rt.submit_source_buffer(voice, &buffer(4800), 0).unwrap();
    (rt, engine, voice)
}

#[test]
fn one_quantum_moves_a_started_voice_by_the_quantum_of_the_mastering_rate() {
    let (mut rt, engine, voice) = engine_with_voice(48_000, 48_000);
    rt.start(voice, 0, 0).unwrap();
    assert_eq!(rt.advance_engine_clock(engine).unwrap(), 480);
    assert_eq!(rt.state(voice, 0).unwrap().samples_played, 480);
    assert_eq!(rt.advance_engine_clock(engine).unwrap(), 480);
    assert_eq!(rt.state(voice, 0).unwrap().samples_played, 960);
}

#[test]
fn a_voice_that_does_not_play_does_not_move() {
    let (mut rt, engine, voice) = engine_with_voice(48_000, 48_000);
    assert_eq!(rt.advance_engine_clock(engine).unwrap(), 480);
    assert_eq!(rt.state(voice, 0).unwrap().samples_played, 0);
}

#[test]
fn a_stopped_engine_clock_does_not_move_any_voice() {
    let (mut rt, engine, voice) = engine_with_voice(48_000, 48_000);
    rt.start(voice, 0, 0).unwrap();
    rt.stop_engine(engine).unwrap();
    assert_eq!(rt.advance_engine_clock(engine).unwrap(), 0);
    assert_eq!(rt.state(voice, 0).unwrap().samples_played, 0);
    rt.start_engine(engine).unwrap();
    assert_eq!(rt.advance_engine_clock(engine).unwrap(), 480);
    assert_eq!(rt.state(voice, 0).unwrap().samples_played, 480);
}

#[test]
fn the_quantum_scales_with_the_frequency_ratio_and_the_source_rate() {
    let (mut rt, engine, voice) = engine_with_voice(44_100, 48_000);
    rt.start(voice, 0, 0).unwrap();
    // The quantum of a 44100 hertz engine is 441 mastering frames, which is
    // 480 frames of a 48000 hertz source voice.
    assert_eq!(rt.advance_engine_clock(engine).unwrap(), 441);
    assert_eq!(rt.state(voice, 0).unwrap().samples_played, 480);
    rt.set_frequency_ratio(voice, 2.0, 0).unwrap();
    assert_eq!(rt.advance_engine_clock(engine).unwrap(), 441);
    assert_eq!(rt.state(voice, 0).unwrap().samples_played, 1440);
}

#[test]
fn the_clock_of_an_engine_that_is_gone_is_refused() {
    let (mut rt, engine, _voice) = engine_with_voice(48_000, 48_000);
    rt.release_engine(engine).unwrap();
    assert!(rt.advance_engine_clock(engine).is_err());
}
