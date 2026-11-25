// SPDX-License-Identifier: GPL-3.0-or-later

//! Critical sections, slim reader/writer locks, condition variables, one-time
//! initialization, interlocked singly linked lists and thread-local storage.
//!
//! Ownership is keyed by the calling guest TEB. An operation that has to wait
//! for another guest thread blocks through the attached scheduler. Without one
//! it stops the guest with the runtime-limit fault instead of pretending to
//! synchronize. The synchronization state is locked before the thread table.

use super::threads::{ConditionSleep, Pending, SleepLock, SleepPhase, SrwMode};
use super::{
    ERROR_INVALID_PARAMETER, ERROR_NOT_ENOUGH_MEMORY, ERROR_NOT_SUPPORTED, KERNEL32, ServicesState,
};
use crate::cpu::ExitReason;
use crate::imports::{CallbackStart, CallbackStep, CallbackTask, ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError};
use std::collections::BTreeMap;
use std::sync::Arc;

const SYNCH_API: &str = "api-ms-win-core-synch-l1-1-0.dll";
const SYNCH_API_2: &str = "api-ms-win-core-synch-l1-2-0.dll";
const SLIST_API: &str = "api-ms-win-core-interlocked-l1-1-0.dll";
const TLS_API: &str = "api-ms-win-core-processthreads-l1-1-0.dll";
const FLS_API: &str = "api-ms-win-core-fibers-l1-1-0.dll";

const ERROR_GEN_FAILURE: u32 = 31;
const ERROR_TIMEOUT: u32 = 1460;

/// `RTL_CRITICAL_SECTION` on x64 (winnt.h): DebugInfo, LockCount,
/// RecursionCount, OwningThread, LockSemaphore, SpinCount.
const CS_SIZE: usize = 40;
const CS_LOCK_COUNT: u64 = 8;
const CS_RECURSION: u64 = 12;
const CS_OWNER: u64 = 16;
const CS_SPIN: u64 = 32;
const CS_SPIN_MASK: u64 = 0x00ff_ffff;
const CRITICAL_SECTION_NO_DEBUG_INFO: u32 = 0x0100_0000;
const MAX_CRITICAL_SECTIONS: usize = 4096;

const MAX_SRW_LOCKS: usize = 4096;
const MAX_PENDING_INIT_ONCE: usize = 4096;
/// Owner of a lock whose thread ended while holding it. Never a TEB address.
const ENDED_THREAD: u64 = u64::MAX;
const CONDITION_VARIABLE_LOCKMODE_SHARED: u32 = 1;

const INIT_ONCE_CHECK_ONLY: u32 = 1;
const INIT_ONCE_ASYNC: u32 = 2;
const INIT_ONCE_INIT_FAILED: u32 = 4;
const INIT_ONCE_CTX_RESERVED_MASK: u64 = 3;
/// Opaque `INIT_ONCE` values. Completion stores the context with this bit set,
/// as the context's two reserved low bits allow.
const INIT_ONCE_PENDING: u64 = 1;
const INIT_ONCE_COMPLETE: u64 = 2;

/// `SLIST_HEADER` on x64 is 16 bytes: Depth:16, Sequence:48 in the first
/// quadword and Reserved:4, NextEntry:60 (address >> 4) in the second.
const SLIST_ALIGNMENT: u64 = 16;
const SLIST_MAX_DEPTH: u64 = 0xffff;

/// TLS_MINIMUM_AVAILABLE (64) plus the documented expansion slots.
const TLS_MAX_INDEXES: u32 = 1088;
const TLS_OUT_OF_INDEXES: u64 = u32::MAX as u64;
/// FLS_MAXIMUM_AVAILABLE in the MinGW-w64 winnt.h.
const FLS_MAX_INDEXES: u32 = 128;
const FLS_OUT_OF_INDEXES: u64 = u32::MAX as u64;

fn unsupported<T>() -> Result<T, ExitReason> {
    Err(ExitReason::IllegalInstruction(0xFE))
}

/// Wait for another guest thread: through the scheduler, or not at all.
fn blocked<T>(state: &ServicesState) -> Result<T, ExitReason> {
    if state.hooks.is_attached() {
        Err(ExitReason::GuestBlocked)
    } else {
        unsupported()
    }
}

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

struct CriticalSection {
    owner: Option<u64>,
    recursion: u32,
    spin: u64,
}

#[derive(Default)]
struct SrwLock {
    exclusive: Option<u64>,
    shared: BTreeMap<u64, u32>,
}

impl SrwLock {
    fn is_free(&self) -> bool {
        self.exclusive.is_none() && self.shared.is_empty()
    }
}

struct FlsSlot {
    callback: u64,
}

struct FlsCleanup {
    callback: u64,
    values: std::vec::IntoIter<u64>,
}

impl CallbackTask for FlsCleanup {
    fn resume(&mut self, _: u64, _: u64, _: &mut GuestMemory) -> Result<CallbackStep, ExitReason> {
        Ok(match self.values.next() {
            Some(value) => CallbackStep::Call {
                entry: self.callback,
                args: [value, 0, 0, 0],
            },
            None => CallbackStep::Return(1),
        })
    }
}

/// `InitOnceExecuteOnce` after its initializer returns. The initializer wrote
/// its context to the service's scratch word.
struct InitOnceExecution {
    state: Arc<ServicesState>,
    once: u64,
    context: u64,
    scratch: u64,
}

impl CallbackTask for InitOnceExecution {
    fn resume(
        &mut self,
        result: u64,
        teb: u64,
        memory: &mut GuestMemory,
    ) -> Result<CallbackStep, ExitReason> {
        let succeeded = result as u32 != 0;
        let produced = memory.read_u64(self.scratch).map_err(fault)?;
        let mut synch = self
            .state
            .synch
            .lock()
            .expect("guest synchronization state");
        synch.init_once_pending.remove(&self.once);
        self.state.wake_blocked();
        if !succeeded || produced & INIT_ONCE_CTX_RESERVED_MASK != 0 {
            memory.write_u64(self.once, 0).map_err(fault)?;
            drop(synch);
            if succeeded {
                self.state.set_last_error(teb, ERROR_INVALID_PARAMETER);
            }
            return Ok(CallbackStep::Return(0));
        }
        memory
            .write_u64(self.once, produced | INIT_ONCE_COMPLETE)
            .map_err(fault)?;
        if self.context != 0 {
            memory.write_u64(self.context, produced).map_err(fault)?;
        }
        Ok(CallbackStep::Return(1))
    }
}

#[derive(Default)]
pub(super) struct Synch {
    sections: BTreeMap<u64, CriticalSection>,
    srw: BTreeMap<u64, SrwLock>,
    init_once_pending: BTreeMap<u64, u64>,
    tls_allocated: BTreeMap<u32, ()>,
    tls_values: BTreeMap<(u64, u32), u64>,
    fls_slots: BTreeMap<u32, FlsSlot>,
    fls_values: BTreeMap<(u64, u32), u64>,
}

impl Synch {
    /// Drop the thread-local values of a guest thread that has ended, without
    /// running FLS callbacks. Locks it held stay held: its TEB address may be
    /// reused, and the next thread there must not own them.
    pub(super) fn retire_thread(&mut self, teb: u64) {
        self.tls_values.retain(|&(owner, _), _| owner != teb);
        self.fls_values.retain(|&(owner, _), _| owner != teb);
        for section in self.sections.values_mut() {
            if section.owner == Some(teb) {
                section.owner = Some(ENDED_THREAD);
            }
        }
        for lock in self.srw.values_mut() {
            if lock.exclusive == Some(teb) {
                lock.exclusive = Some(ENDED_THREAD);
            }
            if let Some(count) = lock.shared.remove(&teb) {
                let held = lock.shared.entry(ENDED_THREAD).or_default();
                *held = held.saturating_add(count);
            }
        }
        for owner in self.init_once_pending.values_mut() {
            if *owner == teb {
                *owner = ENDED_THREAD;
            }
        }
    }

    fn section(&mut self, address: u64) -> Result<&mut CriticalSection, ExitReason> {
        // Using an uninitialized critical section is undefined; report it.
        self.sections.get_mut(&address).map_or_else(unsupported, Ok)
    }
}

/// Write the documented observable fields: LockCount is -1 when free and -2
/// when held with no waiters, and OwningThread holds the owner's thread ID.
fn mirror_section(
    memory: &mut GuestMemory,
    address: u64,
    section: &CriticalSection,
    thread_id: u32,
) -> Result<(), ExitReason> {
    let (lock_count, owner) = match section.owner {
        Some(_) => (-2i32, u64::from(thread_id)),
        None => (-1, 0),
    };
    memory
        .write_u32(address + CS_LOCK_COUNT, lock_count as u32)
        .map_err(fault)?;
    memory
        .write_u32(address + CS_RECURSION, section.recursion)
        .map_err(fault)?;
    memory.write_u64(address + CS_OWNER, owner).map_err(fault)
}

fn initialize_section(
    state: &ServicesState,
    memory: &mut GuestMemory,
    address: u64,
    spin: u64,
) -> Result<bool, ExitReason> {
    if address == 0 {
        return Err(fault(MemoryError::UnmappedAddress(0)));
    }
    address
        .checked_add(CS_SIZE as u64)
        .ok_or(fault(MemoryError::Overflow))?;
    let mut synch = state.synch.lock().expect("guest synchronization state");
    match synch.sections.get(&address) {
        Some(existing) if existing.owner.is_some() => return unsupported(),
        None if synch.sections.len() >= MAX_CRITICAL_SECTIONS => return Ok(false),
        _ => {}
    }
    let mut image = [0u8; CS_SIZE];
    image[CS_LOCK_COUNT as usize..CS_RECURSION as usize].copy_from_slice(&(-1i32).to_le_bytes());
    image[CS_SPIN as usize..].copy_from_slice(&(spin & CS_SPIN_MASK).to_le_bytes());
    memory.write_bytes(address, &image).map_err(fault)?;
    synch.sections.insert(
        address,
        CriticalSection {
            owner: None,
            recursion: 0,
            spin: spin & CS_SPIN_MASK,
        },
    );
    Ok(true)
}

/// Acquire for `teb`. Returns false when another guest thread owns it.
fn enter_section(
    state: &ServicesState,
    memory: &mut GuestMemory,
    address: u64,
    teb: u64,
) -> Result<bool, ExitReason> {
    let thread_id = state.thread_id_of(teb);
    let mut synch = state.synch.lock().expect("guest synchronization state");
    let section = synch.section(address)?;
    match section.owner {
        Some(owner) if owner != teb => return Ok(false),
        Some(_) if section.recursion >= i32::MAX as u32 => return unsupported(),
        _ => {}
    }
    section.owner = Some(teb);
    section.recursion += 1;
    mirror_section(memory, address, section, thread_id)?;
    Ok(true)
}

fn leave_section(
    state: &ServicesState,
    memory: &mut GuestMemory,
    address: u64,
    teb: u64,
) -> Result<(), ExitReason> {
    let thread_id = state.thread_id_of(teb);
    let mut synch = state.synch.lock().expect("guest synchronization state");
    let section = synch.section(address)?;
    if section.owner != Some(teb) {
        return unsupported();
    }
    section.recursion -= 1;
    if section.recursion == 0 {
        section.owner = None;
    }
    mirror_section(memory, address, section, thread_id)?;
    drop(synch);
    state.wake_blocked();
    Ok(())
}

/// Check that a pointer-sized synchronization object is readable and writable.
fn check_word(memory: &mut GuestMemory, address: u64) -> Result<u64, ExitReason> {
    if address == 0 {
        return Err(fault(MemoryError::UnmappedAddress(0)));
    }
    let value = memory.read_u64(address).map_err(fault)?;
    memory.write_u64(address, value).map_err(fault)?;
    Ok(value)
}

/// Acquire without blocking. Returns false when the lock is unavailable.
fn try_acquire_srw(
    state: &ServicesState,
    memory: &mut GuestMemory,
    address: u64,
    teb: u64,
    mode: SrwMode,
) -> Result<bool, ExitReason> {
    check_word(memory, address)?;
    let mut synch = state.synch.lock().expect("guest synchronization state");
    if !synch.srw.contains_key(&address) && synch.srw.len() >= MAX_SRW_LOCKS {
        return unsupported();
    }
    let lock = synch.srw.entry(address).or_default();
    let acquired = match mode {
        SrwMode::Exclusive if lock.is_free() => {
            lock.exclusive = Some(teb);
            true
        }
        SrwMode::Shared if lock.exclusive.is_none() => {
            let count = lock.shared.entry(teb).or_default();
            if *count == u32::MAX {
                return unsupported();
            }
            *count += 1;
            true
        }
        _ => false,
    };
    if lock.is_free() {
        synch.srw.remove(&address);
    }
    Ok(acquired)
}

fn release_srw(
    state: &ServicesState,
    memory: &mut GuestMemory,
    address: u64,
    teb: u64,
    mode: SrwMode,
) -> Result<(), ExitReason> {
    check_word(memory, address)?;
    let mut synch = state.synch.lock().expect("guest synchronization state");
    let Some(lock) = synch.srw.get_mut(&address) else {
        return unsupported();
    };
    match mode {
        SrwMode::Exclusive if lock.exclusive == Some(teb) => lock.exclusive = None,
        SrwMode::Shared => match lock.shared.get_mut(&teb) {
            Some(count) if *count > 1 => *count -= 1,
            Some(_) => {
                lock.shared.remove(&teb);
            }
            None => return unsupported(),
        },
        SrwMode::Exclusive => return unsupported(),
    }
    if lock.is_free() {
        synch.srw.remove(&address);
    }
    drop(synch);
    state.wake_blocked();
    Ok(())
}

/// Acquire a slim lock, or block the calling guest thread until it is available.
/// A reader waits behind a writer that was waiting first, so a stream of
/// overlapping readers cannot keep a writer out.
fn acquire_srw(
    state: &ServicesState,
    memory: &mut GuestMemory,
    address: u64,
    teb: u64,
    mode: SrwMode,
) -> Result<u64, ExitReason> {
    let ticket = {
        let table = state.threads.lock().expect("guest thread table");
        match table.waits.get(&teb) {
            Some(Pending::Srw { ticket, .. }) => Some(*ticket),
            _ => None,
        }
    };
    if take_srw(state, memory, address, teb, mode, ticket)? {
        if ticket.is_some() {
            let mut table = state.threads.lock().expect("guest thread table");
            table.waits.remove(&teb);
        }
        return Ok(0);
    }
    if !state.hooks.is_attached() {
        return unsupported();
    }
    if ticket.is_none() {
        let mut table = state.threads.lock().expect("guest thread table");
        let pending = Pending::Srw {
            lock: address,
            mode,
            ticket: table.ticket(),
        };
        table.waits.insert(teb, pending);
    }
    Err(ExitReason::GuestBlocked)
}

/// Acquire without blocking, behind any writer that has waited longer. No ticket
/// means the caller has not waited yet.
fn take_srw(
    state: &ServicesState,
    memory: &mut GuestMemory,
    address: u64,
    teb: u64,
    mode: SrwMode,
    ticket: Option<u64>,
) -> Result<bool, ExitReason> {
    let behind = mode == SrwMode::Shared
        && state
            .threads
            .lock()
            .expect("guest thread table")
            .writer_waits(address, teb, ticket.unwrap_or(u64::MAX));
    Ok(!behind && try_acquire_srw(state, memory, address, teb, mode)?)
}

fn holds_srw(state: &ServicesState, address: u64, teb: u64, mode: SrwMode) -> bool {
    let synch = state.synch.lock().expect("guest synchronization state");
    synch.srw.get(&address).is_some_and(|lock| match mode {
        SrwMode::Exclusive => lock.exclusive == Some(teb),
        SrwMode::Shared => lock.shared.contains_key(&teb),
    })
}

/// Read an aligned `SLIST_HEADER` as (depth, sequence, first entry).
fn read_slist(memory: &GuestMemory, header: u64) -> Result<(u64, u64, u64), ExitReason> {
    if header == 0 || !header.is_multiple_of(SLIST_ALIGNMENT) {
        return unsupported();
    }
    let low = memory.read_u64(header).map_err(fault)?;
    let high = memory.read_u64(header + 8).map_err(fault)?;
    Ok((low & 0xffff, low >> 16, (high >> 4) << 4))
}

fn write_slist(
    memory: &mut GuestMemory,
    header: u64,
    depth: u64,
    sequence: u64,
    first: u64,
) -> Result<(), ExitReason> {
    let low = (depth & 0xffff) | (sequence << 16);
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&low.to_le_bytes());
    bytes[8..].copy_from_slice(&first.to_le_bytes());
    memory.write_bytes(header, &bytes).map_err(fault)
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    register_critical_sections(registry, state);
    register_srw_and_conditions(registry, state);
    register_init_once(registry, state);
    register_slist(registry, state);
    register_tls(registry, state);
}

fn register_critical_sections(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let s = Arc::clone(state);
    registry.register(
        KERNEL32,
        "InitializeCriticalSection",
        move |args, _, mem| {
            if !initialize_section(&s, mem, args.rcx, 0)? {
                return unsupported();
            }
            Ok(0)
        },
    );

    let s = Arc::clone(state);
    registry.register(
        KERNEL32,
        "InitializeCriticalSectionAndSpinCount",
        move |args, cpu, mem| {
            if !initialize_section(&s, mem, args.rcx, args.rdx & 0xffff_ffff)? {
                return s.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
            }
            Ok(1)
        },
    );

    let s = Arc::clone(state);
    registry.register(
        KERNEL32,
        "InitializeCriticalSectionEx",
        move |args, cpu, mem| {
            let flags = args.r8 as u32;
            if flags & !CRITICAL_SECTION_NO_DEBUG_INFO != 0 {
                return s.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            if !initialize_section(&s, mem, args.rcx, args.rdx & 0xffff_ffff)? {
                return s.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
            }
            Ok(1)
        },
    );

    let s = Arc::clone(state);
    registry.register(KERNEL32, "EnterCriticalSection", move |args, cpu, mem| {
        if !enter_section(&s, mem, args.rcx, cpu.gs_base)? {
            return blocked(&s);
        }
        Ok(0)
    });

    let s = Arc::clone(state);
    registry.register(
        KERNEL32,
        "TryEnterCriticalSection",
        move |args, cpu, mem| Ok(u64::from(enter_section(&s, mem, args.rcx, cpu.gs_base)?)),
    );

    let s = Arc::clone(state);
    registry.register(KERNEL32, "LeaveCriticalSection", move |args, cpu, mem| {
        leave_section(&s, mem, args.rcx, cpu.gs_base)?;
        Ok(0)
    });

    let s = Arc::clone(state);
    registry.register(KERNEL32, "DeleteCriticalSection", move |args, _, _| {
        let mut synch = s.synch.lock().expect("guest synchronization state");
        if synch.sections.remove(&args.rcx).is_none() {
            return unsupported();
        }
        Ok(0)
    });

    let s = Arc::clone(state);
    registry.register(
        KERNEL32,
        "SetCriticalSectionSpinCount",
        move |args, _, mem| {
            let mut synch = s.synch.lock().expect("guest synchronization state");
            let section = synch.section(args.rcx)?;
            let previous = section.spin;
            section.spin = args.rdx & 0xffff_ffff & CS_SPIN_MASK;
            mem.write_u64(args.rcx + CS_SPIN, section.spin)
                .map_err(fault)?;
            Ok(previous)
        },
    );

    for symbol in [
        "InitializeCriticalSection",
        "InitializeCriticalSectionAndSpinCount",
        "InitializeCriticalSectionEx",
        "EnterCriticalSection",
        "TryEnterCriticalSection",
        "LeaveCriticalSection",
        "DeleteCriticalSection",
        "SetCriticalSectionSpinCount",
    ] {
        registry.alias(SYNCH_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
}

fn register_srw_and_conditions(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let s = Arc::clone(state);
    registry.register(KERNEL32, "InitializeSRWLock", move |args, _, mem| {
        check_word(mem, args.rcx)?;
        let synch = s.synch.lock().expect("guest synchronization state");
        if synch.srw.get(&args.rcx).is_some_and(|lock| !lock.is_free()) {
            return unsupported();
        }
        mem.write_u64(args.rcx, 0).map_err(fault)?;
        Ok(0)
    });

    for (acquire, try_acquire, release, mode) in [
        (
            "AcquireSRWLockExclusive",
            "TryAcquireSRWLockExclusive",
            "ReleaseSRWLockExclusive",
            SrwMode::Exclusive,
        ),
        (
            "AcquireSRWLockShared",
            "TryAcquireSRWLockShared",
            "ReleaseSRWLockShared",
            SrwMode::Shared,
        ),
    ] {
        let s = Arc::clone(state);
        registry.register(KERNEL32, acquire, move |args, cpu, mem| {
            acquire_srw(&s, mem, args.rcx, cpu.gs_base, mode)
        });
        let s = Arc::clone(state);
        registry.register(KERNEL32, try_acquire, move |args, cpu, mem| {
            Ok(u64::from(try_acquire_srw(
                &s,
                mem,
                args.rcx,
                cpu.gs_base,
                mode,
            )?))
        });
        let s = Arc::clone(state);
        registry.register(KERNEL32, release, move |args, cpu, mem| {
            release_srw(&s, mem, args.rcx, cpu.gs_base, mode)?;
            Ok(0)
        });
    }

    registry.register(KERNEL32, "InitializeConditionVariable", |args, _, mem| {
        check_word(mem, args.rcx)?;
        mem.write_u64(args.rcx, 0).map_err(fault)?;
        Ok(0)
    });
    for (symbol, all) in [
        ("WakeConditionVariable", false),
        ("WakeAllConditionVariable", true),
    ] {
        let s = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, _, mem| {
            check_word(mem, args.rcx)?;
            s.wake_condition(args.rcx, all);
            Ok(0)
        });
    }

    let s = Arc::clone(state);
    registry.register(
        KERNEL32,
        "SleepConditionVariableCS",
        move |args, cpu, mem| {
            if let Some(result) = resume_sleep(&s, mem, cpu.gs_base) {
                return result;
            }
            check_word(mem, args.rcx)?;
            let timeout = args.r8 as u32;
            let recursion = {
                let synch = s.synch.lock().expect("guest synchronization state");
                synch
                    .sections
                    .get(&args.rdx)
                    .filter(|section| section.owner == Some(cpu.gs_base))
                    .map(|section| section.recursion)
            };
            // A sleep releases one entry, so another one would keep the section
            // owned while its owner sleeps.
            if recursion.is_none_or(|count| timeout != 0 && count != 1) {
                return unsupported();
            }
            let lock = SleepLock::Section(args.rdx);
            sleep_condition(&s, mem, cpu.gs_base, args.rcx, lock, timeout)
        },
    );

    let s = Arc::clone(state);
    registry.register(
        KERNEL32,
        "SleepConditionVariableSRW",
        move |args, cpu, mem| {
            if let Some(result) = resume_sleep(&s, mem, cpu.gs_base) {
                return result;
            }
            check_word(mem, args.rcx)?;
            let flags = args.r9 as u32;
            if flags & !CONDITION_VARIABLE_LOCKMODE_SHARED != 0 {
                return s.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let mode = if flags == 0 {
                SrwMode::Exclusive
            } else {
                SrwMode::Shared
            };
            if !holds_srw(&s, args.rdx, cpu.gs_base, mode) {
                return unsupported();
            }
            let lock = SleepLock::Srw(args.rdx, mode);
            sleep_condition(&s, mem, cpu.gs_base, args.rcx, lock, args.r8 as u32)
        },
    );

    for symbol in [
        "InitializeSRWLock",
        "AcquireSRWLockExclusive",
        "AcquireSRWLockShared",
        "TryAcquireSRWLockExclusive",
        "TryAcquireSRWLockShared",
        "ReleaseSRWLockExclusive",
        "ReleaseSRWLockShared",
    ] {
        registry.alias(SYNCH_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
    for symbol in [
        "InitializeConditionVariable",
        "WakeConditionVariable",
        "WakeAllConditionVariable",
        "SleepConditionVariableCS",
        "SleepConditionVariableSRW",
    ] {
        registry.alias(SYNCH_API_2, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
}

/// Release the lock and sleep on a condition variable. A zero timeout releases
/// and reacquires the lock within one service call, so no other guest thread can
/// observe it, then reports the timeout.
fn sleep_condition(
    state: &ServicesState,
    memory: &mut GuestMemory,
    teb: u64,
    variable: u64,
    lock: SleepLock,
    timeout: u32,
) -> Result<u64, ExitReason> {
    if timeout == 0 {
        return state.fail(teb, ERROR_TIMEOUT, 0);
    }
    if !state.hooks.is_attached() {
        return state.fail(teb, ERROR_NOT_SUPPORTED, 0);
    }
    match lock {
        SleepLock::Section(address) => leave_section(state, memory, address, teb)?,
        SleepLock::Srw(address, mode) => release_srw(state, memory, address, teb, mode)?,
    }
    let mut table = state.threads.lock().expect("guest thread table");
    let sleep = ConditionSleep {
        variable,
        lock,
        deadline: state.deadline(timeout),
        phase: SleepPhase::Asleep,
        ticket: table.ticket(),
    };
    table.waits.insert(teb, Pending::Condition(sleep));
    Err(ExitReason::GuestBlocked)
}

/// Continue the condition-variable sleep this thread is blocked in, if any. It
/// ends, woken or timed out, only with the lock held again.
fn resume_sleep(
    state: &ServicesState,
    memory: &mut GuestMemory,
    teb: u64,
) -> Option<Result<u64, ExitReason>> {
    let sleep = {
        let mut table = state.threads.lock().expect("guest thread table");
        let Some(Pending::Condition(sleep)) = table.waits.get(&teb) else {
            return None;
        };
        let mut sleep = *sleep;
        if sleep.phase == SleepPhase::Asleep {
            let now = state.clock.performance_count();
            if sleep.deadline.is_none_or(|deadline| now < deadline) {
                return Some(Err(ExitReason::GuestBlocked));
            }
            sleep.phase = SleepPhase::TimedOut;
            table.end_sleep(teb, sleep.phase);
        }
        sleep
    };
    let taken = match sleep.lock {
        SleepLock::Section(address) => enter_section(state, memory, address, teb),
        SleepLock::Srw(address, mode) => {
            take_srw(state, memory, address, teb, mode, Some(sleep.ticket))
        }
    };
    match taken {
        Err(reason) => return Some(Err(reason)),
        Ok(false) => return Some(Err(ExitReason::GuestBlocked)),
        Ok(true) => {}
    }
    let mut table = state.threads.lock().expect("guest thread table");
    table.waits.remove(&teb);
    drop(table);
    Some(match sleep.phase {
        SleepPhase::TimedOut => state.fail(teb, ERROR_TIMEOUT, 0),
        SleepPhase::Asleep | SleepPhase::Woken => Ok(1),
    })
}

/// Block until another thread's one-time initialization ends. Waiting for the
/// calling thread's own, or for one the runtime did not begin, is a guest error.
fn wait_for_once<T>(state: &ServicesState, owner: Option<&u64>, teb: u64) -> Result<T, ExitReason> {
    match owner {
        Some(&owner) if owner != teb => blocked(state),
        _ => unsupported(),
    }
}

fn register_init_once(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let s = Arc::clone(state);
    registry.register(KERNEL32, "InitOnceInitialize", move |args, _, mem| {
        check_word(mem, args.rcx)?;
        let synch = s.synch.lock().expect("guest synchronization state");
        if synch.init_once_pending.contains_key(&args.rcx) {
            return unsupported();
        }
        mem.write_u64(args.rcx, 0).map_err(fault)?;
        Ok(0)
    });

    let s = Arc::clone(state);
    registry.register(
        KERNEL32,
        "InitOnceBeginInitialize",
        move |args, cpu, mem| {
            let (once, flags, pending, context) = (args.rcx, args.rdx as u32, args.r8, args.r9);
            let teb = cpu.gs_base;
            if flags & !(INIT_ONCE_CHECK_ONLY | INIT_ONCE_ASYNC) != 0 || pending == 0 {
                return s.fail(teb, ERROR_INVALID_PARAMETER, 0);
            }
            if flags & INIT_ONCE_ASYNC != 0 {
                return s.fail(teb, ERROR_NOT_SUPPORTED, 0);
            }
            let value = check_word(mem, once)?;
            let mut synch = s.synch.lock().expect("guest synchronization state");
            let pending_here = synch.init_once_pending.contains_key(&once);
            match value & INIT_ONCE_CTX_RESERVED_MASK {
                INIT_ONCE_COMPLETE => {
                    mem.check_write(pending, 4).map_err(fault)?;
                    if context != 0 {
                        mem.check_write(context, 8).map_err(fault)?;
                        mem.write_u64(context, value & !INIT_ONCE_CTX_RESERVED_MASK)
                            .map_err(fault)?;
                    }
                    mem.write_u32(pending, 0).map_err(fault)?;
                    Ok(1)
                }
                _ if flags & INIT_ONCE_CHECK_ONLY != 0
                    && (value == 0 || (value == INIT_ONCE_PENDING && pending_here)) =>
                {
                    s.fail(teb, ERROR_GEN_FAILURE, 0)
                }
                0 if value == 0 => {
                    if synch.init_once_pending.len() >= MAX_PENDING_INIT_ONCE {
                        return unsupported();
                    }
                    mem.write_u32(pending, 1).map_err(fault)?;
                    mem.write_u64(once, INIT_ONCE_PENDING).map_err(fault)?;
                    synch.init_once_pending.insert(once, teb);
                    Ok(1)
                }
                INIT_ONCE_PENDING if value == INIT_ONCE_PENDING => {
                    wait_for_once(&s, synch.init_once_pending.get(&once), teb)
                }
                _ => unsupported(),
            }
        },
    );

    let s = Arc::clone(state);
    registry.register(KERNEL32, "InitOnceComplete", move |args, cpu, mem| {
        let (once, flags, context) = (args.rcx, args.rdx as u32, args.r8);
        let teb = cpu.gs_base;
        if flags & !(INIT_ONCE_ASYNC | INIT_ONCE_INIT_FAILED) != 0 {
            return s.fail(teb, ERROR_INVALID_PARAMETER, 0);
        }
        if flags & INIT_ONCE_ASYNC != 0 {
            return s.fail(teb, ERROR_NOT_SUPPORTED, 0);
        }
        let failed = flags & INIT_ONCE_INIT_FAILED != 0;
        if context & INIT_ONCE_CTX_RESERVED_MASK != 0 || (failed && context != 0) {
            return s.fail(teb, ERROR_INVALID_PARAMETER, 0);
        }
        let value = check_word(mem, once)?;
        let mut synch = s.synch.lock().expect("guest synchronization state");
        if value != INIT_ONCE_PENDING || synch.init_once_pending.get(&once) != Some(&teb) {
            return s.fail(teb, ERROR_GEN_FAILURE, 0);
        }
        let next = if failed {
            0
        } else {
            context | INIT_ONCE_COMPLETE
        };
        mem.write_u64(once, next).map_err(fault)?;
        synch.init_once_pending.remove(&once);
        drop(synch);
        s.wake_blocked();
        Ok(1)
    });

    let s = Arc::clone(state);
    registry.register_callback_task(
        KERNEL32,
        "InitOnceExecuteOnce",
        ServiceStatus::Partial,
        move |args, teb, mem, scratch| {
            let (once, initializer, parameter, context) = (args.rcx, args.rdx, args.r8, args.r9);
            let value = check_word(mem, once)?;
            if value & INIT_ONCE_CTX_RESERVED_MASK == INIT_ONCE_COMPLETE {
                if context != 0 {
                    mem.write_u64(context, value & !INIT_ONCE_CTX_RESERVED_MASK)
                        .map_err(fault)?;
                }
                return Ok(CallbackStart::Return(1));
            }
            let mut synch = s.synch.lock().expect("guest synchronization state");
            if value == INIT_ONCE_PENDING {
                return wait_for_once(&s, synch.init_once_pending.get(&once), teb);
            }
            if value != 0 {
                return unsupported();
            }
            if synch.init_once_pending.len() >= MAX_PENDING_INIT_ONCE {
                return unsupported();
            }
            mem.write_u64(scratch, 0).map_err(fault)?;
            mem.write_u64(once, INIT_ONCE_PENDING).map_err(fault)?;
            synch.init_once_pending.insert(once, teb);
            Ok(CallbackStart::Call {
                task: Box::new(InitOnceExecution {
                    state: Arc::clone(&s),
                    once,
                    context,
                    scratch,
                }),
                entry: initializer,
                args: [once, parameter, scratch, 0],
            })
        },
    );

    for symbol in [
        "InitOnceInitialize",
        "InitOnceBeginInitialize",
        "InitOnceComplete",
        "InitOnceExecuteOnce",
    ] {
        registry.alias(SYNCH_API_2, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
}

fn register_slist(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    registry.register(KERNEL32, "InitializeSListHead", |args, _, mem| {
        read_slist(mem, args.rcx)?;
        write_slist(mem, args.rcx, 0, 0, 0)?;
        Ok(0)
    });

    registry.register(KERNEL32, "QueryDepthSList", |args, _, mem| {
        Ok(read_slist(mem, args.rcx)?.0)
    });

    // Services run one at a time, which makes each list update atomic with
    // respect to other services.
    let s = Arc::clone(state);
    registry.register(
        KERNEL32,
        "InterlockedPushEntrySList",
        move |args, _, mem| {
            let _serial = s.synch.lock().expect("guest synchronization state");
            let (header, entry) = (args.rcx, args.rdx);
            let (depth, sequence, first) = read_slist(mem, header)?;
            if entry == 0 || !entry.is_multiple_of(SLIST_ALIGNMENT) || depth == SLIST_MAX_DEPTH {
                return unsupported();
            }
            mem.write_u64(entry, first).map_err(fault)?;
            write_slist(
                mem,
                header,
                depth + 1,
                (sequence + 1) & 0xffff_ffff_ffff,
                entry,
            )?;
            Ok(first)
        },
    );

    let s = Arc::clone(state);
    registry.register(KERNEL32, "InterlockedPopEntrySList", move |args, _, mem| {
        let _serial = s.synch.lock().expect("guest synchronization state");
        let (depth, sequence, first) = read_slist(mem, args.rcx)?;
        if first == 0 {
            return Ok(0);
        }
        let next = mem.read_u64(first).map_err(fault)?;
        if !next.is_multiple_of(SLIST_ALIGNMENT) || depth == 0 {
            return unsupported();
        }
        write_slist(mem, args.rcx, depth - 1, sequence, next)?;
        Ok(first)
    });

    let s = Arc::clone(state);
    registry.register(KERNEL32, "InterlockedFlushSList", move |args, _, mem| {
        let _serial = s.synch.lock().expect("guest synchronization state");
        let (_, sequence, first) = read_slist(mem, args.rcx)?;
        write_slist(mem, args.rcx, 0, sequence, 0)?;
        Ok(first)
    });

    for symbol in [
        "InitializeSListHead",
        "QueryDepthSList",
        "InterlockedPushEntrySList",
        "InterlockedPopEntrySList",
        "InterlockedFlushSList",
    ] {
        registry.alias(SLIST_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
}

fn register_tls(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let s = Arc::clone(state);
    registry.register(KERNEL32, "TlsAlloc", move |_, cpu, _| {
        let mut synch = s.synch.lock().expect("guest synchronization state");
        let Some(index) = (0..TLS_MAX_INDEXES).find(|i| !synch.tls_allocated.contains_key(i))
        else {
            drop(synch);
            return s.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, TLS_OUT_OF_INDEXES);
        };
        synch.tls_allocated.insert(index, ());
        synch.tls_values.retain(|&(_, slot), _| slot != index);
        Ok(u64::from(index))
    });

    let s = Arc::clone(state);
    registry.register(KERNEL32, "TlsFree", move |args, cpu, _| {
        let index = args.rcx as u32;
        let mut synch = s.synch.lock().expect("guest synchronization state");
        if synch.tls_allocated.remove(&index).is_none() {
            drop(synch);
            return s.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
        }
        synch.tls_values.retain(|&(_, slot), _| slot != index);
        Ok(1)
    });

    let s = Arc::clone(state);
    registry.register(KERNEL32, "TlsGetValue", move |args, cpu, _| {
        let index = args.rcx as u32;
        let synch = s.synch.lock().expect("guest synchronization state");
        if !synch.tls_allocated.contains_key(&index) {
            drop(synch);
            return s.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
        }
        let value = synch
            .tls_values
            .get(&(cpu.gs_base, index))
            .copied()
            .unwrap_or(0);
        drop(synch);
        // A successful call clears the last error, so zero is distinguishable.
        s.set_last_error(cpu.gs_base, 0);
        Ok(value)
    });

    let s = Arc::clone(state);
    registry.register(KERNEL32, "TlsSetValue", move |args, cpu, _| {
        let index = args.rcx as u32;
        let mut synch = s.synch.lock().expect("guest synchronization state");
        if !synch.tls_allocated.contains_key(&index) {
            drop(synch);
            return s.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
        }
        if args.rdx == 0 {
            synch.tls_values.remove(&(cpu.gs_base, index));
        } else {
            synch.tls_values.insert((cpu.gs_base, index), args.rdx);
        }
        Ok(1)
    });

    let s = Arc::clone(state);
    registry.register(KERNEL32, "FlsAlloc", move |args, cpu, _| {
        let callback = args.rcx;
        let mut synch = s.synch.lock().expect("guest synchronization state");
        let Some(index) = (0..FLS_MAX_INDEXES).find(|i| !synch.fls_slots.contains_key(i)) else {
            drop(synch);
            return s.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, FLS_OUT_OF_INDEXES);
        };
        synch.fls_slots.insert(index, FlsSlot { callback });
        synch.fls_values.retain(|&(_, slot), _| slot != index);
        Ok(u64::from(index))
    });

    let s = Arc::clone(state);
    registry.register_callback_task(
        KERNEL32,
        "FlsFree",
        ServiceStatus::Partial,
        move |args, teb, _, _| {
            let index = args.rcx as u32;
            let mut synch = s.synch.lock().expect("guest synchronization state");
            let Some(slot) = synch.fls_slots.remove(&index) else {
                drop(synch);
                s.fail(teb, ERROR_INVALID_PARAMETER, 0)?;
                return Ok(CallbackStart::Return(0));
            };
            // The slot is released first; its callback then receives each
            // thread's nonnull value, in TEB order, on the calling thread.
            let mut values = Vec::new();
            synch.fls_values.retain(|&(_, slot), value| {
                if slot == index {
                    values.push(*value);
                }
                slot != index
            });
            if slot.callback == 0 || values.is_empty() {
                return Ok(CallbackStart::Return(1));
            }
            let first = values.remove(0);
            Ok(CallbackStart::Call {
                task: Box::new(FlsCleanup {
                    callback: slot.callback,
                    values: values.into_iter(),
                }),
                entry: slot.callback,
                args: [first, 0, 0, 0],
            })
        },
    );

    let s = Arc::clone(state);
    registry.register(KERNEL32, "FlsGetValue", move |args, cpu, _| {
        let index = args.rcx as u32;
        let synch = s.synch.lock().expect("guest synchronization state");
        if !synch.fls_slots.contains_key(&index) {
            drop(synch);
            return s.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
        }
        let value = synch
            .fls_values
            .get(&(cpu.gs_base, index))
            .copied()
            .unwrap_or(0);
        drop(synch);
        s.set_last_error(cpu.gs_base, 0);
        Ok(value)
    });

    let s = Arc::clone(state);
    registry.register(KERNEL32, "FlsSetValue", move |args, cpu, _| {
        let index = args.rcx as u32;
        let mut synch = s.synch.lock().expect("guest synchronization state");
        if !synch.fls_slots.contains_key(&index) {
            drop(synch);
            return s.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
        }
        if args.rdx == 0 {
            synch.fls_values.remove(&(cpu.gs_base, index));
        } else {
            synch.fls_values.insert((cpu.gs_base, index), args.rdx);
        }
        Ok(1)
    });

    for symbol in ["TlsAlloc", "TlsFree", "TlsGetValue", "TlsSetValue"] {
        registry.alias(TLS_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
    for symbol in ["FlsAlloc", "FlsFree", "FlsGetValue", "FlsSetValue"] {
        registry.alias(FLS_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
}
