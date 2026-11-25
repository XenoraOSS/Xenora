// SPDX-License-Identifier: GPL-3.0-or-later

//! Integer `abs`, `labs`, `llabs`, `div`, `ldiv` and `lldiv`.
//!
//! These follow the Windows LLP64 data model, where `long` is 32 bits. The
//! macOS `long` is 64 bits, so the host `labs` and `ldiv` would read a guest
//! argument's upper half and return the wrong width; they are implemented
//! here instead.
//!
//! Returns follow the Win64 calling convention: `div_t` and `ldiv_t` (two
//! 32-bit fields, 8 bytes) come back in RAX with the quotient in the low half.
//! `lldiv_t` (16 bytes) is returned through a hidden pointer the caller passes
//! in RCX, so the operands move to RDX and R8 and RAX returns that pointer.
//!
//! C leaves the absolute value of the minimum integer, division by zero and
//! the minimum divided by -1 undefined. `abs` wraps, as the x86 `neg`
//! instruction does. The divisions stop the guest with a divide error, as the
//! x86 `idiv` instruction does for the same operands; they never return an
//! invented quotient.

use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::GuestMemory;

const UCRT: &str = "ucrtbase.dll";
const UTILITY_API: &str = "api-ms-win-crt-utility-l1-1-0.dll";

/// Quotient and remainder truncated toward zero, as C requires. `checked_*`
/// fails for exactly the operands on which `idiv` raises a divide error.
fn divide32(numerator: i32, denominator: i32) -> Result<(i32, i32), ExitReason> {
    numerator
        .checked_div(denominator)
        .zip(numerator.checked_rem(denominator))
        .ok_or(ExitReason::DivideError)
}

fn divide64(numerator: i64, denominator: i64) -> Result<(i64, i64), ExitReason> {
    numerator
        .checked_div(denominator)
        .zip(numerator.checked_rem(denominator))
        .ok_or(ExitReason::DivideError)
}

/// Pack a 32-bit quotient and remainder as an 8-byte struct in RAX.
fn pack32((quotient, remainder): (i32, i32)) -> u64 {
    u64::from(quotient as u32) | (u64::from(remainder as u32) << 32)
}

fn write_lldiv(
    memory: &mut GuestMemory,
    result: u64,
    value: (i64, i64),
) -> Result<u64, ExitReason> {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&value.0.to_le_bytes());
    bytes[8..].copy_from_slice(&value.1.to_le_bytes());
    memory
        .write_bytes(result, &bytes)
        .map_err(ExitReason::MemoryFault)?;
    Ok(result)
}

pub(super) fn register(registry: &mut ImportRegistry) {
    // int and long are both 32 bits under LLP64; the result zero-extends
    // into RAX like any 32-bit return.
    for symbol in ["abs", "labs"] {
        registry.register_with_status(UCRT, symbol, ServiceStatus::Implemented, |args, _, _| {
            Ok(u64::from((args.rcx as i32).wrapping_abs() as u32))
        });
    }
    registry.register_with_status(UCRT, "llabs", ServiceStatus::Implemented, |args, _, _| {
        Ok((args.rcx as i64).wrapping_abs() as u64)
    });
    for symbol in ["div", "ldiv"] {
        registry.register_with_status(UCRT, symbol, ServiceStatus::Implemented, |args, _, _| {
            divide32(args.rcx as i32, args.rdx as i32).map(pack32)
        });
    }
    registry.register_with_status(
        UCRT,
        "lldiv",
        ServiceStatus::Implemented,
        |args, _, memory| {
            let value = divide64(args.rdx as i64, args.r8 as i64)?;
            write_lldiv(memory, args.rcx, value)
        },
    );
    for symbol in ["abs", "labs", "llabs", "div", "ldiv", "lldiv"] {
        registry.alias(UTILITY_API, UCRT, symbol);
    }
}
