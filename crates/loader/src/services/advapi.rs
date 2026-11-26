// SPDX-License-Identifier: GPL-3.0-or-later

//! The cryptographic randomness and user identity of advapi32.
//!
//! The randomness is real host entropy: a simulated machine must not offer
//! predictable bytes behind an interface whose entire contract is
//! unpredictability, so the bytes come from the host's own entropy source.
//! The provider handle is a token in the process table - acquire creates
//! it, generate validates it, release destroys it - because that lifecycle
//! is the observable behavior of the API.

use super::{ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ServicesState};
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

const MODULE: &str = "advapi32.dll";
const MAX_RANDOM_BYTES: usize = 65_536;
const ERROR_NOT_SUPPORTED: u32 = 50;

/// The live provider tokens of the process.
#[derive(Default)]
pub(super) struct CryptoProviders {
    next: u64,
    live: BTreeSet<u64>,
}

fn fill_random(buffer: &mut [u8]) -> Result<(), u32> {
    use std::io::Read;
    let mut source = std::fs::File::open("/dev/urandom").map_err(|_| ERROR_NOT_SUPPORTED)?;
    source.read_exact(buffer).map_err(|_| ERROR_NOT_SUPPORTED)
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let providers = Arc::new(Mutex::new(CryptoProviders::default()));

    let acquire = Arc::clone(&providers);
    let acquire_state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "CryptAcquireContextA",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            // The provider type must be RSA full and the flags one of the
            // documented machine-default forms; the machine has no key
            // containers, so anything but a fresh default context is honest
            // refusal.
            if args.r9 as u32 != 1
                || args.get(4, memory).map_err(ExitReason::MemoryFault)? & !0xF0000000 != 0
            {
                return acquire_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            if args.rcx == 0 {
                return acquire_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let mut live = acquire.lock().expect("crypto provider state");
            if live.live.len() >= 64 {
                return acquire_state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            live.next += 4;
            let token = 0xC0DE_0000 + live.next;
            live.live.insert(token);
            memory
                .write_u64(args.rcx, token)
                .map_err(ExitReason::MemoryFault)?;
            Ok(1)
        },
    );

    let release = Arc::clone(&providers);
    let release_state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "CryptReleaseContext",
        ServiceStatus::Partial,
        move |args, cpu, _| {
            let mut live = release.lock().expect("crypto provider state");
            if !live.live.remove(&args.rcx) {
                return release_state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
            }
            Ok(1)
        },
    );

    let generate = Arc::clone(&providers);
    let generate_state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "CryptGenRandom",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            if !generate
                .lock()
                .expect("crypto provider state")
                .live
                .contains(&args.rcx)
            {
                return generate_state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
            }
            let length = args.rdx as usize;
            if length > MAX_RANDOM_BYTES {
                return generate_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let mut bytes = vec![0u8; length];
            if fill_random(&mut bytes).is_err() {
                return generate_state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            memory
                .write_bytes(args.r8, &bytes)
                .map_err(ExitReason::MemoryFault)?;
            Ok(1)
        },
    );

    // SystemFunction036 is RtlGenRandom, the unexported-by-name form the
    // CRT and mingw binaries call directly: no provider, just the bytes.
    let sysfunc_state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "SystemFunction036",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let length = args.rdx as usize;
            if length > MAX_RANDOM_BYTES {
                return sysfunc_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let mut bytes = vec![0u8; length];
            if fill_random(&mut bytes).is_err() {
                return sysfunc_state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            memory
                .write_bytes(args.rcx, &bytes)
                .map_err(ExitReason::MemoryFault)?;
            Ok(1)
        },
    );

    let username_state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "GetUserNameA",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            // The machine has one fixed user on its simulated LAN, written
            // with its terminator included in the size contract.
            let name = b"xenora\0";
            if args.rdx == 0 {
                return username_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let capacity = memory.read_u32(args.rdx).map_err(ExitReason::MemoryFault)? as usize;
            memory
                .write_u32(args.rdx, name.len() as u32)
                .map_err(ExitReason::MemoryFault)?;
            if capacity < name.len() {
                return username_state.fail(cpu.gs_base, 122, 0); // ERROR_INSUFFICIENT_BUFFER
            }
            memory
                .write_bytes(args.rcx, name)
                .map_err(ExitReason::MemoryFault)?;
            Ok(1)
        },
    );
}

#[cfg(test)]
#[test]
fn crypto_randomness_is_real_and_the_provider_lifecycle_validates() {
    use crate::cpu::{CpuState, Register};
    use crate::import_table::ImportName;
    use crate::imports::Win64Args;
    use crate::memory::{GuestMemory, MemoryPermissions};
    use crate::services::MockClock;

    let mut registry = ImportRegistry::new();
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    register(&mut registry, &state);
    let mut memory = GuestMemory::new();
    memory
        .map_region(0x1000, 8192, MemoryPermissions::READ_WRITE)
        .unwrap();
    let call = |memory: &mut GuestMemory, symbol: &str, args: &[u64]| {
        let mut cpu = CpuState::default();
        for (reg, &value) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
            .into_iter()
            .zip(args)
        {
            cpu.set(reg, value);
        }
        cpu.set(Register::Rsp, 0x2000);
        for (index, &value) in args.iter().enumerate().skip(4) {
            memory
                .write_u64(0x2000 + 0x28 + (index as u64 - 4) * 8, value)
                .unwrap();
        }
        let descriptor = registry
            .resolve(MODULE, &ImportName::Name(symbol.to_owned()))
            .unwrap();
        (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap()
    };
    // Acquire, generate, release: the lifecycle validates.
    assert_eq!(
        call(
            &mut memory,
            "CryptAcquireContextA",
            &[0x1100, 0, 0, 1, 0xF0000000]
        ),
        1
    );
    let token = memory.read_u64(0x1100).unwrap();
    assert_ne!(token, 0);
    assert_eq!(call(&mut memory, "CryptGenRandom", &[token, 32, 0x1200]), 1);
    // Two generations differ: the bytes are real entropy, not a fixed fill.
    let mut first = [0; 32];
    memory.read_bytes(0x1200, &mut first).unwrap();
    assert_eq!(call(&mut memory, "CryptGenRandom", &[token, 32, 0x1200]), 1);
    let mut second = [0; 32];
    memory.read_bytes(0x1200, &mut second).unwrap();
    assert_ne!(first, second, "real entropy never repeats a 32-byte draw");
    // RtlGenRandom needs no provider.
    assert_eq!(call(&mut memory, "SystemFunction036", &[0x1200, 16]), 1);
    // Release destroys the token; reuse is a bad handle.
    assert_eq!(call(&mut memory, "CryptReleaseContext", &[token, 0]), 1);
    assert_eq!(call(&mut memory, "CryptGenRandom", &[token, 32, 0x1200]), 0);
    // The fixed user name with its size contract.
    memory.write_u32(0x1300, 32).unwrap();
    assert_eq!(call(&mut memory, "GetUserNameA", &[0x1400, 0x1300]), 1);
    assert_eq!(memory.read_u32(0x1300).unwrap(), 7);
    let mut name = [0; 7];
    memory.read_bytes(0x1400, &mut name).unwrap();
    assert_eq!(&name, b"xenora\0");
    // A small buffer reports the needed size and fails honestly.
    memory.write_u32(0x1300, 3).unwrap();
    assert_eq!(call(&mut memory, "GetUserNameA", &[0x1400, 0x1300]), 0);
    assert_eq!(memory.read_u32(0x1300).unwrap(), 7);
}
