// SPDX-License-Identifier: GPL-3.0-or-later

//! Environment block services. The environment starts empty - no host
//! variable passes through - and `SetEnvironmentVariable` maintains it:
//! queries answer case-insensitively with the documented size semantics,
//! expansion resolves `%name%` with unresolved names copied verbatim, and
//! the block strings come sorted upper-case first. The C runtime's own
//! `_environ` copy does not follow these writes, which is what Windows
//! programs observe: only the CRT's own `_putenv` would update that copy.

use super::codepages::{self, CodePage};
use super::{ERROR_INVALID_PARAMETER, ERROR_NOT_ENOUGH_MEMORY, KERNEL32, ServicesState};
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, PROCESS_HEAP};
use std::sync::{Arc, Mutex};

const PROCESSENV_API: &str = "api-ms-win-core-processenvironment-l1-1-0.dll";

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    for (symbol, wide) in [
        ("ExpandEnvironmentStringsW", true),
        ("ExpandEnvironmentStringsA", false),
    ] {
        let service_state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, _, memory| {
            let fault = ExitReason::MemoryFault;
            let source = args.get(0, memory).map_err(fault)?;
            let target = args.get(1, memory).map_err(fault)?;
            let capacity = args.get(2, memory).map_err(fault)?;

            // Read the source in the service's width.
            let mut input: Vec<u16> = Vec::new();
            loop {
                let unit = if wide {
                    memory
                        .read_u16(source + 2 * input.len() as u64)
                        .map_err(fault)?
                } else {
                    u16::from(memory.read_u8(source + input.len() as u64).map_err(fault)?)
                };
                if unit == 0 {
                    break;
                }
                input.push(unit);
                if input.len() > 32768 {
                    return Ok(0);
                }
            }

            // Expand each %name% against the process environment. A name
            // that resolves to nothing keeps its percent signs verbatim,
            // the documented behaviour.
            let mut output: Vec<u16> = Vec::new();
            let mut index = 0;
            while index < input.len() {
                if input[index] == u16::from(b'%')
                    && let Some(end) = input[index + 1..]
                        .iter()
                        .position(|&unit| unit == u16::from(b'%'))
                {
                    let close = index + 1 + end;
                    // Only decode the token name. Literal units stay in their
                    // original encoding even when a lookup cannot be made.
                    let name_units = &input[index + 1..close];
                    let name = if wide {
                        String::from_utf16(name_units).ok()
                    } else {
                        let bytes = name_units.iter().map(|&unit| unit as u8).collect();
                        String::from_utf8(bytes).ok()
                    };
                    if let Some(name) = name.filter(|name| !name.is_empty())
                        && let Some(value) = service_state.environment_variable_value(&name)
                    {
                        if wide {
                            output.extend(value.encode_utf16());
                        } else {
                            output.extend(value.bytes().map(u16::from));
                        }
                        index = close + 1;
                        continue;
                    }
                }
                output.push(input[index]);
                index += 1;
            }

            let mut bytes: Vec<u8> = Vec::new();
            for unit in &output {
                if wide {
                    bytes.extend_from_slice(&unit.to_le_bytes());
                } else {
                    bytes.push(*unit as u8);
                }
            }

            // The count of the copy, terminator included, in the service's
            // units: the documentation counts the null in both the success
            // return and the required size.
            let needed = output.len() as u64 + 1;
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

    // The environment block: the variables as NAME=value strings sorted
    // upper-case first, each terminated, and the block terminator after the
    // last one. Each call hands the caller its own copy from the process
    // heap, because the documentation requires the caller to free it with
    // FreeEnvironmentStringsW. An empty environment is the two terminators.
    let service_state = Arc::clone(state);
    registry.register(KERNEL32, "GetEnvironmentStringsW", move |_, _, memory| {
        let bytes: Vec<u8> = environment_units(&service_state)
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        let length = bytes.len();
        let target = memory
            .map_heap_allocation(PROCESS_HEAP, length)
            .unwrap_or(0);
        if target == 0 {
            return Ok(0);
        }
        if memory.write_bytes(target, &bytes).is_err() {
            return Ok(0);
        }
        Ok(target)
    });

    register_narrow_environment_block(registry, state);

    // Set and delete variables. The name must be non-empty and free of '=';
    // a null value deletes the variable. The C runtime's own copy does not
    // follow, which is what Windows programs observe.
    for (symbol, wide) in [
        ("SetEnvironmentVariableW", true),
        ("SetEnvironmentVariableA", false),
    ] {
        let service_state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let fault = ExitReason::MemoryFault;
            let name_ptr = args.get(0, memory).map_err(fault)?;
            let value_ptr = args.get(1, memory).map_err(fault)?;
            if name_ptr == 0 {
                return service_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let name = if wide {
                read_wide(memory, name_ptr)?
            } else {
                read_narrow(memory, name_ptr)?
            };
            if name.is_empty() || name.contains('=') {
                return service_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let value = if value_ptr == 0 {
                None
            } else if wide {
                Some(read_wide(memory, value_ptr)?)
            } else {
                Some(read_narrow(memory, value_ptr)?)
            };
            service_state.set_environment_variable(&name, value.as_deref());
            Ok(1)
        });
    }

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
        "SetEnvironmentVariableA",
        "SetEnvironmentVariableW",
    ] {
        registry.alias(PROCESSENV_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
}

fn register_narrow_environment_block(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    // Give narrow copies their own allocation owner, so neither the wide
    // release nor a process-heap free can release them.
    let heap = Arc::new(Mutex::new(None));
    for symbol in ["GetEnvironmentStrings", "GetEnvironmentStringsA"] {
        let service_state = Arc::clone(state);
        let heap = Arc::clone(&heap);
        registry.register_with_status(
            KERNEL32,
            symbol,
            ServiceStatus::Partial,
            move |_, cpu, memory| {
                let units = environment_units(&service_state);
                let (bytes, _) = codepages::encode(CodePage::Cp437, &units, b'?');

                let Some(heap) = narrow_heap(&heap, memory) else {
                    return service_state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
                };
                let Ok(target) = memory.map_heap_allocation(heap, bytes.len()) else {
                    return service_state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
                };
                if let Err(error) = memory.write_bytes(target, &bytes) {
                    memory.release_heap_allocation(heap, target);
                    return Err(ExitReason::MemoryFault(error));
                }
                Ok(target)
            },
        );
    }

    let service_state = Arc::clone(state);
    registry.register_with_status(
        KERNEL32,
        "FreeEnvironmentStringsA",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let heap = *heap.lock().expect("narrow environment heap");
            if args.rcx != 0
                && heap.is_some_and(|heap| memory.release_heap_allocation(heap, args.rcx))
            {
                Ok(1)
            } else {
                service_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0)
            }
        },
    );
    for symbol in ["GetEnvironmentStrings", "FreeEnvironmentStringsA"] {
        registry.alias(PROCESSENV_API, KERNEL32, symbol);
    }
}

fn environment_units(state: &ServicesState) -> Vec<u16> {
    let entries = state.environment_block_entries();
    let mut units = Vec::new();
    for (name, value) in &entries {
        units.extend(name.encode_utf16());
        units.push(u16::from(b'='));
        units.extend(value.encode_utf16());
        units.push(0);
    }
    units.push(0);
    if entries.is_empty() {
        units.push(0);
    }
    units
}

fn narrow_heap(stored: &Mutex<Option<u64>>, memory: &mut GuestMemory) -> Option<u64> {
    let mut stored = stored.lock().expect("narrow environment heap");
    if let Some(heap) = *stored
        && memory.has_heap(heap)
    {
        return Some(heap);
    }
    let heap = memory.create_private_heap(0)?;
    *stored = Some(heap);
    Some(heap)
}

/// Read a null-terminated wide string as text.
fn read_wide(
    memory: &crate::memory::GuestMemory,
    address: u64,
) -> Result<String, crate::cpu::ExitReason> {
    let mut units = Vec::new();
    loop {
        let unit = memory
            .read_u16(address + 2 * units.len() as u64)
            .map_err(crate::cpu::ExitReason::MemoryFault)?;
        if unit == 0 {
            break;
        }
        units.push(unit);
        if units.len() > 32768 {
            break;
        }
    }
    Ok(String::from_utf16_lossy(&units))
}

/// Read a null-terminated narrow string as text; the console code page is
/// UTF-8, so narrow text is UTF-8.
fn read_narrow(
    memory: &crate::memory::GuestMemory,
    address: u64,
) -> Result<String, crate::cpu::ExitReason> {
    let mut bytes = Vec::new();
    loop {
        let byte = memory
            .read_u8(address + bytes.len() as u64)
            .map_err(crate::cpu::ExitReason::MemoryFault)?;
        if byte == 0 {
            break;
        }
        bytes.push(byte);
        if bytes.len() > 32768 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
