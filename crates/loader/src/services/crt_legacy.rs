//! Small legacy CRT services and the corresponding UCRT clock and random
//! exports. Each CRT owns its own random state in the guest process.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError};

use super::ServicesState;

/// The legacy CRT module that pre-UCRT mingw binaries import directly.
const MSVCRT: &str = "msvcrt.dll";
const UCRT: &str = "ucrtbase.dll";
const UCRT_TIME_API: &str = "api-ms-win-crt-time-l1-1-0.dll";
const UCRT_UTILITY_API: &str = "api-ms-win-crt-utility-l1-1-0.dll";

/// 100-nanosecond intervals between the Windows and the Unix epochs
/// (Microsoft Learn, "FILETIME").
const FILETIME_TO_UNIX: u64 = 116_444_736_000_000_000;
const TICKS_PER_SECOND: u64 = 10_000_000;
/// Upper bound used by the Windows SDK 10.0.22621 UCRT `_time64`.
const MAX_UCRT_TIME64: i128 = 32_536_799_999;

fn fault(error: MemoryError) -> crate::cpu::ExitReason {
    crate::cpu::ExitReason::MemoryFault(error)
}

/// One CRT's `rand` state. Microsoft documents the recurrence in the
/// CRT itself — `hold = hold * 214013 + 2531011` — and the result is bits 16
/// to 30 of it, as the reference sequence for seed 1 shows:
/// 41, 18467, 6334, 26500, 19169. The calling thread starts at seed 1.
#[derive(Default)]
struct RandState(Mutex<BTreeMap<u32, u32>>);

impl RandState {
    fn update(
        &self,
        state: &ServicesState,
        teb: u64,
        operation: impl FnOnce(&mut u32) -> u64,
    ) -> u64 {
        let threads = state.threads.lock().expect("guest thread table");
        // Without the scheduler, only the initial guest thread exists.
        let id = threads.id_of(teb).unwrap_or(state.thread_id);
        let mut seeds = self.0.lock().expect("CRT rand state");
        if !seeds.contains_key(&id) {
            // A worker can inherit a TEB address but never a thread ID.
            // Drop retired workers when a new caller first needs a seed.
            seeds.retain(|known, _| {
                *known == state.thread_id || threads.threads.contains_key(known)
            });
        }
        operation(seeds.entry(id).or_insert(1))
    }
}

fn store_time64(
    memory: &mut GuestMemory,
    destination: u64,
    seconds: u64,
) -> Result<u64, crate::cpu::ExitReason> {
    if destination != 0 {
        memory.write_u64(destination, seconds).map_err(fault)?;
    }
    Ok(seconds)
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    // `__iob_func` answers the legacy CRT's array of three `FILE` objects,
    // materialized in the shared streams so the legacy stream services
    // recognize the pointers: the direction flag sits at offset 24 and the
    // file descriptor at 28, as the mingw-w64 `_iobuf` header fixes them,
    // and the zero buffer fields make every stream unbuffered, exactly as
    // on Windows.
    let iob_state = Arc::clone(state);
    registry.register(MSVCRT, "__iob_func", move |_, _, memory| {
        iob_state.crt_streams().materialize_legacy_files(memory)
    });

    // `_time64` answers the seconds since the Unix epoch and stores them
    // through the argument when it is not null. The hosted clock is UTC, so
    // no timezone conversion applies.
    let s = Arc::clone(state);
    registry.register(MSVCRT, "_time64", move |args, _, memory| {
        let filetime = s.clock.now_filetime();
        let seconds = filetime.saturating_sub(FILETIME_TO_UNIX) / TICKS_PER_SECOND;
        store_time64(memory, args.rcx, seconds)
    });

    let clock_state = Arc::clone(state);
    registry.register_with_status(
        UCRT,
        "_time64",
        ServiceStatus::Implemented,
        move |args, _, memory| {
            let filetime = i128::from(clock_state.clock.now_filetime());
            let seconds = (filetime - i128::from(FILETIME_TO_UNIX)) / i128::from(TICKS_PER_SECOND);
            let timestamp = if seconds <= MAX_UCRT_TIME64 {
                seconds as i64
            } else {
                -1
            };
            store_time64(memory, args.rcx, timestamp as u64)
        },
    );
    registry.alias(UCRT_TIME_API, UCRT, "_time64");

    // `_tzset` computes the timezone variables from the host's rules. The
    // hosted environment is UTC — documented at the `_timezone` data import —
    // so the variables already hold their only values and there is nothing
    // to compute. The function is void: on Windows it returns nothing, so a
    // caller may use whatever RAX it happens to leave; zero is honest here.
    registry.register(MSVCRT, "_tzset", |_, _, _| Ok(0));

    // `_umask` sets the file creation mask and answers the previous one. The
    // hosted runtime's files carry no permission bits yet, so the mask is
    // state without effect today, which is what makes this partial.
    let umask = Arc::new(Mutex::new(0u64));
    let mask = Arc::clone(&umask);
    registry.register(MSVCRT, "_umask", move |args, _, _| {
        let mut current = mask.lock().expect("creation mask");
        Ok(std::mem::replace(&mut *current, args.rcx & 0o777))
    });

    // `_getpid` answers the same identifier as GetCurrentProcessId.
    let pid = state.process_id;
    registry.register(MSVCRT, "_getpid", move |_, _, _| Ok(u64::from(pid)));

    register_random(registry, state, MSVCRT);
    register_random(registry, state, UCRT);
    for symbol in ["rand", "srand"] {
        registry.set_status(UCRT, symbol, ServiceStatus::Implemented);
        registry.alias(UCRT_UTILITY_API, UCRT, symbol);
    }
}

fn register_random(registry: &mut ImportRegistry, state: &Arc<ServicesState>, library: &str) {
    let generator = Arc::new(RandState::default());
    let rand = Arc::clone(&generator);
    let rand_state = Arc::clone(state);
    registry.register(library, "rand", move |_, cpu, _| {
        Ok(rand.update(&rand_state, cpu.gs_base, |seed| {
            *seed = seed.wrapping_mul(214_013).wrapping_add(2_531_011);
            u64::from((*seed >> 16) & 0x7FFF)
        }))
    });

    let srand_state = Arc::clone(state);
    registry.register(library, "srand", move |args, cpu, _| {
        Ok(generator.update(&srand_state, cpu.gs_base, |seed| {
            *seed = args.rcx as u32;
            0
        }))
    });
}
