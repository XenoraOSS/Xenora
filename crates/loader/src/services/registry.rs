// SPDX-License-Identifier: GPL-3.0-or-later

//! The synthetic guest registry exposes predefined roots only.
//!
//! Documented (Microsoft Learn, "RegOpenKeyExA", "RegQueryValueExA",
//! "RegCloseKey", "Predefined Keys"): the registry is a hierarchical store
//! whose predefined roots - HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_USERS,
//! HKEY_LOCAL_MACHINE and HKEY_CURRENT_CONFIG - always exist. Opening a
//! predefined root with an empty subkey returns that same handle. Querying a
//! missing value returns ERROR_FILE_NOT_FOUND, and closing a predefined
//! handle is valid.
//!
//! Every named subkey and every value is absent in this model. Programs
//! can probe the roots and take their ordinary missing-entry paths.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::imports::ImportRegistry;
use crate::memory::GuestMemory;
use std::sync::Arc;

const ADVAPI32: &str = "advapi32.dll";
const REGISTRY_API: &str = "api-ms-win-core-registry-l1-1-0.dll";
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_INVALID_PARAMETER: u32 = 87;

/// The predefined root handles, by winreg.h. Programs pass them as pseudo
/// handles, so they are valid keys without being opened.
const ROOTS: [u32; 5] = [
    0x8000_0000, // HKEY_CLASSES_ROOT
    0x8000_0001, // HKEY_CURRENT_USER
    0x8000_0002, // HKEY_LOCAL_MACHINE
    0x8000_0003, // HKEY_USERS
    0x8000_0005, // HKEY_CURRENT_CONFIG
];

fn is_predefined_root(key: u64) -> bool {
    // winreg.h casts a signed LONG to pointer width. Also accept the
    // zero-extended form used by existing synthetic guests.
    ROOTS
        .iter()
        .any(|&root| key == u64::from(root) || key == i64::from(root as i32) as u64)
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
    for (symbol, wide) in [("RegOpenKeyExA", false), ("RegOpenKeyExW", true)] {
        let state = Arc::clone(state);
        registry.register(ADVAPI32, symbol, move |args, cpu, memory| {
            let (key, subkey, options, _sam, output) = (
                args.rcx,
                args.rdx,
                args.r8 as u32,
                args.r9,
                args.get(4, memory).map_err(ExitReason::MemoryFault)?,
            );
            if output == 0 || options != 0 {
                return state.fail(
                    cpu.gs_base,
                    ERROR_INVALID_PARAMETER,
                    u64::from(ERROR_INVALID_PARAMETER),
                );
            }
            if !is_predefined_root(key) {
                return state.fail(
                    cpu.gs_base,
                    ERROR_INVALID_HANDLE,
                    u64::from(ERROR_INVALID_HANDLE),
                );
            }
            // The empty registry has no subkeys: any named path is absent,
            // while a null or empty subkey opens the key itself again.
            let path = if subkey == 0 {
                Vec::new()
            } else {
                read_string(memory, subkey, wide)?
            };
            if !path.is_empty() {
                return state.fail(
                    cpu.gs_base,
                    ERROR_FILE_NOT_FOUND,
                    u64::from(ERROR_FILE_NOT_FOUND),
                );
            }
            memory
                .write_u64(output, key)
                .map_err(ExitReason::MemoryFault)?;
            Ok(0)
        });
    }

    {
        let state = Arc::clone(state);
        registry.register(ADVAPI32, "RegOpenKeyW", move |args, cpu, memory| {
            let (key, subkey, output) = (args.rcx, args.rdx, args.r8);
            if output == 0 {
                return state.fail(
                    cpu.gs_base,
                    ERROR_INVALID_PARAMETER,
                    u64::from(ERROR_INVALID_PARAMETER),
                );
            }
            if !is_predefined_root(key) {
                return state.fail(
                    cpu.gs_base,
                    ERROR_INVALID_HANDLE,
                    u64::from(ERROR_INVALID_HANDLE),
                );
            }
            let path = if subkey == 0 {
                Vec::new()
            } else {
                read_string(memory, subkey, true)?
            };
            if !path.is_empty() {
                return state.fail(
                    cpu.gs_base,
                    ERROR_FILE_NOT_FOUND,
                    u64::from(ERROR_FILE_NOT_FOUND),
                );
            }
            memory
                .write_u64(output, key)
                .map_err(ExitReason::MemoryFault)?;
            Ok(0)
        });
    }

    for (symbol, wide) in [("RegQueryValueExA", false), ("RegQueryValueExW", true)] {
        let state = Arc::clone(state);
        registry.register(ADVAPI32, symbol, move |args, cpu, memory| {
            let (key, value_name) = (args.rcx, args.rdx);
            if !is_predefined_root(key) {
                return state.fail(
                    cpu.gs_base,
                    ERROR_INVALID_HANDLE,
                    u64::from(ERROR_INVALID_HANDLE),
                );
            }
            // Read the name only to validate the pointer, as a program
            // passing an unreadable one must fault like any other read.
            if value_name != 0 {
                read_string(memory, value_name, wide)?;
            }
            // Every value is absent in the empty registry.
            state.fail(
                cpu.gs_base,
                ERROR_FILE_NOT_FOUND,
                u64::from(ERROR_FILE_NOT_FOUND),
            )
        });
    }

    registry.register(ADVAPI32, "RegCloseKey", move |args, _, _| {
        // Closing a predefined key is a no-op.
        if is_predefined_root(args.rcx) {
            Ok(0)
        } else {
            Ok(u64::from(ERROR_INVALID_HANDLE))
        }
    });

    registry.set_status(
        ADVAPI32,
        "RegOpenKeyW",
        crate::imports::ServiceStatus::Partial,
    );

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
