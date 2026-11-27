// SPDX-License-Identifier: GPL-3.0-or-later

//! Handle ownership for the documented manual GDK task queue subset.

use super::ServicesState;
use super::events::{INFINITE, TICKS_PER_MILLI};
use super::threads::Pending;
use super::{ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED};
use crate::cpu::ExitReason;
use crate::imports::{GuestCallback, ImportRegistry, ServiceStatus, Win64Args};
use crate::memory::GuestMemory;
use crate::runtime::HResult;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MODULE: &str = "xgameruntime.dll";
const MAX_HANDLES: usize = 64;
const MAX_CALLBACKS_PER_PORT: usize = 32;
const ERROR_OUTOFMEMORY: u32 = 14;
const MANUAL: u32 = 0;
const E_ABORT: u64 = 0x8000_4004;

#[derive(Default)]
pub(super) struct QueueHandles {
    last_id: u64,
    queues: BTreeMap<u64, Queue>,
    terminated: BTreeSet<u64>,
    pending: BTreeMap<u64, VecDeque<ScheduledCallback>>,
}

#[derive(Clone, Copy)]
struct Queue {
    id: u64,
    ports: [u64; 2],
}

struct ScheduledCallback {
    owner: u64,
    ready_at: Instant,
    ready_ticks: Option<u64>,
    callback: GuestCallback,
}

impl QueueHandles {
    /// Task-queue handles the guest has open.
    pub(super) fn open_count(&self) -> usize {
        self.queues.len()
    }

    /// Process exit: every handle is closed and nothing stays scheduled.
    pub(super) fn close_all(&mut self) {
        self.queues.clear();
        self.terminated.clear();
        self.pending.clear();
    }

    fn open(
        &mut self,
        duplicate: Option<Queue>,
        composite: Option<[u64; 2]>,
        output: u64,
        memory: &mut GuestMemory,
    ) -> Result<(), u32> {
        if output == 0 {
            return Err(ERROR_INVALID_PARAMETER);
        }
        if self.queues.len() == MAX_HANDLES {
            return Err(ERROR_OUTOFMEMORY);
        }
        let next = self
            .last_id
            .checked_add(1)
            .filter(|id| *id < (1 << 48))
            .ok_or(ERROR_OUTOFMEMORY)?;
        let handle = 0x5144_0000_0000_0000 | next;
        memory
            .write_u64(output, handle)
            .map_err(|_| ERROR_INVALID_PARAMETER)?;
        self.last_id = next;
        self.queues.insert(
            handle,
            duplicate.unwrap_or(Queue {
                id: handle,
                ports: composite
                    .unwrap_or([0x5157_0000_0000_0000 | next, 0x5143_0000_0000_0000 | next]),
            }),
        );
        Ok(())
    }

    fn close(&mut self, handle: u64) {
        if let Some(queue) = self.queues.remove(&handle) {
            if !self.queues.values().any(|live| live.id == queue.id) {
                self.terminated.remove(&queue.id);
            }
            for port in queue.ports {
                if !self.queues.values().any(|live| live.ports.contains(&port)) {
                    self.pending.remove(&port);
                }
            }
        }
    }

    fn submit(
        &mut self,
        handle: u64,
        selector: u32,
        mut callback: ScheduledCallback,
    ) -> Result<(), u32> {
        let queue = self.queues.get(&handle).ok_or(ERROR_INVALID_HANDLE)?;
        callback.owner = queue.id;
        let port = queue
            .ports
            .get(selector as usize)
            .ok_or(ERROR_INVALID_PARAMETER)?;
        let queue = self.pending.entry(*port).or_default();
        if queue.len() >= MAX_CALLBACKS_PER_PORT {
            return Err(ERROR_OUTOFMEMORY);
        }
        let index = queue.partition_point(|item| match (item.ready_ticks, callback.ready_ticks) {
            (Some(item), Some(callback)) => item <= callback,
            _ => item.ready_at <= callback.ready_at,
        });
        queue.insert(index, callback);
        Ok(())
    }
}

enum DispatchPoll {
    Ready(GuestCallback),
    Wait {
        wake_at: Option<Instant>,
        wake_ticks: Option<u64>,
    },
    Done,
}

/// Poll one manual queue port. Termination does not skip the queue's own
/// cancelled callbacks; those are delivered before the dispatch reports false.
fn poll_dispatch(
    queues: &mut QueueHandles,
    handle: u64,
    selector: u32,
    memory: &GuestMemory,
    clock_ticks: Option<u64>,
) -> Result<DispatchPoll, ExitReason> {
    let Some(&queue) = queues.queues.get(&handle) else {
        return Ok(DispatchPoll::Done);
    };
    let Some(&port) = queue.ports.get(selector as usize) else {
        return Ok(DispatchPoll::Done);
    };
    let terminated = queues.terminated.contains(&queue.id);
    let now = Instant::now();
    let mut wake_at = None;
    let mut wake_ticks = None;
    if let Some(pending) = queues.pending.get_mut(&port)
        && let Some((index, item)) = pending
            .iter()
            .enumerate()
            .find(|(_, item)| !terminated || item.owner == queue.id)
    {
        let ready = match (clock_ticks, item.ready_ticks) {
            (Some(now), Some(ready)) => ready <= now,
            _ => item.ready_at <= now,
        };
        if ready {
            memory
                .fetch_instruction_byte(item.callback.entry)
                .map_err(ExitReason::MemoryFault)?;
            let callback = pending.remove(index).expect("polled callback").callback;
            return Ok(DispatchPoll::Ready(callback));
        }
        wake_at = Some(item.ready_at);
        wake_ticks = item.ready_ticks;
    }
    if terminated {
        Ok(DispatchPoll::Done)
    } else {
        Ok(DispatchPoll::Wait {
            wake_at,
            wake_ticks,
        })
    }
}

fn clear_task_wait(state: &ServicesState, teb: u64) {
    let mut table = state.threads.lock().expect("guest thread table");
    if matches!(table.waits.get(&teb), Some(Pending::TaskQueue { .. })) {
        table.waits.remove(&teb);
    }
}

fn existing_task_wait(state: &ServicesState, teb: u64) -> Option<Option<u64>> {
    let table = state.threads.lock().expect("guest thread table");
    match table.waits.get(&teb) {
        Some(Pending::TaskQueue {
            timeout_deadline, ..
        }) => Some(*timeout_deadline),
        _ => None,
    }
}

fn initial_timeout_deadline(state: &ServicesState, timeout: u32) -> Option<u64> {
    (timeout != INFINITE).then(|| {
        state
            .clock
            .performance_count()
            .saturating_add(u64::from(timeout).saturating_mul(TICKS_PER_MILLI))
    })
}

fn update_task_wait(
    state: &ServicesState,
    teb: u64,
    timeout_deadline: Option<u64>,
    callback_deadline: Option<u64>,
) {
    let deadline = match (timeout_deadline, callback_deadline) {
        (Some(timeout), Some(callback)) => Some(timeout.min(callback)),
        (timeout, None) => timeout,
        (None, callback) => callback,
    };
    state
        .threads
        .lock()
        .expect("guest thread table")
        .waits
        .insert(
            teb,
            Pending::TaskQueue {
                timeout_deadline,
                deadline,
            },
        );
}

/// Scheduler-backed dispatch: the service call stays blocked on its trampoline,
/// so other guest threads continue to run and can submit work or terminate the
/// queue. The queue is polled again on every scheduler retry.
fn dispatch_scheduled(
    state: &ServicesState,
    args: &Win64Args,
    teb: u64,
    timeout: u32,
    memory: &GuestMemory,
) -> Result<Option<GuestCallback>, ExitReason> {
    // A retry whose deadline has passed is over before the queue is polled:
    // work submitted after that deadline stays queued, matching the scheduler's
    // timed-wait ordering rule.
    let prior_wait = existing_task_wait(state, teb);
    if prior_wait.is_some_and(|deadline| {
        deadline.is_some_and(|deadline| state.clock.performance_count() >= deadline)
    }) {
        clear_task_wait(state, teb);
        return Ok(None);
    }
    let poll = {
        let mut queues = state.gdk_queues.lock().expect("GDK queue handle table");
        poll_dispatch(
            &mut queues,
            args.rcx,
            args.rdx as u32,
            memory,
            Some(state.clock.performance_count()),
        )?
    };
    match poll {
        DispatchPoll::Ready(callback) => {
            clear_task_wait(state, teb);
            Ok(Some(callback))
        }
        DispatchPoll::Done => {
            clear_task_wait(state, teb);
            Ok(None)
        }
        DispatchPoll::Wait { .. } if timeout == 0 => Ok(None),
        DispatchPoll::Wait { wake_ticks, .. } => {
            let timeout_deadline =
                prior_wait.unwrap_or_else(|| initial_timeout_deadline(state, timeout));
            update_task_wait(state, teb, timeout_deadline, wake_ticks);
            Err(ExitReason::GuestBlocked)
        }
    }
}

/// Direct service call without a scheduler. This is the only place a host wait
/// is appropriate: there are no other guest threads that a host sleep could
/// starve. INFINITE waits for a queue change, with timed-out delayed callbacks
/// as the only finite self-wake.
fn dispatch_host(
    state: &ServicesState,
    args: &Win64Args,
    timeout: u32,
    memory: &GuestMemory,
) -> Result<Option<GuestCallback>, ExitReason> {
    let deadline = if timeout == INFINITE {
        None
    } else {
        Instant::now().checked_add(Duration::from_millis(u64::from(timeout)))
    };
    let mut queues = state.gdk_queues.lock().expect("GDK queue handle table");
    loop {
        let poll = poll_dispatch(&mut queues, args.rcx, args.rdx as u32, memory, None)?;
        match poll {
            DispatchPoll::Ready(callback) => return Ok(Some(callback)),
            DispatchPoll::Done => return Ok(None),
            DispatchPoll::Wait { wake_at, .. } => {
                if timeout == 0 {
                    return Ok(None);
                }
                let now = Instant::now();
                if deadline.is_some_and(|deadline| now >= deadline) {
                    return Ok(None);
                }
                let wake_at = match (deadline, wake_at) {
                    (Some(deadline), Some(callback)) => Some(deadline.min(callback)),
                    (Some(deadline), None) => Some(deadline),
                    (None, callback) => callback,
                };
                if let Some(wake_at) = wake_at {
                    let remaining = wake_at.saturating_duration_since(now);
                    if remaining.is_zero() {
                        continue;
                    }
                    (queues, _) = state
                        .gdk_queue_changed
                        .wait_timeout(queues, remaining)
                        .expect("GDK queue handle table");
                } else {
                    queues = state
                        .gdk_queue_changed
                        .wait(queues)
                        .expect("GDK queue handle table");
                }
            }
        }
    }
}

fn guest_hresult(result: Result<(), u32>) -> u64 {
    match result {
        Ok(()) => HResult::OK.guest_return(),
        Err(error) => HResult::from_win32(error).guest_return(),
    }
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let create = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XTaskQueueCreate",
        ServiceStatus::Partial,
        move |args, _, memory| {
            if args.rcx as u32 != MANUAL || args.rdx as u32 != MANUAL {
                return Ok(guest_hresult(Err(ERROR_NOT_SUPPORTED)));
            }
            let mut queues = create.gdk_queues.lock().expect("GDK queue handle table");
            Ok(guest_hresult(queues.open(None, None, args.r8, memory)))
        },
    );

    let duplicate = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XTaskQueueDuplicateHandle",
        ServiceStatus::Partial,
        move |args, _, memory| {
            let mut queues = duplicate.gdk_queues.lock().expect("GDK queue handle table");
            let Some(&object) = queues.queues.get(&args.rcx) else {
                return Ok(guest_hresult(Err(ERROR_INVALID_HANDLE)));
            };
            Ok(guest_hresult(queues.open(
                Some(object),
                None,
                args.rdx,
                memory,
            )))
        },
    );

    let close = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XTaskQueueCloseHandle",
        ServiceStatus::Partial,
        move |args, _, _| {
            close
                .gdk_queues
                .lock()
                .expect("GDK queue handle table")
                .close(args.rcx);
            close.gdk_queue_changed.notify_all();
            Ok(0)
        },
    );

    let get = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XTaskQueueGetPort",
        ServiceStatus::Partial,
        move |args, _, memory| {
            let result = (|| {
                let queues = get.gdk_queues.lock().expect("GDK queue handle table");
                let ports = queues.queues.get(&args.rcx).ok_or(ERROR_INVALID_HANDLE)?;
                let port = ports
                    .ports
                    .get(args.rdx as u32 as usize)
                    .ok_or(ERROR_INVALID_PARAMETER)?;
                if args.r8 == 0 {
                    return Err(ERROR_INVALID_PARAMETER);
                }
                memory
                    .write_u64(args.r8, *port)
                    .map_err(|_| ERROR_INVALID_PARAMETER)
            })();
            Ok(guest_hresult(result))
        },
    );

    let composite = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XTaskQueueCreateComposite",
        ServiceStatus::Partial,
        move |args, _, memory| {
            let mut queues = composite.gdk_queues.lock().expect("GDK queue handle table");
            let ports = [args.rcx, args.rdx];
            if ports
                .iter()
                .any(|port| !queues.queues.values().any(|live| live.ports.contains(port)))
            {
                return Ok(guest_hresult(Err(ERROR_INVALID_HANDLE)));
            }
            Ok(guest_hresult(queues.open(
                None,
                Some(ports),
                args.r8,
                memory,
            )))
        },
    );

    for (symbol, delayed) in [
        ("XTaskQueueSubmitCallback", false),
        ("XTaskQueueSubmitDelayedCallback", true),
    ] {
        let submit = Arc::clone(state);
        registry.register_with_status(
            MODULE,
            symbol,
            ServiceStatus::Partial,
            move |args, _, memory| {
                let (delay, context, entry) = if delayed {
                    let Ok(entry) = args.get(4, memory) else {
                        return Ok(guest_hresult(Err(ERROR_INVALID_PARAMETER)));
                    };
                    (args.r8 as u32, args.r9, entry)
                } else {
                    (0, args.r8, args.r9)
                };
                if entry == 0 || memory.fetch_instruction_byte(entry).is_err() {
                    return Ok(guest_hresult(Err(ERROR_INVALID_PARAMETER)));
                }
                let mut queues = submit.gdk_queues.lock().expect("GDK queue handle table");
                if queues
                    .queues
                    .get(&args.rcx)
                    .is_some_and(|queue| queues.terminated.contains(&queue.id))
                {
                    return Ok(E_ABORT);
                }
                let ready_ticks = submit.hooks.is_attached().then(|| {
                    submit
                        .clock
                        .performance_count()
                        .saturating_add(u64::from(delay).saturating_mul(TICKS_PER_MILLI))
                });
                let callback = ScheduledCallback {
                    owner: 0,
                    ready_at: Instant::now() + Duration::from_millis(u64::from(delay)),
                    ready_ticks,
                    callback: GuestCallback {
                        entry,
                        context,
                        cancelled: false,
                    },
                };
                let result = queues.submit(args.rcx, args.rdx as u32, callback);
                if result.is_ok() {
                    submit.gdk_queue_changed.notify_all();
                }
                Ok(guest_hresult(result))
            },
        );
    }
    let terminate = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XTaskQueueTerminate",
        ServiceStatus::Partial,
        move |args, _, _| {
            if args.rdx as u8 != 0 || args.r9 != 0 {
                return Ok(guest_hresult(Err(ERROR_NOT_SUPPORTED)));
            }
            let mut queues = terminate.gdk_queues.lock().expect("GDK queue handle table");
            let Some(&queue) = queues.queues.get(&args.rcx) else {
                return Ok(guest_hresult(Err(ERROR_INVALID_HANDLE)));
            };
            if queues.terminated.insert(queue.id) {
                let now = Instant::now();
                let now_ticks = terminate
                    .hooks
                    .is_attached()
                    .then(|| terminate.clock.performance_count());
                for pending in queues.pending.values_mut() {
                    for item in pending.iter_mut().filter(|item| item.owner == queue.id) {
                        item.ready_at = item.ready_at.min(now);
                        if let (Some(ready), Some(now)) = (item.ready_ticks, now_ticks) {
                            item.ready_ticks = Some(ready.min(now));
                        }
                        item.callback.cancelled = true;
                    }
                    pending.make_contiguous().sort_by_key(|item| item.ready_at);
                }
                terminate.gdk_queue_changed.notify_all();
            }
            Ok(0)
        },
    );
    let dispatch = Arc::clone(state);
    registry.register_callback_dispatch(MODULE, "XTaskQueueDispatch", move |args, teb, memory| {
        let timeout = args.r8 as u32;
        let scheduled = dispatch
            .threads
            .lock()
            .expect("guest thread table")
            .id_of(teb)
            .is_some();
        if scheduled {
            dispatch_scheduled(&dispatch, args, teb, timeout, memory)
        } else {
            dispatch_host(&dispatch, args, timeout, memory)
        }
    });
}
