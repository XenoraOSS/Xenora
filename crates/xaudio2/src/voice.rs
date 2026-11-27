// SPDX-License-Identifier: GPL-3.0-or-later

//! Voice handles and the state a source voice keeps while it plays.
//!
//! The module holds the model of one voice: the mastering voice that carries
//! the format of an engine, and the source voice that owns the buffer queue
//! and the fixed sample clock.

use std::collections::VecDeque;

use crate::buffer::{BufferDesc, Event};
use crate::consts::{XAUDIO2_END_OF_STREAM, XAUDIO2_LOOP_INFINITE};
use crate::engine::EngineId;
use crate::format::WaveFormat;

/// One live voice.
pub(crate) struct Voice {
    /// What kind of voice this is.
    pub kind: VoiceKind,
    /// The engine the voice belongs to.
    pub engine: EngineId,
}

/// A handle to a voice.
///
/// The handle is a slot number and a generation. Destroying a voice frees
/// its slot, and the next voice that takes the slot gets a new generation, so
/// a stale handle never names a live voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoiceId {
    slot: u32,
    generation: u32,
}

impl VoiceId {
    /// Builds the handle of the voice in a slot with a generation.
    pub(crate) fn new(slot: u32, generation: u32) -> Self {
        VoiceId { slot, generation }
    }

    /// The slot the voice lives in.
    pub(crate) fn slot(&self) -> u32 {
        self.slot
    }

    /// The generation the voice has in its slot.
    pub(crate) fn generation(&self) -> u32 {
        self.generation
    }
}

/// The two kinds of voice the model knows.
pub(crate) enum VoiceKind {
    /// The mastering voice of an engine.
    Mastering(MasteringState),
    /// A source voice that plays buffers.
    Source(SourceState),
}

/// The state of a mastering voice.
pub(crate) struct MasteringState {
    /// The number of channels the engine mixes to.
    pub channels: u32,
    /// The sample rate the engine runs at.
    pub sample_rate: u32,
    /// The stream category the guest asked for.
    #[allow(dead_code)]
    pub stream_category: u32,
    /// The current volume level.
    pub volume: f32,
}

/// The state of a source voice.
pub(crate) struct SourceState {
    /// The wave format the voice plays.
    pub format: WaveFormat,
    /// The largest frequency ratio the voice accepts.
    pub max_frequency_ratio: f32,
    /// The current volume level.
    pub volume: f32,
    /// The current frequency ratio.
    pub frequency_ratio: f32,
    /// Whether the voice is playing.
    pub started: bool,
    /// The number of samples the voice has played since the last stream end.
    pub samples_played: u64,
    /// The buffers the voice holds, the playing one first.
    pub queue: VecDeque<QueuedBuffer>,
}

/// One buffer waiting in the queue of a source voice.
pub(crate) struct QueuedBuffer {
    /// The description the guest submitted.
    pub desc: BufferDesc,
    /// The frame after the last frame of the play region.
    pub play_end: u32,
    /// The first frame of the loop region.
    pub loop_begin: u32,
    /// The frame after the last frame of the loop region, when the buffer loops.
    pub loop_end: Option<u32>,
    /// The frame the voice plays next.
    pub cursor: u32,
    /// How many passes of the loop region the voice already made.
    pub loops_done: u32,
    /// Whether the voice already reported the start of this buffer.
    pub notified_start: bool,
}

impl QueuedBuffer {
    /// Whether the voice may make another pass of the loop region.
    pub fn loops_remaining(&self) -> bool {
        if self.loop_end.is_none() {
            return false;
        }
        self.desc.loop_count == XAUDIO2_LOOP_INFINITE || self.loops_done < self.desc.loop_count
    }
}

/// Advances the sample clock of a source voice and reports what happened.
///
/// The clock only moves while the voice is playing. The voice jumps from one
/// boundary to the next instead of stepping frame by frame, so the cost is
/// proportional to the number of events, not the number of frames. The
/// boundary of the front buffer is the end of its loop region while loops
/// remain, and the end of its play region otherwise. Reaching a loop end
/// reports it and sends the cursor back to the start of the loop region;
/// reaching the play end reports a buffer end and drops the buffer, and a
/// buffer submitted with the end-of-stream flag then reports a stream end and
/// resets the sample count.
pub(crate) fn advance(state: &mut SourceState, frames: u32) -> Vec<Event> {
    let mut events = Vec::new();
    if !state.started {
        return events;
    }
    let mut remaining = frames;
    while remaining > 0 {
        let Some(buffer) = state.queue.front_mut() else {
            break;
        };
        let context = buffer.desc.context;
        if !buffer.notified_start {
            buffer.notified_start = true;
            events.push(Event::BufferStart { context });
        }
        // The cursor never passes the loop end while loops remain, and a buffer
        // at its play end has already left the queue, so the step is positive.
        let loop_target = buffer
            .loop_end
            .filter(|&end| buffer.loops_remaining() && buffer.cursor < end);
        let boundary = loop_target.unwrap_or(buffer.play_end);
        let step = remaining.min(boundary - buffer.cursor);
        buffer.cursor += step;
        state.samples_played += u64::from(step);
        remaining -= step;
        if buffer.cursor != boundary {
            break;
        }
        if loop_target.is_some() {
            buffer.loops_done += 1;
            buffer.cursor = buffer.loop_begin;
            events.push(Event::LoopEnd { context });
            continue;
        }
        let end_of_stream = buffer.desc.flags & XAUDIO2_END_OF_STREAM != 0;
        state.queue.pop_front();
        events.push(Event::BufferEnd { context });
        if end_of_stream {
            events.push(Event::StreamEnd);
            state.samples_played = 0;
        }
    }
    events
}
