// SPDX-License-Identifier: GPL-3.0-or-later

//! Guest-visible buffer descriptions, voice state and playback events.
//!
//! The structures in this module are the ones the guest lays out in its own
//! memory and hands to XAudio2. The model reads them in the x64 layout the
//! public documentation describes and never copies guest audio data.

use crate::consts::{
    XAUDIO2_END_OF_STREAM, XAUDIO2_LOOP_INFINITE, XAUDIO2_MAX_BUFFER_BYTES, XAUDIO2_MAX_LOOP_COUNT,
};
use crate::hresult::Error;

/// An XAUDIO2_BUFFER as the guest lays it out on x64.
///
/// The field order and the padding are the x64 layout of the public
/// structure: the flags, the byte count and the 64-bit audio address, then
/// the play region, the loop region and the loop count, then four bytes of
/// padding that the structure carries, and finally the 64-bit client
/// context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferDesc {
    /// Buffer flags. Only XAUDIO2_END_OF_STREAM is understood.
    pub flags: u32,
    /// The size of the audio data in bytes.
    pub audio_bytes: u32,
    /// The guest address of the audio data.
    pub audio_data: u64,
    /// The first frame of the play region, counted from the start of the buffer.
    pub play_begin: u32,
    /// The length of the play region in frames. Zero means the whole buffer.
    pub play_length: u32,
    /// The first frame of the loop region.
    pub loop_begin: u32,
    /// The length of the loop region in frames. Zero means the region runs
    /// from `loop_begin` to the end of the play region.
    pub loop_length: u32,
    /// How many times playback jumps back to `loop_begin`, so the region
    /// plays `loop_count + 1` times. 255 means forever; 0 means no loop.
    pub loop_count: u32,
    /// The opaque client context value the voice reports back.
    pub context: u64,
}

impl BufferDesc {
    /// The size of the XAUDIO2_BUFFER structure on x64, in bytes.
    pub const SIZE: usize = 44;

    /// Reads an XAUDIO2_BUFFER from the bytes the guest laid out.
    ///
    /// The padding at bytes 36 to 40 is not part of any field and is ignored.
    pub fn parse(bytes: &[u8; 44]) -> Self {
        BufferDesc {
            flags: read_u32(bytes, 0),
            audio_bytes: read_u32(bytes, 4),
            audio_data: read_u64(bytes, 8),
            play_begin: read_u32(bytes, 16),
            play_length: read_u32(bytes, 20),
            loop_begin: read_u32(bytes, 24),
            loop_length: read_u32(bytes, 28),
            loop_count: read_u32(bytes, 32),
            context: read_u64(bytes, 36),
        }
    }

    /// The play and loop regions the model derives from this description.
    ///
    /// The plan is computed against the block alignment of the voice, because
    /// every offset and length in a buffer description is counted in frames.
    pub(crate) fn plan(&self, block_align: u16) -> Result<BufferPlan, Error> {
        if self.flags & !XAUDIO2_END_OF_STREAM != 0 {
            return Err(Error::InvalidArg("the buffer carries an unknown flag"));
        }
        if self.audio_data == 0 {
            return Err(Error::Pointer("the buffer audio address is null"));
        }
        if self.audio_bytes == 0 {
            return Err(Error::InvalidArg("the buffer holds no audio data"));
        }
        if self.audio_bytes > XAUDIO2_MAX_BUFFER_BYTES {
            return Err(Error::InvalidArg(
                "the buffer is larger than the documented maximum",
            ));
        }
        let align = u32::from(block_align);
        if !self.audio_bytes.is_multiple_of(align) {
            return Err(Error::InvalidArg(
                "the buffer is not a whole number of frames",
            ));
        }
        if self
            .audio_data
            .checked_add(u64::from(self.audio_bytes))
            .is_none()
        {
            return Err(Error::InvalidArg("the buffer address range overflows"));
        }

        let frames = self.audio_bytes / align;
        let (play_begin, play_end) = if self.play_length == 0 {
            // A play length of zero plays the whole buffer, so the play region must start at zero.
            if self.play_begin != 0 {
                return Err(Error::InvalidArg(
                    "a whole-buffer play region must start at frame zero",
                ));
            }
            (0, frames)
        } else {
            let end = self
                .play_begin
                .checked_add(self.play_length)
                .ok_or(Error::InvalidArg("the play region overflows"))?;
            if end > frames {
                return Err(Error::InvalidArg(
                    "the play region ends past the end of the buffer",
                ));
            }
            (self.play_begin, end)
        };

        let mut loop_begin = 0;
        let mut loop_end = None;
        if self.loop_count == 0 {
            if self.loop_begin != 0 || self.loop_length != 0 {
                return Err(Error::InvalidArg("a loop region needs a loop count"));
            }
        } else {
            if self.loop_count > XAUDIO2_MAX_LOOP_COUNT && self.loop_count != XAUDIO2_LOOP_INFINITE
            {
                return Err(Error::InvalidArg("the loop count is out of range"));
            }
            loop_begin = self.loop_begin;
            // A loop length of zero loops the whole play region.
            let span = if self.loop_length == 0 {
                play_end.checked_sub(loop_begin).ok_or(Error::InvalidArg(
                    "the loop region starts past the play region",
                ))?
            } else {
                self.loop_length
            };
            let end = loop_begin
                .checked_add(span)
                .ok_or(Error::InvalidArg("the loop region overflows"))?;
            if loop_begin >= play_end {
                return Err(Error::InvalidArg(
                    "the loop region starts at or past the play region end",
                ));
            }
            if end <= loop_begin {
                return Err(Error::InvalidArg("the loop region is empty"));
            }
            if end > play_end {
                return Err(Error::InvalidArg(
                    "the loop region ends past the play region end",
                ));
            }
            if end <= play_begin {
                return Err(Error::InvalidArg(
                    "the loop region ends before the play region begins",
                ));
            }
            loop_end = Some(end);
        }

        Ok(BufferPlan {
            play_begin,
            play_end,
            loop_begin,
            loop_end,
        })
    }
}

/// The play and loop regions of one queued buffer, in frames.
pub(crate) struct BufferPlan {
    /// The first frame of the play region.
    pub play_begin: u32,
    /// The frame after the last frame of the play region.
    pub play_end: u32,
    /// The first frame of the loop region.
    pub loop_begin: u32,
    /// The frame after the last frame of the loop region, when the buffer loops.
    pub loop_end: Option<u32>,
}

/// An XAUDIO2_VOICE_STATE as the guest lays it out on x64.
///
/// The layout is the public structure: the 64-bit context of the current
/// buffer, the number of queued buffers, four bytes of padding and the
/// 64-bit count of samples played.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceState {
    /// The client context of the buffer the voice is playing.
    pub current_buffer_context: u64,
    /// The number of buffers the voice holds, the playing one included.
    pub buffers_queued: u32,
    /// The number of samples the voice has played.
    pub samples_played: u64,
}

impl VoiceState {
    /// The size of the XAUDIO2_VOICE_STATE structure on x64, in bytes.
    pub const SIZE: usize = 20;

    /// Writes the voice state in the x64 layout, with zeroed padding.
    pub fn to_bytes(&self) -> [u8; 20] {
        let mut bytes = [0u8; 20];
        bytes[0..8].copy_from_slice(&self.current_buffer_context.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.buffers_queued.to_le_bytes());
        bytes[12..20].copy_from_slice(&self.samples_played.to_le_bytes());
        bytes
    }
}

/// A playback event a source voice reports.
///
/// The events stand for the notifications the public documentation describes
/// for a source voice with a notification callback: the beginning and the end
/// of a buffer, the end of a loop region and the end of the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The voice started to play the buffer with this client context.
    BufferStart {
        /// The client context of the buffer.
        context: u64,
    },
    /// The voice finished the buffer with this client context.
    BufferEnd {
        /// The client context of the buffer.
        context: u64,
    },
    /// The voice finished one pass of the loop region.
    LoopEnd {
        /// The client context of the buffer.
        context: u64,
    },
    /// The voice reached the end of a stream.
    StreamEnd,
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn read_u64(bytes: &[u8], at: usize) -> u64 {
    let mut field = [0u8; 8];
    field.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(field)
}
