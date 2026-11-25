// SPDX-License-Identifier: GPL-3.0-or-later

//! Module and export lookup for built-in guest services.

use super::{ERROR_INVALID_HANDLE, ERROR_NOT_ENOUGH_MEMORY, KERNEL32, ServicesState};
use crate::cpu::ExitReason;
use crate::import_table::ImportName;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

const LOADER_API: &str = "api-ms-win-core-libraryloader-l1-2-0.dll";
const UCRT: &str = "ucrtbase.dll";
const MODULE_NOT_FOUND: u32 = 126;
const PROCEDURE_NOT_FOUND: u32 = 127;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const MODULE_BASE: u64 = 0x0000_0007_e000_0000;
/// Synthetic path for the running guest image (`GetModuleFileName(NULL, …)`).
const GUEST_IMAGE_PATH: &str = r"C:\xenora\guest.exe";

#[derive(Default)]
struct Modules {
    names: BTreeMap<String, u64>,
    exports: BTreeMap<u64, HashMap<ImportName, usize>>,
    references: Mutex<BTreeMap<u64, u32>>,
}

fn read_name(
    memory: &GuestMemory,
    address: u64,
    wide: bool,
) -> Result<Option<String>, MemoryError> {
    let mut name = String::new();
    let width = if wide { 2 } else { 1 };
    for index in 0..260 {
        let slot = address
            .checked_add(index * width)
            .ok_or(MemoryError::Overflow)?;
        let unit = if wide {
            memory.read_u16(slot)?
        } else {
            u16::from(memory.read_u8(slot)?)
        };
        if unit == 0 {
            return Ok(Some(name));
        }
        if !(0x20..=0x7e).contains(&unit) {
            return Ok(None);
        }
        name.push(char::from(unit as u8));
    }
    Ok(None)
}

fn synthetic_module_path(modules: &Modules, handle: u64) -> Option<String> {
    if handle == 0 {
        return Some(GUEST_IMAGE_PATH.to_owned());
    }
    let mut canonical = None;
    for (name, &mapped) in &modules.names {
        if mapped != handle {
            continue;
        }
        if name.ends_with(".dll") && !name.starts_with("api-ms-") {
            canonical = Some(name.clone());
            break;
        }
    }
    canonical.map(|name| format!(r"C:\Windows\System32\{name}"))
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let modules = Arc::new(OnceLock::<Modules>::new());
    let named_operations = [
        ("GetModuleHandleA", false, false),
        ("GetModuleHandleW", true, false),
        ("LoadLibraryA", false, true),
        ("LoadLibraryW", true, true),
    ];
    for (symbol, wide, load) in named_operations {
        let state = Arc::clone(state);
        let modules = Arc::clone(&modules);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            if args.rcx == 0 {
                return state.fail(cpu.gs_base, MODULE_NOT_FOUND, 0);
            }
            let Some(name) = read_name(memory, args.rcx, wide).map_err(ExitReason::MemoryFault)?
            else {
                return state.fail(cpu.gs_base, MODULE_NOT_FOUND, 0);
            };
            let mut name = name.to_ascii_lowercase();
            if !name.contains('.') {
                name.push_str(".dll");
            }
            let modules = modules.get().expect("built-in module catalog");
            let Some(&handle) = modules.names.get(&name) else {
                return state.fail(cpu.gs_base, MODULE_NOT_FOUND, 0);
            };
            let mut references = modules.references.lock().expect("module references");
            if load {
                let count = references.entry(handle).or_default();
                let Some(next) = count.checked_add(1) else {
                    return state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
                };
                *count = next;
            } else if handle != MODULE_BASE && !references.contains_key(&handle) {
                return state.fail(cpu.gs_base, MODULE_NOT_FOUND, 0);
            }
            Ok(handle)
        });
    }

    let release = Arc::clone(&modules);
    let release_state = Arc::clone(state);
    let release_trampolines = registry.dynamic_trampolines();
    registry.register(KERNEL32, "FreeLibrary", move |args, cpu, _| {
        let modules = release.get().expect("built-in module catalog");
        let mut references = modules.references.lock().expect("module references");
        let Some(count) = references.get_mut(&args.rcx) else {
            return release_state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        };
        *count -= 1;
        if *count == 0 {
            references.remove(&args.rcx);
            if args.rcx != MODULE_BASE {
                release_trampolines.revoke(modules.exports[&args.rcx].values().copied());
            }
        }
        Ok(1)
    });

    let lookup = Arc::clone(&modules);
    let trampolines = registry.dynamic_trampolines();
    let proc_state = Arc::clone(state);
    registry.register(KERNEL32, "GetProcAddress", move |args, cpu, memory| {
        let modules = lookup.get().expect("built-in module catalog");
        let references = modules.references.lock().expect("module references");
        if args.rcx != MODULE_BASE && !references.contains_key(&args.rcx) {
            return proc_state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        }
        let export = if let Ok(ordinal) = u16::try_from(args.rdx) {
            ImportName::Ordinal(ordinal)
        } else {
            let Some(symbol) =
                read_name(memory, args.rdx, false).map_err(ExitReason::MemoryFault)?
            else {
                return proc_state.fail(cpu.gs_base, PROCEDURE_NOT_FOUND, 0);
            };
            ImportName::Name(symbol)
        };
        match modules.exports[&args.rcx].get(&export) {
            Some(&index) => Ok(trampolines.expose(index)),
            None => proc_state.fail(cpu.gs_base, PROCEDURE_NOT_FOUND, 0),
        }
    });

    for (symbol, wide) in [("GetModuleFileNameA", false), ("GetModuleFileNameW", true)] {
        let file_state = Arc::clone(state);
        let modules = Arc::clone(&modules);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let (handle, buffer, size) = (args.rcx, args.rdx, args.r8 as u32);
            let modules = modules.get().expect("built-in module catalog");
            let Some(path) = synthetic_module_path(modules, handle) else {
                return file_state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
            };
            let units: Vec<u16> = if wide {
                path.encode_utf16().collect()
            } else {
                path.bytes().map(u16::from).collect()
            };
            let needed = units.len() + 1;
            if buffer == 0 || size == 0 {
                return Ok(needed as u64);
            }
            let copy = (size as usize).saturating_sub(1).min(units.len());
            if wide {
                for (index, unit) in units[..copy].iter().enumerate() {
                    memory
                        .write_u16(buffer + index as u64 * 2, *unit)
                        .map_err(ExitReason::MemoryFault)?;
                }
                memory
                    .write_u16(buffer + copy as u64 * 2, 0)
                    .map_err(ExitReason::MemoryFault)?;
            } else {
                let mut bytes: Vec<u8> = units[..copy].iter().map(|u| *u as u8).collect();
                bytes.push(0);
                memory
                    .write_bytes(buffer, &bytes)
                    .map_err(ExitReason::MemoryFault)?;
            }
            if (size as usize) < needed {
                return file_state.fail(cpu.gs_base, ERROR_INSUFFICIENT_BUFFER, size as u64);
            }
            Ok(units.len() as u64)
        });
    }

    for symbol in named_operations
        .map(|(symbol, _, _)| symbol)
        .into_iter()
        .chain([
            "GetProcAddress",
            "FreeLibrary",
            "GetModuleFileNameA",
            "GetModuleFileNameW",
        ])
    {
        registry.alias(LOADER_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }

    let mut catalog = Modules::default();
    // Preserve the startup UCRT binding. Kernel services are resident.
    catalog.references.get_mut().unwrap().insert(UCRT_HANDLE, 1);
    for (module, handle) in [(KERNEL32, MODULE_BASE), (UCRT, UCRT_HANDLE)] {
        catalog.names.insert(module.to_owned(), handle);
        catalog.exports.insert(
            handle,
            registry
                .canonical_exports(module)
                .map(|(key, index)| (key.clone(), index))
                .collect(),
        );
        for (alias, _, canonical) in registry.iter_aliases() {
            if canonical == module {
                catalog.names.insert(alias.to_owned(), handle);
            }
        }
    }
    assert!(modules.set(catalog).is_ok());
}

const UCRT_HANDLE: u64 = MODULE_BASE + 0x10000;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{CpuState, Register};
    use crate::imports::Win64Args;
    use crate::memory::MemoryPermissions;
    use crate::services::{MockClock, register_standard_services};

    #[test]
    fn guest_module_references_control_dynamic_export_lifetime() {
        let mut registry = ImportRegistry::new();
        registry.register(UCRT, "SyntheticOrdinalExport", |_, _, _| Ok(42));
        registry
            .try_ordinal_alias(UCRT, "SyntheticOrdinalExport", 42)
            .unwrap();
        registry.register(UCRT, "OtherSyntheticExport", |_, _, _| Ok(43));
        assert!(
            registry
                .try_ordinal_alias(UCRT, "OtherSyntheticExport", 42)
                .is_err()
        );
        register_standard_services(
            &mut registry,
            Arc::new(ServicesState::new(Arc::new(MockClock::new(0)))),
        );
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x10000, 4096, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory.write_bytes(0x10000, b"UCRTBASE\0").unwrap();
        memory.write_bytes(0x10100, b"sqrt\0").unwrap();
        memory.write_bytes(0x10200, b"/unknown.dll\0").unwrap();
        memory.write_bytes(0x10300, b"Ordinal#42\0").unwrap();
        let mut call = |symbol: &str, first, second| {
            let mut cpu = CpuState::default();
            cpu.set(Register::Rcx, first);
            cpu.set(Register::Rdx, second);
            let service = registry
                .resolve(LOADER_API, &ImportName::Name(symbol.to_owned()))
                .unwrap();
            (service.handler)(&Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap()
        };
        assert_eq!(call("LoadLibraryA", 0x10200, 0), 0);
        let handle = call("GetModuleHandleA", 0x10000, 0);
        assert_ne!(handle, 0);
        assert_eq!(call("LoadLibraryA", 0x10000, 0), handle);
        let export = call("GetProcAddress", handle, 0x10100);
        let ordinal = call("GetProcAddress", handle, 42);
        assert!(registry.is_service_trampoline(ordinal));
        assert_ne!(ordinal, export);
        assert_eq!(call("GetProcAddress", handle, 43), 0);
        assert_eq!(call("GetProcAddress", handle, 0x10300), 0);
        assert!(registry.is_service_trampoline(export));
        assert_eq!(call("FreeLibrary", handle, 0), 1);
        assert!(registry.is_service_trampoline(export));
        assert_eq!(call("FreeLibrary", handle, 0), 1);
        assert!(!registry.is_service_trampoline(export));
        assert!(!registry.is_service_trampoline(ordinal));
        assert_eq!(call("GetModuleHandleA", 0x10000, 0), 0);
        assert_eq!(call("GetProcAddress", handle, 0x10100), 0);
        assert_eq!(call("FreeLibrary", handle, 0), 0);
        assert_eq!(call("LoadLibraryA", 0x10000, 0), handle);
        assert_ne!(call("GetProcAddress", handle, 0x10100), 0);
        let ordinal = call("GetProcAddress", handle, 42);
        let mut cpu = CpuState::new(ordinal, 0x10400);
        memory.write_u64(0x10400, 0x1234).unwrap();
        registry.dispatch(ordinal, &mut cpu, &mut memory).unwrap();
        assert_eq!(cpu.get(Register::Rax), 42);
        assert_eq!(cpu.rip, 0x1234);

        let mut cpu = CpuState::default();
        cpu.set(Register::Rcx, 0);
        cpu.set(Register::Rdx, 0x10500);
        cpu.set(Register::R8, 64);
        let service = registry
            .resolve(LOADER_API, &ImportName::Name("GetModuleFileNameA".into()))
            .unwrap();
        let written =
            (service.handler)(&Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap() as usize;
        assert_eq!(written, GUEST_IMAGE_PATH.len());
        let mut bytes = vec![0u8; written + 1];
        memory.read_bytes(0x10500, &mut bytes).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes[..written]).unwrap(),
            GUEST_IMAGE_PATH
        );
        cpu.set(Register::Rcx, MODULE_BASE);
        cpu.set(Register::Rdx, 0x10600);
        cpu.set(Register::R8, 64);
        let written =
            (service.handler)(&Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap() as usize;
        let mut bytes = vec![0u8; written + 1];
        memory.read_bytes(0x10600, &mut bytes).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes[..written]).unwrap(),
            r"C:\Windows\System32\kernel32.dll"
        );
    }
}
