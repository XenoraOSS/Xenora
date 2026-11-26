// SPDX-License-Identifier: GPL-3.0-or-later

//! Guest heap services backed by owned guest memory regions.

use super::{
    ERROR_INVALID_ADDRESS, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_NOT_ENOUGH_MEMORY,
    ERROR_NOT_SUPPORTED, KERNEL32, ServicesState,
};
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use std::sync::Arc;

use crate::memory::PROCESS_HEAP;
const HEAP_ZERO_MEMORY: u32 = 8;
const HEAP_REALLOC_IN_PLACE_ONLY: u32 = 16;
const HEAP_API: &str = "api-ms-win-core-heap-l1-1-0.dll";
const HEAP_OBSOLETE_API: &str = "api-ms-win-core-heap-obsolete-l1-1-0.dll";
// The legacy memory services accept the documented flags: the moveable
// bit, the zero-initialization bit and the discardable/nocompact/nodiscard
// hints of the 16-bit memory manager. The hints are inert on every 32- or
// 64-bit Windows - memory is always fixed and the handle is the pointer.
const GMEM_MOVEABLE: u32 = 0x2;
const GMEM_ZEROINIT: u32 = 0x40;
const LEGACY_INERT_HINTS: u32 = 0x0f00 | 0x10 | 0x20;
const LEGACY_ACCEPTED: u32 = GMEM_MOVEABLE | GMEM_ZEROINIT | LEGACY_INERT_HINTS;

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    registry.register(KERNEL32, "GetProcessHeap", |_, _, _| Ok(PROCESS_HEAP));

    registry.register(KERNEL32, "HeapAlloc", |args, _, memory| {
        let handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        let flags = args.get(1, memory).map_err(ExitReason::MemoryFault)? as u32;
        let size = args.get(2, memory).map_err(ExitReason::MemoryFault)?;
        if !memory.has_heap(handle) || flags & !HEAP_ZERO_MEMORY != 0 {
            return Ok(0);
        }
        Ok(memory
            .map_heap_allocation(handle, size.max(1) as usize)
            .unwrap_or(0))
    });

    registry.register(KERNEL32, "HeapReAlloc", |args, _, memory| {
        let handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        let flags = args.get(1, memory).map_err(ExitReason::MemoryFault)? as u32;
        let address = args.get(2, memory).map_err(ExitReason::MemoryFault)?;
        let size = args.get(3, memory).map_err(ExitReason::MemoryFault)?;
        if !memory.has_heap(handle) || flags & !(HEAP_ZERO_MEMORY | HEAP_REALLOC_IN_PLACE_ONLY) != 0
        {
            return Ok(0);
        }
        let in_place_only = flags & HEAP_REALLOC_IN_PLACE_ONLY != 0;
        Ok(memory
            .resize_heap_allocation(handle, address, size.max(1) as usize, in_place_only)
            .unwrap_or(0))
    });

    // Heap allocations use separate guest memory regions, with no free-block
    // list to compact. Return zero for both this case and an invalid handle.
    registry.register(KERNEL32, "HeapCompact", |_, _, _| Ok(0));

    // The allocator keeps its bookkeeping outside guest memory. Check the
    // heap handle or the exact base of a live allocation owned by that heap.
    registry.register(KERNEL32, "HeapValidate", |args, _, memory| {
        let valid = if args.r8 == 0 {
            memory.has_heap(args.rcx)
        } else {
            memory.heap_allocation_size(args.rcx, args.r8).is_some()
        };
        Ok(u64::from(valid))
    });

    registry.register(KERNEL32, "HeapSize", |args, _, memory| {
        let handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        let flags = args.get(1, memory).map_err(ExitReason::MemoryFault)? as u32;
        let address = args.get(2, memory).map_err(ExitReason::MemoryFault)?;
        if !memory.has_heap(handle) || flags != 0 {
            return Ok(u64::MAX);
        }
        Ok(memory
            .heap_allocation_size(handle, address)
            .map_or(u64::MAX, |size| size as u64))
    });

    let service_state = Arc::clone(state);
    registry.register(KERNEL32, "HeapCreate", move |args, cpu, memory| {
        let flags = args.get(0, memory).map_err(ExitReason::MemoryFault)? as u32;
        let initial = args.get(1, memory).map_err(ExitReason::MemoryFault)?;
        let maximum = args.get(2, memory).map_err(ExitReason::MemoryFault)?;
        if flags != 0 || maximum != 0 {
            return service_state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
        }
        match memory.create_private_heap(initial) {
            Some(handle) => Ok(handle),
            None => service_state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0),
        }
    });

    let service_state = Arc::clone(state);
    registry.register(KERNEL32, "HeapDestroy", move |args, cpu, memory| {
        let handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        if !memory.destroy_private_heap(handle) {
            return service_state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        }
        Ok(1)
    });

    let service_state = Arc::clone(state);
    registry.register(KERNEL32, "HeapFree", move |args, cpu, memory| {
        let handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        let flags = args.get(1, memory).map_err(ExitReason::MemoryFault)? as u32;
        let address = args.get(2, memory).map_err(ExitReason::MemoryFault)?;
        if !memory.has_heap(handle) {
            return service_state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        }
        if flags != 0 {
            return service_state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
        }
        if address != 0 && !memory.release_heap_allocation(handle, address) {
            return service_state.fail(cpu.gs_base, ERROR_INVALID_ADDRESS, 0);
        }
        Ok(1)
    });

    // The 16-bit compatibility services. On every 32- or 64-bit Windows they
    // are thin wrappers over the process heap: the moveable bit and the
    // discardable hints change nothing, the handle is the pointer itself,
    // and GlobalLock - not needed here - would return the same pointer.
    for symbol in ["GlobalAlloc", "LocalAlloc"] {
        let service_state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let flags = args.get(0, memory).map_err(ExitReason::MemoryFault)? as u32;
            let size = args.get(1, memory).map_err(ExitReason::MemoryFault)?;
            if flags & !LEGACY_ACCEPTED != 0 {
                return service_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            // Fresh guest regions are always zero-filled, so the zero-init
            // flag needs no extra work; the heap flags stay honest.
            match memory.map_heap_allocation(PROCESS_HEAP, size.max(1) as usize) {
                Ok(address) => Ok(address),
                Err(_) => service_state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0),
            }
        });
    }

    for symbol in ["GlobalFree", "LocalFree"] {
        let service_state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
            // Freeing the null handle is a documented no-op success.
            if handle == 0 {
                return Ok(0);
            }
            if memory.release_heap_allocation(PROCESS_HEAP, handle) {
                Ok(0)
            } else {
                // A failure answers the handle itself, as documented.
                service_state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, handle)
            }
        });
    }

    for symbol in [
        "GetProcessHeap",
        "HeapAlloc",
        "HeapReAlloc",
        "HeapSize",
        "HeapCompact",
        "HeapValidate",
        "HeapFree",
        "HeapCreate",
        "HeapDestroy",
    ] {
        registry.alias(HEAP_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
    for symbol in ["GlobalAlloc", "GlobalFree", "LocalAlloc", "LocalFree"] {
        registry.alias(HEAP_OBSOLETE_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
}
