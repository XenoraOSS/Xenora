// SPDX-License-Identifier: GPL-3.0-or-later

//! User-mode CNG random number generation backed by host entropy.

use super::{MAX_RANDOM_BYTES, ServicesState, fill_random};
use crate::imports::{ImportRegistry, ServiceStatus, Win64Args};
use crate::memory::GuestMemory;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const MODULE: &str = "bcrypt.dll";
const MAX_PROVIDERS: usize = 64;
const MAX_NAME_UNITS: u64 = 256;

const BCRYPT_RNG_ALG_HANDLE: u64 = 0x81;
const BCRYPT_RNG_USE_ENTROPY_IN_BUFFER: u32 = 0x1;
const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x2;

const STATUS_SUCCESS: u32 = 0;
const STATUS_UNSUCCESSFUL: u32 = 0xC000_0001;
const STATUS_INVALID_HANDLE: u32 = 0xC000_0008;
const STATUS_INVALID_PARAMETER: u32 = 0xC000_000D;
const STATUS_NO_MEMORY: u32 = 0xC000_0017;
const STATUS_NOT_SUPPORTED: u32 = 0xC000_00BB;
const STATUS_PROCESS_IS_TERMINATING: u32 = 0xC000_010A;
const STATUS_NOT_FOUND: u32 = 0xC000_0225;

// The table is per registry. Allocate distinct tokens across registries so a
// handle from a different process cannot equal a handle in the current table.
static NEXT_PROVIDER: AtomicU64 = AtomicU64::new(0x4243_5259_0000_0004);

fn provider_name(memory: &GuestMemory, address: u64) -> Result<String, u32> {
    if address == 0 {
        return Err(STATUS_INVALID_PARAMETER);
    }
    let mut units = Vec::new();
    for index in 0..MAX_NAME_UNITS {
        let unit_address = address
            .checked_add(index * 2)
            .ok_or(STATUS_INVALID_PARAMETER)?;
        let unit = memory
            .read_u16(unit_address)
            .map_err(|_| STATUS_INVALID_PARAMETER)?;
        if unit == 0 {
            return String::from_utf16(&units).map_err(|_| STATUS_INVALID_PARAMETER);
        }
        units.push(unit);
    }
    Err(STATUS_INVALID_PARAMETER)
}

fn open(
    args: &Win64Args,
    memory: &mut GuestMemory,
    providers: &mut BTreeSet<u64>,
) -> Result<(), u32> {
    if args.rcx == 0 || !memory.can_write(args.rcx, size_of::<u64>()) {
        return Err(STATUS_INVALID_PARAMETER);
    }
    // ULONG uses only the low 32 bits of its argument register on Win64.
    if args.r9 as u32 != 0 {
        return Err(STATUS_NOT_SUPPORTED);
    }
    let algorithm = provider_name(memory, args.rdx)?;
    let implementation = if args.r8 == 0 {
        None
    } else {
        Some(provider_name(memory, args.r8)?)
    };
    // Support the exact documented spellings of the RNG and provider names.
    if algorithm != "RNG"
        || !matches!(
            implementation.as_deref(),
            None | Some("Microsoft Primitive Provider")
        )
    {
        return Err(STATUS_NOT_FOUND);
    }
    if providers.len() >= MAX_PROVIDERS {
        return Err(STATUS_NO_MEMORY);
    }
    let handle = NEXT_PROVIDER
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            next.checked_add(4)
        })
        .map_err(|_| STATUS_NO_MEMORY)?;
    memory
        .write_u64(args.rcx, handle)
        .map_err(|_| STATUS_INVALID_PARAMETER)?;
    providers.insert(handle);
    Ok(())
}

fn generate(
    args: &Win64Args,
    memory: &mut GuestMemory,
    providers: &BTreeSet<u64>,
) -> Result<(), u32> {
    let length = args.r8 as u32 as usize;
    let flags = args.r9 as u32;
    let system_preferred = flags & BCRYPT_USE_SYSTEM_PREFERRED_RNG != 0;
    // Windows 8 and later ignore BCRYPT_RNG_USE_ENTROPY_IN_BUFFER.
    if flags & !(BCRYPT_RNG_USE_ENTROPY_IN_BUFFER | BCRYPT_USE_SYSTEM_PREFERRED_RNG) != 0
        || (system_preferred && args.rcx != 0)
        || length > MAX_RANDOM_BYTES
        || args.rdx == 0
        || !memory.can_write(args.rdx, length)
    {
        return Err(STATUS_INVALID_PARAMETER);
    }
    if !system_preferred && args.rcx != BCRYPT_RNG_ALG_HANDLE && !providers.contains(&args.rcx) {
        return Err(STATUS_INVALID_HANDLE);
    }
    if length == 0 {
        return Ok(());
    }
    let mut bytes = vec![0; length];
    fill_random(&mut bytes).map_err(|_| STATUS_UNSUCCESSFUL)?;
    memory
        .write_bytes(args.rdx, &bytes)
        .map_err(|_| STATUS_INVALID_PARAMETER)
}

fn close(args: &Win64Args, providers: &mut BTreeSet<u64>) -> Result<(), u32> {
    if args.rdx as u32 != 0 {
        return Err(STATUS_INVALID_PARAMETER);
    }
    if !providers.remove(&args.rcx) {
        return Err(STATUS_INVALID_HANDLE);
    }
    Ok(())
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    type Handler = fn(&Win64Args, &mut GuestMemory, &mut BTreeSet<u64>) -> Result<(), u32>;

    let providers = Arc::new(Mutex::new(BTreeSet::new()));
    let handlers: [(&str, Handler); 3] = [
        ("BCryptOpenAlgorithmProvider", open),
        ("BCryptGenRandom", |args, memory, providers| {
            generate(args, memory, providers)
        }),
        ("BCryptCloseAlgorithmProvider", |args, _, providers| {
            close(args, providers)
        }),
    ];
    for (symbol, handler) in handlers {
        let state = Arc::clone(state);
        let providers = Arc::clone(&providers);
        registry.register_with_status(
            MODULE,
            symbol,
            ServiceStatus::Partial,
            move |args, _, memory| {
                // Serialize with ExitProcess so operations cannot publish a handle
                // or write random bytes after process teardown starts.
                let exited = state.exit_status.lock().expect("guest exit status");
                if exited.is_some() {
                    return Ok(u64::from(STATUS_PROCESS_IS_TERMINATING));
                }
                let mut providers = providers.lock().expect("CNG RNG provider table");
                let status = handler(args, memory, &mut providers)
                    .err()
                    .unwrap_or(STATUS_SUCCESS);
                Ok(u64::from(status))
            },
        );
    }
}
