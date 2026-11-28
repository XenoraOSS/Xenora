// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded Microsoft STL `ios_base::Init` reference-count services.
//!
//! The public Microsoft STL `stl/src/iostream.cpp` at commit
//! `0cabaf3c7e0a73fe3040aabce40cd8833e80392b` defines the count and
//! operations. The final destructor flushes the bounded synthetic `std::cout`
//! and `std::clog` objects when present.

use super::ServicesState;
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
const COUT: &str = "?cout@std@@3V?$basic_ostream@DU?$char_traits@D@std@@@1@A";
const CLOG: &str = "?clog@std@@3V?$basic_ostream@DU?$char_traits@D@std@@@1@A";
const STANDARD_OSTREAMS: [(u32, &str); 2] = [(1, COUT), (2, CLOG)];
type StandardOstreamSlots = Mutex<[Option<u64>; STANDARD_OSTREAMS.len()]>;
/// A guest object large enough for the documented x64 `ios_base` fields this
/// slice reads. It is not a Microsoft STL object layout.
const STANDARD_OSTREAM_BYTES: usize = 128;
/// Matches the CRT transfer bound used by the host stream services.
const MAX_STANDARD_OSTREAM_TRANSFER: u64 = 1 << 20;
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

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let standard_ostreams = Arc::new(StandardOstreamSlots::default());

    // Microsoft x64 locale::facet has a 32-bit reference count at offset 8.
    // Both calls share a lock so their guest-memory update is indivisible.
    let facet_lock = Arc::new(Mutex::new(()));
    for (symbol, increment) in [
        ("?_Incref@facet@locale@std@@UEAAXXZ", true),
        ("?_Decref@facet@locale@std@@UEAAPEAV_Facet_base@3@XZ", false),
    ] {
        let lock = Arc::clone(&facet_lock);
        registry.register_with_status(
            MODULE,
            symbol,
            ServiceStatus::Partial,
            move |args, _, memory| {
                let _guard = lock.lock().expect("Microsoft facet reference count");
                let address = field(args.rcx, 8)?;
                let previous = memory.read_u32(address).map_err(ExitReason::MemoryFault)?;
                let next = if increment {
                    previous.wrapping_add(1)
                } else {
                    previous.wrapping_sub(1)
                };
                memory
                    .write_u32(address, next)
                    .map_err(ExitReason::MemoryFault)?;
                Ok(if !increment && next == 0 { args.rcx } else { 0 })
            },
        );
    }
    registry.register_with_status(
        MODULE,
        "?do_widen@?$ctype@D@std@@MEBADD@Z",
        ServiceStatus::Partial,
        |args, _, _| Ok(args.rdx & 0xff),
    );
    register_ostream_flush(registry, state, Arc::clone(&standard_ostreams));
    register_ostream_write(registry, state, Arc::clone(&standard_ostreams));
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
    // A data import receives the address of the provider's guest cell. Here
    // the cell is a small writable object whose state word the supported
    // queries and methods read; no streambuf vtable or locale is synthesized.
    for (index, (_, symbol)) in STANDARD_OSTREAMS.into_iter().enumerate() {
        let streams = Arc::clone(&standard_ostreams);
        registry.register_data(MODULE, symbol, ServiceStatus::Partial, move |memory| {
            let mut slots = streams.lock().expect("Microsoft standard ostream objects");
            if let Some(address) = slots[index] {
                return Ok(address);
            }
            let address = memory.map_heap_allocation(PROCESS_HEAP, STANDARD_OSTREAM_BYTES)?;
            for offset in (0..STANDARD_OSTREAM_BYTES as u64).step_by(8) {
                memory.write_u64(address + offset, 0)?;
            }
            slots[index] = Some(address);
            Ok(address)
        });
    }
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
        let streams = Arc::clone(&standard_ostreams);
        let state = Arc::clone(state);
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
                    let slots = *streams.lock().expect("Microsoft standard ostream objects");
                    // Microsoft STL flushes clog before cout in the last Init destructor.
                    for ((file, _), object) in STANDARD_OSTREAMS.into_iter().zip(slots).rev() {
                        if let Some(object) = object {
                            flush_standard_ostream(&state, memory, object, file)?;
                        }
                    }
                }
                Ok(0)
            },
        );
    }
}

/// Resolve a synthesized guest ostream to its CRT standard file index.
fn standard_ostream_file(streams: &StandardOstreamSlots, object: u64) -> Option<u32> {
    let slots = streams.lock().expect("Microsoft standard ostream objects");
    STANDARD_OSTREAMS
        .into_iter()
        .zip(slots.iter())
        .find_map(|((file, _), address)| (*address == Some(object)).then_some(file))
}

/// Validate and flush one synthesized standard ostream. Writes go through
/// immediately, so the CRT flush has no pending bytes.
fn flush_standard_ostream(
    state: &ServicesState,
    memory: &mut GuestMemory,
    object: u64,
    file: u32,
) -> Result<(), ExitReason> {
    // The state word must still belong to the live object.
    memory
        .read_u32(field(object, 16)?)
        .map_err(ExitReason::MemoryFault)?;
    let file = state.crt_streams().file(memory, file)?;
    let result = state.crt_streams().flush(file)?;
    if result != 0 {
        return Err(LIMIT);
    }
    Ok(())
}

/// Write through a synthesized ostream to its associated CRT stream. A short
/// or failed transfer sets badbit. Oversized requests and exceptions stop.
fn write_standard_ostream(
    state: &ServicesState,
    memory: &mut GuestMemory,
    object: u64,
    file: u32,
    input: u64,
    count: u64,
) -> Result<u64, ExitReason> {
    if count > MAX_STANDARD_OSTREAM_TRANSFER {
        return Err(LIMIT);
    }
    let flags = field(object, 16)?;
    let state_bits = memory.read_u32(flags).map_err(ExitReason::MemoryFault)?;
    let exceptions = memory
        .read_u32(field(object, 20)?)
        .map_err(ExitReason::MemoryFault)?;
    if state_bits & exceptions != 0 {
        return Err(LIMIT);
    }
    if state_bits != 0 || count == 0 {
        return Ok(object);
    }
    let file = state.crt_streams().file(memory, file)?;
    let mut objects = state.objects.lock().expect("guest object state");
    let answer = state
        .crt_streams()
        .write_items(memory, &mut objects, input, 1, count, file)?;
    let written = answer.unwrap_or_default();
    if written != count {
        let next = state_bits | 4;
        memory
            .write_u32(flags, next)
            .map_err(ExitReason::MemoryFault)?;
        if next & exceptions != 0 {
            return Err(LIMIT);
        }
    }
    Ok(object)
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

fn register_ostream_flush(
    registry: &mut ImportRegistry,
    state: &Arc<ServicesState>,
    streams: Arc<StandardOstreamSlots>,
) {
    let state = Arc::clone(state);
    registry.register_callback_task(
        MODULE,
        OSTREAM_FLUSH,
        ServiceStatus::Partial,
        move |args, _, memory, _| {
            let stream = args.rcx;
            if let Some(file) = standard_ostream_file(&streams, stream) {
                flush_standard_ostream(&state, memory, stream, file)?;
                return Ok(CallbackStart::Return(stream));
            }
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
fn register_ostream_write(
    registry: &mut ImportRegistry,
    state: &Arc<ServicesState>,
    streams: Arc<StandardOstreamSlots>,
) {
    let state = Arc::clone(state);
    registry.register_callback_task(
        MODULE,
        OSTREAM_WRITE,
        ServiceStatus::Partial,
        move |args, _, memory, _| {
            let stream = args.rcx;
            if let Some(file) = standard_ostream_file(&streams, stream) {
                let object =
                    write_standard_ostream(&state, memory, stream, file, args.rdx, args.r8)?;
                return Ok(CallbackStart::Return(object));
            }
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

/// Expose the standard UCRT streams' live, unbuffered pointer fields. Opened
/// file streams still use a different representation and stop explicitly.
pub(super) fn register_stream_buffer_pointers(
    registry: &mut ImportRegistry,
    state: &Arc<super::ServicesState>,
) {
    let state = Arc::clone(state);
    registry.register_with_status(
        "ucrtbase.dll",
        "_get_stream_buffer_pointers",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            if args.rcx == 0 {
                super::crt::set_errno(memory, cpu.gs_base, super::crt::EINVAL)?;
                return Ok(u64::from(super::crt::EINVAL));
            }
            let fields = state.crt_streams().standard_buffer_fields(args.rcx)?;
            for (out, value) in [args.rdx, args.r8, args.r9].into_iter().zip(fields) {
                if out != 0 {
                    memory
                        .write_u64(out, value)
                        .map_err(ExitReason::MemoryFault)?;
                }
            }
            Ok(0)
        },
    );
    registry.alias(
        "api-ms-win-crt-stdio-l1-1-0.dll",
        "ucrtbase.dll",
        "_get_stream_buffer_pointers",
    );
}
