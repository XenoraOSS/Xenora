// SPDX-License-Identifier: GPL-3.0-or-later

//! The runtime that owns the engines, their voices and their buffers.
//!
//! The module holds the runtime itself, the handle table that keeps the
//! handles of the objects, and the calls that act on one voice: the buffer
//! queue, the sample clock, the volume and the frequency ratio. The engine
//! and voice lifetime rules live in the engine module.

use crate::buffer::{BufferDesc, Event, VoiceState};
use crate::consts::{
    XAUDIO2_MAX_QUEUED_BUFFERS, XAUDIO2_MAX_VOLUME_LEVEL, XAUDIO2_MIN_FREQ_RATIO,
    XAUDIO2_PLAY_TAILS, XAUDIO2_VOICE_NOSAMPLESPLAYED,
};
use crate::engine::Engine;
use crate::hresult::Error;
use crate::voice::{QueuedBuffer, Voice, VoiceId, VoiceKind, advance};

/// A slot of a handle table.
pub(crate) struct Slot<T> {
    /// The generation of the object that last used the slot.
    generation: u32,
    /// The live object, when the slot is taken.
    value: Option<T>,
}

/// A handle table that hands out slots and generations.
pub(crate) struct SlotTable<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
}

impl<T> SlotTable<T> {
    /// Builds an empty table.
    pub(crate) fn new() -> Self {
        SlotTable {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    /// Takes a slot for a new object and returns its slot and generation.
    pub(crate) fn insert(&mut self, value: T) -> (u32, u32) {
        match self.free.pop() {
            Some(slot) => {
                let entry = &mut self.slots[slot as usize];
                // `remove` retires a slot at the last generation, so this never wraps.
                entry.generation += 1;
                entry.value = Some(value);
                (slot, entry.generation)
            }
            None => {
                let slot = self.slots.len() as u32;
                self.slots.push(Slot {
                    generation: 1,
                    value: Some(value),
                });
                (slot, 1)
            }
        }
    }

    /// Frees a slot and returns the object that held it.
    ///
    /// A slot that reached the last generation is retired instead of reused,
    /// so a stale handle can never match a later object in the same slot.
    pub(crate) fn remove(&mut self, slot: u32) -> Option<T> {
        let entry = self.slots.get_mut(slot as usize)?;
        let value = entry.value.take()?;
        if entry.generation != u32::MAX {
            self.free.push(slot);
        }
        Some(value)
    }

    /// The live object of a slot, when the generation still matches.
    pub(crate) fn get(&self, slot: u32, generation: u32) -> Option<&T> {
        let entry = self.slots.get(slot as usize)?;
        if entry.generation != generation {
            return None;
        }
        entry.value.as_ref()
    }

    /// The live object of a slot, when the generation still matches.
    pub(crate) fn get_mut(&mut self, slot: u32, generation: u32) -> Option<&mut T> {
        let entry = self.slots.get_mut(slot as usize)?;
        if entry.generation != generation {
            return None;
        }
        entry.value.as_mut()
    }

    /// The live objects of the table.
    pub(crate) fn values(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(|slot| slot.value.as_ref())
    }

    /// The number of live objects in the table.
    pub(crate) fn live(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.value.is_some())
            .count()
    }
}

/// A count of the live objects of a runtime.
///
/// The counters describe the model only. They say nothing about audio that a
/// guest would hear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Diagnostics {
    /// The number of live engines.
    pub engines: usize,
    /// The number of live mastering voices.
    pub mastering_voices: usize,
    /// The number of live source voices.
    pub source_voices: usize,
    /// The number of queued buffers over all source voices.
    pub queued_buffers: usize,
    /// The number of queued audio bytes over all source voices.
    pub queued_bytes: u64,
}

/// The bounded XAudio2 core.
///
/// The runtime owns the engines, the voices and the buffers. It models the
/// lifetime rules, the buffer queue, the fixed sample clock and the error
/// codes the public XAudio2 documentation describes. It does not read guest
/// memory, mix or resample anything: a buffer names a guest address and a
/// byte count, and the model never touches the bytes behind that address.
pub struct Runtime {
    pub(crate) engines: SlotTable<Engine>,
    pub(crate) voices: SlotTable<Voice>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    /// Builds a runtime without any engine.
    pub fn new() -> Self {
        Runtime {
            engines: SlotTable::new(),
            voices: SlotTable::new(),
        }
    }

    /// Queues a buffer on a source voice.
    ///
    /// The buffer names a guest address and a byte count. The model never
    /// reads the bytes behind that address.
    pub fn submit_source_buffer(
        &mut self,
        voice: VoiceId,
        desc: &BufferDesc,
        wma: u64,
    ) -> Result<(), Error> {
        if wma != 0 {
            return Err(Error::NotImplemented("WMA buffers are not implemented"));
        }
        let state = self
            .voices
            .get_mut(voice.slot(), voice.generation())
            .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
        let VoiceKind::Source(source) = &mut state.kind else {
            return Err(Error::InvalidCall("the voice is not a source voice"));
        };
        let plan = desc.plan(source.format.block_align)?;
        if source.queue.len() >= XAUDIO2_MAX_QUEUED_BUFFERS as usize {
            return Err(Error::InvalidCall("the buffer queue is full"));
        }
        source.queue.push_back(QueuedBuffer {
            desc: *desc,
            play_end: plan.play_end,
            loop_begin: plan.loop_begin,
            loop_end: plan.loop_end,
            cursor: plan.play_begin,
            loops_done: 0,
            notified_start: false,
        });
        Ok(())
    }

    /// Starts a source voice.
    ///
    /// Starting a voice that already plays is harmless and does not rewind
    /// it.
    pub fn start(&mut self, voice: VoiceId, flags: u32, operation_set: u32) -> Result<(), Error> {
        let state = self
            .voices
            .get_mut(voice.slot(), voice.generation())
            .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
        let VoiceKind::Source(source) = &mut state.kind else {
            return Err(Error::InvalidCall("the voice is not a source voice"));
        };
        if flags != 0 {
            return Err(Error::InvalidArg("start flags are not implemented"));
        }
        if operation_set != 0 {
            return Err(Error::NotImplemented("operation sets are not implemented"));
        }
        source.started = true;
        Ok(())
    }

    /// Stops a source voice.
    ///
    /// The voice keeps the buffer it was playing and the sample count it
    /// reached, so a later start resumes where the voice stopped.
    pub fn stop(&mut self, voice: VoiceId, flags: u32, operation_set: u32) -> Result<(), Error> {
        let state = self
            .voices
            .get_mut(voice.slot(), voice.generation())
            .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
        let VoiceKind::Source(source) = &mut state.kind else {
            return Err(Error::InvalidCall("the voice is not a source voice"));
        };
        if flags & !XAUDIO2_PLAY_TAILS != 0 {
            return Err(Error::InvalidArg("unknown stop flags"));
        }
        if operation_set != 0 {
            return Err(Error::NotImplemented("operation sets are not implemented"));
        }
        source.started = false;
        Ok(())
    }

    /// Removes the buffers of a source voice that are not playing.
    ///
    /// A playing voice keeps the buffer it plays; a stopped voice loses every
    /// buffer. Every removed buffer is reported as a buffer end.
    pub fn flush_source_buffers(&mut self, voice: VoiceId) -> Result<Vec<Event>, Error> {
        let state = self
            .voices
            .get_mut(voice.slot(), voice.generation())
            .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
        let VoiceKind::Source(source) = &mut state.kind else {
            return Err(Error::InvalidCall("the voice is not a source voice"));
        };
        let keep = usize::from(source.started);
        let mut removed = Vec::new();
        while source.queue.len() > keep {
            if let Some(buffer) = source.queue.pop_back() {
                removed.push(buffer);
            }
        }
        removed.reverse();
        Ok(removed
            .into_iter()
            .map(|buffer| Event::BufferEnd {
                context: buffer.desc.context,
            })
            .collect())
    }

    /// The state of a source voice.
    pub fn state(&self, voice: VoiceId, flags: u32) -> Result<VoiceState, Error> {
        let state = self
            .voices
            .get(voice.slot(), voice.generation())
            .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
        let VoiceKind::Source(source) = &state.kind else {
            return Err(Error::InvalidCall("the voice is not a source voice"));
        };
        match flags {
            0 | XAUDIO2_VOICE_NOSAMPLESPLAYED => {}
            _ => return Err(Error::InvalidArg("unknown voice state flags")),
        }
        let current_buffer_context = source.queue.front().map_or(0, |buffer| buffer.desc.context);
        Ok(VoiceState {
            current_buffer_context,
            buffers_queued: source.queue.len() as u32,
            samples_played: if flags == XAUDIO2_VOICE_NOSAMPLESPLAYED {
                0
            } else {
                source.samples_played
            },
        })
    }

    /// Sets the volume of a voice.
    ///
    /// The volume applies to mastering voices and source voices alike.
    pub fn set_volume(
        &mut self,
        voice: VoiceId,
        volume: f32,
        operation_set: u32,
    ) -> Result<(), Error> {
        let state = self
            .voices
            .get_mut(voice.slot(), voice.generation())
            .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
        if operation_set != 0 {
            return Err(Error::NotImplemented("operation sets are not implemented"));
        }
        if !volume.is_finite() || volume.abs() > XAUDIO2_MAX_VOLUME_LEVEL {
            return Err(Error::InvalidArg("the volume level is out of range"));
        }
        match &mut state.kind {
            VoiceKind::Mastering(mastering) => mastering.volume = volume,
            VoiceKind::Source(source) => source.volume = volume,
        }
        Ok(())
    }

    /// The volume of a voice.
    pub fn volume(&self, voice: VoiceId) -> Result<f32, Error> {
        let state = self
            .voices
            .get(voice.slot(), voice.generation())
            .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
        Ok(match &state.kind {
            VoiceKind::Mastering(mastering) => mastering.volume,
            VoiceKind::Source(source) => source.volume,
        })
    }

    /// Sets the frequency ratio of a source voice.
    ///
    /// The ratio must stay inside the range the voice was created with.
    pub fn set_frequency_ratio(
        &mut self,
        voice: VoiceId,
        ratio: f32,
        operation_set: u32,
    ) -> Result<(), Error> {
        let state = self
            .voices
            .get_mut(voice.slot(), voice.generation())
            .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
        let VoiceKind::Source(source) = &mut state.kind else {
            return Err(Error::InvalidCall("the voice is not a source voice"));
        };
        if operation_set != 0 {
            return Err(Error::NotImplemented("operation sets are not implemented"));
        }
        if !ratio.is_finite()
            || ratio < XAUDIO2_MIN_FREQ_RATIO
            || ratio > source.max_frequency_ratio
        {
            return Err(Error::InvalidArg("the frequency ratio is out of range"));
        }
        source.frequency_ratio = ratio;
        Ok(())
    }

    /// The frequency ratio of a source voice.
    pub fn frequency_ratio(&self, voice: VoiceId) -> Result<f32, Error> {
        let state = self
            .voices
            .get(voice.slot(), voice.generation())
            .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
        let VoiceKind::Source(source) = &state.kind else {
            return Err(Error::InvalidCall("the voice is not a source voice"));
        };
        Ok(source.frequency_ratio)
    }

    /// Advances the sample clock of a source voice.
    ///
    /// The clock only moves while the voice plays, and the call reports the
    /// buffer starts, loop ends, buffer ends and the stream end it reached.
    pub fn consume(&mut self, voice: VoiceId, frames: u32) -> Result<Vec<Event>, Error> {
        let state = self
            .voices
            .get_mut(voice.slot(), voice.generation())
            .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
        let VoiceKind::Source(source) = &mut state.kind else {
            return Err(Error::InvalidCall("the voice is not a source voice"));
        };
        Ok(advance(source, frames))
    }

    /// A count of the live objects and the queued audio of the runtime.
    pub fn diagnostics(&self) -> Diagnostics {
        let mut diagnostics = Diagnostics {
            engines: self.engines.live(),
            ..Diagnostics::default()
        };
        for voice in self.voices.values() {
            match &voice.kind {
                VoiceKind::Mastering(_) => diagnostics.mastering_voices += 1,
                VoiceKind::Source(source) => {
                    diagnostics.source_voices += 1;
                    diagnostics.queued_buffers += source.queue.len();
                    diagnostics.queued_bytes += source
                        .queue
                        .iter()
                        .map(|buffer| u64::from(buffer.desc.audio_bytes))
                        .sum::<u64>();
                }
            }
        }
        diagnostics
    }
}
