// SPDX-License-Identifier: GPL-3.0-or-later

//! Environment block services. The environment itself is empty - the same
//! table the C runtime hands to `_environ` - so expansion is the identity:
//! with no variable defined, every `%name%` is copied verbatim, which is
//! exactly what the documented behavior produces. The value of these
//! services is mechanical: programs that expand strings or walk the block
//! keep running instead of stopping at a missing import, and the size
//! semantics are the documented ones.

use super::{ERROR_INVALID_PARAMETER, KERNEL32, ServicesState};
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::PROCESS_HEAP;
use std::sync::Arc;

const PROCESSENV_API: &str = "api-ms-win-core-processenvironment-l1-1-0.dll";

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    for (symbol, wide) in [
        ("ExpandEnvironmentStringsW", true),
        ("ExpandEnvironmentStringsA", false),
    ] {
        registry.register(KERNEL32, symbol, move |args, _, memory| {
            let fault = ExitReason::MemoryFault;
            let source = args.get(0, memory).map_err(fault)?;
            let target = args.get(1, memory).map_err(fault)?;
            let capacity = args.get(2, memory).map_err(fault)?;

            // Read the source in the service's width.
            let mut bytes: Vec<u8> = Vec::new();
            let mut units_in = 0u64;
            loop {
                let unit = if wide {
                    memory.read_u16(source + 2 * units_in).map_err(fault)?
                } else {
                    u16::from(memory.read_u8(source + units_in).map_err(fault)?)
                };
                if unit == 0 {
                    break;
                }
                units_in += 1;
                if wide {
                    bytes.extend_from_slice(&unit.to_le_bytes());
                } else {
                    bytes.push(unit as u8);
                }
                if units_in > 32768 {
                    return Ok(0);
                }
            }

            // The count of the copy, terminator included, in the service's
            // units: the documentation counts the null in both the success
            // return and the required size.
            let needed = units_in + 1;
            if needed > capacity {
                // A small buffer answers the required size; what it holds
                // is undefined, and nothing is written here.
                return Ok(needed);
            }
            bytes.extend_from_slice(if wide { &[0, 0] } else { &[0] });
            memory.write_bytes(target, &bytes).map_err(fault)?;
            Ok(needed)
        });
    }

    // The environment block: empty, which is one empty string and the block
    // terminator - two null units. Each call hands the caller its own copy
    // from the process heap, because the documentation requires the caller
    // to free it with FreeEnvironmentStringsW.
    registry.register(KERNEL32, "GetEnvironmentStringsW", |_, _, memory| {
        // Fresh heap memory is zero-filled, so four zero bytes are the two
        // null wide units of the empty block.
        Ok(memory.map_heap_allocation(PROCESS_HEAP, 4).unwrap_or(0))
    });

    let service_state = Arc::clone(state);
    registry.register(
        KERNEL32,
        "FreeEnvironmentStringsW",
        move |args, cpu, memory| {
            let block = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
            if block != 0 && memory.release_heap_allocation(PROCESS_HEAP, block) {
                Ok(1)
            } else {
                service_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0)
            }
        },
    );

    for symbol in [
        "ExpandEnvironmentStringsA",
        "ExpandEnvironmentStringsW",
        "FreeEnvironmentStringsW",
        "GetEnvironmentStringsW",
    ] {
        registry.alias(PROCESSENV_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
}
