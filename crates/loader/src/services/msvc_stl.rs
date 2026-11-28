// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded Microsoft STL `ios_base::Init` reference-count services.
//!
//! The public Microsoft STL `stl/src/iostream.cpp` at commit
//! `0cabaf3c7e0a73fe3040aabce40cd8833e80392b` defines the count and
//! operations. Standard stream objects and their flushing are not implemented;
//! the final destructor therefore stops instead of claiming to have flushed.

use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, PROCESS_HEAP};
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
