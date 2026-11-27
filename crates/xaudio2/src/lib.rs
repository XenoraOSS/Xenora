// SPDX-License-Identifier: GPL-3.0-or-later

//! A bounded model of the XAudio2 audio API (#888).
//!
//! The crate models the part of XAudio2 a guest program can observe: the
//! lifetime of engines and voices, the buffer queue of a source voice, the
//! fixed sample clock, the voice state and the result codes the API reports.
//! Every rule it applies is recorded in docs/runtime/xaudio2.md, together
//! with the rules that come from the public XAudio2 documentation and the
//! rules that are Xenora choices.
//!
//! The model stops at the boundary of the guest address space. Audio data is
//! named by a guest address and a byte count; the crate never reads, copies,
//! mixes or resamples samples, and it loads no guest code. Mixing belongs to
//! the audio crate that another contributor owns.
//!
//! Nothing here is audible and nothing here runs guest code. The evidence for
//! this crate is the local test suite of the crate and the acceptance test in
//! crates/xaudio2/tests/boundary.rs.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod buffer;
pub mod consts;
pub mod engine;
pub mod format;
pub mod hresult;
pub mod runtime;
pub mod voice;

pub use buffer::{BufferDesc, Event, VoiceState};
pub use engine::EngineId;
pub use format::{SampleFormat, WaveFormat};
pub use hresult::Error;
pub use runtime::{Diagnostics, Runtime};
pub use voice::VoiceId;

/// The number of engines one runtime holds at a time.
pub const MAX_ENGINES: usize = 8;

/// The number of source voices one engine holds at a time.
pub const MAX_SOURCE_VOICES: usize = 256;
