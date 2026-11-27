// SPDX-License-Identifier: GPL-3.0-or-later

//! The user32 surface of a process without windows.
//!
//! The machine is a console host: no window can ever be created, so the
//! message queue is genuinely empty and every window handle is genuinely
//! invalid. That makes the honest answers the correct answers rather than
//! stubs: PeekMessage finds nothing because there is nothing to find,
//! IsWindowVisible is false because the window does not exist. This is
//! documented here rather than hidden, per the no-silent-no-op doctrine:
//! the day windows exist, these handlers must grow real state.

use super::ServicesState;
use crate::imports::{ImportRegistry, ServiceStatus};
use std::sync::Arc;

const MODULE: &str = "user32.dll";

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let _ = state;
    // PeekMessage reports the empty queue honestly: zero is "no message",
    // the correct answer for a process that cannot create windows.
    registry.register_with_status(MODULE, "PeekMessageA", ServiceStatus::Partial, |_, _, _| {
        Ok(0)
    });

    // TranslateMessage and DispatchMessage operate on messages the caller
    // fetched - impossible here - and their documented zero returns for a
    // message needing no translation or having no window procedure are the
    // honest answers.
    registry.register_with_status(
        MODULE,
        "TranslateMessage",
        ServiceStatus::Partial,
        |_, _, _| Ok(0),
    );
    registry.register_with_status(
        MODULE,
        "DispatchMessageA",
        ServiceStatus::Partial,
        |_, _, _| Ok(0),
    );

    // MsgWaitForMultipleObjects waits on object handles and the message
    // queue together. The queue half never fires here - no window can be
    // created, so no message ever arrives - and the wait reduces exactly to
    // the object wait the kernel already models, resume included. MWMO_WAITALL
    // is honored as the wait-all form; MWMO_ALERTABLE is honestly refused,
    // as the alertable kernel waits are. busybox binds the import for its
    // event loops; the wake mask is validated but otherwise means nothing
    // where no input exists.
    let wait_state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "MsgWaitForMultipleObjects",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            use super::events::{WAIT_FAILED, resume_wait, wait_for_objects};
            use super::{ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED};
            use crate::cpu::ExitReason;
            if let Some(result) = resume_wait(&wait_state, cpu.gs_base) {
                return result;
            }
            let count = args.rcx as u32;
            let address = args.rdx;
            let timeout = args.r8 as u32;
            let _wake_mask = args.r9 as u32;
            let flags = args.get(4, memory).map_err(ExitReason::MemoryFault)? as u32;
            const MWMO_WAITALL: u32 = 1;
            const MWMO_ALERTABLE: u32 = 2;
            const MWMO_INPUTAVAILABLE: u32 = 4;
            if flags & !(MWMO_WAITALL | MWMO_ALERTABLE | MWMO_INPUTAVAILABLE) != 0 {
                return wait_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, WAIT_FAILED);
            }
            if flags & MWMO_ALERTABLE != 0 {
                return wait_state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, WAIT_FAILED);
            }
            if !(1..=64).contains(&count) || address == 0 {
                return wait_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, WAIT_FAILED);
            }
            let mut handles = [0; 64];
            let handles = &mut handles[..count as usize];
            for (index, handle) in handles.iter_mut().enumerate() {
                let slot = address
                    .checked_add(index as u64 * 8)
                    .ok_or(ExitReason::MemoryFault(
                        crate::memory::MemoryError::Overflow,
                    ))?;
                *handle = memory.read_u64(slot).map_err(ExitReason::MemoryFault)?;
            }
            wait_for_objects(
                &wait_state,
                cpu.gs_base,
                handles,
                flags & MWMO_WAITALL != 0,
                timeout,
            )
        },
    );

    // Every window handle is invalid in a windowless machine: invisible,
    // not iconic, and ShowWindow reports no previous visibility.
    registry.register_with_status(
        MODULE,
        "IsWindowVisible",
        ServiceStatus::Partial,
        |_, _, _| Ok(0),
    );
    registry.register_with_status(MODULE, "IsIconic", ServiceStatus::Partial, |_, _, _| Ok(0));
    registry.register_with_status(MODULE, "ShowWindow", ServiceStatus::Partial, |_, _, _| {
        Ok(0)
    });
}

#[cfg(test)]
#[test]
fn the_windowless_message_queue_is_honestly_empty() {
    use crate::cpu::CpuState;
    use crate::import_table::ImportName;
    use crate::imports::Win64Args;
    use crate::memory::{GuestMemory, MemoryPermissions};
    use crate::services::MockClock;

    let mut registry = ImportRegistry::new();
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    register(&mut registry, &state);
    let mut memory = GuestMemory::new();
    memory
        .map_region(0x1000, 8192, MemoryPermissions::READ_WRITE)
        .unwrap();
    for (symbol, expected) in [
        ("PeekMessageA", 0),
        ("TranslateMessage", 0),
        ("DispatchMessageA", 0),
        ("IsWindowVisible", 0),
        ("IsIconic", 0),
        ("ShowWindow", 0),
    ] {
        let mut cpu = CpuState::default();
        let descriptor = registry
            .resolve(MODULE, &ImportName::Name(symbol.to_owned()))
            .unwrap();
        assert_eq!(
            (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap(),
            expected,
            "{symbol} answers honestly for a windowless machine"
        );
    }

    // MsgWaitForMultipleObjects: invalid counts and handles fail honestly,
    // and the wait machinery serves the object half of the contract.
    {
        use crate::cpu::Register;
        let wait = registry
            .resolve(
                MODULE,
                &ImportName::Name("MsgWaitForMultipleObjects".to_owned()),
            )
            .unwrap();
        let call_wait = |memory: &mut GuestMemory, args: &[u64]| {
            let mut cpu = CpuState::default();
            for (reg, &value) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
                .into_iter()
                .zip(args)
            {
                cpu.set(reg, value);
            }
            cpu.set(Register::Rsp, 0x2000);
            for (index, &value) in args.iter().enumerate().skip(4) {
                memory
                    .write_u64(0x2000 + 0x28 + (index as u64 - 4) * 8, value)
                    .unwrap();
            }
            (wait.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap()
        };
        memory.write_u64(0x1300, 0xDEAD).unwrap();
        // A count of zero or beyond 64 is invalid, whatever the handles say.
        assert_eq!(
            call_wait(&mut memory, &[0, 0x1300, 0, 0, 0]),
            u32::MAX as u64
        );
        assert_eq!(
            call_wait(&mut memory, &[65, 0x1300, 0, 0, 0]),
            u32::MAX as u64
        );
        // A handle the kernel does not know is WAIT_FAILED.
        assert_eq!(
            call_wait(&mut memory, &[1, 0x1300, 0, 0, 0]),
            u32::MAX as u64
        );
        // Unknown flags are invalid; the alertable form is honestly refused.
        assert_eq!(
            call_wait(&mut memory, &[1, 0x1300, 0, 0, 8]),
            u32::MAX as u64
        );
        assert_eq!(
            call_wait(&mut memory, &[1, 0x1300, 0, 0, 2]),
            u32::MAX as u64
        );
    }
}
