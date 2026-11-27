// SPDX-License-Identifier: GPL-3.0-or-later

//! Itanium C++ ABI guard-variable services (#934).
//!
//! Function-local statics compiled with thread-safe initialization call
//! `__cxa_guard_acquire`, then `__cxa_guard_release` or `__cxa_guard_abort`.
//! The public Itanium C++ ABI defines an eight-byte guard whose first byte is
//! nonzero after initialization. Xenora keeps the in-progress owner in service
//! state rather than interpreting the rest of a guest pointer as host data.
//!
//! The scheduler runs one guest thread at a time, so a guard already held by
//! another guest thread cannot be waited on here. Contention and recursive
//! acquisition fail with `-1` and `ERROR_INVALID_PARAMETER` instead of
//! silently initializing twice. This is a documented Xenora limitation, not a
//! general implementation of the host ABI's blocking behavior.

use std::collections::BTreeMap;

use super::ERROR_INVALID_PARAMETER;
use crate::cpu::ExitReason;
use crate::runtime::ServiceContext;

/// The stop used for a release or abort that the ABI does not permit.
const UNSUPPORTED: ExitReason = ExitReason::IllegalInstruction(0xFE);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GuardState {
    Initialized,
    Initializing(u64),
}

/// Guest guard state. The key is a guest address; no host pointer is retained.
#[derive(Debug, Default)]
pub(crate) struct GuardTable {
    guards: BTreeMap<u64, GuardState>,
}

fn reject<T>(context: &ServiceContext<'_>, value: T) -> Result<T, ExitReason> {
    context
        .state
        .fail(context.teb, ERROR_INVALID_PARAMETER, value)
}

fn aligned_guard(guard: u64) -> bool {
    guard != 0 && guard.is_multiple_of(8)
}

/// Return 1 when the caller owns initialization, 0 when it is already done,
/// and -1 for an invalid or contended guard.
pub(crate) fn cxx_guard_acquire(
    context: &mut ServiceContext<'_>,
    guard: u64,
) -> Result<i32, ExitReason> {
    if !aligned_guard(guard) {
        return reject(context, -1);
    }
    let initialized = match context.memory.read_u8(guard) {
        Ok(byte) => byte != 0,
        Err(_) => return reject(context, -1),
    };
    let mut table = context.state.cxx_guards.lock().expect("C++ guard table");
    match table.guards.get(&guard) {
        Some(GuardState::Initialized) => return Ok(0),
        Some(GuardState::Initializing(_)) => return reject(context, -1),
        None => {}
    }
    if initialized {
        table.guards.insert(guard, GuardState::Initialized);
        Ok(0)
    } else {
        table
            .guards
            .insert(guard, GuardState::Initializing(context.teb));
        Ok(1)
    }
}

/// Mark a guard initialized after a successful acquire by the same thread.
pub(crate) fn cxx_guard_release(
    context: &mut ServiceContext<'_>,
    guard: u64,
) -> Result<(), ExitReason> {
    if !aligned_guard(guard) {
        return Err(UNSUPPORTED);
    }
    let mut table = context.state.cxx_guards.lock().expect("C++ guard table");
    match table.guards.get_mut(&guard) {
        Some(owner) if *owner == GuardState::Initializing(context.teb) => {
            context
                .memory
                .write_u8(guard, 1)
                .map_err(ExitReason::MemoryFault)?;
            *owner = GuardState::Initialized;
            Ok(())
        }
        _ => Err(UNSUPPORTED),
    }
}

/// Abandon an in-progress initialization after a successful acquire by the
/// same thread. The guard byte stays uninitialized.
pub(crate) fn cxx_guard_abort(
    context: &mut ServiceContext<'_>,
    guard: u64,
) -> Result<(), ExitReason> {
    if !aligned_guard(guard) {
        return Err(UNSUPPORTED);
    }
    let mut table = context.state.cxx_guards.lock().expect("C++ guard table");
    match table.guards.get(&guard) {
        Some(state) if *state == GuardState::Initializing(context.teb) => {
            table.guards.remove(&guard);
            Ok(())
        }
        _ => Err(UNSUPPORTED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_states_are_distinguished_by_thread() {
        assert_ne!(
            GuardState::Initializing(1),
            GuardState::Initializing(0x10000)
        );
        assert_ne!(GuardState::Initializing(1), GuardState::Initialized);
    }
}
