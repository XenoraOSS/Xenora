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
        .map_region(0x1000, 4096, MemoryPermissions::READ_WRITE)
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
}
