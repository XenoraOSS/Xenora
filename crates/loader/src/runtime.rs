// SPDX-License-Identifier: GPL-3.0-or-later

//! Typed Win64 adapters for generated service registrations.
//!
//! `runtime_table.rs` is generated from `runtime/services.txt`. Each adapter
//! reads typed arguments by position (Microsoft x64 calling convention), calls a
//! handler that sees only guest memory and service state, and places the typed
//! result. Handlers never receive the CPU, so they cannot disturb nonvolatile
//! registers.

use crate::cpu::{CpuState, ExitReason};
use crate::imports::{ServiceStatus, Win64Args};
use crate::memory::{GuestMemory, MemoryError};
use crate::services::ServicesState;

/// What a service may use.
pub struct ServiceContext<'a> {
    pub state: &'a ServicesState,
    pub teb: u64,
    pub memory: &'a mut GuestMemory,
}

/// Guest pointer to `N` bytes the service writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutPtr<const N: usize>(pub u64);

impl<const N: usize> OutPtr<N> {
    /// Check the entire output span for writes without changing guest memory.
    pub fn check_write(self, memory: &GuestMemory) -> Result<(), MemoryError> {
        memory.check_write(self.0, N)
    }

    /// Checked, all-or-nothing store.
    pub fn store(self, memory: &mut GuestMemory, bytes: [u8; N]) -> Result<(), MemoryError> {
        memory.write_bytes(self.0, &bytes)
    }
}

/// Raw bits of a floating-point argument or result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Float32(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Float64(pub u64);

/// Reads typed arguments. Only the low bits of a narrow argument are defined.
pub struct Call<'a> {
    args: &'a Win64Args,
    cpu: &'a CpuState,
    memory: &'a GuestMemory,
}

impl<'a> Call<'a> {
    pub fn new(args: &'a Win64Args, cpu: &'a CpuState, memory: &'a GuestMemory) -> Self {
        Self { args, cpu, memory }
    }

    pub fn u64(&self, index: usize) -> Result<u64, ExitReason> {
        self.args
            .get(index, self.memory)
            .map_err(ExitReason::MemoryFault)
    }

    pub fn u32(&self, index: usize) -> Result<u32, ExitReason> {
        Ok(self.u64(index)? as u32)
    }

    pub fn i32(&self, index: usize) -> Result<i32, ExitReason> {
        Ok(self.u32(index)? as i32)
    }

    pub fn bool32(&self, index: usize) -> Result<bool, ExitReason> {
        Ok(self.u32(index)? != 0)
    }

    pub fn out<const N: usize>(&self, index: usize) -> Result<OutPtr<N>, ExitReason> {
        self.u64(index).map(OutPtr)
    }

    /// The first four positions use XMM0-XMM3; later ones use the stack.
    fn float_bits(&self, index: usize) -> Result<u64, ExitReason> {
        if index < 4 {
            Ok(self.cpu.xmm[index] as u64)
        } else {
            self.u64(index)
        }
    }

    pub fn f32(&self, index: usize) -> Result<Float32, ExitReason> {
        Ok(Float32(self.float_bits(index)? as u32))
    }

    pub fn f64(&self, index: usize) -> Result<Float64, ExitReason> {
        self.float_bits(index).map(Float64)
    }
}

/// Places a typed result and returns the value for RAX.
pub trait Return {
    fn place(self, cpu: &mut CpuState) -> u64;
}

impl Return for u32 {
    fn place(self, _: &mut CpuState) -> u64 {
        u64::from(self)
    }
}

impl Return for i32 {
    fn place(self, _: &mut CpuState) -> u64 {
        u64::from(self as u32)
    }
}

impl Return for u64 {
    fn place(self, _: &mut CpuState) -> u64 {
        self
    }
}

impl Return for bool {
    fn place(self, _: &mut CpuState) -> u64 {
        u64::from(self)
    }
}

impl Return for Float32 {
    fn place(self, cpu: &mut CpuState) -> u64 {
        cpu.xmm[0] = u128::from(self.0);
        0
    }
}

impl Return for Float64 {
    fn place(self, cpu: &mut CpuState) -> u64 {
        cpu.xmm[0] = u128::from(self.0);
        0
    }
}

/// A declared parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parameter {
    pub name: &'static str,
    pub kind: &'static str,
}

/// Static description of a generated service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceMetadata {
    pub library: &'static str,
    pub symbol: &'static str,
    pub parameters: &'static [Parameter],
    pub result: &'static str,
    pub status: ServiceStatus,
    /// API sets that resolve to this service.
    pub aliases: &'static [&'static str],
    pub failure: &'static str,
    pub reference: &'static str,
    pub tests: &'static [&'static str],
}
/// A 32-bit guest HRESULT returned through the Win64 integer result register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HResult(u32);

impl HResult {
    pub(crate) const OK: Self = Self(0);
    pub(crate) const FAIL: Self = Self(0x8000_4005);

    /// Convert using the Windows SDK's HRESULT_FROM_WIN32 rules.
    pub(crate) const fn from_win32(code: u32) -> Self {
        if (code as i32) <= 0 {
            Self(code)
        } else {
            Self(0x8007_0000 | (code & 0xffff))
        }
    }

    pub(crate) fn guest_return(self) -> u64 {
        u64::from(self.0)
    }
}

#[cfg(test)]
#[test]
fn hresult_win32_conversion_preserves_status_bits() {
    assert_eq!(HResult::from_win32(0), HResult::OK);
    assert_eq!(HResult::from_win32(0x8000_4005), HResult::FAIL);
    assert_eq!(HResult::from_win32(0x10057).guest_return(), 0x8007_0057);
}
