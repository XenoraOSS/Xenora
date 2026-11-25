// SPDX-License-Identifier: GPL-3.0-or-later

//! Process termination and exit codes.
//!
//! The only process the guest can name is its own, through the
//! current-process pseudo-handle. No service here reaches a host process.

use super::{
    CURRENT_PROCESS, ERROR_INVALID_HANDLE, ERROR_NOACCESS, ERROR_NOT_SUPPORTED, KERNEL32,
    ServicesState,
};
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use std::sync::Arc;

const PROCESS_THREADS_API: &str = "api-ms-win-core-processthreads-l1-1-0.dll";
const UCRT: &str = "ucrtbase.dll";
const CRT_RUNTIME_API: &str = "api-ms-win-crt-runtime-l1-1-0.dll";
/// The exit code `abort` ends the process with (Microsoft Learn, "abort").
const ABORT_STATUS: u32 = 3;
/// GetExitCodeProcess result while the process runs (Microsoft Learn,
/// "GetExitCodeProcess").
const STILL_ACTIVE: u32 = 259;

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    // ExitProcess does not return (processthreadsapi.h). It stops the guest
    // with its status and never ends the host. It is partial: there are no
    // other threads or loaded DLLs to notify.
    let s = Arc::clone(state);
    registry.register(KERNEL32, "ExitProcess", move |args, _, _| {
        // The status is a UINT, the low half of the first argument.
        Err(ExitReason::ProcessExit(s.exit_process(args.rcx as u32)))
    });
    registry.alias(PROCESS_THREADS_API, KERNEL32, "ExitProcess");
    registry.set_status(KERNEL32, "ExitProcess", ServiceStatus::Partial);

    // TerminateProcess on the current-process pseudo-handle ends the guest with
    // that status, as ExitProcess does, and does not return. There is no other
    // process object, so any other handle fails with ERROR_INVALID_HANDLE. It is
    // partial: Windows would not notify DLLs, and neither does this, but there
    // are also no other processes it could end.
    let s = Arc::clone(state);
    registry.register(KERNEL32, "TerminateProcess", move |args, cpu, _| {
        if args.rcx != CURRENT_PROCESS {
            return s.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        }
        // The status is a UINT, the low half of the second argument.
        Err(ExitReason::ProcessExit(s.exit_process(args.rdx as u32)))
    });

    // A running process has no exit code yet, so the guest, which only runs
    // while its process does, always reads STILL_ACTIVE for itself.
    let s = Arc::clone(state);
    registry.register(KERNEL32, "GetExitCodeProcess", move |args, cpu, memory| {
        if args.rcx != CURRENT_PROCESS {
            return s.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        }
        if memory.write_u32(args.rdx, STILL_ACTIVE).is_err() {
            return s.fail(cpu.gs_base, ERROR_NOACCESS, 0);
        }
        Ok(1)
    });

    // CreateProcess cannot exist in this world: the guest is the only
    // process, and starting another would require a second emulated machine
    // that does not exist. Both forms fail honestly with ERROR_NOT_SUPPORTED
    // instead of pretending a child started, which the documentation states.
    for symbol in ["CreateProcessA", "CreateProcessW"] {
        let s = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            // The output structure, when given, is zeroed like a failed call
            // leaves it - programs that check it see no process handles.
            let info = args.get(9, memory).map_err(ExitReason::MemoryFault)?;
            if info != 0 {
                for field in 0..4 {
                    memory
                        .write_u64(info + 8 * field, 0)
                        .map_err(ExitReason::MemoryFault)?;
                }
            }
            s.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0)
        });
        registry.alias(PROCESS_THREADS_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }

    for symbol in ["TerminateProcess", "GetExitCodeProcess"] {
        registry.alias(PROCESS_THREADS_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }

    // _exit ends the process without calling the functions registered with
    // atexit (Microsoft Learn, "exit, _Exit, _exit"). `exit`, which does call
    // them, is beside `_cexit` in crt_callbacks.rs.
    let s = Arc::clone(state);
    registry.register(UCRT, "_exit", move |args, _, _| {
        // The status is an int, the low half of the first argument.
        Err(ExitReason::ProcessExit(s.exit_process(args.rcx as u32)))
    });

    // abort ends the process with exit code 3 and calls no registered
    // function. It is partial: it prints no message, and it does not call a
    // SIGABRT handler, which the documentation says it does when one is set.
    let s = Arc::clone(state);
    registry.register(UCRT, "abort", move |_, _, _| {
        Err(ExitReason::ProcessExit(s.exit_process(ABORT_STATUS)))
    });

    for symbol in ["_exit", "abort"] {
        registry.alias(CRT_RUNTIME_API, UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
}
