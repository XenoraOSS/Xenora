// SPDX-License-Identifier: GPL-3.0-or-later

//! The engine clock of the bounded model.
//!
//! The mixer of issue #306 owns the real clock: it reads the buffers a voice
//! queued and moves the sample count of every voice forward. Until that
//! change lands, this module moves the sample count of the started source
//! voices of an engine forward by one quantum when the guest asks a voice for
//! its state. The clock is a stand-in: it produces no audio, it reads no
//! samples and it knows nothing about a device.

use crate::consts::XAUDIO2_QUANTUM_DENOMINATOR;
use crate::engine::EngineId;
use crate::hresult::Error;
use crate::runtime::Runtime;
use crate::voice::{VoiceId, VoiceKind};

impl Runtime {
    /// Starts the clock of an engine.
    ///
    /// A new engine already runs its clock, so this call only starts a clock
    /// that the guest stopped before.
    pub fn start_engine(&mut self, engine: EngineId) -> Result<(), Error> {
        let state = self
            .engines
            .get_mut(engine.slot(), engine.generation())
            .ok_or(Error::InvalidCall("the engine handle is no longer valid"))?;
        state.clock_running = true;
        Ok(())
    }

    /// Stops the clock of an engine.
    ///
    /// A stopped clock leaves every voice where it is: the sample count of a
    /// voice does not move until the guest starts the engine again.
    pub fn stop_engine(&mut self, engine: EngineId) -> Result<(), Error> {
        let state = self
            .engines
            .get_mut(engine.slot(), engine.generation())
            .ok_or(Error::InvalidCall("the engine handle is no longer valid"))?;
        state.clock_running = false;
        Ok(())
    }

    /// Whether the clock of an engine runs.
    pub fn engine_clock_running(&self, engine: EngineId) -> Result<bool, Error> {
        let state = self
            .engines
            .get(engine.slot(), engine.generation())
            .ok_or(Error::InvalidCall("the engine handle is no longer valid"))?;
        Ok(state.clock_running)
    }

    /// Advances every started source voice of an engine by one quantum.
    ///
    /// A quantum is a hundredth of a second of the mastering rate of the
    /// engine: 480 frames at 48000 hertz. The quantum of the mastering rate
    /// becomes frames of the source rate with the frequency ratio of the
    /// voice, the quantum times the ratio times the source rate divided by
    /// the mastering rate, rounded down.
    ///
    /// The call reports the quantum in mastering frames, which is how far the
    /// clock of the engine moved. The events the voices reached are not
    /// reported: a voice without a notification callback has nowhere to send
    /// them.
    pub fn advance_engine_clock(&mut self, engine: EngineId) -> Result<u32, Error> {
        let (sources, mastering, running) = {
            let state = self
                .engines
                .get(engine.slot(), engine.generation())
                .ok_or(Error::InvalidCall("the engine handle is no longer valid"))?;
            (state.sources.clone(), state.mastering, state.clock_running)
        };
        if !running {
            return Ok(0);
        }
        let Some(mastering) = mastering else {
            return Ok(0);
        };
        let (_, mastering_rate) = self.mastering_format(mastering)?;
        if mastering_rate == 0 {
            return Ok(0);
        }
        let quantum = mastering_rate / XAUDIO2_QUANTUM_DENOMINATOR;
        for source in sources {
            let frames = self.source_frames_for(source, quantum, mastering_rate)?;
            self.consume(source, frames)?;
        }
        Ok(quantum)
    }

    /// The frames of the source rate that a quantum of the mastering rate
    /// becomes for one voice.
    fn source_frames_for(
        &self,
        voice: VoiceId,
        quantum: u32,
        mastering_rate: u32,
    ) -> Result<u32, Error> {
        let state = self
            .voices
            .get(voice.slot(), voice.generation())
            .ok_or(Error::InvalidCall("the voice handle is no longer valid"))?;
        let VoiceKind::Source(source) = &state.kind else {
            return Ok(0);
        };
        let frames = f64::from(quantum)
            * f64::from(source.frequency_ratio)
            * f64::from(source.format.sample_rate)
            / f64::from(mastering_rate);
        Ok(frames.floor().clamp(0.0, f64::from(u32::MAX)) as u32)
    }
}
