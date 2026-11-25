// SPDX-License-Identifier: GPL-3.0-or-later

//! The registry of the guest machine, honestly empty.
//!
//! Documented (Microsoft Learn, "RegOpenKeyExA", "RegQueryValueExA",
//! "RegCloseKey", "Predefined Keys"): the registry is a hierarchical store
//! whose predefined roots - HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_USERS,
//! HKEY_LOCAL_MACHINE and HKEY_CURRENT_CONFIG - always exist. Opening a key
//! produces a handle, querying a value that does not exist fails with
//! ERROR_FILE_NOT_FOUND, and closing a predefined handle is valid.
//!
//! Xenora's choice, because there is no host registry to mirror and inventing
//! one would be dishonest: the machine's registry is empty. The predefined
//! roots open - they always exist - but every subkey and every value is
//! absent, which is what a freshly installed Windows answers for the keys a
//! game would probe. Programs that probe the registry for machine state get
//! the truthful empty answer instead of invented data; programs that need
//! values to exist are not supported yet, which the status records say.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::imports::ImportRegistry;
use crate::memory::GuestMemory;
use std::sync::{Arc, Mutex};

const ADVAPI32: &str = "advapi32.dll";
const REGISTRY_API: &str = "api-ms-win-core-registry-l1-1-0.dll";
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_INVALID_PARAMETER: u32 = 87;

/// The predefined root handles, by winreg.h. Programs pass them as pseudo
/// handles, so they are valid keys without being opened.
const ROOTS: [u64; 5] = [
    0x8000_0000, // HKEY_CLASSES_ROOT
    0x8000_0001, // HKEY_CURRENT_USER
    0x8000_0002, // HKEY_LOCAL_MACHINE
    0x8000_0003, // HKEY_USERS
    0x8000_0005, // HKEY_CURRENT_CONFIG
];

/// The opened keys: small pseudo handles allocated upward so they cannot be
/// mistaken for the predefined roots.
struct OpenKeys {
    next: u64,
    keys: std::collections::BTreeSet<u64>,
}

fn read_string(memory: &GuestMemory, address: u64, wide: bool) -> Result<Vec<u8>, ExitReason> {
    let mut bytes = Vec::new();
    if wide {
        for index in 0..512u64 {
            let unit = memory
                .read_u16(address + 2 * index)
                .map_err(ExitReason::MemoryFault)?;
            if unit == 0 {
                break;
            }
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
    } else {
        for index in 0..1024u64 {
            let byte = memory
                .read_u8(address + index)
                .map_err(ExitReason::MemoryFault)?;
            if byte == 0 {
                break;
            }
            bytes.push(byte);
        }
    }
    Ok(bytes)
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let keys = Arc::new(Mutex::new(OpenKeys {
        next: 0x100,
        keys: std::collections::BTreeSet::new(),
    }));

    for (symbol, wide) in [("RegOpenKeyExA", false), ("RegOpenKeyExW", true)] {
        let state = Arc::clone(state);
        let keys = Arc::clone(&keys);
        registry.register(ADVAPI32, symbol, move |args, cpu, memory| {
            let (key, subkey, options, _sam, output) = (
                args.rcx,
                args.rdx,
                args.r8 as u32,
                args.r9,
                args.get(4, memory).map_err(ExitReason::MemoryFault)?,
            );
            if output == 0 {
                return state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 1);
            }
            // A nonzero reserved options word is invalid, as documented.
            if options != 0 {
                return state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 1);
            }
            let valid_root = ROOTS.contains(&key);
            let valid_open = keys.lock().expect("open registry keys").keys.contains(&key);
            if !valid_root && !valid_open {
                return state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 1);
            }
            // The empty registry has no subkeys: any named path is absent,
            // while a null or empty subkey opens the key itself again.
            let path = if subkey == 0 {
                Vec::new()
            } else {
                read_string(memory, subkey, wide)?
            };
            if !path.is_empty() {
                return state.fail(cpu.gs_base, ERROR_FILE_NOT_FOUND, 1);
            }
            let mut open = keys.lock().expect("open registry keys");
            let handle = open.next;
            open.next += 1;
            open.keys.insert(handle);
            memory
                .write_u64(output, handle)
                .map_err(ExitReason::MemoryFault)?;
            Ok(0)
        });
    }

    for (symbol, wide) in [("RegQueryValueExA", false), ("RegQueryValueExW", true)] {
        let state = Arc::clone(state);
        let keys = Arc::clone(&keys);
        registry.register(ADVAPI32, symbol, move |args, cpu, memory| {
            let (key, value_name) = (args.rcx, args.rdx);
            let valid_root = ROOTS.contains(&key);
            let valid_open = keys.lock().expect("open registry keys").keys.contains(&key);
            if !valid_root && !valid_open {
                return state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 1);
            }
            // Read the name only to validate the pointer, as a program
            // passing an unreadable one must fault like any other read.
            if value_name != 0 {
                read_string(memory, value_name, wide)?;
            }
            // Every value is absent in the empty registry.
            state.fail(cpu.gs_base, ERROR_FILE_NOT_FOUND, 1)
        });
    }

    let keys_close = Arc::clone(&keys);
    registry.register(ADVAPI32, "RegCloseKey", move |args, _, _| {
        let key = args.rcx;
        if ROOTS.contains(&key) {
            // Closing a predefined key is valid and does nothing, as the
            // documentation describes.
            return Ok(0);
        }
        let mut open = keys_close.lock().expect("open registry keys");
        if open.keys.remove(&key) {
            Ok(0)
        } else {
            Ok(u64::from(ERROR_INVALID_HANDLE))
        }
    });

    for (module, symbol) in [
        (ADVAPI32, "RegOpenKeyExA"),
        (ADVAPI32, "RegOpenKeyExW"),
        (ADVAPI32, "RegQueryValueExA"),
        (ADVAPI32, "RegQueryValueExW"),
        (ADVAPI32, "RegCloseKey"),
    ] {
        registry.alias(REGISTRY_API, module, symbol);
        registry.set_status(ADVAPI32, symbol, crate::imports::ServiceStatus::Partial);
    }
}
