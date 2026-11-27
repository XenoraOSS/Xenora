// SPDX-License-Identifier: GPL-3.0-or-later

//! Result codes for the bounded XAudio2 core model.
//!
//! The values are the public HRESULTs that the XAudio2 documentation names:
//! the shared Win32 codes E_INVALIDARG, E_POINTER, E_NOTIMPL and
//! E_OUTOFMEMORY, and the XAudio2 specific XAUDIO2_E_INVALID_CALL.

use std::fmt;

/// The successful result code.
pub const S_OK: u32 = 0;

/// E_INVALIDARG: an argument was outside the documented domain.
pub const E_INVALIDARG: u32 = 0x8007_0057;

/// E_POINTER: a required pointer was null.
pub const E_POINTER: u32 = 0x8000_4003;

/// E_NOTIMPL: the argument was well formed but the call is not modelled.
pub const E_NOTIMPL: u32 = 0x8000_4001;

/// E_OUTOFMEMORY: a documented resource limit was reached.
pub const E_OUTOFMEMORY: u32 = 0x8007_000E;

/// XAUDIO2_E_INVALID_CALL: the call does not apply to the object.
pub const XAUDIO2_E_INVALID_CALL: u32 = 0x8896_0001;

/// The failure a bounded XAudio2 call reports.
///
/// Each variant carries a short static explanation of the refused call. The
/// variants mirror the result codes the public XAudio2 documentation names:
/// an out-of-domain argument is E_INVALIDARG, a call that does not apply to
/// the object is XAUDIO2_E_INVALID_CALL, a well formed but unmodelled
/// feature is E_NOTIMPL, a reached resource limit is E_OUTOFMEMORY and a
/// null pointer is E_POINTER.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// An argument was outside the documented domain (E_INVALIDARG).
    InvalidArg(&'static str),
    /// The call does not apply to the object (XAUDIO2_E_INVALID_CALL).
    InvalidCall(&'static str),
    /// The call is well formed but not modelled (E_NOTIMPL).
    NotImplemented(&'static str),
    /// A documented resource limit was reached (E_OUTOFMEMORY).
    OutOfMemory(&'static str),
    /// A required pointer was null (E_POINTER).
    Pointer(&'static str),
}

impl Error {
    /// The HRESULT the guest observes for this failure.
    pub fn hresult(self) -> u32 {
        match self {
            Error::InvalidArg(_) => E_INVALIDARG,
            Error::InvalidCall(_) => XAUDIO2_E_INVALID_CALL,
            Error::NotImplemented(_) => E_NOTIMPL,
            Error::OutOfMemory(_) => E_OUTOFMEMORY,
            Error::Pointer(_) => E_POINTER,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (code, message) = match self {
            Error::InvalidArg(message) => (E_INVALIDARG, message),
            Error::InvalidCall(message) => (XAUDIO2_E_INVALID_CALL, message),
            Error::NotImplemented(message) => (E_NOTIMPL, message),
            Error::OutOfMemory(message) => (E_OUTOFMEMORY, message),
            Error::Pointer(message) => (E_POINTER, message),
        };
        write!(f, "{code:#010x}: {message}")
    }
}

impl std::error::Error for Error {}
