// SPDX-License-Identifier: GPL-3.0-or-later

//! Reversible pointer encoding for guest code.
//!
//! Windows exposes a per-process pointer cookie through `EncodePointer` and
//! `DecodePointer`, but does not publish a stable algorithm for it. Xenora
//! therefore uses a deterministic synthetic cookie derived from the guest
//! process id. The pair is exactly invertible inside one process, which is the
//! contract callers rely on, without claiming Windows cookie compatibility.

use super::{KERNEL32, ServicesState};
use crate::imports::{ImportRegistry, ServiceStatus};
use std::sync::Arc;

const COOKIE_BASE: u64 = 0x9E37_79B9_7F4A_7C15;
const COOKIE_MIX: u64 = 0xD6E8_FEB8_6659_FD93;
const ROTATION: u32 = 13;

fn cookie(process_id: u32) -> u64 {
    COOKIE_BASE ^ u64::from(process_id).wrapping_mul(COOKIE_MIX)
}

fn encode(value: u64, process_id: u32) -> u64 {
    (value ^ cookie(process_id)).rotate_left(ROTATION)
}

fn decode(value: u64, process_id: u32) -> u64 {
    value.rotate_right(ROTATION) ^ cookie(process_id)
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let process_id = state.process_id;
    registry.register(KERNEL32, "EncodePointer", move |args, _, _| {
        Ok(encode(args.rcx, process_id))
    });
    registry.register(KERNEL32, "DecodePointer", move |args, _, _| {
        Ok(decode(args.rcx, process_id))
    });
    for symbol in ["EncodePointer", "DecodePointer"] {
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_is_reversible_for_every_representative_pointer() {
        for process_id in [0, 1, 42, u32::MAX] {
            for value in [0, 1, 0x1000, 0x1234_5678_9ABC_DEF0, u64::MAX] {
                assert_eq!(decode(encode(value, process_id), process_id), value);
            }
        }
    }

    #[test]
    fn the_cookie_is_process_local_and_deterministic() {
        let value = 0x1_4000_0000;
        assert_eq!(cookie(7), cookie(7));
        assert_ne!(cookie(7), cookie(8));
        assert_ne!(encode(value, 7), encode(value, 8));
    }
}
