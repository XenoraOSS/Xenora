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

use super::ServicesState;
use super::advapi::fill_random;
use super::crt::set_errno;
use crate::cpu::ExitReason;
use crate::imports::{CallbackStart, CallbackStep, CallbackTask, ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError, PROCESS_HEAP};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

const PTHREAD: &str = "libwinpthread-1.dll";
const UCRT: &str = "ucrtbase.dll";
const EPERM: u64 = 1;
const EAGAIN: u64 = 11;
const EBUSY: u64 = 16;
const EIO: u64 = 5;
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

fn rand_s_result(
    memory: &mut GuestMemory,
    teb: u64,
    output: u64,
    draw: impl FnOnce(&mut [u8]) -> Result<(), u32>,
) -> Result<u64, ExitReason> {
    if output == 0 {
        set_errno(memory, teb, EINVAL as u32)?;
        return Ok(EINVAL);
    }
    let mut bytes = [0; 4];
    if draw(&mut bytes).is_err() {
        memory.write_u32(output, 0).map_err(fault)?;
        set_errno(memory, teb, EIO as u32)?;
        return Ok(EIO);
    }
    memory.write_bytes(output, &bytes).map_err(fault)?;
    Ok(0)
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

/// The CRT environment lookup shared by `getenv` and `getenv_s`: read the
/// narrow name at the guest address and find it case-insensitively in the
/// CRT's own copy, answering the original casing and the guest pointer of
/// the CRT-owned `NAME=VALUE` string.
fn crt_environment_lookup(
    state: &Arc<ServicesState>,
    memory: &mut GuestMemory,
    name_addr: u64,
) -> Result<Option<(String, u64)>, ExitReason> {
    let mut name_bytes = Vec::new();
    let mut at = name_addr;
    loop {
        let byte = memory.read_u8(at).map_err(fault)?;
        if byte == 0 {
            break;
        }
        name_bytes.push(byte);
        at = at.checked_add(1).ok_or(fault(MemoryError::Overflow))?;
        if name_bytes.len() >= 1024 {
            break;
        }
    }
    let name = String::from_utf8_lossy(&name_bytes).into_owned();
    let found = state
        .crt_environment
        .lock()
        .expect("crt environment")
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(&name))
        .map(|(key, ptr)| (key.clone(), *ptr));
    Ok(found)
}

pub(super) fn register(registry: &mut ImportRegistry, crt_state: &Arc<ServicesState>) {
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
    // The C runtime's own environment, the copy `_putenv` maintains and
    // `getenv` reads - separate from the kernel32 block exactly as on
    // Windows, where the CRT keeps its own. Strings are copied into
    // CRT-owned guest memory because the CRT owns them once inserted;
    // removals leak the old copy, which no correct program can observe.
    // What this does not yet do is rebuild the `_environ` table the
    // startup snapshot fixed: programs reading `_environ` directly see
    // the empty startup table, honestly documented.
    let get_state = Arc::clone(crt_state);
    registry.register_with_status(
        UCRT,
        "getenv",
        ServiceStatus::Partial,
        move |args, _, memory| {
            // An empty or null name never resolves.
            if args.rcx == 0 || memory.read_u8(args.rcx).map_err(fault)? == 0 {
                return Ok(0);
            }
            match crt_environment_lookup(&get_state, memory, args.rcx)? {
                Some((key, pointer)) => Ok(pointer + key.len() as u64 + 1),
                None => Ok(0),
            }
        },
    );

    // `_putenv` inserts or removes one variable of the CRT's own
    // environment. `NAME=` removes; a missing equals sign or empty name is
    // the documented EINVAL.
    let put_state = Arc::clone(crt_state);
    registry.register_with_status(
        UCRT,
        "_putenv",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let mut bytes = Vec::new();
            let mut at = args.rcx;
            loop {
                let byte = memory.read_u8(at).map_err(fault)?;
                if byte == 0 {
                    break;
                }
                bytes.push(byte);
                at = at.checked_add(1).ok_or(fault(MemoryError::Overflow))?;
                if bytes.len() >= 32768 {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let invalid = |memory: &mut GuestMemory| -> Result<u64, ExitReason> {
                set_errno(memory, cpu.gs_base, EINVAL as u32)?;
                Ok(u64::from(u32::MAX))
            };
            let Some(equals) = text.find('=') else {
                return invalid(memory);
            };
            let name = &text[..equals];
            if name.is_empty() {
                return invalid(memory);
            }
            let mut environment = put_state.crt_environment.lock().expect("crt environment");
            if equals == text.len() - 1 {
                // `NAME=` removes the variable.
                environment.retain(|(key, _)| !key.eq_ignore_ascii_case(name));
                return Ok(0);
            }
            // The CRT owns the string once inserted: copy it into CRT-owned
            // guest memory so the answer survives the caller reusing its buffer.
            let allocation = memory
                .map_heap_allocation(PROCESS_HEAP, bytes.len() + 1)
                .map_err(fault)?;
            memory.write_bytes(allocation, &bytes).map_err(fault)?;
            memory
                .write_u8(allocation + bytes.len() as u64, 0)
                .map_err(fault)?;
            if let Some(entry) = environment
                .iter_mut()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
            {
                entry.0 = name.to_owned();
                entry.1 = allocation;
            } else {
                environment.push((name.to_owned(), allocation));
            }
            Ok(0)
        },
    );

    // `getenv_s` is the bounded form over the same CRT copy: undefined is
    // the documented empty answer, a defined variable reports its size and
    // copies while the buffer holds it, ERANGE honestly when it cannot.
    let gets_state = Arc::clone(crt_state);
    registry.register_with_status(
        UCRT,
        "getenv_s",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let (size_at, buffer, buffer_size, name) = (args.rcx, args.rdx, args.r8, args.r9);
            if size_at == 0 || name == 0 {
                set_errno(memory, cpu.gs_base, EINVAL as u32)?;
                return Ok(EINVAL);
            }
            let found = if memory.read_u8(name).map_err(fault)? == 0 {
                None
            } else {
                crt_environment_lookup(&gets_state, memory, name)?
            };
            let Some((key, pointer)) = found else {
                memory
                    .write_u64(size_at, 0)
                    .map_err(ExitReason::MemoryFault)?;
                if buffer != 0 && buffer_size != 0 {
                    memory
                        .write_u8(buffer, 0)
                        .map_err(ExitReason::MemoryFault)?;
                }
                return Ok(0);
            };
            // Read the value out of the CRT-owned NAME=VALUE string.
            let value_at = pointer + key.len() as u64 + 1;
            let mut value = Vec::new();
            let mut at = value_at;
            loop {
                let byte = memory.read_u8(at).map_err(fault)?;
                if byte == 0 {
                    break;
                }
                value.push(byte);
                at = at.checked_add(1).ok_or(fault(MemoryError::Overflow))?;
            }
            let needed = value.len() as u64 + 1;
            memory
                .write_u64(size_at, needed)
                .map_err(ExitReason::MemoryFault)?;
            if buffer == 0 || buffer_size < needed {
                set_errno(memory, cpu.gs_base, 34)?; // ERANGE
                return Ok(34);
            }
            memory.write_bytes(buffer, &value).map_err(fault)?;
            memory
                .write_u8(buffer + value.len() as u64, 0)
                .map_err(fault)?;
            Ok(0)
        },
    );
    // `LocalFree` is no longer a null-only stub here: the heap services
    // implement the legacy memory family since package 9 of #459.
    // `__stdio_common_vsprintf` is no longer a stub here: the typed registry
    // serves it since #466.
    // `FormatMessageA` is no longer a stub here: the error services
    // implement it since package 9 of #459. `_read` is served since #493.
    // Microsoft documents `rand_s` as operating-system cryptographic
    // randomness independent of `srand`. A null pointer continues here as
    // EINVAL, as the CRT documents when the invalid-parameter handler returns;
    // Xenora does not yet dispatch that handler. Another host-entropy failure
    // zeroes the output before reporting EIO, Xenora's errno for an entropy
    // read failure.
    registry.register_with_status(
        UCRT,
        "rand_s",
        ServiceStatus::Partial,
        |args, cpu, memory| rand_s_result(memory, cpu.gs_base, args.rcx, fill_random),
    );
    for (api_set, symbol) in [
        ("api-ms-win-crt-private-l1-1-0.dll", "memchr"),
        ("api-ms-win-crt-private-l1-1-0.dll", "strchr"),
        ("api-ms-win-crt-string-l1-1-0.dll", "memchr"),
        ("api-ms-win-crt-string-l1-1-0.dll", "strchr"),
        ("api-ms-win-crt-environment-l1-1-0.dll", "getenv"),
        ("api-ms-win-crt-environment-l1-1-0.dll", "_putenv"),
        ("api-ms-win-crt-environment-l1-1-0.dll", "getenv_s"),
        ("api-ms-win-crt-utility-l1-1-0.dll", "rand_s"),
    ] {
        registry.alias(api_set, UCRT, symbol);
    }
    // The legacy CRT exports the same memory, string and environment forms
    // under the same names, as it does for the forms crt.rs serves.
    for symbol in ["memchr", "strchr", "getenv", "_putenv", "getenv_s"] {
        registry.alias("msvcrt.dll", UCRT, symbol);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{CpuState, Register};
    use crate::import_table::ImportName;
    use crate::imports::Win64Args;
    use crate::memory::MemoryPermissions;

    fn memory() -> GuestMemory {
        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 0x1000, MemoryPermissions::READ_WRITE)
            .expect("map rand_s test memory");
        memory
    }

    fn errno(memory: &mut GuestMemory, teb: u64) -> u32 {
        let address = memory.crt_errno_address(teb).expect("errno cell");
        memory.read_u32(address).expect("read errno")
    }

    #[test]
    fn rand_s_host_entropy_writes_full_words_and_resolves_its_api_set() {
        let mut registry = ImportRegistry::new();
        let state = Arc::new(ServicesState::new(Arc::new(
            crate::services::MockClock::new(0),
        )));
        register(&mut registry, &state);
        let mut memory = memory();
        let call = |memory: &mut GuestMemory, output: u64| {
            let mut cpu = CpuState::default();
            cpu.set(Register::Rcx, output);
            cpu.set(Register::Rsp, 0x1f00);
            let descriptor = registry
                .resolve(UCRT, &ImportName::Name("rand_s".to_owned()))
                .expect("rand_s registration");
            (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).expect("rand_s call")
        };

        for index in 0..8 {
            assert_eq!(call(&mut memory, 0x1000 + index * 4), 0);
        }
        let mut values = [0u32; 8];
        for (index, value) in values.iter_mut().enumerate() {
            *value = memory
                .read_u32(0x1000 + index as u64 * 4)
                .expect("read random word");
        }
        assert!(values.iter().any(|&value| value != 0));
        assert!(values.windows(2).any(|pair| pair[0] != pair[1]));
        assert!(
            registry
                .resolve(
                    "api-ms-win-crt-utility-l1-1-0.dll",
                    &ImportName::Name("rand_s".to_owned()),
                )
                .is_some()
        );
    }

    #[test]
    fn rand_s_null_output_returns_and_records_einval() {
        let mut memory = memory();
        assert_eq!(
            rand_s_result(&mut memory, 0x5000, 0, |_| Ok(())).expect("null result"),
            EINVAL
        );
        assert_eq!(errno(&mut memory, 0x5000), EINVAL as u32);
    }

    #[test]
    fn rand_s_entropy_failure_zeroes_output_and_reports_eio() {
        let mut memory = memory();
        memory.write_u32(0x1000, 0xaaaa_5555).unwrap();
        assert_eq!(
            rand_s_result(&mut memory, 0x6000, 0x1000, |_| Err(50)).expect("failure result"),
            EIO
        );
        assert_eq!(memory.read_u32(0x1000).unwrap(), 0);
        assert_eq!(errno(&mut memory, 0x6000), EIO as u32);
    }

    #[test]
    fn rand_s_success_writes_little_endian_bytes_without_clearing_errno() {
        let mut memory = memory();
        set_errno(&mut memory, 0x7000, EINVAL as u32).expect("seed errno");
        assert_eq!(
            rand_s_result(&mut memory, 0x7000, 0x1000, |bytes| {
                bytes.copy_from_slice(&[0x78, 0x56, 0x34, 0x12]);
                Ok(())
            })
            .expect("success result"),
            0
        );
        assert_eq!(memory.read_u32(0x1000).unwrap(), 0x1234_5678);
        assert_eq!(errno(&mut memory, 0x7000), EINVAL as u32);
    }
}

#[cfg(test)]
#[test]
fn putenv_maintains_the_crt_copy_getenv_reads() {
    use crate::cpu::{CpuState, Register};
    use crate::import_table::ImportName;
    use crate::imports::Win64Args;
    use crate::memory::MemoryPermissions;
    use crate::services::MockClock;

    let mut registry = ImportRegistry::new();
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    register(&mut registry, &state);
    let mut memory = GuestMemory::new();
    memory
        .map_region(0x1000, 0x1000, MemoryPermissions::READ_WRITE)
        .unwrap();
    memory
        .map_region(0x4000, 0x4000, MemoryPermissions::READ_WRITE)
        .unwrap();
    let call = |memory: &mut GuestMemory, symbol: &str, args: &[u64]| {
        let mut cpu = CpuState::default();
        for (reg, &value) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
            .into_iter()
            .zip(args)
        {
            cpu.set(reg, value);
        }
        cpu.set(Register::Rsp, 0x5000);
        let descriptor = registry
            .resolve(UCRT, &ImportName::Name(symbol.to_owned()))
            .unwrap();
        (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap()
    };
    let write = |memory: &mut GuestMemory, at: u64, text: &[u8]| {
        memory.write_bytes(at, text).unwrap();
        memory.write_u8(at + text.len() as u64, 0).unwrap();
    };
    // Undefined before any insertion.
    write(&mut memory, 0x6000, b"XENO_TEST");
    assert_eq!(call(&mut memory, "getenv", &[0x6000]), 0);
    // Insert, then read back the value bytes.
    write(&mut memory, 0x6100, b"XENO_TEST=hello");
    assert_eq!(call(&mut memory, "_putenv", &[0x6100]), 0);
    let value = call(&mut memory, "getenv", &[0x6000]);
    assert_ne!(value, 0);
    let mut bytes = [0; 5];
    memory.read_bytes(value, &mut bytes).unwrap();
    assert_eq!(&bytes, b"hello");
    // Case-insensitive lookup, as the Windows environment is.
    write(&mut memory, 0x6000, b"xeno_test");
    assert_eq!(call(&mut memory, "getenv", &[0x6000]), value);
    // Replacement answers the new value even though the caller's buffer
    // is gone - the CRT owns its copy.
    write(&mut memory, 0x6100, b"XENO_TEST=world!");
    assert_eq!(call(&mut memory, "_putenv", &[0x6100]), 0);
    write(&mut memory, 0x6100, b"xxxxxxxxxxxxxxx");
    let value = call(&mut memory, "getenv", &[0x6000]);
    let mut bytes = [0; 6];
    memory.read_bytes(value, &mut bytes).unwrap();
    assert_eq!(&bytes, b"world!");
    // `NAME=` removes.
    write(&mut memory, 0x6100, b"XENO_TEST=");
    assert_eq!(call(&mut memory, "_putenv", &[0x6100]), 0);
    write(&mut memory, 0x6000, b"XENO_TEST");
    assert_eq!(call(&mut memory, "getenv", &[0x6000]), 0);
    // Malformed inputs are the documented failure, not a crash.
    write(&mut memory, 0x6100, b"NOEQUALSSIGN");
    assert_eq!(call(&mut memory, "_putenv", &[0x6100]) as u32, u32::MAX);
    write(&mut memory, 0x6100, b"=empty");
    assert_eq!(call(&mut memory, "_putenv", &[0x6100]) as u32, u32::MAX);
}
