// SPDX-License-Identifier: GPL-3.0-or-later

//! Process environment block for CRT `getenv` and Win32 environment queries.
//!
//! Values are stored in the runtime and copied into durable guest buffers on
//! read. The guest cannot see the host process environment unless an entry is
//! explicitly installed here.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError, PROCESS_HEAP};
use std::collections::BTreeMap;
use std::sync::Arc;

const UCRT: &str = "ucrtbase.dll";
const CRT_RUNTIME_API: &str = "api-ms-win-crt-runtime-l1-1-0.dll";
const KERNEL32: &str = "kernel32.dll";
const PROCESS_ENV_API: &str = "api-ms-win-core-processenvironment-l1-1-0.dll";
const EINVAL: u32 = 22;
const MAX_NAME: usize = 256;
const MAX_VALUE: usize = 32 * 1024;

#[derive(Default)]
pub(super) struct EnvState {
    values: BTreeMap<String, String>,
    /// Guest VA of the last getenv result for each key (ASCII).
    buffers: BTreeMap<String, u64>,
}

impl EnvState {
    pub(super) fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    fn with_defaults() -> Self {
        let mut values = BTreeMap::new();
        values.insert("OS".into(), "Windows_NT".into());
        values.insert("NUMBER_OF_PROCESSORS".into(), "1".into());
        Self {
            values,
            buffers: BTreeMap::new(),
        }
    }
}

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

fn set_errno(memory: &mut GuestMemory, teb: u64, error: u32) -> Result<(), ExitReason> {
    let address = memory.crt_errno_address(teb).map_err(fault)?;
    memory.write_u32(address, error).map_err(fault)
}

fn read_c_string(memory: &GuestMemory, address: u64, limit: usize) -> Result<String, ExitReason> {
    if address == 0 {
        return Err(ExitReason::IllegalInstruction(0xFE));
    }
    let mut bytes = Vec::new();
    for index in 0..limit {
        let byte = memory.read_u8(address + index as u64).map_err(fault)?;
        if byte == 0 {
            return String::from_utf8(bytes).map_err(|_| ExitReason::IllegalInstruction(0xFE));
        }
        bytes.push(byte);
    }
    Err(ExitReason::IllegalInstruction(0xFE))
}

fn read_wide_string(
    memory: &GuestMemory,
    address: u64,
    limit: usize,
) -> Result<String, ExitReason> {
    if address == 0 {
        return Err(ExitReason::IllegalInstruction(0xFE));
    }
    let mut units = Vec::new();
    for index in 0..limit {
        let unit = memory.read_u16(address + index as u64 * 2).map_err(fault)?;
        if unit == 0 {
            return String::from_utf16(&units).map_err(|_| ExitReason::IllegalInstruction(0xFE));
        }
        units.push(unit);
    }
    Err(ExitReason::IllegalInstruction(0xFE))
}

fn store_ascii(
    memory: &mut GuestMemory,
    env: &mut EnvState,
    key: &str,
    value: &str,
) -> Result<u64, ExitReason> {
    if value.len() > MAX_VALUE {
        return Err(ExitReason::IllegalInstruction(0xFE));
    }
    let needed = value.len() + 1;
    let buffer = if let Some(&existing) = env.buffers.get(key) {
        existing
    } else {
        let address = memory
            .map_heap_allocation(PROCESS_HEAP, needed.max(1))
            .map_err(fault)?;
        env.buffers.insert(key.to_owned(), address);
        address
    };
    // Grow by allocating a replacement if the old buffer is too small.
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    if memory.write_bytes(buffer, &bytes).is_err() {
        let address = memory
            .map_heap_allocation(PROCESS_HEAP, bytes.len())
            .map_err(fault)?;
        memory.write_bytes(address, &bytes).map_err(fault)?;
        env.buffers.insert(key.to_owned(), address);
        return Ok(address);
    }
    Ok(buffer)
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    {
        let mut env = state.env.lock().expect("env state");
        if env.values.is_empty() {
            *env = EnvState::with_defaults();
        }
    }

    let getenv = Arc::clone(state);
    registry.register(UCRT, "getenv", move |args, cpu, memory| {
        if args.rcx == 0 {
            set_errno(memory, cpu.gs_base, EINVAL)?;
            return Ok(0);
        }
        let name = read_c_string(memory, args.rcx, MAX_NAME)?;
        let key = name.to_ascii_uppercase();
        let mut env = getenv.env.lock().expect("env state");
        let Some(value) = env.values.get(&key).cloned() else {
            return Ok(0);
        };
        store_ascii(memory, &mut env, &key, &value)
    });

    let get_a = Arc::clone(state);
    registry.register(
        KERNEL32,
        "GetEnvironmentVariableA",
        move |args, cpu, memory| {
            let (name, buffer, size) = (args.rcx, args.rdx, args.r8 as u32);
            if name == 0 {
                return get_a.fail(cpu.gs_base, 87, 0);
            }
            let key = read_c_string(memory, name, MAX_NAME)?.to_ascii_uppercase();
            let env = get_a.env.lock().expect("env state");
            let Some(value) = env.values.get(&key) else {
                return get_a.fail(cpu.gs_base, 203, 0); // ERROR_ENVVAR_NOT_FOUND
            };
            let needed = value.len() + 1;
            if buffer == 0 || size == 0 {
                return Ok(needed as u64);
            }
            if (size as usize) < needed {
                let copy = (size as usize).saturating_sub(1).min(value.len());
                memory
                    .write_bytes(buffer, &value.as_bytes()[..copy])
                    .map_err(fault)?;
                if size > 0 {
                    memory.write_u8(buffer + copy as u64, 0).map_err(fault)?;
                }
                return get_a.fail(cpu.gs_base, 122, needed as u64); // ERROR_INSUFFICIENT_BUFFER
            }
            let mut bytes = value.as_bytes().to_vec();
            bytes.push(0);
            memory.write_bytes(buffer, &bytes).map_err(fault)?;
            Ok((needed - 1) as u64)
        },
    );

    let get_w = Arc::clone(state);
    registry.register(
        KERNEL32,
        "GetEnvironmentVariableW",
        move |args, cpu, memory| {
            let (name, buffer, size) = (args.rcx, args.rdx, args.r8 as u32);
            if name == 0 {
                return get_w.fail(cpu.gs_base, 87, 0);
            }
            let key = read_wide_string(memory, name, MAX_NAME)?.to_ascii_uppercase();
            let env = get_w.env.lock().expect("env state");
            let Some(value) = env.values.get(&key) else {
                return get_w.fail(cpu.gs_base, 203, 0);
            };
            let wide: Vec<u16> = value.encode_utf16().collect();
            let needed = wide.len() + 1;
            if buffer == 0 || size == 0 {
                return Ok(needed as u64);
            }
            if (size as usize) < needed {
                return get_w.fail(cpu.gs_base, 122, needed as u64);
            }
            for (index, unit) in wide.iter().enumerate() {
                memory
                    .write_u16(buffer + index as u64 * 2, *unit)
                    .map_err(fault)?;
            }
            memory
                .write_u16(buffer + wide.len() as u64 * 2, 0)
                .map_err(fault)?;
            Ok(wide.len() as u64)
        },
    );

    let putenv = Arc::clone(state);
    registry.register(UCRT, "_putenv", move |args, cpu, memory| {
        if args.rcx == 0 {
            set_errno(memory, cpu.gs_base, EINVAL)?;
            return Ok(u64::from(u32::MAX));
        }
        let entry = read_c_string(memory, args.rcx, MAX_NAME + MAX_VALUE + 1)?;
        let Some((name, value)) = entry.split_once('=') else {
            set_errno(memory, cpu.gs_base, EINVAL)?;
            return Ok(u64::from(u32::MAX));
        };
        if name.is_empty() || name.len() > MAX_NAME || value.len() > MAX_VALUE {
            set_errno(memory, cpu.gs_base, EINVAL)?;
            return Ok(u64::from(u32::MAX));
        }
        let key = name.to_ascii_uppercase();
        let mut env = putenv.env.lock().expect("env state");
        if value.is_empty() {
            env.values.remove(&key);
            env.buffers.remove(&key);
        } else {
            env.values.insert(key.clone(), value.to_owned());
            env.buffers.remove(&key);
        }
        Ok(0)
    });

    let set_a = Arc::clone(state);
    registry.register(
        KERNEL32,
        "SetEnvironmentVariableA",
        move |args, cpu, memory| {
            let (name, value) = (args.rcx, args.rdx);
            if name == 0 {
                return set_a.fail(cpu.gs_base, 87, 0);
            }
            let key = read_c_string(memory, name, MAX_NAME)?.to_ascii_uppercase();
            if key.is_empty() {
                return set_a.fail(cpu.gs_base, 87, 0);
            }
            let mut env = set_a.env.lock().expect("env state");
            if value == 0 {
                env.values.remove(&key);
                env.buffers.remove(&key);
                return Ok(1);
            }
            let text = read_c_string(memory, value, MAX_VALUE)?;
            if text.len() > MAX_VALUE {
                return set_a.fail(cpu.gs_base, 206, 0); // ERROR_FILENAME_EXCED_RANGE
            }
            env.values.insert(key.clone(), text);
            env.buffers.remove(&key);
            Ok(1)
        },
    );

    let set_w = Arc::clone(state);
    registry.register(
        KERNEL32,
        "SetEnvironmentVariableW",
        move |args, cpu, memory| {
            let (name, value) = (args.rcx, args.rdx);
            if name == 0 {
                return set_w.fail(cpu.gs_base, 87, 0);
            }
            let key = read_wide_string(memory, name, MAX_NAME)?.to_ascii_uppercase();
            if key.is_empty() {
                return set_w.fail(cpu.gs_base, 87, 0);
            }
            let mut env = set_w.env.lock().expect("env state");
            if value == 0 {
                env.values.remove(&key);
                env.buffers.remove(&key);
                return Ok(1);
            }
            let text = read_wide_string(memory, value, MAX_VALUE)?;
            if text.len() > MAX_VALUE {
                return set_w.fail(cpu.gs_base, 206, 0);
            }
            env.values.insert(key.clone(), text);
            env.buffers.remove(&key);
            Ok(1)
        },
    );

    registry.alias(CRT_RUNTIME_API, UCRT, "getenv");
    registry.alias(CRT_RUNTIME_API, UCRT, "_putenv");
    registry.set_status(UCRT, "getenv", ServiceStatus::Partial);
    registry.set_status(UCRT, "_putenv", ServiceStatus::Partial);
    for symbol in [
        "GetEnvironmentVariableA",
        "GetEnvironmentVariableW",
        "SetEnvironmentVariableA",
        "SetEnvironmentVariableW",
    ] {
        registry.alias(PROCESS_ENV_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{CpuState, Register};
    use crate::import_table::ImportName;
    use crate::imports::Win64Args;
    use crate::services::MockClock;

    fn call(
        registry: &ImportRegistry,
        module: &str,
        symbol: &str,
        args: [u64; 4],
        cpu: &mut CpuState,
        memory: &mut GuestMemory,
    ) -> u64 {
        cpu.set(Register::Rcx, args[0]);
        cpu.set(Register::Rdx, args[1]);
        cpu.set(Register::R8, args[2]);
        cpu.set(Register::R9, args[3]);
        let service = registry
            .resolve(module, &ImportName::Name(symbol.into()))
            .unwrap();
        (service.handler)(&Win64Args::new(cpu), cpu, memory).unwrap()
    }

    #[test]
    fn getenv_returns_installed_values() {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register(&mut registry, &state);
        let mut memory = GuestMemory::new();
        let mut cpu = CpuState {
            gs_base: 0x10000,
            ..CpuState::default()
        };
        let name = memory.map_heap_allocation(PROCESS_HEAP, 8).unwrap();
        memory.write_bytes(name, b"OS\0").unwrap();
        let value = call(
            &registry,
            UCRT,
            "getenv",
            [name, 0, 0, 0],
            &mut cpu,
            &mut memory,
        );
        assert_ne!(value, 0);
        let mut bytes = [0u8; 11];
        memory.read_bytes(value, &mut bytes).unwrap();
        assert_eq!(&bytes[..10], b"Windows_NT");
    }

    #[test]
    fn putenv_and_set_environment_variable_update_the_block() {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register(&mut registry, &state);
        let mut memory = GuestMemory::new();
        let mut cpu = CpuState {
            gs_base: 0x10000,
            ..CpuState::default()
        };
        let entry = memory.map_heap_allocation(PROCESS_HEAP, 32).unwrap();
        memory.write_bytes(entry, b"XENORA_TEST=one\0").unwrap();
        assert_eq!(
            call(
                &registry,
                UCRT,
                "_putenv",
                [entry, 0, 0, 0],
                &mut cpu,
                &mut memory
            ),
            0
        );
        let name = memory.map_heap_allocation(PROCESS_HEAP, 16).unwrap();
        memory.write_bytes(name, b"XENORA_TEST\0").unwrap();
        let value = call(
            &registry,
            UCRT,
            "getenv",
            [name, 0, 0, 0],
            &mut cpu,
            &mut memory,
        );
        assert_ne!(value, 0);
        let mut bytes = [0u8; 4];
        memory.read_bytes(value, &mut bytes).unwrap();
        assert_eq!(&bytes, b"one\0");
        let next = memory.map_heap_allocation(PROCESS_HEAP, 8).unwrap();
        memory.write_bytes(next, b"two\0").unwrap();
        assert_eq!(
            call(
                &registry,
                KERNEL32,
                "SetEnvironmentVariableA",
                [name, next, 0, 0],
                &mut cpu,
                &mut memory
            ),
            1
        );
        let value = call(
            &registry,
            UCRT,
            "getenv",
            [name, 0, 0, 0],
            &mut cpu,
            &mut memory,
        );
        let mut bytes = [0u8; 4];
        memory.read_bytes(value, &mut bytes).unwrap();
        assert_eq!(&bytes, b"two\0");
        assert_eq!(
            call(
                &registry,
                KERNEL32,
                "SetEnvironmentVariableA",
                [name, 0, 0, 0],
                &mut cpu,
                &mut memory
            ),
            1
        );
        assert_eq!(
            call(
                &registry,
                UCRT,
                "getenv",
                [name, 0, 0, 0],
                &mut cpu,
                &mut memory
            ),
            0
        );
    }
}
