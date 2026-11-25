// SPDX-License-Identifier: GPL-3.0-or-later

//! CRT services that call guest functions: sorting, searching and the
//! initializer tables run during C and C++ static initialization.
//!
//! Each service is a callback task. The comparator or initializer runs as
//! ordinary translated guest code, and the service continues when it returns.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::imports::{CallbackStart, CallbackStep, CallbackTask, ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError, PROCESS_HEAP};
use std::sync::Arc;

const UCRT: &str = "ucrtbase.dll";
const CRT_UTILITY_API: &str = "api-ms-win-crt-utility-l1-1-0.dll";
const CRT_RUNTIME_API: &str = "api-ms-win-crt-runtime-l1-1-0.dll";
/// Largest array, in bytes, that sorting may permute.
const SORT_BUDGET: u64 = 1_048_576;
/// Largest initializer table, in entries.
const MAX_INITIALIZERS: u64 = 65_536;

/// An invalid argument would reach the CRT invalid-parameter handler, which
/// the runtime does not model.
fn unsupported<T>() -> Result<T, ExitReason> {
    Err(ExitReason::IllegalInstruction(0xFE))
}

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

/// The comparator's `int` result.
fn ordering(result: u64) -> i32 {
    result as u32 as i32
}

/// Comparator arguments. The secure variants pass the caller's context first.
#[derive(Clone, Copy)]
struct Comparator {
    entry: u64,
    context: Option<u64>,
}

impl Comparator {
    fn call(self, left: u64, right: u64) -> CallbackStep {
        let args = match self.context {
            Some(context) => [context, left, right, 0],
            None => [left, right, 0, 0],
        };
        CallbackStep::Call {
            entry: self.entry,
            args,
        }
    }
}

/// Binary insertion sort over element indices. Elements stay in place while
/// the comparator runs and are permuted once at the end.
struct Sort {
    base: u64,
    width: u64,
    count: u64,
    compare: Comparator,
    sorted: Vec<u64>,
    next: u64,
    low: usize,
    high: usize,
}

impl Sort {
    fn element(&self, index: u64) -> u64 {
        self.base + index * self.width
    }

    fn probe(&self) -> CallbackStep {
        let middle = (self.low + self.high) / 2;
        self.compare
            .call(self.element(self.next), self.element(self.sorted[middle]))
    }

    fn permute(&self, memory: &mut GuestMemory) -> Result<(), ExitReason> {
        let mut original = vec![0; (self.count * self.width) as usize];
        memory.read_bytes(self.base, &mut original).map_err(fault)?;
        let width = self.width as usize;
        let mut ordered = Vec::with_capacity(original.len());
        for &index in &self.sorted {
            let start = index as usize * width;
            ordered.extend_from_slice(&original[start..start + width]);
        }
        memory.write_bytes(self.base, &ordered).map_err(fault)
    }
}

impl CallbackTask for Sort {
    fn resume(
        &mut self,
        result: u64,
        _: u64,
        memory: &mut GuestMemory,
    ) -> Result<CallbackStep, ExitReason> {
        let middle = (self.low + self.high) / 2;
        // Inserting after equal elements keeps equal keys in input order.
        if ordering(result) < 0 {
            self.high = middle;
        } else {
            self.low = middle + 1;
        }
        if self.low < self.high {
            return Ok(self.probe());
        }
        self.sorted.insert(self.low, self.next);
        self.next += 1;
        if self.next == self.count {
            self.permute(memory)?;
            return Ok(CallbackStep::Return(0));
        }
        self.low = 0;
        self.high = self.sorted.len();
        Ok(self.probe())
    }
}

fn start_sort(
    memory: &mut GuestMemory,
    base: u64,
    count: u64,
    width: u64,
    compare: Comparator,
) -> Result<CallbackStart, ExitReason> {
    if count > 0 && (base == 0 || width == 0) || compare.entry == 0 {
        return unsupported();
    }
    if count < 2 {
        return Ok(CallbackStart::Return(0));
    }
    let Some(bytes) = count
        .checked_mul(width)
        .filter(|&bytes| bytes <= SORT_BUDGET)
    else {
        return unsupported();
    };
    base.checked_add(bytes)
        .ok_or(fault(MemoryError::Overflow))?;
    memory.check_write(base, bytes as usize).map_err(fault)?;
    let sort = Sort {
        base,
        width,
        count,
        compare,
        sorted: vec![0],
        next: 1,
        low: 0,
        high: 1,
    };
    let CallbackStep::Call { entry, args } = sort.probe() else {
        unreachable!("a probe always calls the comparator");
    };
    Ok(CallbackStart::Call {
        task: Box::new(sort),
        entry,
        args,
    })
}

struct Search {
    key: u64,
    base: u64,
    width: u64,
    compare: Comparator,
    low: u64,
    high: u64,
}

impl Search {
    fn middle(&self) -> u64 {
        self.low + (self.high - self.low) / 2
    }

    fn probe(&self) -> CallbackStep {
        self.compare
            .call(self.key, self.base + self.middle() * self.width)
    }
}

impl CallbackTask for Search {
    fn resume(
        &mut self,
        result: u64,
        _: u64,
        _: &mut GuestMemory,
    ) -> Result<CallbackStep, ExitReason> {
        let middle = self.middle();
        match ordering(result) {
            0 => return Ok(CallbackStep::Return(self.base + middle * self.width)),
            order if order < 0 => self.high = middle,
            _ => self.low = middle + 1,
        }
        if self.low < self.high {
            Ok(self.probe())
        } else {
            Ok(CallbackStep::Return(0))
        }
    }
}

fn start_search(
    key: u64,
    base: u64,
    count: u64,
    width: u64,
    compare: Comparator,
) -> Result<CallbackStart, ExitReason> {
    if compare.entry == 0 || count > 0 && (base == 0 || width == 0) {
        return unsupported();
    }
    if count == 0 {
        return Ok(CallbackStart::Return(0));
    }
    count
        .checked_mul(width)
        .and_then(|bytes| base.checked_add(bytes))
        .ok_or(fault(MemoryError::Overflow))?;
    let search = Search {
        key,
        base,
        width,
        compare,
        low: 0,
        high: count,
    };
    let CallbackStep::Call { entry, args } = search.probe() else {
        unreachable!("a probe always calls the comparator");
    };
    Ok(CallbackStart::Call {
        task: Box::new(search),
        entry,
        args,
    })
}

/// Calls each nonnull function pointer in `[cursor, end)` in order.
/// `_initterm_e` stops at the first initializer that returns nonzero.
struct Initializers {
    cursor: u64,
    end: u64,
    stop_on_error: bool,
}

impl Initializers {
    fn next(&mut self, memory: &GuestMemory) -> Result<CallbackStep, ExitReason> {
        while self.cursor < self.end {
            let entry = memory.read_u64(self.cursor).map_err(fault)?;
            self.cursor += 8;
            if entry != 0 {
                return Ok(CallbackStep::Call {
                    entry,
                    args: [0; 4],
                });
            }
        }
        Ok(CallbackStep::Return(0))
    }
}

impl CallbackTask for Initializers {
    fn resume(
        &mut self,
        result: u64,
        _: u64,
        memory: &mut GuestMemory,
    ) -> Result<CallbackStep, ExitReason> {
        let status = result as u32;
        if self.stop_on_error && status != 0 {
            return Ok(CallbackStep::Return(u64::from(status)));
        }
        self.next(memory)
    }
}

fn start_initializers(
    memory: &mut GuestMemory,
    first: u64,
    last: u64,
    stop_on_error: bool,
) -> Result<CallbackStart, ExitReason> {
    let Some(span) = last.checked_sub(first) else {
        return unsupported();
    };
    if !span.is_multiple_of(8) || span / 8 > MAX_INITIALIZERS {
        return unsupported();
    }
    let mut table = Initializers {
        cursor: first,
        end: last,
        stop_on_error,
    };
    Ok(match table.next(memory)? {
        CallbackStep::Call { entry, args } => CallbackStart::Call {
            task: Box::new(table),
            entry,
            args,
        },
        CallbackStep::Return(value) => CallbackStart::Return(value),
    })
}

/// `_onexit_table_t` from corecrt_startup.h: `_first`, `_last` and `_end`
/// pointers to an array of function pointers.
const ONEXIT_TABLE_SIZE: usize = 24;
const ONEXIT_INITIAL_ENTRIES: u64 = 32;
const MAX_ONEXIT_ENTRIES: u64 = 65_536;
const CRT_FAILURE: u64 = u32::MAX as u64;

fn read_onexit_table(memory: &GuestMemory, table: u64) -> Result<[u64; 3], ExitReason> {
    let mut bytes = [0; ONEXIT_TABLE_SIZE];
    memory.read_bytes(table, &mut bytes).map_err(fault)?;
    Ok(std::array::from_fn(|index| {
        u64::from_le_bytes(bytes[8 * index..8 * index + 8].try_into().unwrap())
    }))
}

fn write_onexit_table(
    memory: &mut GuestMemory,
    table: u64,
    pointers: [u64; 3],
) -> Result<(), ExitReason> {
    let mut bytes = [0; ONEXIT_TABLE_SIZE];
    for (chunk, pointer) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(pointers) {
        *chunk = pointer.to_le_bytes();
    }
    memory.write_bytes(table, &bytes).map_err(fault)
}

/// Append `function`, growing the table's storage on the default process heap.
/// Returns false when the storage cannot grow.
fn register_onexit(
    memory: &mut GuestMemory,
    table: u64,
    function: u64,
) -> Result<bool, ExitReason> {
    let [mut first, mut last, mut end] = read_onexit_table(memory, table)?;
    if first == 0 {
        let bytes = (ONEXIT_INITIAL_ENTRIES * 8) as usize;
        let Ok(storage) = memory.map_heap_allocation(PROCESS_HEAP, bytes) else {
            return Ok(false);
        };
        (first, last, end) = (storage, storage, storage + bytes as u64);
    }
    if last < first || end < last || !(end - first).is_multiple_of(8) {
        return unsupported();
    }
    if last == end {
        let capacity = (end - first) / 8;
        if capacity >= MAX_ONEXIT_ENTRIES {
            return Ok(false);
        }
        let bytes = (capacity * 2 * 8) as usize;
        let Ok(storage) = memory.resize_heap_allocation(PROCESS_HEAP, first, bytes, false) else {
            return Ok(false);
        };
        (first, last, end) = (storage, storage + capacity * 8, storage + bytes as u64);
    }
    memory.write_u64(last, function).map_err(fault)?;
    write_onexit_table(memory, table, [first, last + 8, end])?;
    Ok(true)
}

/// Runs registered functions in reverse order of registration.
struct ExitFunctions {
    pending: Vec<u64>,
}

impl ExitFunctions {
    fn next(&mut self) -> CallbackStep {
        match self.pending.pop() {
            Some(entry) => CallbackStep::Call {
                entry,
                args: [0; 4],
            },
            None => CallbackStep::Return(0),
        }
    }

    fn start(mut self) -> CallbackStart {
        match self.next() {
            CallbackStep::Call { entry, args } => CallbackStart::Call {
                task: Box::new(self),
                entry,
                args,
            },
            CallbackStep::Return(value) => CallbackStart::Return(value),
        }
    }
}

impl CallbackTask for ExitFunctions {
    fn resume(&mut self, _: u64, _: u64, _: &mut GuestMemory) -> Result<CallbackStep, ExitReason> {
        Ok(self.next())
    }
}

/// `exit`: calls the functions registered with `atexit` and `_crt_atexit`,
/// most recently registered first, and then ends the guest process.
///
/// It takes one function at a time from the shared list. A function that is
/// registered while `exit` runs is therefore called next, before the older
/// ones that have not run yet, as ISO C requires (7.22.4.2, 7.22.4.4). A
/// registered function that calls `exit` again, which ISO C leaves undefined,
/// starts a second task on the same list: the remaining functions run once
/// each and the process ends with the later status.
struct Exit {
    state: Arc<ServicesState>,
    status: u32,
}

impl Exit {
    fn next(&mut self) -> Result<CallbackStep, ExitReason> {
        let function = self
            .state
            .atexit
            .lock()
            .expect("guest atexit functions")
            .pop();
        match function {
            Some(entry) => Ok(CallbackStep::Call {
                entry,
                args: [0; 4],
            }),
            None => Err(ExitReason::ProcessExit(
                self.state.exit_process(self.status),
            )),
        }
    }

    fn start(mut self) -> Result<CallbackStart, ExitReason> {
        match self.next()? {
            CallbackStep::Call { entry, args } => Ok(CallbackStart::Call {
                task: Box::new(self),
                entry,
                args,
            }),
            CallbackStep::Return(value) => Ok(CallbackStart::Return(value)),
        }
    }
}

impl CallbackTask for Exit {
    fn resume(&mut self, _: u64, _: u64, _: &mut GuestMemory) -> Result<CallbackStep, ExitReason> {
        self.next()
    }
}

fn register_exit_tables(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    registry.register(UCRT, "_initialize_onexit_table", |args, _, mem| {
        let table = args.rcx;
        if table == 0 {
            return Ok(CRT_FAILURE);
        }
        // An initialized table keeps its registrations.
        if read_onexit_table(mem, table)?[0] == 0 {
            write_onexit_table(mem, table, [0; 3])?;
        }
        Ok(0)
    });

    registry.register(UCRT, "_register_onexit_function", |args, _, mem| {
        if args.rcx == 0 {
            return Ok(CRT_FAILURE);
        }
        Ok(if register_onexit(mem, args.rcx, args.rdx)? {
            0
        } else {
            CRT_FAILURE
        })
    });

    registry.register_callback_task(
        UCRT,
        "_execute_onexit_table",
        ServiceStatus::Partial,
        |args, _, mem, _| {
            let table = args.rcx;
            if table == 0 {
                return Ok(CallbackStart::Return(CRT_FAILURE));
            }
            let [first, last, _] = read_onexit_table(mem, table)?;
            if first == 0 {
                return Ok(CallbackStart::Return(0));
            }
            if last < first || !(last - first).is_multiple_of(8) {
                return unsupported();
            }
            let mut pending = Vec::with_capacity(((last - first) / 8) as usize);
            for slot in (first..last).step_by(8) {
                let entry = mem.read_u64(slot).map_err(fault)?;
                if entry != 0 {
                    pending.push(entry);
                }
            }
            // The table is reset before the functions run, so a function
            // registered while they run starts a new table.
            write_onexit_table(mem, table, [0; 3])?;
            mem.release_heap_allocation(PROCESS_HEAP, first);
            Ok(ExitFunctions { pending }.start())
        },
    );

    for symbol in ["atexit", "_crt_atexit"] {
        let s = Arc::clone(state);
        registry.register(UCRT, symbol, move |args, _, _| {
            let mut functions = s.atexit.lock().expect("guest atexit functions");
            if args.rcx == 0 || functions.len() as u64 >= MAX_ONEXIT_ENTRIES {
                return Ok(CRT_FAILURE);
            }
            functions.push(args.rcx);
            Ok(0)
        });
    }

    let s = Arc::clone(state);
    registry.register_callback_task(UCRT, "_cexit", ServiceStatus::Partial, move |_, _, _, _| {
        let pending = std::mem::take(&mut *s.atexit.lock().expect("guest atexit functions"));
        Ok(ExitFunctions { pending }.start())
    });

    // exit does not return. It is partial: no stream is flushed, because
    // there are none, and thread-local destructors are not run.
    let s = Arc::clone(state);
    registry.register_callback_task(
        UCRT,
        "exit",
        ServiceStatus::Partial,
        move |args, _, _, _| {
            // The status is an int, the low half of the first argument.
            Exit {
                state: Arc::clone(&s),
                status: args.rcx as u32,
            }
            .start()
        },
    );
    registry.alias(CRT_RUNTIME_API, UCRT, "exit");

    for symbol in [
        "_initialize_onexit_table",
        "_register_onexit_function",
        "atexit",
        "_crt_atexit",
    ] {
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
    for symbol in [
        "_initialize_onexit_table",
        "_register_onexit_function",
        "_execute_onexit_table",
        "_crt_atexit",
        "_cexit",
    ] {
        registry.alias(CRT_RUNTIME_API, UCRT, symbol);
    }
    registry.alias(CRT_RUNTIME_API, UCRT, "atexit");
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    register_exit_tables(registry, state);
    registry.register_callback_task(UCRT, "qsort", ServiceStatus::Partial, |args, _, mem, _| {
        let compare = Comparator {
            entry: args.r9,
            context: None,
        };
        start_sort(mem, args.rcx, args.rdx, args.r8, compare)
    });
    registry.register_callback_task(
        UCRT,
        "qsort_s",
        ServiceStatus::Partial,
        |args, _, mem, _| {
            let context = args.get(4, mem).map_err(fault)?;
            let compare = Comparator {
                entry: args.r9,
                context: Some(context),
            };
            start_sort(mem, args.rcx, args.rdx, args.r8, compare)
        },
    );
    registry.register_callback_task(
        UCRT,
        "bsearch",
        ServiceStatus::Partial,
        |args, _, mem, _| {
            let compare = Comparator {
                entry: args.get(4, mem).map_err(fault)?,
                context: None,
            };
            start_search(args.rcx, args.rdx, args.r8, args.r9, compare)
        },
    );
    registry.register_callback_task(
        UCRT,
        "bsearch_s",
        ServiceStatus::Partial,
        |args, _, mem, _| {
            let compare = Comparator {
                entry: args.get(4, mem).map_err(fault)?,
                context: Some(args.get(5, mem).map_err(fault)?),
            };
            start_search(args.rcx, args.rdx, args.r8, args.r9, compare)
        },
    );
    for (symbol, stop_on_error) in [("_initterm", false), ("_initterm_e", true)] {
        registry.register_callback_task(
            UCRT,
            symbol,
            ServiceStatus::Partial,
            move |args, _, mem, _| start_initializers(mem, args.rcx, args.rdx, stop_on_error),
        );
        registry.alias(CRT_RUNTIME_API, UCRT, symbol);
    }
    for symbol in ["qsort", "qsort_s", "bsearch", "bsearch_s"] {
        registry.alias(CRT_UTILITY_API, UCRT, symbol);
    }
}
