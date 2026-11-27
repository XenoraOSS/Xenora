// SPDX-License-Identifier: GPL-3.0-or-later
//! The fixed sample clock does not depend on how a renderer slices time.
//!
//! A mixer advances voices in quanta of whatever size its output needs. The
//! events and the voice state must be the same whether the clock moves one
//! frame at a time or in a single call, and a long render must cost time in
//! proportion to its events, not its frames.

use std::time::{Duration, Instant};

use xenora_xaudio2::consts::{XAUDIO2_END_OF_STREAM, XAUDIO2_LOOP_INFINITE};
use xenora_xaudio2::{BufferDesc, Event, Runtime, VoiceId, VoiceState, WaveFormat};

/// A mono PCM16 48 kHz WAVEFORMATEX: one frame is two bytes.
fn mono_pcm16() -> WaveFormat {
    let mut b = [0u8; 18];
    b[0..2].copy_from_slice(&1u16.to_le_bytes());
    b[2..4].copy_from_slice(&1u16.to_le_bytes());
    b[4..8].copy_from_slice(&48_000u32.to_le_bytes());
    b[8..12].copy_from_slice(&96_000u32.to_le_bytes());
    b[12..14].copy_from_slice(&2u16.to_le_bytes());
    b[14..16].copy_from_slice(&16u16.to_le_bytes());
    WaveFormat::parse(&b).unwrap()
}

fn desc(
    frames: u32,
    play: (u32, u32),
    looping: (u32, u32, u32),
    flags: u32,
    context: u64,
) -> BufferDesc {
    BufferDesc {
        flags,
        audio_bytes: frames * 2,
        audio_data: 0x4000_0000,
        play_begin: play.0,
        play_length: play.1,
        loop_begin: looping.0,
        loop_length: looping.1,
        loop_count: looping.2,
        context,
    }
}

/// A started voice holding a mix of plain, partial-play, looping and
/// end-of-stream buffers: 150 + 40 + (60 + 3 * 25) + 30 + 50 frames in all.
fn loaded_voice() -> (Runtime, VoiceId) {
    let mut rt = Runtime::new();
    let engine = rt.create_engine(0, 1).unwrap();
    rt.create_mastering_voice(engine, 0, 0, 0, 0, 0, 6).unwrap();
    let voice = rt
        .create_source_voice(engine, &mono_pcm16(), 0, 2.0, 0, 0, 0)
        .unwrap();
    for buffer in [
        desc(150, (0, 0), (0, 0, 0), 0, 1),
        desc(100, (30, 40), (0, 0, 0), 0, 2),
        desc(60, (0, 0), (20, 25, 3), XAUDIO2_END_OF_STREAM, 3),
        desc(64, (10, 30), (5, 30, 1), 0, 4),
        desc(50, (0, 0), (0, 0, 0), XAUDIO2_END_OF_STREAM, 5),
    ] {
        rt.submit_source_buffer(voice, &buffer, 0).unwrap();
    }
    rt.start(voice, 0, 0).unwrap();
    (rt, voice)
}

fn run_in_chunks(chunk: u32, total: u32) -> (Vec<Event>, VoiceState) {
    let (mut rt, voice) = loaded_voice();
    let mut events = Vec::new();
    let mut left = total;
    while left > 0 {
        let step = chunk.min(left);
        events.extend(rt.consume(voice, step).unwrap());
        left -= step;
    }
    (events, rt.state(voice, 0).unwrap())
}

#[test]
fn every_chunking_gives_the_same_events_and_state() {
    // Buffer 3 plays 0..45, loops back to 20 three times (3 * 25) and finishes
    // 45..60: 135 frames. Buffer 4 plays 10..35, loops back to 5 once and
    // plays 5..40: 25 + 35 = 60 frames. Total 150 + 40 + 135 + 60 + 50.
    let total = 150 + 40 + 135 + 60 + 50;
    for stop_at in [1, 149, 150, 151, 260, total - 1, total, total + 100] {
        let reference = run_in_chunks(1, stop_at);
        for chunk in [2, 3, 7, 25, 64, 149, 150, 1_000, u32::MAX] {
            assert_eq!(
                run_in_chunks(chunk, stop_at),
                reference,
                "chunk {chunk}, {stop_at} frames"
            );
        }
    }
    let (events, state) = run_in_chunks(1, total);
    let ends: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, Event::BufferEnd { .. } | Event::StreamEnd))
        .collect();
    assert_eq!(
        ends,
        [
            &Event::BufferEnd { context: 1 },
            &Event::BufferEnd { context: 2 },
            &Event::BufferEnd { context: 3 },
            &Event::StreamEnd,
            &Event::BufferEnd { context: 4 },
            &Event::BufferEnd { context: 5 },
            &Event::StreamEnd,
        ]
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| **e == Event::LoopEnd { context: 3 })
            .count(),
        3
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| **e == Event::LoopEnd { context: 4 })
            .count(),
        1
    );
    assert_eq!((state.buffers_queued, state.samples_played), (0, 0));
}

#[test]
fn an_hour_of_looping_audio_advances_in_one_fast_call() {
    let mut rt = Runtime::new();
    let engine = rt.create_engine(0, 1).unwrap();
    rt.create_mastering_voice(engine, 0, 0, 0, 0, 0, 6).unwrap();
    let voice = rt
        .create_source_voice(engine, &mono_pcm16(), 0, 2.0, 0, 0, 0)
        .unwrap();
    let loop_frames = 48_000; // one second
    rt.submit_source_buffer(
        voice,
        &desc(loop_frames, (0, 0), (0, 0, XAUDIO2_LOOP_INFINITE), 0, 9),
        0,
    )
    .unwrap();
    rt.start(voice, 0, 0).unwrap();
    let hour = 3_600 * 48_000;
    let started = Instant::now();
    let events = rt.consume(voice, hour).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(
        events.len(),
        1 + 3_600,
        "one start and one loop end per second"
    );
    assert_eq!(rt.state(voice, 0).unwrap().samples_played, u64::from(hour));
    // Generous bound for a debug build: stepping 172.8 million frames one at a
    // time takes far longer than this.
    assert!(elapsed < Duration::from_millis(250), "took {elapsed:?}");
}
