// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded Microsoft STL `ios_base::Init` reference-count services.
//!
//! The public Microsoft STL `stl/src/iostream.cpp` at commit
//! `0cabaf3c7e0a73fe3040aabce40cd8833e80392b` defines the count and
//! operations. Standard stream objects and their flushing are not implemented;
//! the final destructor therefore stops instead of claiming to have flushed.

use crate::cpu::ExitReason;
use crate::imports::{CallbackStart, CallbackStep, CallbackTask, ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError, PROCESS_HEAP};
use std::sync::{Arc, Mutex};

const MODULE: &str = "msvcp140.dll";
const COUNT_DATA: &str = "?_Init_cnt@Init@ios_base@std@@0HA";
const COUNT: &str = "?_Init_cnt_func@Init@ios_base@std@@CAAEAHXZ";
const CTOR: &str = "?_Init_ctor@Init@ios_base@std@@CAXPEAV123@@Z";
const DTOR: &str = "?_Init_dtor@Init@ios_base@std@@CAXPEAV123@@Z";
const PUBLIC_CTOR: &str = "??0Init@ios_base@std@@QEAA@XZ";
const PUBLIC_DTOR: &str = "??1Init@ios_base@std@@QEAA@XZ";
const LIMIT: ExitReason = ExitReason::IllegalInstruction(0xFE);

fn count_address(slot: &mut Option<u64>, memory: &mut GuestMemory) -> Result<u64, ExitReason> {
    if let Some(address) = *slot {
        return Ok(address);
    }
    let address = memory
        .map_heap_allocation(PROCESS_HEAP, 4)
        .map_err(ExitReason::MemoryFault)?;
    memory
        .write_u32(address, u32::MAX)
        .map_err(ExitReason::MemoryFault)?;
    *slot = Some(address);
    Ok(address)
}

pub(super) fn register(registry: &mut ImportRegistry) {
    register_ostream_flush(registry);
    register_ostream_write(registry);
    // x64 ios_base stores its iostate after the vptr and _Stdstr. These
    // accessors inspect only that field; construction remains unsupported.
    for (symbol, mask, raw) in [
        ("?rdstate@ios_base@std@@QEBAHXZ", 0_u32, true),
        ("?good@ios_base@std@@QEBA_NXZ", 0, false),
        ("?eof@ios_base@std@@QEBA_NXZ", 1, false),
        ("?fail@ios_base@std@@QEBA_NXZ", 6, false),
        ("?bad@ios_base@std@@QEBA_NXZ", 4, false),
    ] {
        registry.register_with_status(
            MODULE,
            symbol,
            ServiceStatus::Partial,
            move |args, _, memory| {
                let address = args
                    .rcx
                    .checked_add(16)
                    .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
                let state = memory.read_u32(address).map_err(ExitReason::MemoryFault)?;
                Ok(if raw {
                    u64::from(state)
                } else if mask == 0 {
                    u64::from(state == 0)
                } else {
                    u64::from(state & mask != 0)
                })
            },
        );
    }
    // Microsoft STL ulocale.cpp defines one zero-initialized locale::id per
    // specialization. Each is a distinct pointer-sized guest object.
    for symbol in [
        "?id@?$codecvt@_SDU_Mbstatet@@@std@@2V0locale@2@A",
        "?id@?$codecvt@_S_QU_Mbstatet@@@std@@2V0locale@2@A",
        "?id@?$codecvt@_UDU_Mbstatet@@@std@@2V0locale@2@A",
        "?id@?$codecvt@_U_QU_Mbstatet@@@std@@2V0locale@2@A",
    ] {
        let address = Mutex::new(None);
        registry.register_data(
            "msvcp140_codecvt_ids.dll",
            symbol,
            ServiceStatus::Partial,
            move |memory| {
                let mut slot = address.lock().expect("Microsoft codecvt id");
                if let Some(existing) = *slot {
                    return Ok(existing);
                }
                let value = memory.map_heap_allocation(PROCESS_HEAP, 8)?;
                memory.write_u64(value, 0)?;
                *slot = Some(value);
                Ok(value)
            },
        );
    }
    let cell = Arc::new(Mutex::new(None));
    let data = Arc::clone(&cell);
    registry.register_data(MODULE, COUNT_DATA, ServiceStatus::Partial, move |memory| {
        super::cell_memory(count_address(
            &mut data.lock().expect("MSVC initialization count"),
            memory,
        ))
    });
    let count = Arc::clone(&cell);
    registry.register_with_status(
        MODULE,
        COUNT,
        ServiceStatus::Partial,
        move |_, _, memory| {
            count_address(
                &mut count.lock().expect("MSVC initialization count"),
                memory,
            )
        },
    );
    for symbol in [CTOR, PUBLIC_CTOR] {
        let count = Arc::clone(&cell);
        registry.register_with_status(
            MODULE,
            symbol,
            ServiceStatus::Partial,
            move |args, _, memory| {
                let mut guard = count.lock().expect("MSVC initialization count");
                let address = count_address(&mut guard, memory)?;
                let old = memory.read_u32(address).map_err(ExitReason::MemoryFault)? as i32;
                let new = if old < 0 {
                    1
                } else {
                    old.checked_add(1).ok_or(LIMIT)?
                };
                memory
                    .write_u32(address, new as u32)
                    .map_err(ExitReason::MemoryFault)?;
                Ok(if symbol == PUBLIC_CTOR { args.rcx } else { 0 })
            },
        );
    }
    for symbol in [DTOR, PUBLIC_DTOR] {
        let count = Arc::clone(&cell);
        registry.register_with_status(
            MODULE,
            symbol,
            ServiceStatus::Partial,
            move |_, _, memory| {
                let mut guard = count.lock().expect("MSVC initialization count");
                let address = count_address(&mut guard, memory)?;
                let old = memory.read_u32(address).map_err(ExitReason::MemoryFault)? as i32;
                let new = old.checked_sub(1).ok_or(LIMIT)?;
                memory
                    .write_u32(address, new as u32)
                    .map_err(ExitReason::MemoryFault)?;
                if new == 0 {
                    return Err(LIMIT);
                }
                Ok(0)
            },
        );
    }
}

/// VCRUNTIME exports these same C memory contracts. The existing services
/// retain their guest bounds and fault handling; no host pointers are exposed.
pub(super) fn register_vcruntime_memory(registry: &mut ImportRegistry) {
    for symbol in ["memcpy", "memmove", "memset", "memcmp", "memchr"] {
        registry.alias("vcruntime140.dll", "ucrtbase.dll", symbol);
    }
}

/// Microsoft STL atomic-wait deadline arithmetic, using the same guest clock
/// as GetTickCount64. This does not implement atomic waiting or notification.
pub(super) fn register_atomic_deadlines(
    registry: &mut ImportRegistry,
    state: &Arc<super::ServicesState>,
) {
    const ATOMIC: &str = "msvcp140_atomic_wait.dll";
    let clock = Arc::clone(state);
    registry.register_with_status(
        ATOMIC,
        "__std_atomic_wait_get_deadline",
        ServiceStatus::Partial,
        move |args, _, _| {
            Ok(if args.rcx == u64::MAX {
                u64::MAX
            } else {
                clock.uptime_millis().wrapping_add(args.rcx)
            })
        },
    );
    let clock = Arc::clone(state);
    registry.register_with_status(
        ATOMIC,
        "__std_atomic_wait_get_remaining_timeout",
        ServiceStatus::Partial,
        move |args, _, _| {
            if args.rcx == u64::MAX {
                return Ok(u64::from(u32::MAX));
            }
            Ok(args
                .rcx
                .saturating_sub(clock.uptime_millis())
                .min(864_000_000))
        },
    );
}

const OSTREAM_FLUSH: &str = "?flush@?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV12@XZ";

fn field(base: u64, offset: u64) -> Result<u64, ExitReason> {
    base.checked_add(offset)
        .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))
}

fn stream_virtual(memory: &GuestMemory, buffer: u64, index: u64) -> Result<u64, ExitReason> {
    let table = memory.read_u64(buffer).map_err(ExitReason::MemoryFault)?;
    memory
        .read_u64(field(table, index * 8)?)
        .map_err(ExitReason::MemoryFault)
}

// Equivalent to basic_ios::setstate for the supported non-throwing path.
fn merge_state(memory: &mut GuestMemory, ios: u64, bits: u32) -> Result<(), ExitReason> {
    let address = field(ios, 16)?;
    let current = memory.read_u32(address).map_err(ExitReason::MemoryFault)?;
    let buffer = memory
        .read_u64(field(ios, 72)?)
        .map_err(ExitReason::MemoryFault)?;
    let next = (current | bits | if buffer == 0 { 4 } else { 0 }) & 0x17;
    memory
        .write_u32(address, next)
        .map_err(ExitReason::MemoryFault)?;
    let exceptions = memory
        .read_u32(field(ios, 20)?)
        .map_err(ExitReason::MemoryFault)?;
    if next & exceptions != 0 {
        return Err(LIMIT);
    }
    Ok(())
}

enum FlushPhase {
    Locked,
    Synced,
    Unlocked,
}
struct FlushTask {
    stream: u64,
    ios: u64,
    buffer: u64,
    phase: FlushPhase,
}
impl FlushTask {
    fn unlock(&mut self, memory: &GuestMemory) -> Result<CallbackStep, ExitReason> {
        let buffer = memory
            .read_u64(field(self.ios, 72)?)
            .map_err(ExitReason::MemoryFault)?;
        if buffer == 0 {
            return Ok(CallbackStep::Return(self.stream));
        }
        self.phase = FlushPhase::Unlocked;
        Ok(CallbackStep::Call {
            entry: stream_virtual(memory, buffer, 2)?,
            args: [buffer, 0, 0, 0],
        })
    }
}
impl CallbackTask for FlushTask {
    fn resume(
        &mut self,
        result: u64,
        _: u64,
        memory: &mut GuestMemory,
    ) -> Result<CallbackStep, ExitReason> {
        match self.phase {
            FlushPhase::Locked => {
                let state = memory
                    .read_u32(field(self.ios, 16)?)
                    .map_err(ExitReason::MemoryFault)?;
                let tie = memory
                    .read_u64(field(self.ios, 80)?)
                    .map_err(ExitReason::MemoryFault)?;
                let flags = memory
                    .read_u32(field(self.ios, 24)?)
                    .map_err(ExitReason::MemoryFault)?;
                if (state == 0 && tie != 0 && tie != self.stream) || flags & 2 != 0 {
                    return Err(LIMIT);
                }
                if state == 0 {
                    self.phase = FlushPhase::Synced;
                    Ok(CallbackStep::Call {
                        entry: stream_virtual(memory, self.buffer, 13)?,
                        args: [self.buffer, 0, 0, 0],
                    })
                } else {
                    self.unlock(memory)
                }
            }
            FlushPhase::Synced => {
                merge_state(
                    memory,
                    self.ios,
                    if result as u32 == u32::MAX { 4 } else { 0 },
                )?;
                let state = memory
                    .read_u32(field(self.ios, 16)?)
                    .map_err(ExitReason::MemoryFault)?;
                let flags = memory
                    .read_u32(field(self.ios, 24)?)
                    .map_err(ExitReason::MemoryFault)?;
                if state == 0 && flags & 2 != 0 {
                    return Err(LIMIT);
                }
                self.unlock(memory)
            }
            FlushPhase::Unlocked => Ok(CallbackStep::Return(self.stream)),
        }
    }
}

fn register_ostream_flush(registry: &mut ImportRegistry) {
    registry.register_callback_task(
        MODULE,
        OSTREAM_FLUSH,
        ServiceStatus::Partial,
        |args, _, memory, _| {
            let stream = args.rcx;
            let vbtable = memory.read_u64(stream).map_err(ExitReason::MemoryFault)?;
            let displacement = memory
                .read_u32(field(vbtable, 4)?)
                .map_err(ExitReason::MemoryFault)? as i32;
            let ios = stream
                .checked_add_signed(i64::from(displacement))
                .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
            let buffer = memory
                .read_u64(field(ios, 72)?)
                .map_err(ExitReason::MemoryFault)?;
            if buffer == 0 {
                return Ok(CallbackStart::Return(stream));
            }
            let tie = memory
                .read_u64(field(ios, 80)?)
                .map_err(ExitReason::MemoryFault)?;
            let flags = memory
                .read_u32(field(ios, 24)?)
                .map_err(ExitReason::MemoryFault)?;
            // Flushing another tied stream and unit-buffered sentry teardown need
            // additional nested flush/exception semantics; stop explicitly.
            if (tie != 0 && tie != stream) || flags & 2 != 0 {
                return Err(LIMIT);
            }
            let entry = stream_virtual(memory, buffer, 1)?;
            Ok(CallbackStart::Call {
                task: Box::new(FlushTask {
                    stream,
                    ios,
                    buffer,
                    phase: FlushPhase::Locked,
                }),
                entry,
                args: [buffer, 0, 0, 0],
            })
        },
    );
}

const OSTREAM_WRITE: &str =
    "?write@?$basic_ostream@DU?$char_traits@D@std@@@std@@QEAAAEAV12@PEBD_J@Z";
enum WritePhase {
    Locked,
    Written,
    Unlocked,
}
struct WriteTask {
    stream: u64,
    ios: u64,
    input: u64,
    count: u64,
    phase: WritePhase,
}
impl WriteTask {
    fn finish(&mut self, memory: &mut GuestMemory, bad: bool) -> Result<CallbackStep, ExitReason> {
        merge_state(memory, self.ios, if bad { 4 } else { 0 })?;
        let state = memory
            .read_u32(field(self.ios, 16)?)
            .map_err(ExitReason::MemoryFault)?;
        let flags = memory
            .read_u32(field(self.ios, 24)?)
            .map_err(ExitReason::MemoryFault)?;
        if state == 0 && flags & 2 != 0 {
            return Err(LIMIT);
        }
        let buffer = memory
            .read_u64(field(self.ios, 72)?)
            .map_err(ExitReason::MemoryFault)?;
        if buffer == 0 {
            return Ok(CallbackStep::Return(self.stream));
        }
        self.phase = WritePhase::Unlocked;
        Ok(CallbackStep::Call {
            entry: stream_virtual(memory, buffer, 2)?,
            args: [buffer, 0, 0, 0],
        })
    }
    fn after_lock(&mut self, memory: &mut GuestMemory) -> Result<CallbackStep, ExitReason> {
        let state = memory
            .read_u32(field(self.ios, 16)?)
            .map_err(ExitReason::MemoryFault)?;
        let tie = memory
            .read_u64(field(self.ios, 80)?)
            .map_err(ExitReason::MemoryFault)?;
        let flags = memory
            .read_u32(field(self.ios, 24)?)
            .map_err(ExitReason::MemoryFault)?;
        if (state == 0 && tie != 0 && tie != self.stream) || flags & 2 != 0 {
            return Err(LIMIT);
        }
        if state != 0 {
            return self.finish(memory, true);
        }
        if (self.count as i64) <= 0 {
            return self.finish(memory, false);
        }
        let buffer = memory
            .read_u64(field(self.ios, 72)?)
            .map_err(ExitReason::MemoryFault)?;
        self.phase = WritePhase::Written;
        Ok(CallbackStep::Call {
            entry: stream_virtual(memory, buffer, 9)?,
            args: [buffer, self.input, self.count, 0],
        })
    }
}
impl CallbackTask for WriteTask {
    fn resume(
        &mut self,
        result: u64,
        _: u64,
        memory: &mut GuestMemory,
    ) -> Result<CallbackStep, ExitReason> {
        match self.phase {
            WritePhase::Locked => self.after_lock(memory),
            WritePhase::Written => self.finish(memory, result != self.count),
            WritePhase::Unlocked => Ok(CallbackStep::Return(self.stream)),
        }
    }
}
fn register_ostream_write(registry: &mut ImportRegistry) {
    registry.register_callback_task(
        MODULE,
        OSTREAM_WRITE,
        ServiceStatus::Partial,
        |args, _, memory, _| {
            let stream = args.rcx;
            let vbtable = memory.read_u64(stream).map_err(ExitReason::MemoryFault)?;
            let displacement = memory
                .read_u32(field(vbtable, 4)?)
                .map_err(ExitReason::MemoryFault)? as i32;
            let ios = stream
                .checked_add_signed(i64::from(displacement))
                .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
            let buffer = memory
                .read_u64(field(ios, 72)?)
                .map_err(ExitReason::MemoryFault)?;
            let mut task = WriteTask {
                stream,
                ios,
                input: args.rdx,
                count: args.r8,
                phase: WritePhase::Locked,
            };
            let step = if buffer == 0 {
                task.after_lock(memory)?
            } else {
                CallbackStep::Call {
                    entry: stream_virtual(memory, buffer, 1)?,
                    args: [buffer, 0, 0, 0],
                }
            };
            Ok(match step {
                CallbackStep::Return(value) => CallbackStart::Return(value),
                CallbackStep::Call { entry, args } => CallbackStart::Call {
                    task: Box::new(task),
                    entry,
                    args,
                },
            })
        },
    );
}
