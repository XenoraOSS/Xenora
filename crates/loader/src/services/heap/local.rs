// SPDX-License-Identifier: GPL-3.0-or-later

//! Win32 local memory. Movable handles identify objects independently of
//! their backing addresses, including when the backing block is discarded.

use super::{GMEM_MOVEABLE, GMEM_ZEROINIT, HEAP_OBSOLETE_API, LEGACY_ACCEPTED};
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, PROCESS_HEAP};
use crate::services::{
    ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_NOT_ENOUGH_MEMORY, ERROR_NOT_SUPPORTED,
    KERNEL32, MAX_OBJECT_HANDLES, ServicesState,
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const LMEM_MODIFY: u32 = 0x0080;
const LMEM_DISCARDED: u64 = 0x4000;
const LMEM_INVALID_HANDLE: u64 = 0x8000;
const HEAP_API_L2: &str = "api-ms-win-core-heap-l2-1-0.dll";
const ERROR_DISCARDED: u32 = 157;
const ERROR_NOT_LOCKED: u32 = 158;
const ERROR_LOCKED: u32 = 212;

// Fixed allocations are aligned. Odd movable tokens cannot alias a fixed
// pointer even if the same address range has a guest memory mapping.
const FIRST_MOVABLE_HANDLE: u64 = 0x4c4f_0000_0001;

#[derive(Clone, Copy)]
struct LocalBlock {
    address: Option<u64>,
    locks: u8,
}

struct LocalMemory {
    heap: Arc<AtomicU64>,
    last_handle: u64,
    blocks: BTreeMap<u64, LocalBlock>,
}

impl LocalMemory {
    fn backing_heap(&self) -> Option<u64> {
        let heap = self.heap.load(Ordering::Relaxed);
        (heap != 0).then_some(heap)
    }

    fn reserve(&mut self, memory: &mut GuestMemory, size: usize) -> Result<u64, u32> {
        // Use a private guest heap so the backing pointer is not accepted
        // as a fixed handle by LocalFree or the legacy process-heap path.
        let heap = if let Some(heap) = self.backing_heap() {
            heap
        } else {
            let heap = memory
                .create_private_heap(0)
                .ok_or(ERROR_NOT_ENOUGH_MEMORY)?;
            self.heap.store(heap, Ordering::Relaxed);
            heap
        };
        let result = memory
            .map_heap_allocation(heap, size)
            .map_err(|_| ERROR_NOT_ENOUGH_MEMORY);
        if result.is_err() {
            self.release_empty_heap(memory);
        }
        result
    }

    fn release_empty_heap(&mut self, memory: &mut GuestMemory) {
        if self.blocks.values().all(|block| block.address.is_none())
            && let Some(heap) = self.backing_heap()
        {
            memory.destroy_private_heap(heap);
            self.heap.store(0, Ordering::Relaxed);
        }
    }

    fn allocate(
        &mut self,
        memory: &mut GuestMemory,
        size: usize,
        movable: bool,
    ) -> Result<u64, u32> {
        if !movable {
            return memory
                .map_heap_allocation(PROCESS_HEAP, size.max(1))
                .map_err(|_| ERROR_NOT_ENOUGH_MEMORY);
        }
        if self.blocks.len() >= MAX_OBJECT_HANDLES {
            return Err(ERROR_NOT_ENOUGH_MEMORY);
        }
        let handle = self
            .last_handle
            .max(FIRST_MOVABLE_HANDLE - 2)
            .checked_add(2)
            .ok_or(ERROR_NOT_ENOUGH_MEMORY)?;
        let address = if size == 0 {
            None
        } else {
            Some(self.reserve(memory, size)?)
        };
        self.blocks.insert(handle, LocalBlock { address, locks: 0 });
        self.last_handle = handle;
        Ok(handle)
    }

    fn free(&mut self, memory: &mut GuestMemory, handle: u64) -> Result<u64, u32> {
        if handle == 0 {
            return Ok(0);
        }
        let Some(block) = self.blocks.get(&handle) else {
            return if memory.release_heap_allocation(PROCESS_HEAP, handle) {
                Ok(0)
            } else {
                Err(ERROR_INVALID_HANDLE)
            };
        };
        if let Some(address) = block.address
            && !self
                .backing_heap()
                .is_some_and(|heap| memory.release_heap_allocation(heap, address))
        {
            return Err(ERROR_INVALID_HANDLE);
        }
        // Win32 LocalFree accepts a locked movable object too.
        self.blocks.remove(&handle);
        self.release_empty_heap(memory);
        Ok(0)
    }

    fn resize(
        &mut self,
        memory: &mut GuestMemory,
        handle: u64,
        size: usize,
        may_move: bool,
    ) -> Result<u64, u32> {
        let Some(mut block) = self.blocks.get(&handle).copied() else {
            if memory.heap_allocation_size(PROCESS_HEAP, handle).is_none() {
                return Err(ERROR_INVALID_HANDLE);
            }
            return memory
                .resize_heap_allocation(PROCESS_HEAP, handle, size.max(1), !may_move)
                .map_err(|_| ERROR_NOT_ENOUGH_MEMORY);
        };
        block.address = match (block.address, size) {
            (Some(_), 0) if block.locks != 0 => return Err(ERROR_LOCKED),
            (Some(address), 0) => {
                if !self
                    .backing_heap()
                    .is_some_and(|heap| memory.release_heap_allocation(heap, address))
                {
                    return Err(ERROR_INVALID_HANDLE);
                }
                None
            }
            (Some(address), size) => {
                let heap = self.backing_heap().ok_or(ERROR_INVALID_HANDLE)?;
                let in_place = block.locks != 0 && !may_move;
                Some(
                    memory
                        .resize_heap_allocation(heap, address, size, in_place)
                        .map_err(|_| ERROR_NOT_ENOUGH_MEMORY)?,
                )
            }
            (None, 0) => None,
            (None, size) => Some(self.reserve(memory, size)?),
        };
        self.blocks.insert(handle, block);
        self.release_empty_heap(memory);
        Ok(handle)
    }

    fn lock(&mut self, memory: &GuestMemory, handle: u64) -> Result<u64, u32> {
        if let Some(block) = self.blocks.get_mut(&handle) {
            let address = block.address.ok_or(ERROR_DISCARDED)?;
            block.locks = block.locks.checked_add(1).ok_or(ERROR_LOCKED)?;
            return Ok(address);
        }
        if memory.heap_allocation_size(PROCESS_HEAP, handle).is_some() {
            Ok(handle)
        } else {
            Err(ERROR_INVALID_HANDLE)
        }
    }

    fn unlock(&mut self, memory: &GuestMemory, handle: u64) -> Result<u64, u32> {
        if let Some(block) = self.blocks.get_mut(&handle) {
            block.locks = block.locks.checked_sub(1).ok_or(ERROR_NOT_LOCKED)?;
            return Ok(u64::from(block.locks != 0));
        }
        if memory.heap_allocation_size(PROCESS_HEAP, handle).is_some() {
            Err(ERROR_NOT_LOCKED)
        } else {
            Err(ERROR_INVALID_HANDLE)
        }
    }

    fn size(&self, memory: &GuestMemory, handle: u64) -> Result<u64, u32> {
        let (heap, address) = if let Some(block) = self.blocks.get(&handle) {
            let address = block.address.ok_or(ERROR_DISCARDED)?;
            (self.backing_heap().ok_or(ERROR_INVALID_HANDLE)?, address)
        } else {
            (PROCESS_HEAP, handle)
        };
        memory
            .heap_allocation_size(heap, address)
            .map(|size| size as u64)
            .ok_or(ERROR_INVALID_HANDLE)
    }

    fn flags(&self, memory: &GuestMemory, handle: u64) -> Result<u64, u32> {
        if let Some(block) = self.blocks.get(&handle) {
            return Ok(if block.address.is_none() {
                LMEM_DISCARDED
            } else {
                u64::from(block.locks)
            });
        }
        if memory.heap_allocation_size(PROCESS_HEAP, handle).is_some() {
            Ok(0)
        } else {
            Err(ERROR_INVALID_HANDLE)
        }
    }

    fn handle(&self, memory: &GuestMemory, address: u64) -> Result<u64, u32> {
        if let Some((&handle, _)) = self
            .blocks
            .iter()
            .find(|(_, block)| block.address == Some(address))
        {
            return Ok(handle);
        }
        if memory.heap_allocation_size(PROCESS_HEAP, address).is_some() {
            Ok(address)
        } else {
            Err(ERROR_INVALID_HANDLE)
        }
    }
}

fn result(
    state: &ServicesState,
    teb: u64,
    result: Result<u64, u32>,
    on_failure: u64,
) -> Result<u64, ExitReason> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => state.fail(teb, error, on_failure),
    }
}

pub(super) fn register(
    registry: &mut ImportRegistry,
    state: &Arc<ServicesState>,
    heap: Arc<AtomicU64>,
) {
    let locals = Arc::new(Mutex::new(LocalMemory {
        heap,
        last_handle: 0,
        blocks: BTreeMap::new(),
    }));

    let allocations = Arc::clone(&locals);
    let alloc_state = Arc::clone(state);
    registry.register_with_status(
        KERNEL32,
        "LocalAlloc",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let flags = args.rcx as u32;
            if flags & !LEGACY_ACCEPTED != 0 {
                return alloc_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let value = allocations.lock().expect("guest local memory").allocate(
                memory,
                args.rdx as usize,
                flags & GMEM_MOVEABLE != 0,
            );
            result(&alloc_state, cpu.gs_base, value, 0)
        },
    );

    let allocations = Arc::clone(&locals);
    let free_state = Arc::clone(state);
    registry.register_with_status(
        KERNEL32,
        "LocalFree",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let value = allocations
                .lock()
                .expect("guest local memory")
                .free(memory, args.rcx);
            result(&free_state, cpu.gs_base, value, args.rcx)
        },
    );

    let allocations = Arc::clone(&locals);
    let resize_state = Arc::clone(state);
    registry.register_with_status(
        KERNEL32,
        "LocalReAlloc",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let flags = args.r8 as u32;
            if flags & LMEM_MODIFY != 0 {
                return resize_state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            if flags & !(GMEM_MOVEABLE | GMEM_ZEROINIT) != 0 {
                return resize_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let value = allocations.lock().expect("guest local memory").resize(
                memory,
                args.rcx,
                args.rdx as usize,
                flags & GMEM_MOVEABLE != 0,
            );
            result(&resize_state, cpu.gs_base, value, 0)
        },
    );

    let allocations = Arc::clone(&locals);
    let lock_state = Arc::clone(state);
    registry.register_with_status(
        KERNEL32,
        "LocalLock",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let value = allocations
                .lock()
                .expect("guest local memory")
                .lock(memory, args.rcx);
            result(&lock_state, cpu.gs_base, value, 0)
        },
    );

    let allocations = Arc::clone(&locals);
    let unlock_state = Arc::clone(state);
    registry.register_with_status(
        KERNEL32,
        "LocalUnlock",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let value = allocations
                .lock()
                .expect("guest local memory")
                .unlock(memory, args.rcx);
            if value == Ok(0) {
                unlock_state.set_last_error(cpu.gs_base, 0);
            }
            result(&unlock_state, cpu.gs_base, value, 0)
        },
    );

    type Inspect = fn(&LocalMemory, &GuestMemory, u64) -> Result<u64, u32>;
    for (symbol, inspect, fallback) in [
        ("LocalSize", LocalMemory::size as Inspect, 0),
        ("LocalFlags", LocalMemory::flags, LMEM_INVALID_HANDLE),
        ("LocalHandle", LocalMemory::handle, 0),
    ] {
        let allocations = Arc::clone(&locals);
        let state = Arc::clone(state);
        registry.register_with_status(
            KERNEL32,
            symbol,
            ServiceStatus::Partial,
            move |args, cpu, memory| {
                let value = inspect(
                    &allocations.lock().expect("guest local memory"),
                    memory,
                    args.rcx,
                );
                result(&state, cpu.gs_base, value, fallback)
            },
        );
    }

    // The SDK lists both heap contracts for the movable operations.
    for symbol in [
        "LocalAlloc",
        "LocalFree",
        "LocalLock",
        "LocalReAlloc",
        "LocalUnlock",
    ] {
        registry.alias(HEAP_OBSOLETE_API, KERNEL32, symbol);
        registry.alias(HEAP_API_L2, KERNEL32, symbol);
    }
    for symbol in ["LocalFlags", "LocalSize"] {
        registry.alias(HEAP_OBSOLETE_API, KERNEL32, symbol);
    }
}
