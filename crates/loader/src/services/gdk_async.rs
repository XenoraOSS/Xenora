// SPDX-License-Identifier: GPL-3.0-or-later

//! Documented XAsync helpers on top of the manual GDK task-queue subset.
//!
//! With a null queue in the `XAsyncBlock`, `XAsyncRun` runs work and optional
//! completion on the calling thread before it returns (immediate dispatch).
//! With a non-null manual `XTaskQueue` handle, work is submitted to the work
//! port and completion to the completion port; the caller must
//! `XTaskQueueDispatch` those ports. `XAsyncGetStatus` with `wait=true` is
//! unsupported either way.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::imports::{
    CallbackStart, CallbackStep, CallbackTask, GuestCallback, ImportRegistry, ServiceStatus,
    Win64Args,
};
use crate::memory::{GuestMemory, MemoryError};
use crate::runtime::HResult;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const MODULE: &str = "xgameruntime.dll";
const ASYNC_BLOCK_SIZE: u64 = 56;
const E_PENDING: u64 = 0x8000_000A;
const E_ABORT: u64 = 0x8000_4004;
const E_INVALIDARG: u64 = 0x8007_0057;
const WORK_PORT: u32 = 0;
const COMPLETION_PORT: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Pending,
    Done(u32),
}

#[derive(Default)]
pub(super) struct AsyncOps {
    ops: BTreeMap<u64, Status>,
}

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

fn guest_hresult(code: u32) -> u64 {
    HResult::from_win32(code).guest_return()
}

struct AsyncRun {
    block: u64,
    completion: u64,
    /// Non-zero when work was queued; completion is submitted to this handle.
    queue: u64,
    ops: Arc<Mutex<AsyncOps>>,
    state: Arc<ServicesState>,
    phase: Phase,
}

enum Phase {
    Work,
    Complete,
}

impl CallbackTask for AsyncRun {
    fn resume(
        &mut self,
        result: u64,
        _: u64,
        _: &mut GuestMemory,
    ) -> Result<CallbackStep, ExitReason> {
        match self.phase {
            Phase::Work => {
                let status = result as u32;
                self.ops
                    .lock()
                    .expect("async ops")
                    .ops
                    .insert(self.block, Status::Done(status));
                if self.completion == 0 {
                    // Immediate path returns S_OK to XAsyncRun; queued path
                    // returns TRUE to XTaskQueueDispatch.
                    let value = if self.queue == 0 {
                        HResult::OK.guest_return()
                    } else {
                        1
                    };
                    return Ok(CallbackStep::Return(value));
                }
                if self.queue == 0 {
                    self.phase = Phase::Complete;
                    return Ok(CallbackStep::Call {
                        entry: self.completion,
                        args: [self.block, 0, 0, 0],
                    });
                }
                // Queue completion onto the completion port; the guest must
                // dispatch that port separately.
                let mut queues = self
                    .state
                    .gdk_queues
                    .lock()
                    .expect("GDK queue handle table");
                let callback = super::gdk_queue::scheduled(
                    Instant::now(),
                    GuestCallback {
                        entry: self.completion,
                        context: self.block,
                        cancelled: false,
                        task: None,
                    },
                );
                if queues
                    .submit(self.queue, COMPLETION_PORT, callback)
                    .is_err()
                {
                    return Ok(CallbackStep::Return(0));
                }
                self.state.gdk_queue_changed.notify_all();
                Ok(CallbackStep::Return(1))
            }
            Phase::Complete => Ok(CallbackStep::Return(HResult::OK.guest_return())),
        }
    }
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let run = Arc::clone(state);
    registry.register_callback_task(
        MODULE,
        "XAsyncRun",
        ServiceStatus::Partial,
        move |args, _, memory, _| start_run(&run, args, memory),
    );

    let status = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XAsyncGetStatus",
        ServiceStatus::Partial,
        move |args, _, memory| {
            let block = args.rcx;
            if block == 0
                || memory
                    .check_write(block, ASYNC_BLOCK_SIZE as usize)
                    .is_err()
            {
                return Ok(E_INVALIDARG);
            }
            if args.rdx != 0 {
                return Ok(guest_hresult(50)); // ERROR_NOT_SUPPORTED as HRESULT
            }
            let ops = status.gdk_async.lock().expect("async ops");
            Ok(match ops.ops.get(&block) {
                Some(Status::Pending) => E_PENDING,
                Some(Status::Done(code)) => u64::from(*code),
                None => E_INVALIDARG,
            })
        },
    );

    let cancel = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XAsyncCancel",
        ServiceStatus::Partial,
        move |args, _, memory| {
            let block = args.rcx;
            if block == 0
                || memory
                    .check_write(block, ASYNC_BLOCK_SIZE as usize)
                    .is_err()
            {
                return Ok(0);
            }
            let mut ops = cancel.gdk_async.lock().expect("async ops");
            if let Some(Status::Pending) = ops.ops.get(&block) {
                ops.ops.insert(block, Status::Done(E_ABORT as u32));
            }
            Ok(0)
        },
    );

    registry.register_with_status(
        MODULE,
        "XAsyncGetResultSize",
        ServiceStatus::Partial,
        move |args, _, memory| {
            if args.rcx == 0
                || args.rdx == 0
                || memory
                    .check_write(args.rcx, ASYNC_BLOCK_SIZE as usize)
                    .is_err()
            {
                return Ok(E_INVALIDARG);
            }
            memory.write_u64(args.rdx, 0).map_err(fault)?;
            Ok(HResult::OK.guest_return())
        },
    );
}

fn start_run(
    state: &Arc<ServicesState>,
    args: &Win64Args,
    memory: &mut GuestMemory,
) -> Result<CallbackStart, ExitReason> {
    let block = args.rcx;
    let work = args.rdx;
    if block == 0
        || work == 0
        || memory
            .check_write(block, ASYNC_BLOCK_SIZE as usize)
            .is_err()
    {
        return Ok(CallbackStart::Return(E_INVALIDARG));
    }
    memory
        .fetch_instruction_byte(work)
        .map_err(ExitReason::MemoryFault)?;
    let queue = memory.read_u64(block).map_err(fault)?;
    let completion = memory.read_u64(block + 16).map_err(fault)?;
    if completion != 0 {
        memory
            .fetch_instruction_byte(completion)
            .map_err(ExitReason::MemoryFault)?;
    }
    {
        let mut ops = state.gdk_async.lock().expect("async ops");
        if matches!(ops.ops.get(&block), Some(Status::Pending)) {
            return Ok(CallbackStart::Return(E_INVALIDARG));
        }
        ops.ops.insert(block, Status::Pending);
    }

    let task = AsyncRun {
        block,
        completion,
        queue,
        ops: Arc::clone(&state.gdk_async),
        state: Arc::clone(state),
        phase: Phase::Work,
    };

    if queue == 0 {
        return Ok(CallbackStart::Call {
            task: Box::new(task),
            entry: work,
            args: [block, 0, 0, 0],
        });
    }

    // Queue path: schedule work on the work port and return immediately.
    let mut queues = state.gdk_queues.lock().expect("GDK queue handle table");
    if !queues.has_handle(queue) {
        state
            .gdk_async
            .lock()
            .expect("async ops")
            .ops
            .remove(&block);
        return Ok(CallbackStart::Return(E_INVALIDARG));
    }
    let callback = super::gdk_queue::scheduled(
        Instant::now(),
        GuestCallback {
            entry: work,
            context: block,
            cancelled: false,
            task: Some(Arc::new(Mutex::new(Box::new(task)))),
        },
    );
    if let Err(error) = queues.submit(queue, WORK_PORT, callback) {
        state
            .gdk_async
            .lock()
            .expect("async ops")
            .ops
            .remove(&block);
        return Ok(CallbackStart::Return(guest_hresult(error)));
    }
    state.gdk_queue_changed.notify_all();
    Ok(CallbackStart::Return(HResult::OK.guest_return()))
}
