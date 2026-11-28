// SPDX-License-Identifier: GPL-3.0-or-later

//! The XAsync provider protocol core (`xgameruntime.dll`), as Microsoft Learn
//! documents it in XAsync.h and XAsyncProvider.h. Every GDK asynchronous API
//! is specified in terms of the `XAsyncBlock` lifetime and the provider
//! operation codes; these services are the protocol those APIs speak.
//!
//! Layout evidence, all from the public Microsoft Learn reference pages:
//!
//! - `XAsyncBlock` is 56 bytes: the queue handle at 0, the caller's context
//!   at 8, the completion callback at 16, and 32 bytes of `internal` space
//!   at 24 reserved for the implementation. The provider data block handed
//!   to the provider lives in that internal space: it is the
//!   implementation's to use, it is exactly the 32 bytes
//!   `XAsyncProviderData` needs, and the caller may never look at it.
//! - `XAsyncProviderData` is `async` at 0, `bufferSize` at 8, `buffer` at 16
//!   and `context` at 24.
//! - `XAsyncOp` numbers `Begin` 0, `DoWork` 1, `GetResult` 2, `Cancel` 3
//!   and `Cleanup` 4 in declaration order.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::imports::{
    CallbackStart, CallbackStep, CallbackTask, ImportRegistry, ServiceStatus, Win64Args,
};
use crate::memory::{GuestMemory, MemoryError};
use crate::runtime::HResult;
use std::collections::BTreeMap;
use std::sync::Arc;

const MODULE: &str = "xgameruntime.dll";

/// The operation codes of the provider callback, in their documented order.
const OP_BEGIN: u64 = 0;
const OP_DO_WORK: u64 = 1;
const OP_GET_RESULT: u64 = 2;
const OP_CANCEL: u64 = 3;
const OP_CLEANUP: u64 = 4;

/// `E_PENDING`: the operation keeps running. Standard HRESULT.
const E_PENDING: u64 = 0x8000_000A;
/// `E_INVALIDARG` of an identity mismatch, HRESULT_FROM_WIN32 as the
/// XAsyncGetResult page documents the mismatch failure.
const E_INVALIDARG: u64 = 0x8007_0057;
/// The `E_NOT_SUPPORTED` the XAsyncGetResult page names for a payload-less
/// operation: HRESULT_FROM_WIN32 of ERROR_NOT_SUPPORTED (50), the reading
/// the name itself documents. No public page nails the GDK's exact constant.
const E_NOT_SUPPORTED: u64 = 0x8007_0032;
/// The `E_NO_TASK_QUEUE` the XAsyncBlock page names when neither the block
/// nor the process carries a queue: 0x800701AB as independent public
/// reconstructions of the GDK headers record it; the Microsoft pages name
/// the constant without printing its value.
const E_NO_TASK_QUEUE: u64 = 0x8007_01AB;

/// The `XAsyncBlock` field offsets, per the documented layout.
const BLOCK_CALLBACK: u64 = 16;
/// The `internal` space the provider data block is written into.
const BLOCK_INTERNAL: u64 = 24;

/// One in-flight asynchronous operation, keyed in the table by the guest
/// address of its `XAsyncBlock`. The block must outlive the call as the
/// documentation demands, so the address is a stable key.
pub(super) struct AsyncOp {
    provider: u64,
    context: u64,
    identity: u64,
    /// The final result once the operation completed; pending while None.
    result: Option<u64>,
    result_size: u64,
    queue: u64,
}

#[derive(Default)]
pub(super) struct AsyncOperations {
    ops: BTreeMap<u64, AsyncOp>,
}

impl AsyncOperations {
    /// Process exit ends every operation the dead process tracked: the
    /// guest memory the blocks live in dies with it, so nothing here can
    /// ever complete again.
    pub(super) fn clear(&mut self) {
        self.ops.clear();
    }
}

fn block_field(block: u64, offset: u64) -> Result<u64, ExitReason> {
    block
        .checked_add(offset)
        .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))
}

/// Write the provider data block into the operation's internal space:
/// `async` is the block itself, `context` never changes after begin, and
/// the buffer fields are meaningful only for a GetResult invocation.
fn write_provider_data(
    memory: &mut GuestMemory,
    block: u64,
    buffer_size: u64,
    buffer: u64,
    context: u64,
) -> Result<u64, ExitReason> {
    let address = block_field(block, BLOCK_INTERNAL)?;
    let mut data = [0; 32];
    let (fields, _) = data.as_chunks_mut::<8>();
    for (slot, value) in fields.iter_mut().zip([block, buffer_size, buffer, context]) {
        *slot = value.to_le_bytes();
    }
    // GuestMemory validates the entire span before modifying any bytes.
    memory
        .write_bytes(address, &data)
        .map_err(ExitReason::MemoryFault)?;
    Ok(address)
}

/// The queue an operation schedules on: the block's own when it names one,
/// else the process queue, else the `E_NO_TASK_QUEUE` the documentation
/// promises. Validates the handle exists as a queue.
fn resolve_queue(state: &ServicesState, block_queue: u64) -> Result<u64, u64> {
    let queues = state.gdk_queues.lock().expect("GDK queue handle table");
    if block_queue != 0 {
        return if queues.contains(block_queue) {
            Ok(block_queue)
        } else {
            Err(E_INVALIDARG)
        };
    }
    let process = queues.process_queue();
    if process != 0 && queues.contains(process) {
        Ok(process)
    } else {
        Err(E_NO_TASK_QUEUE)
    }
}

struct BeginTask {
    state: Arc<ServicesState>,
}

impl CallbackTask for BeginTask {
    fn resume(
        &mut self,
        _result: u64,
        _teb: u64,
        _memory: &mut GuestMemory,
    ) -> Result<CallbackStep, ExitReason> {
        // The provider's Begin return is the provider's own business: work
        // failures surface through the completion result, as the protocol's
        // error paths all do. XAsyncBegin answers for the setup alone.
        let _ = &self.state;
        Ok(CallbackStep::Return(HResult::OK.guest_return()))
    }
}

fn xasync_begin_start(
    state: &Arc<ServicesState>,
    args: &Win64Args,
    memory: &mut GuestMemory,
) -> Result<CallbackStart, ExitReason> {
    let block = args.rcx;
    let context = args.rdx;
    let identity = args.r8;
    let provider = args.get(4, memory).map_err(ExitReason::MemoryFault)?;
    if block == 0 || provider == 0 || memory.fetch_instruction_byte(provider).is_err() {
        return Ok(CallbackStart::Return(E_INVALIDARG));
    }
    let mut header = [[0; 8]; BLOCK_INTERNAL as usize / 8];
    memory
        .read_bytes(block, header.as_flattened_mut())
        .map_err(ExitReason::MemoryFault)?;
    let block_queue = u64::from_le_bytes(header[0]);
    let queue = match resolve_queue(state, block_queue) {
        Ok(queue) => queue,
        Err(error) => return Ok(CallbackStart::Return(error)),
    };
    let data = write_provider_data(memory, block, 0, 0, context)?;
    {
        let mut ops = state.xasync_ops.lock().expect("XAsync operation table");
        ops.ops.insert(
            block,
            AsyncOp {
                provider,
                context,
                identity,
                result: None,
                result_size: 0,
                queue,
            },
        );
    }
    Ok(CallbackStart::Call {
        task: Box::new(BeginTask {
            state: Arc::clone(state),
        }),
        entry: provider,
        args: [OP_BEGIN, data, 0, 0],
    })
}

/// A single synchronous provider invocation whose answer is fixed: cancel
/// is void and the provider's own return is its own business, exactly as
/// the begin invocation's is.
struct ProviderCallTask {
    answer: u64,
}

impl CallbackTask for ProviderCallTask {
    fn resume(
        &mut self,
        _result: u64,
        _teb: u64,
        _memory: &mut GuestMemory,
    ) -> Result<CallbackStep, ExitReason> {
        Ok(CallbackStep::Return(self.answer))
    }
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let begin = Arc::clone(state);
    registry.register_callback_task(
        MODULE,
        "XAsyncBegin",
        ServiceStatus::Partial,
        move |args, _teb, memory, _scratch| xasync_begin_start(&begin, args, memory),
    );

    // XAsyncSchedule queues a DoWork provider invocation on the operation's
    // work port. The delay is the same millisecond delay the delayed task
    // queue submission models.
    let schedule = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XAsyncSchedule",
        ServiceStatus::Partial,
        move |args, _cpu, _memory| {
            let block = args.rcx;
            let delay = args.rdx as u32;
            let (provider, queue) = {
                let ops = schedule.xasync_ops.lock().expect("XAsync operation table");
                match ops.ops.get(&block) {
                    Some(op) if op.result.is_none() => (op.provider, op.queue),
                    _ => return Ok(E_INVALIDARG),
                }
            };
            let data = block_field(block, BLOCK_INTERNAL)?;
            let mut queues = schedule.gdk_queues.lock().expect("GDK queue handle table");
            match queues.schedule_provider(block, queue, provider, data, OP_DO_WORK, delay) {
                Ok(()) => {
                    schedule.gdk_queue_changed.notify_all();
                    Ok(HResult::OK.guest_return())
                }
                Err(error) => Ok(guest_hresult_err(error)),
            }
        },
    );

    // XAsyncComplete: E_PENDING keeps the operation running; anything else
    // completes it, and a completion callback on the block is queued on the
    // completion port with the block as its argument. The buffer size is
    // recorded for a success and ignored for an error, as the page states.
    let complete = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XAsyncComplete",
        ServiceStatus::Partial,
        move |args, _cpu, memory| {
            let block = args.rcx;
            let result = args.rdx & 0xffff_ffff;
            let size = args.r8;
            if result == E_PENDING {
                return Ok(0);
            }
            let callback = memory
                .read_u64(block_field(block, BLOCK_CALLBACK)?)
                .map_err(ExitReason::MemoryFault)?;
            let queue = {
                let mut ops = complete.xasync_ops.lock().expect("XAsync operation table");
                let Some(op) = ops.ops.get_mut(&block) else {
                    return Ok(0);
                };
                if op.result.is_some() {
                    return Ok(0);
                }
                op.result = Some(result);
                if (result as u32 as i32) >= 0 {
                    op.result_size = size;
                }
                op.queue
            };
            if callback != 0 && memory.fetch_instruction_byte(callback).is_ok() {
                let mut queues = complete.gdk_queues.lock().expect("GDK queue handle table");
                if queues.schedule_completion(block, queue, callback).is_ok() {
                    complete.gdk_queue_changed.notify_all();
                }
            }
            complete.xasync_changed.notify_all();
            complete.hooks.signal();
            Ok(0)
        },
    );

    // XAsyncGetStatus: the pending answer is E_PENDING; once complete, the
    // operation's own result. wait=true blocks the calling thread until the
    // completion lands - with the documented caveat that a manual-queue
    // operation the same thread must pump cannot complete meanwhile, which
    // is the caller's protocol error exactly as on Windows.
    let status = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XAsyncGetStatus",
        ServiceStatus::Partial,
        move |args, _cpu, _memory| {
            let block = args.rcx;
            let wait = args.rdx != 0;
            let mut ops = status.xasync_ops.lock().expect("XAsync operation table");
            loop {
                match ops.ops.get(&block) {
                    None => return Ok(E_INVALIDARG),
                    Some(op) => {
                        if let Some(result) = op.result {
                            return Ok(result);
                        }
                    }
                }
                if !wait {
                    return Ok(E_PENDING);
                }
                ops = status
                    .xasync_changed
                    .wait(ops)
                    .expect("XAsync operation table");
            }
        },
    );

    // XAsyncCancel invokes the provider with the Cancel operation code
    // synchronously; the provider completes the operation with E_ABORT
    // through XAsyncComplete, as the operation page documents.
    let cancel = Arc::clone(state);
    registry.register_callback_task(
        MODULE,
        "XAsyncCancel",
        ServiceStatus::Partial,
        move |args, _teb, memory, _scratch| {
            let block = args.rcx;
            let (provider, context) = {
                let ops = cancel.xasync_ops.lock().expect("XAsync operation table");
                match ops.ops.get(&block) {
                    Some(op) if op.result.is_none() => (op.provider, op.context),
                    _ => return Ok(CallbackStart::Return(0)),
                }
            };
            let data = write_provider_data(memory, block, 0, 0, context)?;
            Ok(CallbackStart::Call {
                task: Box::new(ProviderCallTask { answer: 0 }),
                entry: provider,
                args: [OP_CANCEL, data, 0, 0],
            })
        },
    );

    // XAsyncGetResultSize answers the payload size the completion recorded,
    // as the caller sizes its retrieval buffer by it before asking.
    let result_size = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "XAsyncGetResultSize",
        ServiceStatus::Partial,
        move |args, _cpu, memory| {
            let block = args.rcx;
            let output = args.rdx;
            let (result, size) = {
                let ops = result_size
                    .xasync_ops
                    .lock()
                    .expect("XAsync operation table");
                let Some(op) = ops.ops.get(&block) else {
                    return Ok(E_INVALIDARG);
                };
                match op.result {
                    None => return Ok(E_PENDING),
                    Some(result) => (result, op.result_size),
                }
            };
            if (result as u32 as i32) < 0 {
                return Ok(result);
            }
            memory
                .write_u64(output, size)
                .map_err(ExitReason::MemoryFault)?;
            Ok(HResult::OK.guest_return())
        },
    );

    // XAsyncGetResult: a completed operation with a payload only. The
    // identity must match the one begin recorded when either names one, as
    // the page's mismatch contract states; the provider answers through its
    // own HRESULT, and bufferUsed is the size the completion recorded. A
    // failure result returns without invoking the provider: there is no
    // payload of a failed operation to produce. The operation ends here -
    // the block is no longer associated with the call, as the page states -
    // so the provider's Cleanup runs as the operation's last act.
    let get_result = Arc::clone(state);
    registry.register_callback_task(
        MODULE,
        "XAsyncGetResult",
        ServiceStatus::Partial,
        move |args, _teb, memory, _scratch| {
            let block = args.rcx;
            let identity = args.rdx;
            let buffer_size = args.r8;
            let buffer = args.r9;
            let buffer_used = args.get(4, memory).map_err(ExitReason::MemoryFault)?;
            let (provider, context, result, result_size) = {
                let ops = get_result
                    .xasync_ops
                    .lock()
                    .expect("XAsync operation table");
                let Some(op) = ops.ops.get(&block) else {
                    return Ok(CallbackStart::Return(E_INVALIDARG));
                };
                if op.identity != 0 && identity != op.identity {
                    return Ok(CallbackStart::Return(E_INVALIDARG));
                }
                let Some(result) = op.result else {
                    return Ok(CallbackStart::Return(E_PENDING));
                };
                if (result as u32 as i32) < 0 {
                    return Ok(CallbackStart::Return(result));
                }
                if op.result_size == 0 {
                    return Ok(CallbackStart::Return(E_NOT_SUPPORTED));
                }
                (op.provider, op.context, result, op.result_size)
            };
            if buffer_used != 0 {
                memory
                    .write_u64(buffer_used, result_size)
                    .map_err(ExitReason::MemoryFault)?;
            }
            let _ = result;
            let data = write_provider_data(memory, block, buffer_size, buffer, context)?;
            Ok(CallbackStart::Call {
                task: Box::new(GetResultTask {
                    block,
                    provider,
                    context,
                    state: Arc::clone(&get_result),
                    step: 0,
                    answer: 0,
                }),
                entry: provider,
                args: [OP_GET_RESULT, data, 0, 0],
            })
        },
    );
}

/// GetResult is the operation's last interaction: the provider answers with
/// its own HRESULT, a successful retrieval continues to the Cleanup
/// invocation, and the operation leaves the table as the block ceases to be
/// associated with the call. The task carries its own step because the
/// callback machinery resumes the same task after each invocation.
struct GetResultTask {
    block: u64,
    provider: u64,
    context: u64,
    state: Arc<ServicesState>,
    step: u8,
    answer: u64,
}

impl CallbackTask for GetResultTask {
    fn resume(
        &mut self,
        result: u64,
        _teb: u64,
        memory: &mut GuestMemory,
    ) -> Result<CallbackStep, ExitReason> {
        if self.step == 0 {
            self.answer = result & 0xffff_ffff;
            if (self.answer as u32 as i32) < 0 {
                return Ok(CallbackStep::Return(self.answer));
            }
            // Cleanup is the operation's final provider invocation.
            let data = write_provider_data(memory, self.block, 0, 0, self.context)?;
            self.step = 1;
            return Ok(CallbackStep::Call {
                entry: self.provider,
                args: [OP_CLEANUP, data, 0, 0],
            });
        }
        self.state
            .xasync_ops
            .lock()
            .expect("XAsync operation table")
            .ops
            .remove(&self.block);
        Ok(CallbackStep::Return(self.answer))
    }
}

fn guest_hresult_err(error: u32) -> u64 {
    HResult::from_win32(error).guest_return()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::CpuState;
    use crate::import_table::ImportName;
    use crate::memory::MemoryPermissions;
    use crate::services::{MockClock, register_standard_services};

    #[test]
    fn complete_refuses_an_overflowing_callback_address() {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, state);
        let complete = registry
            .resolve(MODULE, &ImportName::Name("XAsyncComplete".into()))
            .unwrap();
        let mut cpu = CpuState::new(0, 0);
        let mut args = Win64Args::new(&cpu);
        args.rcx = u64::MAX;
        assert_eq!(
            (complete.handler)(&args, &mut cpu, &mut GuestMemory::new()),
            Err(ExitReason::MemoryFault(MemoryError::Overflow))
        );
    }

    fn begin_fixture() -> (Arc<ServicesState>, GuestMemory, Win64Args, u64) {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, Arc::clone(&state));
        let create_queue = registry
            .resolve(MODULE, &ImportName::Name("XTaskQueueCreate".into()))
            .unwrap();

        let mut memory = GuestMemory::new();
        memory
            .map_region(0x1000, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();
        let provider = 0x3000;
        memory
            .map_region_with_data(provider, 0x1000, MemoryPermissions::READ_EXECUTE, &[0xc3])
            .unwrap();

        let mut cpu = CpuState::new(0, 0x1100);
        let mut args = Win64Args::new(&cpu);
        let queue_slot = 0x1200;
        args.r8 = queue_slot;
        assert_eq!((create_queue.handler)(&args, &mut cpu, &mut memory), Ok(0));
        let queue = memory.read_u64(queue_slot).unwrap();
        memory.write_u64(args.rsp + 40, provider).unwrap();
        args.rdx = 0x55;
        args.r8 = 0;
        (state, memory, args, queue)
    }

    #[test]
    fn begin_does_not_publish_or_write_partially_when_provider_data_is_unmapped() {
        let (state, mut memory, mut args, queue) = begin_fixture();
        let block = 0x1fd8;
        let data = 0x1ff0;
        memory.write_u64(block, queue).unwrap();
        let sentinel = [0xa5; 16];
        memory.write_bytes(data, &sentinel).unwrap();
        args.rcx = block;

        assert!(matches!(
            xasync_begin_start(&state, &args, &mut memory),
            Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(
                0x2000
            )))
        ));
        let mut observed = [0; 16];
        memory.read_bytes(data, &mut observed).unwrap();
        assert_eq!(observed, sentinel);
        assert!(!state.xasync_ops.lock().unwrap().ops.contains_key(&block));

        memory
            .map_region(0x2000, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();
        assert!(matches!(
            xasync_begin_start(&state, &args, &mut memory),
            Ok(CallbackStart::Call {
                entry: 0x3000,
                args: [OP_BEGIN, 0x1ff0, 0, 0],
                ..
            })
        ));
        assert_eq!(memory.read_u64(data).unwrap(), block);
        assert_eq!(memory.read_u64(data + 24).unwrap(), args.rdx);
        assert!(state.xasync_ops.lock().unwrap().ops.contains_key(&block));
    }

    #[test]
    fn begin_validates_the_header_before_writing_provider_data() {
        let (state, mut memory, mut args, queue) = begin_fixture();
        let block = 0x4000;
        let context = 0x4008;
        let data = 0x4018;
        let sentinel = [0xa5; 32];
        args.rcx = block;
        memory
            .map_region_with_data(block, 8, MemoryPermissions::READ_ONLY, &queue.to_le_bytes())
            .unwrap();
        memory
            .map_region_with_data(data, 32, MemoryPermissions::READ_WRITE, &sentinel)
            .unwrap();

        assert!(matches!(
            xasync_begin_start(&state, &args, &mut memory),
            Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(address))) if address == context
        ));
        let mut observed = [0; 32];
        memory.read_bytes(data, &mut observed).unwrap();
        assert_eq!(observed, sentinel);
        assert!(!state.xasync_ops.lock().unwrap().ops.contains_key(&block));

        memory
            .map_region(context, 16, MemoryPermissions::READ_ONLY)
            .unwrap();
        assert!(matches!(
            xasync_begin_start(&state, &args, &mut memory),
            Ok(CallbackStart::Call {
                entry: 0x3000,
                args: [OP_BEGIN, 0x4018, 0, 0],
                ..
            })
        ));
        assert_eq!(memory.read_u64(data).unwrap(), block);
        assert!(state.xasync_ops.lock().unwrap().ops.contains_key(&block));
    }
}
