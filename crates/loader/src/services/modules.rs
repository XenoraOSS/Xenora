// SPDX-License-Identifier: GPL-3.0-or-later

//! Module handles and export lookup for the program and the built-in services.
//!
//! The program's handle is the base address its image was loaded at. Kernel32,
//! KernelBase and the UCRT are built in: their handles are identities with no
//! image behind them. Kernel32 and KernelBase, like the program, are never
//! unloaded; the UCRT is when its last reference is released.
//!
//! Documented (Microsoft Learn): the meaning of a null name, the appended `.dll`,
//! case-insensitive comparison, the `GetModuleHandleEx` flags and the exclusion
//! of pinning with an unchanged count, and that a module a call did not count
//! must not be freed. Microsoft gives no error codes here; the ones used are
//! Xenora's choice. No host library is ever searched for or loaded.

use super::{
    CURRENT_PROCESS, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_NOACCESS,
    ERROR_NOT_ENOUGH_MEMORY, ERROR_NOT_SUPPORTED, KERNEL32, ServicesState,
};
use crate::cpu::ExitReason;
use crate::import_table::ImportName;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MainImage, MemoryError};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

const KERNELBASE: &str = "kernelbase.dll";
const PSAPI: &str = "psapi.dll";
const UCRT: &str = "ucrtbase.dll";
/// Where Microsoft's pages list `GetModuleHandle`, `GetModuleHandleEx`,
/// `GetProcAddress` and `FreeLibrary`, besides `kernel32.dll`.
const LOADER_APIS: [&str; 8] = [
    "api-ms-win-core-libraryloader-l1-1-0.dll",
    "api-ms-win-core-libraryloader-l1-1-1.dll",
    LOADER_API,
    "api-ms-win-core-libraryloader-l1-2-1.dll",
    "api-ms-win-core-libraryloader-l1-2-2.dll",
    "api-ms-win-core-libraryloader-l1-2-3.dll",
    "api-ms-win-downlevel-kernel32-l1-1-0.dll",
    KERNELBASE,
];
const LOADER_API: &str = "api-ms-win-core-libraryloader-l1-2-0.dll";
/// The same for `LoadLibraryA` and `LoadLibraryW`.
const LOAD_LIBRARY_APIS: [&str; 12] = [
    "api-ms-win-core-libraryloader-l1-2-1.dll",
    "api-ms-win-core-libraryloader-l1-2-2.dll",
    "api-ms-win-core-libraryloader-l1-2-3.dll",
    "api-ms-win-core-kernel32-legacy-l1-1-0.dll",
    "api-ms-win-core-kernel32-legacy-l1-1-1.dll",
    "api-ms-win-core-kernel32-legacy-l1-1-2.dll",
    "api-ms-win-core-kernel32-legacy-l1-1-3.dll",
    "api-ms-win-core-kernel32-legacy-l1-1-4.dll",
    "api-ms-win-core-kernel32-legacy-l1-1-5.dll",
    "api-ms-win-core-kernel32-legacy-l1-1-6.dll",
    "api-ms-win-downlevel-kernel32-l2-1-0.dll",
    KERNELBASE,
];
/// API sets listed for `GetModuleInformation` and `K32GetModuleInformation`
/// together.
const PSAPI_APIS: [&str; 2] = [
    "api-ms-win-core-psapi-l1-1-0.dll",
    "api-ms-win-core-psapi-obsolete-l1-1-0.dll",
];

const MODULE_NOT_FOUND: u32 = 126;
const PROCEDURE_NOT_FOUND: u32 = 127;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const MODULE_BASE: u64 = 0x0000_0007_e000_0000;
const UCRT_HANDLE: u64 = MODULE_BASE + 0x10000;
const KERNELBASE_HANDLE: u64 = MODULE_BASE + 0x20000;

/// `GET_MODULE_HANDLE_EX_FLAG_*`.
const PIN: u32 = 1;
const UNCHANGED_REFCOUNT: u32 = 2;
const FROM_ADDRESS: u32 = 4;

/// Size of `MODULEINFO` on x64: a pointer, a DWORD and a pointer.
const MODULE_INFO_SIZE: usize = 24;

#[derive(Default)]
struct Modules {
    names: BTreeMap<String, u64>,
    exports: BTreeMap<u64, HashMap<ImportName, usize>>,
    references: Mutex<References>,
}

#[derive(Default)]
struct References {
    counts: BTreeMap<u64, u32>,
    pinned: BTreeSet<u64>,
}

/// Modules that are loaded for as long as the process runs.
fn is_resident(memory: &GuestMemory, handle: u64) -> bool {
    matches!(handle, MODULE_BASE | KERNELBASE_HANDLE)
        || memory.main_image.is_some_and(|image| image.base == handle)
}

impl References {
    fn is_loaded(&self, memory: &GuestMemory, handle: u64) -> bool {
        is_resident(memory, handle)
            || self.counts.contains_key(&handle)
            || self.pinned.contains(&handle)
    }

    /// Add a reference. False if the count cannot grow.
    fn add(&mut self, handle: u64) -> bool {
        let count = self.counts.entry(handle).or_default();
        let next = count.checked_add(1);
        if let Some(next) = next {
            *count = next;
        }
        next.is_some()
    }
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

impl Modules {
    /// Handle of the built-in module a guest name refers to, loaded or not.
    /// Without a dot `.dll` is appended; a trailing dot stands for no extension.
    fn named(
        &self,
        memory: &GuestMemory,
        address: u64,
        wide: bool,
    ) -> Result<Option<u64>, MemoryError> {
        let Some(name) = read_name(memory, address, wide)? else {
            return Ok(None);
        };
        let mut name = name.to_ascii_lowercase();
        if !name.contains('.') {
            name.push_str(".dll");
        }
        Ok(self.names.get(&name).copied())
    }

    /// Handle of a loaded module by name; a null name is the program.
    fn loaded(
        &self,
        memory: &GuestMemory,
        address: u64,
        wide: bool,
    ) -> Result<Option<u64>, MemoryError> {
        if address == 0 {
            return Ok(memory.main_image.map(|image| image.base));
        }
        let handle = self.named(memory, address, wide)?;
        let references = self.references.lock().expect("module references");
        Ok(handle.filter(|&handle| references.is_loaded(memory, handle)))
    }

    /// Handle of the loaded module that contains an address. A built-in module
    /// has no image, so only its base address is inside it.
    fn containing(&self, memory: &GuestMemory, address: u64) -> Option<u64> {
        if let Some(image) = memory.main_image
            && address
                .checked_sub(image.base)
                .is_some_and(|offset| offset < u64::from(image.size))
        {
            return Some(image.base);
        }
        let references = self.references.lock().expect("module references");
        let built_in = self.exports.contains_key(&address);
        (built_in && references.is_loaded(memory, address)).then_some(address)
    }
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let modules = Arc::new(OnceLock::<Modules>::new());
    let catalog = |modules: &Arc<OnceLock<Modules>>| Arc::clone(modules);

    for (symbol, wide) in [("GetModuleHandleA", false), ("GetModuleHandleW", true)] {
        let state = Arc::clone(state);
        let modules = catalog(&modules);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let modules = modules.get().expect("built-in module catalog");
            let handle = modules.loaded(memory, args.rcx, wide);
            match handle.map_err(ExitReason::MemoryFault)? {
                Some(handle) => Ok(handle),
                None => state.fail(cpu.gs_base, MODULE_NOT_FOUND, 0),
            }
        });
    }

    // `GetModuleFileName` answers the path the program was started with for
    // its own image - the same string the command line is built from. Any
    // other module does not exist here, as no library is ever loaded into
    // the address space as an image.
    for (symbol, wide) in [("GetModuleFileNameA", false), ("GetModuleFileNameW", true)] {
        let state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let (module, buffer, capacity) = (args.rcx, args.rdx, args.r8 as u32);
            let owned = module == 0 || memory.main_image.is_some_and(|image| module == image.base);
            if !owned {
                return state.fail(cpu.gs_base, MODULE_NOT_FOUND, 0);
            }
            if capacity == 0 {
                return state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let path = state
                .crt_arguments()
                .for_command_line()
                .first()
                .cloned()
                .unwrap_or_default();
            // The count semantics are the documented truncation form: a
            // small buffer receives what fits, terminated, and the answer
            // is the capacity itself with ERROR_INSUFFICIENT_BUFFER.
            let units: Vec<u16> = if wide {
                path.encode_utf16().collect()
            } else {
                path.bytes().map(u16::from).collect()
            };
            let width = if wide { 2u64 } else { 1u64 };
            let fits = units.len() < capacity as usize;
            let copied = if fits {
                units.len()
            } else {
                capacity as usize - 1
            };
            for (index, unit) in units.iter().take(copied).enumerate() {
                let at = buffer + index as u64 * width;
                if wide {
                    memory
                        .write_u16(at, *unit)
                        .map_err(ExitReason::MemoryFault)?;
                } else {
                    memory
                        .write_u8(at, *unit as u8)
                        .map_err(ExitReason::MemoryFault)?;
                }
            }
            let end = buffer + copied as u64 * width;
            if wide {
                memory.write_u16(end, 0).map_err(ExitReason::MemoryFault)?;
            } else {
                memory.write_u8(end, 0).map_err(ExitReason::MemoryFault)?;
            }
            if fits {
                Ok(units.len() as u64)
            } else {
                state.fail(cpu.gs_base, ERROR_INSUFFICIENT_BUFFER, u64::from(capacity))
            }
        });
    }

    for (symbol, wide) in [("GetModuleHandleExA", false), ("GetModuleHandleExW", true)] {
        let state = Arc::clone(state);
        let modules = catalog(&modules);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let modules = modules.get().expect("built-in module catalog");
            let (flags, name, output) = (args.rcx as u32, args.rdx, args.r8);
            if output == 0 {
                return state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            // The handle is null on failure, so it is written first.
            if memory.write_u64(output, 0).is_err() {
                return state.fail(cpu.gs_base, ERROR_NOACCESS, 0);
            }
            if flags & !(PIN | UNCHANGED_REFCOUNT | FROM_ADDRESS) != 0
                || flags & (PIN | UNCHANGED_REFCOUNT) == PIN | UNCHANGED_REFCOUNT
                || (flags & FROM_ADDRESS != 0 && name == 0)
            {
                return state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let handle = if flags & FROM_ADDRESS != 0 {
                modules.containing(memory, name)
            } else {
                modules
                    .loaded(memory, name, wide)
                    .map_err(ExitReason::MemoryFault)?
            };
            let Some(handle) = handle else {
                return state.fail(cpu.gs_base, MODULE_NOT_FOUND, 0);
            };
            let mut references = modules.references.lock().expect("module references");
            if flags & PIN != 0 {
                references.pinned.insert(handle);
            } else if flags & UNCHANGED_REFCOUNT == 0 && !references.add(handle) {
                return state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
            }
            memory
                .write_u64(output, handle)
                .map_err(ExitReason::MemoryFault)?;
            Ok(1)
        });
    }

    for (symbol, wide) in [("LoadLibraryA", false), ("LoadLibraryW", true)] {
        let state = Arc::clone(state);
        let modules = catalog(&modules);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let modules = modules.get().expect("built-in module catalog");
            let handle = match args.rcx {
                0 => None,
                name => modules
                    .named(memory, name, wide)
                    .map_err(ExitReason::MemoryFault)?,
            };
            let Some(handle) = handle else {
                return state.fail(cpu.gs_base, MODULE_NOT_FOUND, 0);
            };
            let mut references = modules.references.lock().expect("module references");
            if !references.add(handle) {
                return state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
            }
            Ok(handle)
        });
    }

    let release = catalog(&modules);
    let release_state = Arc::clone(state);
    let release_trampolines = registry.dynamic_trampolines();
    registry.register(KERNEL32, "FreeLibrary", move |args, cpu, memory| {
        let modules = release.get().expect("built-in module catalog");
        let mut references = modules.references.lock().expect("module references");
        let Some(count) = references.counts.get_mut(&args.rcx) else {
            return release_state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        };
        *count -= 1;
        if *count == 0 {
            references.counts.remove(&args.rcx);
            if !references.is_loaded(memory, args.rcx) {
                release_trampolines.revoke(modules.exports[&args.rcx].values().copied());
            }
        }
        Ok(1)
    });

    let lookup = catalog(&modules);
    let trampolines = registry.dynamic_trampolines();
    let lookup_state = Arc::clone(state);
    registry.register(KERNEL32, "GetProcAddress", move |args, cpu, memory| {
        let modules = lookup.get().expect("built-in module catalog");
        let references = modules.references.lock().expect("module references");
        if !references.is_loaded(memory, args.rcx) {
            return lookup_state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        }
        let export = if let Ok(ordinal) = u16::try_from(args.rdx) {
            ImportName::Ordinal(ordinal)
        } else {
            let Some(symbol) =
                read_name(memory, args.rdx, false).map_err(ExitReason::MemoryFault)?
            else {
                return lookup_state.fail(cpu.gs_base, PROCEDURE_NOT_FOUND, 0);
            };
            ImportName::Name(symbol)
        };
        // The program's own export table is not read.
        let exports = modules.exports.get(&args.rcx);
        match exports.and_then(|exports| exports.get(&export)) {
            Some(&index) => Ok(trampolines.expose(index)),
            None => lookup_state.fail(cpu.gs_base, PROCEDURE_NOT_FOUND, 0),
        }
    });

    // kernel32.dll exports this with a K32 prefix and psapi.dll without.
    for (module, symbol) in [
        (KERNEL32, "K32GetModuleInformation"),
        (PSAPI, "GetModuleInformation"),
    ] {
        let state = Arc::clone(state);
        let modules = catalog(&modules);
        registry.register(module, symbol, move |args, cpu, memory| {
            let modules = modules.get().expect("built-in module catalog");
            let (process, handle, output, size) =
                (args.rcx, args.rdx, args.r8, args.r9 as u32 as usize);
            let fail = |error| state.fail(cpu.gs_base, error, 0);
            if process != CURRENT_PROCESS {
                return fail(ERROR_INVALID_HANDLE);
            }
            let image = memory.main_image.filter(|image| image.base == handle);
            let Some(MainImage {
                base,
                size: image_size,
                entry_point,
            }) = image
            else {
                let references = modules.references.lock().expect("module references");
                // A built-in module has no image to describe.
                return fail(if references.is_loaded(memory, handle) {
                    ERROR_NOT_SUPPORTED
                } else {
                    ERROR_INVALID_HANDLE
                });
            };
            if size < MODULE_INFO_SIZE {
                return fail(ERROR_INSUFFICIENT_BUFFER);
            }
            let mut info = [0; MODULE_INFO_SIZE];
            info[..8].copy_from_slice(&base.to_le_bytes());
            info[8..12].copy_from_slice(&image_size.to_le_bytes());
            info[16..].copy_from_slice(&entry_point.to_le_bytes());
            if memory.write_bytes(output, &info).is_err() {
                return fail(ERROR_NOACCESS);
            }
            Ok(1)
        });
        for api_set in PSAPI_APIS {
            registry.alias(api_set, module, symbol);
        }
        registry.set_status(module, symbol, ServiceStatus::Partial);
    }
    registry.alias(KERNELBASE, KERNEL32, "K32GetModuleInformation");

    for symbol in [
        "GetModuleHandleA",
        "GetModuleHandleW",
        "GetModuleHandleExA",
        "GetModuleHandleExW",
        "GetModuleFileNameA",
        "GetModuleFileNameW",
        "GetProcAddress",
        "FreeLibrary",
    ] {
        for module in LOADER_APIS {
            registry.alias(module, KERNEL32, symbol);
        }
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
    for symbol in ["LoadLibraryA", "LoadLibraryW"] {
        for module in LOAD_LIBRARY_APIS {
            registry.alias(module, KERNEL32, symbol);
        }
        // Not on Microsoft's list, but registered here before it was checked.
        registry.alias(LOADER_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }

    let mut built_in = Modules::default();
    // Preserve the startup UCRT binding.
    let references = built_in.references.get_mut().expect("module references");
    references.counts.insert(UCRT_HANDLE, 1);
    for (module, handle) in [(KERNEL32, MODULE_BASE), (UCRT, UCRT_HANDLE)] {
        built_in.names.insert(module.to_owned(), handle);
        built_in.exports.insert(
            handle,
            registry
                .canonical_exports(module)
                .map(|(key, index)| (key.clone(), index))
                .collect(),
        );
        for (alias, _, canonical) in registry.iter_aliases() {
            if canonical == module {
                built_in.names.insert(alias.to_owned(), handle);
            }
        }
    }
    // A module of its own, not another name for kernel32.
    built_in
        .names
        .insert(KERNELBASE.to_owned(), KERNELBASE_HANDLE);
    built_in.exports.insert(
        KERNELBASE_HANDLE,
        registry
            .aliased_exports(KERNELBASE)
            .map(|(key, index)| (key.clone(), index))
            .collect(),
    );
    assert!(modules.set(built_in).is_ok());
}

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
    }

    #[test]
    fn the_module_file_name_answers_the_program_path_with_truncation() {
        let mut registry = ImportRegistry::new();
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        state
            .set_program_arguments(&["c:\\games\\probe.exe"])
            .unwrap();
        register_standard_services(&mut registry, state);
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x20000, 4096, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory.main_image = Some(MainImage {
            base: 0x150000000,
            size: 0x1000,
            entry_point: 0x150001000,
        });
        let narrow_length;
        let wide_length;
        let truncated_length;
        let foreign;
        {
            let memory = &mut memory;
            let mut call = |symbol: &str, module, buffer, capacity| {
                let mut cpu = CpuState::default();
                cpu.set(Register::Rcx, module);
                cpu.set(Register::Rdx, buffer);
                cpu.set(Register::R8, capacity);
                let service = registry
                    .resolve(LOADER_API, &ImportName::Name(symbol.to_owned()))
                    .unwrap();
                (service.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap()
            };
            narrow_length = call("GetModuleFileNameA", 0, 0x20000, 64);
            wide_length = call("GetModuleFileNameW", 0x150000000, 0x20100, 64);
            truncated_length = call("GetModuleFileNameA", 0, 0x20200, 8);
            foreign = call("GetModuleFileNameA", 0xdead0000, 0x20000, 64);
        }
        // The narrow form copies the whole path and answers its length.
        let length = narrow_length;
        assert_eq!(length, 18);
        let mut bytes = [0u8; 20];
        memory.read_bytes(0x20000, &mut bytes).unwrap();
        assert_eq!(&bytes[..18], b"c:\\games\\probe.exe");
        assert_eq!(bytes[18], 0);
        // The wide form counts wide units; the main-image handle is the
        // module itself.
        assert_eq!(wide_length, 18);
        assert_eq!(memory.read_u16(0x20100).unwrap(), u16::from(b'c'));
        // A small buffer truncates, terminates and answers the capacity
        // with ERROR_INSUFFICIENT_BUFFER - the documented truncation form.
        assert_eq!(truncated_length, 8);
        let mut small = [0u8; 8];
        memory.read_bytes(0x20200, &mut small).unwrap();
        assert_eq!(&small[..7], b"c:\\game");
        assert_eq!(small[7], 0);
        // A foreign module does not exist here.
        assert_eq!(foreign, 0);
    }
}
