// SPDX-License-Identifier: GPL-3.0-or-later

//! Guest heap services backed by owned guest memory regions.

use super::{
    ERROR_INVALID_ADDRESS, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_NOT_ENOUGH_MEMORY,
    ERROR_NOT_SUPPORTED, KERNEL32, ServicesState,
};
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::memory::PROCESS_HEAP;

mod local;

const HEAP_NO_SERIALIZE: u32 = 1;
const HEAP_ZERO_MEMORY: u32 = 8;
const HEAP_REALLOC_IN_PLACE_ONLY: u32 = 16;
const HEAP_API: &str = "api-ms-win-core-heap-l1-1-0.dll";
const HEAP_OBSOLETE_API: &str = "api-ms-win-core-heap-obsolete-l1-1-0.dll";
// The legacy allocation services accept the movable and zero-initialization
// flags, plus the obsolete discardable, nocompact and nodiscard hints.
const GMEM_MOVEABLE: u32 = 0x2;
const GMEM_ZEROINIT: u32 = 0x40;
const LEGACY_INERT_HINTS: u32 = 0x0f00 | 0x10 | 0x20;
const LEGACY_ACCEPTED: u32 = GMEM_MOVEABLE | GMEM_ZEROINIT | LEGACY_INERT_HINTS;

fn heap_flags_supported(handle: u64, flags: u32, allowed: u32) -> bool {
    // Guest heap operations already run with exclusive access to GuestMemory.
    // Private heaps can accept the caller's request to omit native heap locking.
    let private_flags = if handle == PROCESS_HEAP {
        0
    } else {
        HEAP_NO_SERIALIZE
    };
    flags & !(allowed | private_flags) == 0
}

fn public_heap(memory: &crate::memory::GuestMemory, backing_heap: &AtomicU64, handle: u64) -> bool {
    handle != backing_heap.load(Ordering::Relaxed) && memory.has_heap(handle)
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    registry.register(KERNEL32, "GetProcessHeap", |_, _, _| Ok(PROCESS_HEAP));

    // Heap* must not expose the internal heap behind movable local handles.
    let local_heap = Arc::new(AtomicU64::new(0));
    let backing_heap = Arc::clone(&local_heap);
    registry.register(KERNEL32, "HeapAlloc", move |args, _, memory| {
        let handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        let flags = args.get(1, memory).map_err(ExitReason::MemoryFault)? as u32;
        let size = args.get(2, memory).map_err(ExitReason::MemoryFault)?;
        if !public_heap(memory, &backing_heap, handle)
            || !heap_flags_supported(handle, flags, HEAP_ZERO_MEMORY)
        {
            return Ok(0);
        }
        Ok(memory
            .map_heap_allocation(handle, size.max(1) as usize)
            .unwrap_or(0))
    });

    let backing_heap = Arc::clone(&local_heap);
    registry.register(KERNEL32, "HeapReAlloc", move |args, _, memory| {
        let handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        let flags = args.get(1, memory).map_err(ExitReason::MemoryFault)? as u32;
        let address = args.get(2, memory).map_err(ExitReason::MemoryFault)?;
        let size = args.get(3, memory).map_err(ExitReason::MemoryFault)?;
        if !public_heap(memory, &backing_heap, handle)
            || !heap_flags_supported(handle, flags, HEAP_ZERO_MEMORY | HEAP_REALLOC_IN_PLACE_ONLY)
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
    let backing_heap = Arc::clone(&local_heap);
    registry.register(KERNEL32, "HeapValidate", move |args, _, memory| {
        let valid = public_heap(memory, &backing_heap, args.rcx)
            && (args.r8 == 0 || memory.heap_allocation_size(args.rcx, args.r8).is_some());
        Ok(u64::from(valid))
    });

    let backing_heap = Arc::clone(&local_heap);
    registry.register(KERNEL32, "HeapSize", move |args, _, memory| {
        let handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        let flags = args.get(1, memory).map_err(ExitReason::MemoryFault)? as u32;
        let address = args.get(2, memory).map_err(ExitReason::MemoryFault)?;
        if !public_heap(memory, &backing_heap, handle) || !heap_flags_supported(handle, flags, 0) {
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
        if flags & !HEAP_NO_SERIALIZE != 0 || maximum != 0 {
            return service_state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
        }
        match memory.create_private_heap(initial) {
            Some(handle) => Ok(handle),
            None => service_state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0),
        }
    });

    let service_state = Arc::clone(state);
    let backing_heap = Arc::clone(&local_heap);
    registry.register(KERNEL32, "HeapDestroy", move |args, cpu, memory| {
        let handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        if !public_heap(memory, &backing_heap, handle) || !memory.destroy_private_heap(handle) {
            return service_state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        }
        Ok(1)
    });

    let service_state = Arc::clone(state);
    let backing_heap = Arc::clone(&local_heap);
    registry.register(KERNEL32, "HeapFree", move |args, cpu, memory| {
        let handle = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
        let flags = args.get(1, memory).map_err(ExitReason::MemoryFault)? as u32;
        let address = args.get(2, memory).map_err(ExitReason::MemoryFault)?;
        if !public_heap(memory, &backing_heap, handle) {
            return service_state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        }
        if !heap_flags_supported(handle, flags, 0) {
            return service_state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
        }
        if address != 0 && !memory.release_heap_allocation(handle, address) {
            return service_state.fail(cpu.gs_base, ERROR_INVALID_ADDRESS, 0);
        }
        Ok(1)
    });

    // Xenora's bounded GlobalAlloc compatibility path uses fixed blocks.
    let service_state = Arc::clone(state);
    registry.register(KERNEL32, "GlobalAlloc", move |args, cpu, memory| {
        let flags = args.rcx as u32;
        if flags & !LEGACY_ACCEPTED != 0 {
            return service_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
        }
        match memory.map_heap_allocation(PROCESS_HEAP, args.rdx.max(1) as usize) {
            Ok(address) => Ok(address),
            Err(_) => service_state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0),
        }
    });

    let service_state = Arc::clone(state);
    registry.register(KERNEL32, "GlobalFree", move |args, cpu, memory| {
        let handle = args.rcx;
        if handle == 0 || memory.release_heap_allocation(PROCESS_HEAP, handle) {
            Ok(0)
        } else {
            service_state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, handle)
        }
    });
    local::register(registry, state, local_heap);

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
    for symbol in ["GlobalAlloc", "GlobalFree"] {
        registry.alias(HEAP_OBSOLETE_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
}

#[cfg(test)]
#[test]
fn public_heap_calls_cannot_access_movable_local_backing() {
    use crate::cpu::CpuState;
    use crate::import_table::ImportName;
    use crate::imports::Win64Args;
    use crate::memory::GuestMemory;
    use crate::services::MockClock;

    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register(&mut registry, &state);
    let mut memory = GuestMemory::new();
    let call = |memory: &mut GuestMemory, name: &str, [rcx, rdx, r8, r9]: [u64; 4]| {
        let entry = registry
            .resolve(KERNEL32, &ImportName::Name(name.into()))
            .unwrap();
        let args = Win64Args {
            rcx,
            rdx,
            r8,
            r9,
            rsp: 0,
        };
        (entry.handler)(&args, &mut CpuState::default(), memory).unwrap()
    };

    let public = call(&mut memory, "HeapCreate", [0, 0, 0, 0]);
    assert_ne!(public, 0);
    let local = call(
        &mut memory,
        "LocalAlloc",
        [u64::from(GMEM_MOVEABLE), 16, 0, 0],
    );
    let address = call(&mut memory, "LocalLock", [local, 0, 0, 0]);
    assert_ne!(address, 0);
    // Guest private-heap IDs are currently sequential. Pin the case where
    // a guest passes a valid but undisclosed allocator ID to public APIs.
    let hidden = public - 1;
    assert_eq!(memory.heap_allocation_size(hidden, address), Some(16));
    assert_eq!(call(&mut memory, "HeapValidate", [hidden, 0, 0, 0]), 0);
    assert_eq!(call(&mut memory, "HeapAlloc", [hidden, 0, 8, 0]), 0);
    assert_eq!(
        call(&mut memory, "HeapSize", [hidden, 0, address, 0]),
        u64::MAX
    );
    assert_eq!(call(&mut memory, "HeapReAlloc", [hidden, 0, address, 8]), 0);
    assert_eq!(call(&mut memory, "HeapFree", [hidden, 0, address, 0]), 0);
    assert_eq!(call(&mut memory, "HeapDestroy", [hidden, 0, 0, 0]), 0);
    assert_eq!(call(&mut memory, "LocalFree", [address, 0, 0, 0]), address);
    assert_eq!(call(&mut memory, "LocalSize", [local, 0, 0, 0]), 16);
    assert_eq!(call(&mut memory, "LocalFree", [local, 0, 0, 0]), 0);
    assert!(!memory.has_heap(hidden));
    assert_eq!(call(&mut memory, "HeapDestroy", [public, 0, 0, 0]), 1);
}
