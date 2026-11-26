// SPDX-License-Identifier: GPL-3.0-or-later

//! Unnamed event, semaphore and mutex objects and waits. A wait that cannot
//! resolve at once blocks the guest thread if a scheduler is attached, and is
//! unsupported otherwise.

use super::threads::{Pending, PendingWait};
use super::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_NOACCESS,
    ERROR_NOT_ENOUGH_MEMORY, ERROR_NOT_SUPPORTED, GuestObject, KERNEL32, ServicesState,
};
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::MemoryError;
use std::sync::{Arc, Mutex};

const SYNCH_API: &str = "api-ms-win-core-synch-l1-1-0.dll";
const CREATE_EVENT_MANUAL_RESET: u32 = 0x0001;
const CREATE_EVENT_INITIAL_SET: u32 = 0x0002;
const EVENT_MODIFY_STATE: u32 = 0x0002;
pub(super) const SYNCHRONIZE: u32 = 0x0010_0000;
const EVENT_ALL_ACCESS: u32 = 0x001f_0003;
const WAIT_OBJECT_0: u64 = 0;
pub(super) const INFINITE: u32 = u32::MAX;
pub(super) const TICKS_PER_MILLI: u64 = 10_000;
const WAIT_TIMEOUT: u64 = 258;
const WAIT_FAILED: u64 = u32::MAX as u64;
const WAIT_ABANDONED_0: u64 = 0x80;

const SEMAPHORE_ALL_ACCESS: u32 = 0x001f_0003;
const SEMAPHORE_MODIFY_STATE: u32 = 2;
const ERROR_TOO_MANY_POSTS: u32 = 298;

const MUTEX_ALL_ACCESS: u32 = 0x001f_0001;
const CREATE_MUTEX_INITIAL_OWNER: u32 = 1;
const ERROR_NOT_OWNER: u32 = 288;

pub(super) enum Waitable {
    Event {
        manual_reset: bool,
        signaled: bool,
    },
    Semaphore {
        count: i32,
        maximum: i32,
    },
    /// Owned by a guest TEB; the owner may acquire it recursively.
    Mutex {
        owner: Option<u64>,
        recursion: u32,
        /// The owner ended without releasing it.
        abandoned: bool,
    },
    /// Signaled for good once the guest thread has ended.
    Thread {
        id: u32,
        exit_code: Option<u32>,
    },
}

impl Waitable {
    fn is_ready(&self, teb: u64) -> bool {
        match self {
            Self::Event { signaled, .. } => *signaled,
            Self::Semaphore { count, .. } => *count > 0,
            Self::Mutex { owner, .. } => owner.is_none_or(|owner| owner == teb),
            Self::Thread { exit_code, .. } => exit_code.is_some(),
        }
    }

    /// Take the signal; true if it was an abandoned mutex.
    fn consume(&mut self, teb: u64) -> bool {
        match self {
            Self::Event {
                manual_reset,
                signaled,
            } => *signaled = *manual_reset,
            Self::Semaphore { count, .. } => *count -= 1,
            Self::Mutex {
                owner,
                recursion,
                abandoned,
            } => {
                *owner = Some(teb);
                *recursion += 1;
                return std::mem::take(abandoned);
            }
            Self::Thread { .. } => {}
        }
        false
    }

    fn at_recursion_limit(&self) -> bool {
        matches!(self, Self::Mutex { recursion, .. } if *recursion >= i32::MAX as u32)
    }
}

fn insert_waitable(
    state: &ServicesState,
    teb: u64,
    object: Waitable,
    access: u32,
) -> Result<u64, ExitReason> {
    let mut objects = state.objects.lock().expect("guest object state");
    let Some(handle) = objects.next_handle() else {
        return state.fail(teb, ERROR_NOT_ENOUGH_MEMORY, 0);
    };
    objects.insert_handle(
        handle,
        GuestObject::Waitable {
            state: Arc::new(Mutex::new(object)),
            access,
        },
    );
    Ok(handle)
}

fn insert_mutex(
    state: &ServicesState,
    teb: u64,
    initial_owner: bool,
    access: u32,
) -> Result<u64, ExitReason> {
    let object = Waitable::Mutex {
        owner: initial_owner.then_some(teb),
        recursion: u32::from(initial_owner),
        abandoned: false,
    };
    insert_waitable(state, teb, object, access)
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    for symbol in ["CreateEventA", "CreateEventW"] {
        let state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, mem| {
            let attributes = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
            let manual_reset = args.get(1, mem).map_err(ExitReason::MemoryFault)? as u32 != 0;
            let signaled = args.get(2, mem).map_err(ExitReason::MemoryFault)? as u32 != 0;
            let name = args.get(3, mem).map_err(ExitReason::MemoryFault)?;
            if attributes != 0 || name != 0 {
                return state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            insert_waitable(
                &state,
                cpu.gs_base,
                Waitable::Event {
                    manual_reset,
                    signaled,
                },
                EVENT_ALL_ACCESS,
            )
        });
    }

    for symbol in ["CreateEventExA", "CreateEventExW"] {
        let state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, mem| {
            let attributes = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
            let name = args.get(1, mem).map_err(ExitReason::MemoryFault)?;
            let flags = args.get(2, mem).map_err(ExitReason::MemoryFault)? as u32;
            let access = args.get(3, mem).map_err(ExitReason::MemoryFault)? as u32;
            if attributes != 0 || name != 0 || access & !EVENT_ALL_ACCESS != 0 {
                return state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            if flags & !(CREATE_EVENT_MANUAL_RESET | CREATE_EVENT_INITIAL_SET) != 0 {
                return state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            insert_waitable(
                &state,
                cpu.gs_base,
                Waitable::Event {
                    manual_reset: flags & CREATE_EVENT_MANUAL_RESET != 0,
                    signaled: flags & CREATE_EVENT_INITIAL_SET != 0,
                },
                access,
            )
        });
    }

    for (symbol, signaled) in [("SetEvent", true), ("ResetEvent", false)] {
        let state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, mem| {
            let handle = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
            let objects = state.objects.lock().expect("guest object state");
            let Some(GuestObject::Waitable {
                state: event,
                access,
            }) = objects.handles.get(&handle)
            else {
                return state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
            };
            let mut event = event.lock().expect("guest waitable state");
            let Waitable::Event {
                signaled: current, ..
            } = &mut *event
            else {
                return state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
            };
            if access & EVENT_MODIFY_STATE == 0 {
                return state.fail(cpu.gs_base, ERROR_ACCESS_DENIED, 0);
            }
            *current = signaled;
            if signaled {
                state.hooks.signal();
            }
            Ok(1)
        });
    }

    for (symbol, extended) in [
        ("CreateSemaphoreA", false),
        ("CreateSemaphoreW", false),
        ("CreateSemaphoreExA", true),
        ("CreateSemaphoreExW", true),
    ] {
        let state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, mem| {
            let attributes = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
            let count = args.get(1, mem).map_err(ExitReason::MemoryFault)? as i32;
            let maximum = args.get(2, mem).map_err(ExitReason::MemoryFault)? as i32;
            let name = args.get(3, mem).map_err(ExitReason::MemoryFault)?;
            let access = if extended {
                let flags = args.get(4, mem).map_err(ExitReason::MemoryFault)? as u32;
                let access = args.get(5, mem).map_err(ExitReason::MemoryFault)? as u32;
                if flags != 0 {
                    return state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
                }
                access
            } else {
                SEMAPHORE_ALL_ACCESS
            };
            if attributes != 0 || name != 0 || access & !SEMAPHORE_ALL_ACCESS != 0 {
                return state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            if maximum <= 0 || count < 0 || count > maximum {
                return state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            insert_waitable(
                &state,
                cpu.gs_base,
                Waitable::Semaphore { count, maximum },
                access,
            )
        });
    }

    let release = Arc::clone(state);
    registry.register(KERNEL32, "ReleaseSemaphore", move |args, cpu, mem| {
        let handle = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
        let amount = args.get(1, mem).map_err(ExitReason::MemoryFault)? as i32;
        let output = args.get(2, mem).map_err(ExitReason::MemoryFault)?;
        if amount <= 0 {
            return release.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
        }
        let objects = release.objects.lock().expect("guest object state");
        let Some(GuestObject::Waitable { state, access }) = objects.handles.get(&handle) else {
            return release.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        };
        let mut state = state.lock().expect("guest waitable state");
        let Waitable::Semaphore { count, maximum } = &mut *state else {
            return release.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        };
        if access & SEMAPHORE_MODIFY_STATE == 0 {
            return release.fail(cpu.gs_base, ERROR_ACCESS_DENIED, 0);
        }
        if amount > *maximum - *count {
            return release.fail(cpu.gs_base, ERROR_TOO_MANY_POSTS, 0);
        }
        if output != 0 && mem.write_u32(output, *count as u32).is_err() {
            return release.fail(cpu.gs_base, ERROR_NOACCESS, 0);
        }
        *count += amount;
        release.hooks.signal();
        Ok(1)
    });

    for symbol in ["CreateMutexA", "CreateMutexW"] {
        let state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, mem| {
            let attributes = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
            let initial_owner = args.get(1, mem).map_err(ExitReason::MemoryFault)? as u32 != 0;
            let name = args.get(2, mem).map_err(ExitReason::MemoryFault)?;
            if attributes != 0 || name != 0 {
                return state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            insert_mutex(&state, cpu.gs_base, initial_owner, MUTEX_ALL_ACCESS)
        });
    }

    for symbol in ["CreateMutexExA", "CreateMutexExW"] {
        let state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, mem| {
            let attributes = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
            let name = args.get(1, mem).map_err(ExitReason::MemoryFault)?;
            let flags = args.get(2, mem).map_err(ExitReason::MemoryFault)? as u32;
            let access = args.get(3, mem).map_err(ExitReason::MemoryFault)? as u32;
            if attributes != 0 || name != 0 || access & !MUTEX_ALL_ACCESS != 0 {
                return state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            if flags & !CREATE_MUTEX_INITIAL_OWNER != 0 {
                return state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let initial_owner = flags & CREATE_MUTEX_INITIAL_OWNER != 0;
            insert_mutex(&state, cpu.gs_base, initial_owner, access)
        });
    }

    let release = Arc::clone(state);
    registry.register(KERNEL32, "ReleaseMutex", move |args, cpu, mem| {
        let handle = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
        let objects = release.objects.lock().expect("guest object state");
        let Some(GuestObject::Waitable { state, .. }) = objects.handles.get(&handle) else {
            return release.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        };
        let mut state = state.lock().expect("guest waitable state");
        let Waitable::Mutex {
            owner, recursion, ..
        } = &mut *state
        else {
            return release.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        };
        if *owner != Some(cpu.gs_base) {
            return release.fail(cpu.gs_base, ERROR_NOT_OWNER, 0);
        }
        *recursion -= 1;
        if *recursion == 0 {
            *owner = None;
            release.hooks.signal();
        }
        Ok(1)
    });

    for (symbol, extended) in [
        ("WaitForSingleObject", false),
        ("WaitForSingleObjectEx", true),
    ] {
        let single = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, mem| {
            if let Some(result) = resume_wait(&single, cpu.gs_base) {
                return result;
            }
            let handle = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
            let timeout = args.get(1, mem).map_err(ExitReason::MemoryFault)? as u32;
            if extended && args.get(2, mem).map_err(ExitReason::MemoryFault)? as u32 != 0 {
                return single.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, WAIT_FAILED);
            }
            wait_for_objects(&single, cpu.gs_base, &[handle], false, timeout)
        });
    }
    for (symbol, extended) in [
        ("WaitForMultipleObjects", false),
        ("WaitForMultipleObjectsEx", true),
    ] {
        let multiple = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, mem| {
            if let Some(result) = resume_wait(&multiple, cpu.gs_base) {
                return result;
            }
            let count = args.get(0, mem).map_err(ExitReason::MemoryFault)? as u32;
            let address = args.get(1, mem).map_err(ExitReason::MemoryFault)?;
            let all = args.get(2, mem).map_err(ExitReason::MemoryFault)? as u32 != 0;
            let timeout = args.get(3, mem).map_err(ExitReason::MemoryFault)? as u32;
            if extended && args.get(4, mem).map_err(ExitReason::MemoryFault)? as u32 != 0 {
                return multiple.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, WAIT_FAILED);
            }
            if !(1..=64).contains(&count) || address == 0 {
                return multiple.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, WAIT_FAILED);
            }
            let mut handles = [0; 64];
            let handles = &mut handles[..count as usize];
            for (index, handle) in handles.iter_mut().enumerate() {
                let slot = address
                    .checked_add(index as u64 * 8)
                    .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
                *handle = mem.read_u64(slot).map_err(ExitReason::MemoryFault)?;
            }
            wait_for_objects(&multiple, cpu.gs_base, handles, all, timeout)
        });
    }

    for symbol in [
        "CreateEventA",
        "CreateEventW",
        "CreateEventExA",
        "CreateEventExW",
        "SetEvent",
        "ResetEvent",
        "CreateSemaphoreA",
        "CreateSemaphoreW",
        "ReleaseSemaphore",
        "CreateMutexA",
        "CreateMutexW",
        "CreateMutexExA",
        "CreateMutexExW",
        "ReleaseMutex",
        "WaitForSingleObject",
        "WaitForSingleObjectEx",
        "WaitForMultipleObjects",
        "WaitForMultipleObjectsEx",
    ] {
        registry.alias(SYNCH_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
    for module in [
        "api-ms-win-core-synch-l1-2-0.dll",
        "api-ms-win-core-synch-l1-2-1.dll",
    ] {
        for symbol in ["CreateEventExA", "CreateEventExW"] {
            registry.alias(module, KERNEL32, symbol);
        }
    }
    // Microsoft’s API-set catalog lists the ANSI entry separately and the
    // wide entry in three revisions of the core synchronization API set.
    for (module, symbol) in [
        (
            "api-ms-win-core-synch-ansi-l1-1-0.dll",
            "CreateSemaphoreExA",
        ),
        (SYNCH_API, "CreateSemaphoreExW"),
        ("api-ms-win-core-synch-l1-2-0.dll", "CreateSemaphoreExW"),
        ("api-ms-win-core-synch-l1-2-1.dll", "CreateSemaphoreExW"),
    ] {
        registry.alias(module, KERNEL32, symbol);
    }
    registry.set_status(KERNEL32, "CreateSemaphoreExA", ServiceStatus::Partial);
    registry.set_status(KERNEL32, "CreateSemaphoreExW", ServiceStatus::Partial);
    for symbol in ["WaitForMultipleObjects", "WaitForMultipleObjectsEx"] {
        registry.alias("api-ms-win-core-synch-l1-2-1.dll", KERNEL32, symbol);
    }
}

fn wait_for_objects(
    state: &ServicesState,
    teb: u64,
    handles: &[u64],
    all: bool,
    timeout: u32,
) -> Result<u64, ExitReason> {
    // The object table remains locked through validation and consumption.
    let objects = state.objects.lock().expect("guest object state");
    let mut waitables: Vec<&Arc<Mutex<Waitable>>> = Vec::with_capacity(handles.len());
    for (index, handle) in handles.iter().enumerate() {
        if handles[..index].contains(handle) {
            return state.fail(teb, ERROR_INVALID_PARAMETER, WAIT_FAILED);
        }
        match objects.handles.get(handle) {
            Some(GuestObject::Waitable {
                state: event,
                access,
            }) => {
                if access & SYNCHRONIZE == 0 {
                    return state.fail(teb, ERROR_ACCESS_DENIED, WAIT_FAILED);
                }
                if all
                    && waitables
                        .iter()
                        .any(|existing| Arc::ptr_eq(existing, event))
                {
                    return state.fail(teb, ERROR_INVALID_PARAMETER, WAIT_FAILED);
                }
                waitables.push(event);
            }
            Some(_) => return state.fail(teb, ERROR_NOT_SUPPORTED, WAIT_FAILED),
            None if matches!(*handle, super::CURRENT_THREAD | super::CURRENT_PROCESS) => {
                return state.fail(teb, ERROR_NOT_SUPPORTED, WAIT_FAILED);
            }
            None => return state.fail(teb, ERROR_INVALID_HANDLE, WAIT_FAILED),
        }
    }
    if let Some(result) = try_satisfy(&waitables, all, teb) {
        return result;
    }
    if timeout == 0 {
        return Ok(WAIT_TIMEOUT);
    }
    if !state.hooks.is_attached() {
        return state.fail(teb, ERROR_NOT_SUPPORTED, WAIT_FAILED);
    }
    // Keep the objects themselves: closing a handle or changing the guest's
    // handle array does not change a wait already in progress.
    let pending = Pending::Objects(PendingWait {
        objects: waitables.into_iter().map(Arc::clone).collect(),
        all,
        deadline: state.deadline(timeout),
    });
    state
        .threads
        .lock()
        .expect("guest thread table")
        .waits
        .insert(teb, pending);
    Err(ExitReason::GuestBlocked)
}

/// Continue the wait this thread is blocked in, if any. Waiters are retried
/// directly after a signal, so an object found signaled once the deadline has
/// passed was signaled too late and is left alone.
fn resume_wait(state: &ServicesState, teb: u64) -> Option<Result<u64, ExitReason>> {
    let mut table = state.threads.lock().expect("guest thread table");
    let Some(Pending::Objects(pending)) = table.waits.get(&teb) else {
        return None;
    };
    let expired = pending
        .deadline
        .is_some_and(|deadline| state.clock.performance_count() >= deadline);
    let result = if expired {
        Some(Ok(WAIT_TIMEOUT))
    } else {
        let objects: Vec<&Arc<Mutex<Waitable>>> = pending.objects.iter().collect();
        try_satisfy(&objects, pending.all, teb)
    };
    if result.is_some() {
        table.waits.remove(&teb);
    }
    Some(result.unwrap_or(Err(ExitReason::GuestBlocked)))
}

/// Consume the objects if the wait is satisfied now.
fn try_satisfy(
    waitables: &[&Arc<Mutex<Waitable>>],
    all: bool,
    teb: u64,
) -> Option<Result<u64, ExitReason>> {
    let mut ready = Vec::with_capacity(waitables.len());
    for (index, object) in waitables.iter().enumerate() {
        if object.lock().expect("guest waitable state").is_ready(teb) {
            ready.push((index, object));
            if !all {
                break;
            }
        }
    }
    if ready.is_empty() || (all && ready.len() != waitables.len()) {
        return None;
    }
    if ready.iter().any(|(_, object)| {
        object
            .lock()
            .expect("guest waitable state")
            .at_recursion_limit()
    }) {
        // Windows raises STATUS_MUTANT_LIMIT_EXCEEDED; exceptions are unsupported.
        return Some(Err(ExitReason::IllegalInstruction(0xFE)));
    }
    let first = ready[0].0 as u64;
    let mut abandoned = None;
    for (index, object) in ready {
        if object.lock().expect("guest waitable state").consume(teb) {
            abandoned.get_or_insert(index as u64);
        }
    }
    Some(Ok(match abandoned {
        Some(index) => WAIT_ABANDONED_0 + index,
        None => WAIT_OBJECT_0 + first,
    }))
}

impl ServicesState {
    /// Release every mutex owned by a guest thread that has ended.
    pub(super) fn abandon_mutexes(&self, teb: u64) {
        let objects = self.objects.lock().expect("guest object state");
        let table = self.threads.lock().expect("guest thread table");
        let handles = objects.handles.values().filter_map(|object| match object {
            GuestObject::Waitable { state, .. } => Some(state),
            _ => None,
        });
        // A waiter keeps an object alive after its last handle is closed.
        let waiting = table.waits.values().flat_map(|pending| match pending {
            Pending::Objects(wait) => wait.objects.as_slice(),
            _ => &[],
        });
        for object in handles.chain(waiting) {
            if let Waitable::Mutex {
                owner,
                recursion,
                abandoned,
            } = &mut *object.lock().expect("guest waitable state")
                && *owner == Some(teb)
            {
                (*owner, *recursion, *abandoned) = (None, 0, true);
            }
        }
    }
}
