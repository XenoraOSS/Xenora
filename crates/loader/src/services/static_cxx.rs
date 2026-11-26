// SPDX-License-Identifier: GPL-3.0-or-later

//! Services a statically linked mingw-w64 C++ program imports (#447).
//!
//! Linked with `-static-libstdc++ -static-libgcc`, GCC's unwinder, its
//! emulated thread-local storage and the C++ runtime run as guest code. They
//! import a few `libwinpthread-1.dll` functions (once, keys and mutexes, used
//! by emulated TLS) and some C runtime and `kernel32.dll` functions the
//! dynamically linked program does not.
//!
//! The pthread services follow the winpthreads ABI in the mingw-w64 14.0.0
//! `pthread.h`: `pthread_once_t` is a 32-bit `long` starting at 0,
//! `pthread_key_t` is `unsigned` and `pthread_mutex_t` is an `intptr_t` the
//! program may set to -1 (`PTHREAD_MUTEX_INITIALIZER`). Error numbers are the
//! mingw-w64 `errno.h` values. The scheduler runs one guest thread at a time,
//! so the host keeps the lock and key state and never reads the mutex word.

use crate::cpu::ExitReason;
use crate::imports::{CallbackStart, CallbackStep, CallbackTask, ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

const PTHREAD: &str = "libwinpthread-1.dll";
const UCRT: &str = "ucrtbase.dll";
const EPERM: u64 = 1;
const EAGAIN: u64 = 11;
const EBUSY: u64 = 16;
const EINVAL: u64 = 22;
const EDEADLK: u64 = 36;
/// Keys one process may create. ponytail: a fixed bound; winpthreads' own
/// limit was not looked up.
const MAX_KEYS: u32 = 1024;
/// The stop a stub or an unmodelled request makes (see `services.txt`).
const UNSUPPORTED: ExitReason = ExitReason::IllegalInstruction(0xFE);

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

#[derive(Default)]
struct Pthreads {
    next_key: u32,
    keys: BTreeSet<u32>,
    values: HashMap<(u64, u32), u64>,
    /// Locked mutexes by guest address, with the owning thread's TEB.
    owners: HashMap<u64, u64>,
}

/// `pthread_once`: runs the routine, then returns 0.
struct Once;

impl CallbackTask for Once {
    fn resume(&mut self, _: u64, _: u64, _: &mut GuestMemory) -> Result<CallbackStep, ExitReason> {
        Ok(CallbackStep::Return(0))
    }
}

pub(super) fn register(registry: &mut ImportRegistry) {
    let state = Arc::new(Mutex::new(Pthreads::default()));
    let lock = move || Arc::clone(&state);

    // ponytail: the flag is set before the routine runs, so a thread that
    // gets a turn during the routine returns before it has finished.
    registry.register_callback_task(
        PTHREAD,
        "pthread_once",
        ServiceStatus::Partial,
        |args, _, memory, _| {
            if memory.read_u32(args.rcx).map_err(fault)? != 0 {
                return Ok(CallbackStart::Return(0));
            }
            memory.write_u32(args.rcx, 1).map_err(fault)?;
            Ok(CallbackStart::Call {
                task: Box::new(Once),
                entry: args.rdx,
                args: [0; 4],
            })
        },
    );

    // Destructors are accepted and never called.
    let s = lock();
    registry.register_with_status(
        PTHREAD,
        "pthread_key_create",
        ServiceStatus::Partial,
        move |args, _, memory| {
            let mut s = s.lock().expect("pthread state");
            if s.next_key == MAX_KEYS {
                return Ok(EAGAIN);
            }
            memory.write_u32(args.rcx, s.next_key).map_err(fault)?;
            let key = s.next_key;
            s.keys.insert(key);
            s.next_key += 1;
            Ok(0)
        },
    );
    let s = lock();
    registry.register_with_status(
        PTHREAD,
        "pthread_getspecific",
        ServiceStatus::Implemented,
        move |args, cpu, _| {
            let s = s.lock().expect("pthread state");
            Ok(s.values
                .get(&(cpu.gs_base, args.rcx as u32))
                .copied()
                .unwrap_or(0))
        },
    );
    let s = lock();
    registry.register_with_status(
        PTHREAD,
        "pthread_setspecific",
        ServiceStatus::Implemented,
        move |args, cpu, _| {
            let mut s = s.lock().expect("pthread state");
            let key = args.rcx as u32;
            if !s.keys.contains(&key) {
                return Ok(EINVAL);
            }
            s.values.insert((cpu.gs_base, key), args.rdx);
            Ok(0)
        },
    );

    let s = lock();
    registry.register_with_status(
        PTHREAD,
        "pthread_mutex_init",
        ServiceStatus::Partial,
        move |args, _, memory| {
            if args.rdx != 0 {
                // Mutex attributes (recursive and error-checking types) are not modelled.
                return Err(UNSUPPORTED);
            }
            memory.check_write(args.rcx, 8).map_err(fault)?;
            s.lock().expect("pthread state").owners.remove(&args.rcx);
            Ok(0)
        },
    );
    let s = lock();
    registry.register_with_status(
        PTHREAD,
        "pthread_mutex_lock",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            memory.check_write(args.rcx, 8).map_err(fault)?;
            let mut s = s.lock().expect("pthread state");
            match s.owners.get(&args.rcx) {
                None => {
                    s.owners.insert(args.rcx, cpu.gs_base);
                    Ok(0)
                }
                // A default mutex locked twice by its owner deadlocks; report it.
                Some(&owner) if owner == cpu.gs_base => Ok(EDEADLK),
                Some(_) => Err(ExitReason::GuestBlocked),
            }
        },
    );
    let s = lock();
    registry.register_with_status(
        PTHREAD,
        "pthread_mutex_unlock",
        ServiceStatus::Implemented,
        move |args, cpu, _| {
            let mut s = s.lock().expect("pthread state");
            if s.owners.get(&args.rcx) != Some(&cpu.gs_base) {
                return Ok(EPERM);
            }
            s.owners.remove(&args.rcx);
            Ok(0)
        },
    );
    let s = lock();
    registry.register_with_status(
        PTHREAD,
        "pthread_mutex_destroy",
        ServiceStatus::Implemented,
        move |args, _, _| {
            let s = s.lock().expect("pthread state");
            Ok(if s.owners.contains_key(&args.rcx) {
                EBUSY
            } else {
                0
            })
        },
    );

    registry.register_with_status(
        UCRT,
        "memchr",
        ServiceStatus::Implemented,
        |args, _, memory| {
            for index in 0..args.r8 {
                let address = args
                    .rcx
                    .checked_add(index)
                    .ok_or(fault(MemoryError::Overflow))?;
                if memory.read_u8(address).map_err(fault)? == args.rdx as u8 {
                    return Ok(address);
                }
            }
            Ok(0)
        },
    );
    registry.register_with_status(
        UCRT,
        "strchr",
        ServiceStatus::Implemented,
        |args, _, memory| {
            let wanted = args.rdx as u8;
            let mut address = args.rcx;
            loop {
                let byte = memory.read_u8(address).map_err(fault)?;
                if byte == wanted {
                    return Ok(address);
                }
                if byte == 0 {
                    return Ok(0);
                }
                address = address.checked_add(1).ok_or(fault(MemoryError::Overflow))?;
            }
        },
    );
    // The environment table is empty (see crt_arguments.rs), so no variable is set.
    registry.register_with_status(UCRT, "getenv", ServiceStatus::Partial, |args, _, memory| {
        memory.read_u8(args.rcx).map_err(fault)?;
        Ok(0)
    });
    // `LocalFree` is no longer a null-only stub here: the heap services
    // implement the legacy memory family since package 9 of #459.
    // `__stdio_common_vsprintf` is no longer a stub here: the typed registry
    // serves it since #466.
    // `FormatMessageA` is no longer a stub here: the error services
    // implement it since package 9 of #459. `_read` is served since #493.
    for (module, symbol) in [(UCRT, "rand_s")] {
    // `FormatMessageA` is no longer a stub here: the error services
    // implement it since package 9 of #459.
    for (module, symbol) in [(UCRT, "rand_s"), (UCRT, "_read")] {
        registry.register_with_status(module, symbol, ServiceStatus::Stub, |_, _, _| {
            Err(UNSUPPORTED)
        });
    }
    for (api_set, symbol) in [
        ("api-ms-win-crt-private-l1-1-0.dll", "memchr"),
        ("api-ms-win-crt-private-l1-1-0.dll", "strchr"),
        ("api-ms-win-crt-string-l1-1-0.dll", "memchr"),
        ("api-ms-win-crt-string-l1-1-0.dll", "strchr"),
        ("api-ms-win-crt-environment-l1-1-0.dll", "getenv"),
        ("api-ms-win-crt-utility-l1-1-0.dll", "rand_s"),
    ] {
        registry.alias(api_set, UCRT, symbol);
    }
    // The legacy CRT exports the same memory, string and environment forms
    // under the same names, as it does for the forms crt.rs serves.
    for symbol in ["memchr", "strchr", "getenv"] {
        registry.alias("msvcrt.dll", UCRT, symbol);
    }
}
