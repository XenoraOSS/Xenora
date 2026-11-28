// SPDX-License-Identifier: GPL-3.0-or-later

//! Current-user registry data and handles live only in the guest's registry.

use super::{ERROR_ACCESS_DENIED, ERROR_NOT_ENOUGH_MEMORY, ERROR_NOT_SUPPORTED, ServicesState};
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus, Win64Args};
use crate::memory::{GuestMemory, MemoryError};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const ADVAPI32: &str = "advapi32.dll";
const REGISTRY_API: &str = "api-ms-win-core-registry-l1-1-0.dll";
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_MORE_DATA: u32 = 234;
const HKCU: u32 = 0x8000_0001;
const ROOTS: [u32; 5] = [
    0x8000_0000, // HKEY_CLASSES_ROOT
    HKCU,
    0x8000_0002, // HKEY_LOCAL_MACHINE
    0x8000_0003, // HKEY_USERS
    0x8000_0005, // HKEY_CURRENT_CONFIG
];
const KEY_QUERY_VALUE: u32 = 1;
const KEY_SET_VALUE: u32 = 2;
const LEGACY_ACCESS: u32 = KEY_QUERY_VALUE | KEY_SET_VALUE;
const WOW64_VIEWS: u32 = 0x0300;
const REG_DWORD: u32 = 4;
const DWORD_BYTES: u32 = 4;
const REG_CREATED_NEW_KEY: u32 = 1;
const REG_OPENED_EXISTING_KEY: u32 = 2;
const MAX_NAME_UNITS: usize = 1024;
const MAX_KEY_DEPTH: usize = 32;
const MAX_KEYS: usize = 128;
const MAX_VALUES: usize = 256;
const MAX_HANDLES: usize = 128;
const HANDLE_BASE: u64 = 0x5245_4700_0000_0000;

enum RegistryError {
    Status(u32),
    Memory(MemoryError),
}

impl From<u32> for RegistryError {
    fn from(status: u32) -> Self {
        Self::Status(status)
    }
}

impl From<MemoryError> for RegistryError {
    fn from(error: MemoryError) -> Self {
        Self::Memory(error)
    }
}

type RegistryResult<T> = Result<T, RegistryError>;

fn predefined_root(handle: u64) -> Option<u32> {
    // Win64 sign-extends the LONG from winreg.h. Older Xenora guests also
    // pass the zero-extended form of the same root constant.
    ROOTS
        .into_iter()
        .find(|&root| handle == u64::from(root) || handle == i64::from(root as i32) as u64)
}

fn requested_access(mask: u32) -> RegistryResult<u32> {
    if mask & WOW64_VIEWS == WOW64_VIEWS {
        return Err(ERROR_INVALID_PARAMETER.into());
    }
    Ok(mask & LEGACY_ACCESS)
}

fn read_name(memory: &GuestMemory, address: u64, wide: bool) -> RegistryResult<String> {
    if address == 0 {
        return Ok(String::new());
    }
    if wide {
        let mut units = Vec::new();
        for index in 0..=MAX_NAME_UNITS {
            let offset = address
                .checked_add(2 * index as u64)
                .ok_or(MemoryError::Overflow)?;
            let unit = memory.read_u16(offset)?;
            if unit == 0 {
                return String::from_utf16(&units).map_err(|_| ERROR_INVALID_PARAMETER.into());
            }
            units.push(unit);
        }
    } else {
        let mut bytes = Vec::new();
        for index in 0..=MAX_NAME_UNITS {
            let offset = address
                .checked_add(index as u64)
                .ok_or(MemoryError::Overflow)?;
            let byte = memory.read_u8(offset)?;
            if byte == 0 {
                return String::from_utf8(bytes).map_err(|_| ERROR_INVALID_PARAMETER.into());
            }
            bytes.push(byte);
        }
    }
    Err(ERROR_INVALID_PARAMETER.into())
}

fn canonical_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            let mut upper = character.to_uppercase();
            let first = upper.next().unwrap_or(character);
            // Keep an expanding lowercase spelling distinct from a name
            // that actually contains the corresponding sequence of letters.
            if upper.next().is_none() {
                first
            } else {
                character
            }
        })
        .collect()
}

fn relative_path(parent: &str, child: &str) -> RegistryResult<String> {
    if child.is_empty() {
        return Ok(parent.to_owned());
    }
    let child = canonical_name(child);
    let path = if parent.is_empty() {
        child
    } else {
        format!("{parent}\\{child}")
    };
    if path.encode_utf16().count() > MAX_NAME_UNITS
        || path.split('\\').count() > MAX_KEY_DEPTH
        || path
            .split('\\')
            .any(|part| part.is_empty() || part.encode_utf16().count() > 255)
    {
        return Err(ERROR_INVALID_PARAMETER.into());
    }
    Ok(path)
}

struct OpenKey {
    path: String,
    access: u32,
}

struct GuestRegistry {
    keys: BTreeMap<String, BTreeMap<String, u32>>,
    handles: BTreeMap<u64, OpenKey>,
    next_handle: u32,
    value_count: usize,
}

impl GuestRegistry {
    fn new() -> Self {
        Self {
            keys: BTreeMap::from([(String::new(), BTreeMap::new())]),
            handles: BTreeMap::new(),
            next_handle: 0,
            value_count: 0,
        }
    }

    fn key(&self, handle: u64) -> RegistryResult<(u32, &str, u32)> {
        if let Some(root) = predefined_root(handle) {
            return Ok((root, "", LEGACY_ACCESS));
        }
        let key = self.handles.get(&handle).ok_or(ERROR_INVALID_HANDLE)?;
        Ok((HKCU, &key.path, key.access))
    }

    fn available_handle(&self) -> RegistryResult<u64> {
        if self.handles.len() >= MAX_HANDLES {
            return Err(ERROR_NOT_ENOUGH_MEMORY.into());
        }
        let next = self
            .next_handle
            .checked_add(1)
            .ok_or(ERROR_NOT_ENOUGH_MEMORY)?;
        Ok(HANDLE_BASE + u64::from(next))
    }

    fn insert_handle(&mut self, handle: u64, path: String, access: u32) {
        self.handles.insert(handle, OpenKey { path, access });
        self.next_handle = (handle - HANDLE_BASE) as u32;
    }

    fn open(
        &mut self,
        args: &Win64Args,
        memory: &mut GuestMemory,
        legacy: bool,
        wide: bool,
    ) -> RegistryResult<()> {
        let (output, access) = if legacy {
            (args.r8, LEGACY_ACCESS)
        } else {
            if args.r8 as u32 != 0 {
                return Err(ERROR_INVALID_PARAMETER.into());
            }
            (args.get(4, memory)?, requested_access(args.r9 as u32)?)
        };
        if output == 0 {
            return Err(ERROR_INVALID_PARAMETER.into());
        }
        let (root, parent, _) = self.key(args.rcx)?;
        let subkey = read_name(memory, args.rdx, wide)?;
        if subkey.is_empty() && (legacy || predefined_root(args.rcx).is_some()) {
            memory.write_u64(output, args.rcx)?;
            return Ok(());
        }
        if root != HKCU {
            return Err(ERROR_FILE_NOT_FOUND.into());
        }
        let path = relative_path(parent, &subkey)?;
        if !self.keys.contains_key(&path) {
            return Err(ERROR_FILE_NOT_FOUND.into());
        }
        let handle = self.available_handle()?;
        memory.write_u64(output, handle)?;
        self.insert_handle(handle, path, access);
        Ok(())
    }

    fn create(&mut self, args: &Win64Args, memory: &mut GuestMemory) -> RegistryResult<()> {
        let (root, parent, _) = self.key(args.rcx)?;
        let options = args.get(4, memory)? as u32;
        let access = requested_access(args.get(5, memory)? as u32)?;
        let security = args.get(6, memory)?;
        let output = args.get(7, memory)?;
        let disposition = args.get(8, memory)?;
        if args.rdx == 0 || args.r8 as u32 != 0 || output == 0 {
            return Err(ERROR_INVALID_PARAMETER.into());
        }
        if root != HKCU {
            return Err(ERROR_ACCESS_DENIED.into());
        }
        // Both ordinary and volatile keys have the same guest-run lifetime.
        if options > 1 || security != 0 {
            return Err(ERROR_NOT_SUPPORTED.into());
        }
        let path = relative_path(parent, &read_name(memory, args.rdx, true)?)?;
        let mut missing = Vec::new();
        if !self.keys.contains_key(&path) {
            for (offset, _) in path.match_indices('\\') {
                let prefix = &path[..offset];
                if !self.keys.contains_key(prefix) {
                    missing.push(prefix);
                }
            }
            missing.push(path.as_str());
        }
        if self.keys.len() + missing.len() > MAX_KEYS {
            return Err(ERROR_NOT_ENOUGH_MEMORY.into());
        }

        let handle = self.available_handle()?;
        memory.check_write(output, 8)?;
        if disposition != 0 {
            memory.write_u32(
                disposition,
                if missing.is_empty() {
                    REG_OPENED_EXISTING_KEY
                } else {
                    REG_CREATED_NEW_KEY
                },
            )?;
        }
        memory.write_u64(output, handle)?;
        for key in missing {
            self.keys.insert(key.to_owned(), BTreeMap::new());
        }
        self.insert_handle(handle, path, access);
        Ok(())
    }

    fn set(&mut self, args: &Win64Args, memory: &GuestMemory) -> RegistryResult<()> {
        let (root, path, access) = self.key(args.rcx)?;
        if root != HKCU || access & KEY_SET_VALUE == 0 {
            return Err(ERROR_ACCESS_DENIED.into());
        }
        if args.r9 as u32 != REG_DWORD {
            return Err(ERROR_NOT_SUPPORTED.into());
        }
        let data = args.get(4, memory)?;
        if args.r8 as u32 != 0 || data == 0 || args.get(5, memory)? as u32 != DWORD_BYTES {
            return Err(ERROR_INVALID_PARAMETER.into());
        }
        let name = canonical_name(&read_name(memory, args.rdx, true)?);
        let value = memory.read_u32(data)?;
        let path = path.to_owned();
        let values = self.keys.get_mut(&path).expect("open registry key");
        if !values.contains_key(&name) {
            if self.value_count >= MAX_VALUES {
                return Err(ERROR_NOT_ENOUGH_MEMORY.into());
            }
            self.value_count += 1;
        }
        values.insert(name, value);
        Ok(())
    }

    fn query(&self, args: &Win64Args, memory: &mut GuestMemory, wide: bool) -> RegistryResult<()> {
        let (root, path, access) = self.key(args.rcx)?;
        if access & KEY_QUERY_VALUE == 0 {
            return Err(ERROR_ACCESS_DENIED.into());
        }
        let name = canonical_name(&read_name(memory, args.rdx, wide)?);
        if root != HKCU {
            return Err(ERROR_FILE_NOT_FOUND.into());
        }
        let values = self.keys.get(path).expect("open registry key");
        let value = *values.get(&name).ok_or(ERROR_FILE_NOT_FOUND)?;

        let data = args.get(4, memory)?;
        let size = args.get(5, memory)?;
        if args.r8 != 0 || data != 0 && size == 0 {
            return Err(ERROR_INVALID_PARAMETER.into());
        }
        let short = data != 0 && memory.read_u32(size)? < DWORD_BYTES;
        if data != 0 && !short {
            memory.check_write(data, DWORD_BYTES as usize)?;
        }
        if size != 0 {
            memory.check_write(size, 4)?;
        }
        if args.r9 != 0 {
            memory.write_u32(args.r9, REG_DWORD)?;
        }
        if size != 0 {
            memory.write_u32(size, DWORD_BYTES)?;
        }
        if short {
            return Err(ERROR_MORE_DATA.into());
        }
        if data != 0 {
            memory.write_u32(data, value)?;
        }
        Ok(())
    }

    fn close(&mut self, handle: u64) -> u32 {
        if predefined_root(handle).is_some() || self.handles.remove(&handle).is_some() {
            0
        } else {
            ERROR_INVALID_HANDLE
        }
    }
}

fn status(state: &ServicesState, teb: u64, result: RegistryResult<()>) -> Result<u64, ExitReason> {
    match result {
        Ok(()) => Ok(0),
        Err(RegistryError::Status(error)) => state.fail(teb, error, u64::from(error)),
        Err(RegistryError::Memory(error)) => Err(ExitReason::MemoryFault(error)),
    }
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let store = Arc::new(Mutex::new(GuestRegistry::new()));
    for (symbol, legacy, wide) in [
        ("RegOpenKeyW", true, true),
        ("RegOpenKeyExA", false, false),
        ("RegOpenKeyExW", false, true),
    ] {
        let state = Arc::clone(state);
        let store = Arc::clone(&store);
        registry.register_with_status(
            ADVAPI32,
            symbol,
            ServiceStatus::Partial,
            move |args, cpu, memory| {
                let result = store
                    .lock()
                    .expect("guest registry")
                    .open(args, memory, legacy, wide);
                status(&state, cpu.gs_base, result)
            },
        );
    }
    for (symbol, wide) in [("RegQueryValueExA", false), ("RegQueryValueExW", true)] {
        let state = Arc::clone(state);
        let store = Arc::clone(&store);
        registry.register_with_status(
            ADVAPI32,
            symbol,
            ServiceStatus::Partial,
            move |args, cpu, memory| {
                let result = store
                    .lock()
                    .expect("guest registry")
                    .query(args, memory, wide);
                status(&state, cpu.gs_base, result)
            },
        );
    }

    let create_state = Arc::clone(state);
    let create_store = Arc::clone(&store);
    registry.register_with_status(
        ADVAPI32,
        "RegCreateKeyExW",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let result = create_store
                .lock()
                .expect("guest registry")
                .create(args, memory);
            status(&create_state, cpu.gs_base, result)
        },
    );

    let set_state = Arc::clone(state);
    let set_store = Arc::clone(&store);
    registry.register_with_status(
        ADVAPI32,
        "RegSetValueExW",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let result = set_store.lock().expect("guest registry").set(args, memory);
            status(&set_state, cpu.gs_base, result)
        },
    );

    registry.register_with_status(
        ADVAPI32,
        "RegCloseKey",
        ServiceStatus::Partial,
        move |args, _, _| {
            Ok(u64::from(
                store.lock().expect("guest registry").close(args.rcx),
            ))
        },
    );

    for symbol in [
        "RegOpenKeyExA",
        "RegOpenKeyExW",
        "RegQueryValueExA",
        "RegQueryValueExW",
        "RegCloseKey",
    ] {
        registry.alias(REGISTRY_API, ADVAPI32, symbol);
    }
}
