// SPDX-License-Identifier: GPL-3.0-or-later

//! The engine, its handle and the lifetime rules of engines and voices.
//!
//! The module holds the model of one XAudio2 engine and the calls that
//! create and destroy engines and voices. The rules come from the public
//! XAudio2 documentation and are recorded in docs/runtime/xaudio2.md.

use std::collections::VecDeque;

use crate::consts::{
    XAUDIO2_MAX_AUDIO_CHANNELS, XAUDIO2_MAX_FREQ_RATIO, XAUDIO2_MAX_SAMPLE_RATE,
    XAUDIO2_MIN_FREQ_RATIO, XAUDIO2_MIN_SAMPLE_RATE, XAUDIO2_QUANTUM_DENOMINATOR,
};
use crate::format::WaveFormat;
use crate::hresult::Error;
use crate::runtime::Runtime;
use crate::voice::{MasteringState, SourceState, Voice, VoiceId, VoiceKind};
use crate::{MAX_ENGINES, MAX_SOURCE_VOICES};

/// A handle to an engine.
///
/// The handle is a slot number and a generation, so releasing an engine and
/// creating a new one in the same slot leaves the old handle stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EngineId {
    slot: u32,
    generation: u32,
}

impl EngineId {
    /// Builds the handle of the engine in a slot with a generation.
    fn new(slot: u32, generation: u32) -> Self {
        EngineId { slot, generation }
    }

    /// The slot the engine lives in.
    pub(crate) fn slot(&self) -> u32 {
        self.slot
    }

    /// The generation the engine has in its slot.
    pub(crate) fn generation(&self) -> u32 {
        self.generation
    }
}

/// The state of one engine.
pub(crate) struct Engine {
    /// The audio processor the guest asked for.
    #[allow(dead_code)]
    pub processor: u32,
    /// Whether the engine clock runs. The clock of a real engine runs until
    /// the guest stops it, so a new engine starts with it running.
    pub clock_running: bool,
    /// The mastering voice of the engine, if it has one.
    pub mastering: Option<VoiceId>,
    /// The source voices of the engine.
    pub sources: Vec<VoiceId>,
}

impl Runtime {
    /// Creates an engine.
    ///
    /// The public documentation allows flags on the engine; none of them is
    /// modelled, so a nonzero flag is refused. The processor is recorded.
    /// The runtime holds a bounded number of engines.
    pub fn create_engine(&mut self, flags: u32, processor: u32) -> Result<EngineId, Error> {
        if flags != 0 {
            return Err(Error::NotImplemented("engine flags are not implemented"));
        }
        if self.engines.live() >= MAX_ENGINES {
            return Err(Error::OutOfMemory("the engine limit is reached"));
        }
        let engine = Engine {
            processor,
            clock_running: true,
            mastering: None,
            sources: Vec::new(),
        };
        let (slot, generation) = self.engines.insert(engine);
        Ok(EngineId::new(slot, generation))
    }

    /// Releases an engine and every voice and buffer it owns.
    pub fn release_engine(&mut self, engine: EngineId) -> Result<(), Error> {
        let (sources, mastering) = {
            let state = self
                .engines
                .get(engine.slot, engine.generation)
                .ok_or(Error::InvalidCall("the engine handle is no longer valid"))?;
            (state.sources.clone(), state.mastering)
        };
        for source in sources {
            self.voices.remove(source.slot());
        }
        if let Some(mastering) = mastering {
            self.voices.remove(mastering.slot());
        }
        self.engines.remove(engine.slot);
        Ok(())
    }

    /// Creates the mastering voice of an engine.
    ///
    /// The argument list follows the public
    /// IXAudio2MasteringVoice::CreateMasteringVoice call, so it is longer
    /// than the usual limit on arguments.
    #[allow(clippy::too_many_arguments)]
    pub fn create_mastering_voice(
        &mut self,
        engine: EngineId,
        channels: u32,
        sample_rate: u32,
        flags: u32,
        device_id: u64,
        effect_chain: u64,
        stream_category: u32,
    ) -> Result<VoiceId, Error> {
        if flags != 0 || device_id != 0 || effect_chain != 0 {
            return Err(Error::NotImplemented(
                "mastering voice flags, device id and effect chains are not implemented",
            ));
        }
        let channels = match channels {
            0 => 2,
            1 | 2 => channels,
            3..=XAUDIO2_MAX_AUDIO_CHANNELS => {
                return Err(Error::NotImplemented(
                    "only mono and stereo mastering voices are modelled",
                ));
            }
            _ => return Err(Error::InvalidArg("the channel count is out of range")),
        };
        let sample_rate = match sample_rate {
            0 => 48_000,
            44_100 | 48_000 => sample_rate,
            rate if (XAUDIO2_MIN_SAMPLE_RATE..=XAUDIO2_MAX_SAMPLE_RATE).contains(&rate)
                && rate % XAUDIO2_QUANTUM_DENOMINATOR == 0 =>
            {
                return Err(Error::NotImplemented(
                    "only 44100 and 48000 hertz mastering voices are modelled",
                ));
            }
            _ => return Err(Error::InvalidArg("the sample rate is out of range")),
        };
        let engine_state = self
            .engines
            .get_mut(engine.slot, engine.generation)
            .ok_or(Error::InvalidCall("the engine handle is no longer valid"))?;
        if engine_state.mastering.is_some() {
            return Err(Error::InvalidCall(
                "the engine already has a mastering voice",
            ));
        }
        let state = MasteringState {
            channels,
            sample_rate,
            stream_category,
            volume: 1.0,
        };
        let (slot, generation) = self.voices.insert(Voice {
            kind: VoiceKind::Mastering(state),
            engine,
        });
        let handle = VoiceId::new(slot, generation);
        engine_state.mastering = Some(handle);
        Ok(handle)
    }

    /// Creates a source voice on an engine.
    ///
    /// The argument list follows the public IXAudio2SourceVoice::CreateSourceVoice
    /// call, so it is longer than the usual limit on arguments. The voice
    /// needs a live mastering voice to send to, and the runtime holds a
    /// bounded number of source voices per engine.
    #[allow(clippy::too_many_arguments)]
    pub fn create_source_voice(
        &mut self,
        engine: EngineId,
        format: &WaveFormat,
        flags: u32,
        max_frequency_ratio: f32,
        callback: u64,
        send_list: u64,
        effect_chain: u64,
    ) -> Result<VoiceId, Error> {
        let engine_state = self
            .engines
            .get_mut(engine.slot, engine.generation)
            .ok_or(Error::InvalidCall("the engine handle is no longer valid"))?;
        let mastering = engine_state
            .mastering
            .ok_or(Error::InvalidCall("the engine has no mastering voice"))?;
        if self
            .voices
            .get(mastering.slot(), mastering.generation())
            .is_none()
        {
            return Err(Error::InvalidCall("the engine has no mastering voice"));
        }
        if flags != 0 {
            return Err(Error::NotImplemented(
                "source voice flags are not implemented",
            ));
        }
        if callback != 0 || send_list != 0 || effect_chain != 0 {
            return Err(Error::NotImplemented(
                "source voice callbacks, send lists and effect chains are not implemented",
            ));
        }
        if !max_frequency_ratio.is_finite()
            || max_frequency_ratio < XAUDIO2_MIN_FREQ_RATIO
            || max_frequency_ratio > XAUDIO2_MAX_FREQ_RATIO
        {
            return Err(Error::InvalidArg(
                "the maximum frequency ratio is out of range",
            ));
        }
        if engine_state.sources.len() >= MAX_SOURCE_VOICES {
            return Err(Error::OutOfMemory("the source voice limit is reached"));
        }
        let state = SourceState {
            format: *format,
            max_frequency_ratio,
            volume: 1.0,
            frequency_ratio: 1.0,
            started: false,
            samples_played: 0,
            queue: VecDeque::new(),
        };
        let (slot, generation) = self.voices.insert(Voice {
            kind: VoiceKind::Source(state),
            engine,
        });
        let handle = VoiceId::new(slot, generation);
        engine_state.sources.push(handle);
        Ok(handle)
    }

    /// Destroys a voice and the buffers it holds.
    ///
    /// A mastering voice cannot go away while the engine still has source
    /// voices, because those voices would lose their destination.
    pub fn destroy_voice(&mut self, voice: VoiceId) -> Result<(), Error> {
        let (engine, mastering) = {
            let state = self
                .voices
                .get(voice.slot(), voice.generation())
                .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
            (state.engine, matches!(state.kind, VoiceKind::Mastering(_)))
        };
        if mastering {
            let engine_state = self
                .engines
                .get(engine.slot, engine.generation)
                .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
            if !engine_state.sources.is_empty() {
                return Err(Error::InvalidCall("the engine still has source voices"));
            }
        }
        self.voices.remove(voice.slot());
        if let Some(engine_state) = self.engines.get_mut(engine.slot, engine.generation) {
            if mastering {
                engine_state.mastering = None;
            } else {
                engine_state.sources.retain(|source| *source != voice);
            }
        }
        Ok(())
    }

    /// The engine a voice belongs to.
    pub fn engine_of(&self, voice: VoiceId) -> Result<EngineId, Error> {
        let state = self
            .voices
            .get(voice.slot(), voice.generation())
            .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
        Ok(state.engine)
    }

    /// The channel count and sample rate of a mastering voice.
    pub fn mastering_format(&self, voice: VoiceId) -> Result<(u32, u32), Error> {
        let state = self
            .voices
            .get(voice.slot(), voice.generation())
            .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
        match &state.kind {
            VoiceKind::Mastering(mastering) => Ok((mastering.channels, mastering.sample_rate)),
            VoiceKind::Source(_) => Err(Error::InvalidCall("the voice is not a mastering voice")),
        }
    }

    /// The channel count and sample rate of a voice.
    ///
    /// A mastering voice reports the format of the engine it mixes to, and a
    /// source voice the format the guest created it with.
    pub fn voice_format(&self, voice: VoiceId) -> Result<(u32, u32), Error> {
        let state = self
            .voices
            .get(voice.slot(), voice.generation())
            .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
        Ok(match &state.kind {
            VoiceKind::Mastering(mastering) => (mastering.channels, mastering.sample_rate),
            VoiceKind::Source(source) => {
                (u32::from(source.format.channels), source.format.sample_rate)
            }
        })
    }
}
