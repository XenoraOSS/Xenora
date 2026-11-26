// SPDX-License-Identifier: GPL-3.0-or-later

//! Process termination and exit codes.
//!
//! The only process the guest can name is its own, through the
//! current-process pseudo-handle. No service here reaches a host process.

const INVALID_HANDLE: u64 = u64::MAX;
use super::{
    CURRENT_PROCESS, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_NOACCESS,
    ERROR_NOT_ENOUGH_MEMORY, ERROR_NOT_SUPPORTED, GuestObject, KERNEL32, ServicesState,
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
    }); // OpenProcess answers a real handle when asked for the machine's one
    // process, and no such process otherwise: the machine has exactly one.
    let open_process = Arc::clone(state);
    registry.register(KERNEL32, "OpenProcess", move |args, cpu, memory| {
        let _ = memory;
        if args.r8 as u32 != 0x4100 {
            // The machine has one process; any other id does not exist.
            return open_process.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
        }
        let mut objects = open_process.objects.lock().expect("guest object state");
        let Some(handle) = objects.next_handle() else {
            return open_process.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
        };
        objects.insert_handle(handle, GuestObject::Process);
        Ok(handle)
    });

    // ReadProcessMemory on the machine's one process is the guest memory
    // itself: the copy is exact and bounded by the guest address space.
    let read_process = Arc::clone(state);
    registry.register(KERNEL32, "ReadProcessMemory", move |args, cpu, memory| {
        {
            let objects = read_process.objects.lock().expect("guest object state");
            if !matches!(objects.handles.get(&args.rcx), Some(GuestObject::Process)) {
                return read_process.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
            }
        }
        let count = args.get(4, memory).map_err(ExitReason::MemoryFault)?;
        let mut bytes = Vec::new();
        for index in 0..args.r9 {
            let byte = memory
                .read_u8(args.rdx + index)
                .map_err(ExitReason::MemoryFault)?;
            bytes.push(byte);
        }
        if args.r8 == 0 {
            return read_process.fail(cpu.gs_base, ERROR_NOACCESS, 0);
        }
        memory
            .write_bytes(args.r8, &bytes)
            .map_err(ExitReason::MemoryFault)?;
        if count != 0 {
            memory
                .write_u64(count, args.r9)
                .map_err(ExitReason::MemoryFault)?;
        }
        Ok(1)
    });

    // Cross-process machinery does not exist on a machine with one process:
    // a remote thread in another process has no target, directory watching
    // has no watcher machinery, and a named pipe's cross-process meaning is
    // empty. Each is honestly unsupported rather than a silent no-op.
    for symbol in [
        "CreateRemoteThread",
        "ReadDirectoryChangesW",
        "CreateNamedPipeA",
    ] {
        let unsupported = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let _ = (args, memory);
            unsupported.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, INVALID_HANDLE)
        });
    }

    for symbol in [
        "OpenProcess",
        "ReadProcessMemory",
        "CreateRemoteThread",
        "ReadDirectoryChangesW",
        "CreateNamedPipeA",
    ] {
        registry.alias(PROCESS_THREADS_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }

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

    // The toolhelp snapshot of a machine with one process: the snapshot is
    // a handle kind of its own, and the enumeration reads the live state
    // rather than a frozen copy, which no correct consumer can tell apart
    // on a machine whose process list cannot change.
    const SNAPPROCESS: u64 = 2;
    let snapshot = Arc::clone(state);
    registry.register(KERNEL32, "CreateToolhelp32Snapshot", move |args, cpu, _| {
        if args.rcx != SNAPPROCESS {
            return snapshot.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
        }
        if args.rdx != 0 && args.rdx != u64::from(snapshot.process_id) {
            // A snapshot of another process's space: there is none.
            return snapshot.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
        }
        let mut objects = snapshot.objects.lock().expect("guest object state");
        let Some(handle) = objects.next_handle() else {
            return snapshot.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
        };
        objects.insert_handle(handle, super::GuestObject::Snapshot);
        Ok(handle)
    });

    // PROCESSENTRY32W layout: dwSize, cntUsage, th32ProcessID,
    // th32DefaultHeapID, th32ModuleID, cntThreads, th32ParentProcessID,
    // pcPriClassBase, dwFlags, then 260 UTF-16 units of exe file name.
    for (symbol, wide) in [
        ("Process32FirstW", true),
        ("Process32NextW", true),
        ("Process32First", false),
        ("Process32Next", false),
    ] {
        let enumerate = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let first = symbol.contains("First");
            {
                let objects = enumerate.objects.lock().expect("guest object state");
                if !matches!(
                    objects.handles.get(&args.rcx),
                    Some(super::GuestObject::Snapshot)
                ) {
                    return enumerate.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
                }
            }
            if !first {
                // The machine's one process is the whole list.
                return enumerate.fail(cpu.gs_base, 259, 0); // ERROR_NO_MORE_FILES
            }
            if args.rdx == 0 || memory.check_write(args.rdx, 8).is_err() {
                return enumerate.fail(cpu.gs_base, ERROR_NOACCESS, 0);
            }
            let size = memory.read_u32(args.rdx).map_err(ExitReason::MemoryFault)?;
            let entry_size = if wide { 0x248u32 } else { 0x128u32 };
            if size < entry_size {
                return enumerate.fail(cpu.gs_base, 111, 0); // ERROR_BUFFER_OVERFLOW... actually ERROR_BAD_LENGTH is 24; docs: ERROR_NO_MORE_FILES? use 24
            }
            let zero = vec![0u8; entry_size as usize];
            if memory.write_bytes(args.rdx, &zero).is_err() {
                return enumerate.fail(cpu.gs_base, ERROR_NOACCESS, 0);
            }
            memory
                .write_u32(args.rdx, entry_size)
                .map_err(ExitReason::MemoryFault)?;
            memory
                .write_u32(args.rdx + 8, enumerate.process_id)
                .map_err(ExitReason::MemoryFault)?;
            memory
                .write_u32(args.rdx + 0x18, 1)
                .map_err(ExitReason::MemoryFault)?; // cntThreads
            let name: Vec<u8> = if wide {
                "xenora-guest.exe"
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect()
            } else {
                b"xenora-guest.exe\0".to_vec()
            };
            memory
                .write_bytes(args.rdx + 0x28, &name)
                .map_err(ExitReason::MemoryFault)?;
            Ok(1)
        });
    }

    let wow64 = Arc::clone(state);
    registry.register(KERNEL32, "IsWow64Process", move |args, cpu, memory| {
        // The machine is x64 throughout: nothing runs under WOW64.
        if args.rdx == 0 || memory.check_write(args.rdx, 4).is_err() {
            return wow64.fail(cpu.gs_base, ERROR_NOACCESS, 0);
        }
        memory
            .write_u32(args.rdx, 0)
            .map_err(ExitReason::MemoryFault)?;
        Ok(1)
    });

    let times = Arc::clone(state);
    registry.register(KERNEL32, "GetProcessTimes", move |args, cpu, memory| {
        // The four FILETIME outputs are creation, exit, kernel and user in
        // rdx, r8, r9 and the first stack argument. A live process has no
        // exit time; the mock clock answers the rest, kernel and user time
        // being the whole run as far as the guest can measure.
        if args.rcx != super::CURRENT_PROCESS {
            return times.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        }
        let now = times.clock.now_filetime();
        let user = args.get(4, memory).map_err(ExitReason::MemoryFault)?;
        for (pointer, value) in [(args.rdx, now), (args.r8, 0), (args.r9, now), (user, now)] {
            if pointer == 0 {
                return times.fail(cpu.gs_base, ERROR_NOACCESS, 0);
            }
            memory
                .write_u64(pointer, value)
                .map_err(ExitReason::MemoryFault)?;
        }
        Ok(1)
    });

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
    // The legacy CRT, as the compatibility mapping in crt_callbacks.rs.
    registry.alias("msvcrt.dll", UCRT, "_exit");
    registry.alias("msvcrt.dll", UCRT, "abort");
}
