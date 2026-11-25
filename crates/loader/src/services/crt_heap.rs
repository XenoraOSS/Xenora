// SPDX-License-Identifier: GPL-3.0-or-later

//! UCRT block-size, aligned and zeroing allocation services on the CRT heap.
//!
//! Behaviour follows the public UCRT reference pages for `_msize`,
//! `_recalloc`, `_aligned_malloc`, `_aligned_realloc`, `_aligned_msize` and
//! `_aligned_free`. Cases those pages send to the invalid-parameter handler
//! set `errno` to `EINVAL` and return the documented failure value instead;
//! the handler itself is not modelled.
//!
//! Heap blocks start on 64 KiB boundaries, so an aligned block is an ordinary
//! CRT heap block whose address is checked against the requested alignment.
//! Aligned blocks are recorded so that `_aligned_free` of any other pointer,
//! including a second free of the same block, stops the guest with a memory
//! fault instead of being ignored.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError, PROCESS_HEAP};
use std::collections::BTreeSet;
use std::sync::Arc;

const UCRT: &str = "ucrtbase.dll";
const CRT_HEAP_API: &str = "api-ms-win-crt-heap-l1-1-0.dll";
const ENOMEM: u32 = 12;
const EINVAL: u32 = 22;
const SIZE_FAILURE: u64 = u64::MAX;

/// Blocks handed out by the aligned allocators.
#[derive(Default)]
pub(super) struct AlignedBlocks(BTreeSet<u64>);

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

fn set_errno(memory: &mut GuestMemory, teb: u64, error: u32) -> Result<(), ExitReason> {
    let address = memory.crt_errno_address(teb).map_err(fault)?;
    memory.write_u32(address, error).map_err(fault)
}

fn failed(memory: &mut GuestMemory, teb: u64, error: u32, value: u64) -> Result<u64, ExitReason> {
    set_errno(memory, teb, error)?;
    Ok(value)
}

/// A new aligned block, or `None` when the heap cannot provide one.
fn aligned_block(memory: &mut GuestMemory, size: u64, alignment: u64) -> Option<u64> {
    let size = usize::try_from(size.max(1)).ok()?;
    let block = memory.map_heap_allocation(PROCESS_HEAP, size).ok()?;
    if block % alignment == 0 {
        Some(block)
    } else {
        // Larger alignments than the heap provides are refused, not faked.
        memory.release_heap_allocation(PROCESS_HEAP, block);
        None
    }
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    registry.register_with_status(
        UCRT,
        "_msize",
        ServiceStatus::Partial,
        |args, cpu, memory| {
            if args.rcx == 0 {
                return failed(memory, cpu.gs_base, EINVAL, SIZE_FAILURE);
            }
            match memory.heap_allocation_size(PROCESS_HEAP, args.rcx) {
                Some(size) => Ok(size as u64),
                None => Err(fault(MemoryError::UnmappedAddress(args.rcx))),
            }
        },
    );

    registry.register_with_status(
        UCRT,
        "_recalloc",
        ServiceStatus::Partial,
        |args, cpu, memory| {
            let (block, count, size) = (args.rcx, args.rdx, args.r8);
            let Some(total) = count.checked_mul(size) else {
                return failed(memory, cpu.gs_base, ENOMEM, 0);
            };
            if block == 0 {
                return match memory.map_heap_allocation(PROCESS_HEAP, total.max(1) as usize) {
                    Ok(address) => Ok(address),
                    Err(_) => failed(memory, cpu.gs_base, ENOMEM, 0),
                };
            }
            let Some(previous) = memory.heap_allocation_size(PROCESS_HEAP, block) else {
                return Err(fault(MemoryError::UnmappedAddress(block)));
            };
            if total == 0 {
                memory.release_heap_allocation(PROCESS_HEAP, block);
                return Ok(0);
            }
            let Ok(total) = usize::try_from(total) else {
                return failed(memory, cpu.gs_base, ENOMEM, 0);
            };
            match memory.resize_heap_allocation(PROCESS_HEAP, block, total, false) {
                Ok(address) => {
                    // Bytes past the old block start at zero, as documented.
                    if total > previous {
                        memory
                            .write_bytes(address + previous as u64, &vec![0; total - previous])
                            .map_err(fault)?;
                    }
                    Ok(address)
                }
                // The original block is left unchanged.
                Err(_) => failed(memory, cpu.gs_base, ENOMEM, 0),
            }
        },
    );

    let malloc = Arc::clone(state);
    registry.register_with_status(
        UCRT,
        "_aligned_malloc",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let (size, alignment) = (args.rcx, args.rdx);
            if !alignment.is_power_of_two() {
                return failed(memory, cpu.gs_base, EINVAL, 0);
            }
            match aligned_block(memory, size, alignment) {
                Some(block) => {
                    malloc
                        .aligned
                        .lock()
                        .expect("aligned blocks")
                        .0
                        .insert(block);
                    Ok(block)
                }
                None => failed(memory, cpu.gs_base, ENOMEM, 0),
            }
        },
    );

    let realloc = Arc::clone(state);
    registry.register_with_status(
        UCRT,
        "_aligned_realloc",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let (block, size, alignment) = (args.rcx, args.rdx, args.r8);
            if !alignment.is_power_of_two() {
                return failed(memory, cpu.gs_base, EINVAL, 0);
            }
            let mut blocks = realloc.aligned.lock().expect("aligned blocks");
            if block == 0 {
                return match aligned_block(memory, size, alignment) {
                    Some(new) => {
                        blocks.0.insert(new);
                        Ok(new)
                    }
                    None => failed(memory, cpu.gs_base, ENOMEM, 0),
                };
            }
            if !blocks.0.contains(&block) {
                return Err(fault(MemoryError::UnmappedAddress(block)));
            }
            if size == 0 {
                blocks.0.remove(&block);
                memory.release_heap_allocation(PROCESS_HEAP, block);
                return Ok(0);
            }
            let Ok(size) = usize::try_from(size) else {
                return failed(memory, cpu.gs_base, ENOMEM, 0);
            };
            let previous = memory
                .heap_allocation_size(PROCESS_HEAP, block)
                .ok_or(fault(MemoryError::UnmappedAddress(block)))?;
            // Grow or shrink in place when that keeps the alignment.
            if let Ok(same) = memory.resize_heap_allocation(PROCESS_HEAP, block, size, true) {
                return Ok(same);
            }
            let Some(new) = aligned_block(memory, size as u64, alignment) else {
                return failed(memory, cpu.gs_base, ENOMEM, 0);
            };
            let mut bytes = vec![0; previous.min(size)];
            memory.read_bytes(block, &mut bytes).map_err(fault)?;
            memory.write_bytes(new, &bytes).map_err(fault)?;
            memory.release_heap_allocation(PROCESS_HEAP, block);
            blocks.0.remove(&block);
            blocks.0.insert(new);
            Ok(new)
        },
    );

    let msize = Arc::clone(state);
    registry.register_with_status(
        UCRT,
        "_aligned_msize",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let (block, alignment, offset) = (args.rcx, args.rdx, args.r8);
            if block == 0 || !alignment.is_power_of_two() || offset != 0 {
                // A nonzero offset belongs to `_aligned_offset_malloc`, which
                // is not provided, so no block here can have one.
                return failed(memory, cpu.gs_base, EINVAL, SIZE_FAILURE);
            }
            if !msize
                .aligned
                .lock()
                .expect("aligned blocks")
                .0
                .contains(&block)
            {
                return Err(fault(MemoryError::UnmappedAddress(block)));
            }
            memory
                .heap_allocation_size(PROCESS_HEAP, block)
                .map(|size| size as u64)
                .ok_or(fault(MemoryError::UnmappedAddress(block)))
        },
    );

    let free = Arc::clone(state);
    registry.register_with_status(
        UCRT,
        "_aligned_free",
        ServiceStatus::Implemented,
        move |args, _, memory| {
            if args.rcx == 0 {
                return Ok(0);
            }
            // A pointer this allocator never returned, or one already freed.
            if !free
                .aligned
                .lock()
                .expect("aligned blocks")
                .0
                .remove(&args.rcx)
            {
                return Err(fault(MemoryError::UnmappedAddress(args.rcx)));
            }
            memory.release_heap_allocation(PROCESS_HEAP, args.rcx);
            Ok(0)
        },
    );

    for symbol in [
        "_msize",
        "_recalloc",
        "_aligned_malloc",
        "_aligned_realloc",
        "_aligned_msize",
        "_aligned_free",
    ] {
        registry.alias(CRT_HEAP_API, UCRT, symbol);
    }
}
