// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounds-checked UCRT string copy and append: `strcpy_s`, `strcat_s`,
//! `strncpy_s`, `strncat_s` and their `wcs` counterparts.
//!
//! Behaviour follows the public UCRT reference pages for those functions.
//! Each returns its `errno_t` and also stores it in `errno`, except
//! `STRUNCATE`, which is a successful truncation. Where those pages invoke the
//! invalid-parameter handler, the handler is not modelled: the function
//! returns the documented error instead. Sizes and counts are in characters.
//! When an error is reported after the destination was validated, its first
//! character is set to the terminator, as documented.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError};
use std::sync::Arc;

const UCRT: &str = "ucrtbase.dll";
const CRT_STRING_API: &str = "api-ms-win-crt-string-l1-1-0.dll";
const EINVAL: u32 = 22;
const ERANGE: u32 = 34;
const STRUNCATE: u32 = 80;
/// `_TRUNCATE`: copy as much as fits.
const TRUNCATE: u64 = u64::MAX;
/// Longest string, in characters, these functions scan.
const SCAN_LIMIT: u64 = 1 << 20;

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

#[derive(Clone, Copy)]
struct Unit(u64);

impl Unit {
    fn at(self, base: u64, index: u64) -> Result<u64, ExitReason> {
        index
            .checked_mul(self.0)
            .and_then(|offset| base.checked_add(offset))
            .ok_or(fault(MemoryError::Overflow))
    }

    fn read(self, memory: &GuestMemory, base: u64, index: u64) -> Result<u16, ExitReason> {
        let address = self.at(base, index)?;
        match self.0 {
            1 => memory.read_u8(address).map(u16::from),
            _ => memory.read_u16(address),
        }
        .map_err(fault)
    }

    fn write(
        self,
        memory: &mut GuestMemory,
        base: u64,
        index: u64,
        value: u16,
    ) -> Result<(), ExitReason> {
        let address = self.at(base, index)?;
        match self.0 {
            1 => memory.write_u8(address, value as u8),
            _ => memory.write_u16(address, value),
        }
        .map_err(fault)
    }

    /// Characters before the terminator, if one occurs within `limit`.
    fn length(
        self,
        memory: &GuestMemory,
        base: u64,
        limit: u64,
    ) -> Result<Option<u64>, ExitReason> {
        for index in 0..limit.min(SCAN_LIMIT) {
            if self.read(memory, base, index)? == 0 {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

    fn copy(
        self,
        memory: &mut GuestMemory,
        to: u64,
        from: u64,
        count: u64,
    ) -> Result<(), ExitReason> {
        for index in 0..count {
            let value = self.read(memory, from, index)?;
            self.write(memory, to, index, value)?;
        }
        Ok(())
    }
}

struct Call<'a> {
    memory: &'a mut GuestMemory,
    teb: u64,
    unit: Unit,
}

impl Call<'_> {
    fn finish(&mut self, error: u32) -> Result<u64, ExitReason> {
        if error != 0 && error != STRUNCATE {
            let address = self.memory.crt_errno_address(self.teb).map_err(fault)?;
            self.memory.write_u32(address, error).map_err(fault)?;
        }
        Ok(u64::from(error))
    }

    /// Clear the destination and report `error`.
    fn reset(&mut self, dest: u64, error: u32) -> Result<u64, ExitReason> {
        self.unit.write(self.memory, dest, 0, 0)?;
        self.finish(error)
    }

    /// Write `count` characters of `src` at `dest + start`, then a terminator.
    fn place(&mut self, dest: u64, start: u64, src: u64, count: u64) -> Result<(), ExitReason> {
        let at = self.unit.at(dest, start)?;
        self.unit.copy(self.memory, at, src, count)?;
        self.unit.write(self.memory, dest, start + count, 0)
    }

    /// `strncpy_s` when `count` is `None` means `strcpy_s`; the append
    /// variants first find the end of `dest`.
    fn run(
        &mut self,
        dest: u64,
        size: u64,
        src: u64,
        count: Option<u64>,
        append: bool,
    ) -> Result<u64, ExitReason> {
        if dest == 0 || size == 0 {
            return self.finish(EINVAL);
        }
        let start = if append {
            match self.unit.length(self.memory, dest, size)? {
                Some(length) => length,
                // An unterminated destination is invalid.
                None => return self.reset(dest, EINVAL),
            }
        } else {
            0
        };
        if count == Some(0) {
            // Nothing to copy, even from a null source.
            self.unit.write(self.memory, dest, start, 0)?;
            return self.finish(0);
        }
        if src == 0 {
            return self.reset(dest, EINVAL);
        }
        let room = size - start;
        let truncate = count == Some(TRUNCATE);
        let wanted_limit = match count {
            Some(count) if !truncate => count,
            _ => room.saturating_add(1),
        };
        // Characters of `src` that would be copied, bounded by what can matter.
        let length = self
            .unit
            .length(self.memory, src, wanted_limit)?
            .unwrap_or(wanted_limit);
        if length < room {
            self.place(dest, start, src, length)?;
            return self.finish(0);
        }
        if truncate {
            self.place(dest, start, src, room - 1)?;
            return self.finish(STRUNCATE);
        }
        self.reset(dest, ERANGE)
    }
}

pub(super) fn register(registry: &mut ImportRegistry, _: &Arc<ServicesState>) {
    for (symbol, width, counted, append) in [
        ("strcpy_s", 1, false, false),
        ("strcat_s", 1, false, true),
        ("strncpy_s", 1, true, false),
        ("strncat_s", 1, true, true),
        ("wcscpy_s", 2, false, false),
        ("wcscat_s", 2, false, true),
        ("wcsncpy_s", 2, true, false),
        ("wcsncat_s", 2, true, true),
    ] {
        registry.register_with_status(
            UCRT,
            symbol,
            ServiceStatus::Partial,
            move |args, cpu, memory| {
                let count = counted.then_some(args.r9);
                Call {
                    memory,
                    teb: cpu.gs_base,
                    unit: Unit(width),
                }
                .run(args.rcx, args.rdx, args.r8, count, append)
            },
        );
        registry.alias(CRT_STRING_API, UCRT, symbol);
    }
}
