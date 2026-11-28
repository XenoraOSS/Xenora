// SPDX-License-Identifier: GPL-3.0-or-later

//! Guest memory-region queries and paging-file-backed mappings.
//!
//! Documented (Microsoft Learn, "CreateFileMappingA", "MapViewOfFile",
//! "UnmapViewOfFile"): a file mapping object describes a region of memory
//! backed by a file or by the paging file, a view of it is memory the
//! program can use, and closing the object's last handle destroys it once
//! every view is gone. A fresh view of paging-file-backed memory reads as
//! zero, and every view of one object shares the same content.
//!
//! Xenora's choice, because there is no host file content to share
//! coherently: only paging-file-backed mappings - `INVALID_HANDLE_VALUE`
//! as the file - are supported. Mapping a real file fails honestly with
//! ERROR_NOT_SUPPORTED rather than serving content that writes could not
//! reach. Each object carries one backing byte store shared across the
//! machine, so a child process mapping the same handle - the numeric
//! handles being machine-wide here, as inherited handles are on Windows -
//! reads what the parent wrote. Because guest memory is private to each
//! process and only one process executes at a time, the backing store and
//! the views reconcile at the synchronization points a suspended parent
//! can observe: mapping copies the backing in, unmapping and flushing copy
//! the view out, and spawning a child copies every live view out first.
//! Writes a process makes while another process's view of the same object
//! stays mapped do not propagate until those points, which is exact for
//! the spawn-then-wait hand-off shells use and documented as the limit.

use super::{CURRENT_PROCESS, GuestObject, KERNEL32, ServicesState, handlers};
use crate::cpu::ExitReason;
use crate::imports::ImportRegistry;
use crate::memory::{GuestMemory, PROCESS_HEAP};
use crate::runtime::{OutPtr, ServiceContext};
use std::sync::Arc;

const MAPPING_API: &str = "api-ms-win-core-memory-l1-1-0.dll";
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_FILE_INVALID: u32 = 1006;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const INVALID_HANDLE_VALUE_FILE: u64 = u64::MAX;
const PAGE_PROTECTIONS: u32 = 0x2 | 0x4 | 0x8 | 0x10 | 0x20 | 0x40;
/// Paging-file-backed memory commits lazily on Windows; Xenora allocates
/// the backing eagerly, so an upper bound keeps a hostile size from
/// exhausting the host. 256 MiB is far beyond any shared-memory hand-off.
const MAX_BACKING_SIZE: u64 = 256 * 1024 * 1024;

/// The shared content of one mapping object.
type Backing = Arc<std::sync::Mutex<Vec<u8>>>;
/// A live view: guest base, length and backing store.
type ViewEntry = (u64, u64, Backing);

fn backing_of(
    state: &Arc<ServicesState>,
    object: u64,
) -> Option<(u64, Arc<std::sync::Mutex<Vec<u8>>>)> {
    let objects = state.objects.lock().expect("guest object state");
    match objects.handles.get(&object) {
        Some(GuestObject::FileMapping { size, backing }) => Some((*size, Arc::clone(backing))),
        _ => None,
    }
}

/// Copy the view's guest bytes back into its backing store. A view whose
/// memory is gone - the process that owned it has exited - is skipped.
fn copy_view_to_backing(memory: &GuestMemory, base: u64, length: u64, backing: &Backing) {
    let mut bytes = vec![0u8; length as usize];
    if memory.read_bytes(base, &mut bytes).is_err() {
        return;
    }
    let mut backing = backing.lock().expect("file mapping backing");
    let count = backing.len().min(bytes.len());
    backing[..count].copy_from_slice(&bytes[..count]);
}

/// Every live view belongs to the running process, since only one process
/// executes at a time: copy them all out before a spawn suspends it.
pub(super) fn sync_views_to_backings(state: &Arc<ServicesState>, memory: &GuestMemory) {
    let views: Vec<ViewEntry> = {
        let objects = state.objects.lock().expect("guest object state");
        objects
            .mapping_views
            .iter()
            .map(|(&base, (length, backing))| (base, *length, Arc::clone(backing)))
            .collect()
    };
    for (base, length, backing) in views {
        copy_view_to_backing(memory, base, length, &backing);
    }
}

/// The addresses of the live views, for a spawn to tell apart what the
/// child creates from what the parent already held.
pub(super) fn live_view_addresses(state: &Arc<ServicesState>) -> Vec<u64> {
    state
        .objects
        .lock()
        .expect("guest object state")
        .mapping_views
        .keys()
        .copied()
        .collect()
}

/// Drop every live view whose address is not in `keep`. A finished child
/// never unmaps cleanly - its shell memory is sticky - and its guest
/// memory no longer exists, so its entries would only collide with the
/// next process's allocations.
pub(super) fn drop_views_except(state: &Arc<ServicesState>, keep: &[u64]) {
    let mut objects = state.objects.lock().expect("guest object state");
    objects.mapping_views.retain(|base, _| keep.contains(base));
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    for (symbol, _wide) in [("CreateFileMappingA", false), ("CreateFileMappingW", true)] {
        let state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let (file, _attrs, protect, size_high, size_low) =
                (args.rcx, args.rdx, args.r8 as u32, args.r9 as u32, {
                    args.get(4, memory).map_err(ExitReason::MemoryFault)? as u32
                });
            if protect & !PAGE_PROTECTIONS != 0 {
                return state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            if file != INVALID_HANDLE_VALUE_FILE {
                // File-backed content is not served honestly yet.
                return state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            let size = (u64::from(size_high) << 32) | u64::from(size_low);
            if size == 0 {
                return state.fail(cpu.gs_base, ERROR_FILE_INVALID, 0);
            }
            if size > MAX_BACKING_SIZE {
                return state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
            }
            let mut objects = state.objects.lock().expect("guest object state");
            let Some(handle) = objects.next_handle() else {
                return state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
            };
            objects.insert_handle(
                handle,
                GuestObject::FileMapping {
                    size,
                    backing: Arc::new(std::sync::Mutex::new(vec![0u8; size as usize])),
                },
            );
            Ok(handle)
        });
    }

    for (symbol, all_access) in [("MapViewOfFile", false), ("MapViewOfFileEx", true)] {
        let state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let (object, _access, offset_high, offset_low) =
                (args.rcx, args.rdx, args.r8 as u32, args.r9 as u32);
            let requested = args.get(4, memory).map_err(ExitReason::MemoryFault)?;
            let Some((size, backing)) = backing_of(&state, object) else {
                return state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
            };
            let offset = (u64::from(offset_high) << 32) | u64::from(offset_low);
            if offset != 0
                || (all_access && args.get(5, memory).map_err(ExitReason::MemoryFault)? != 0)
            {
                // Offset views and placed views are not modelled.
                return state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            // A view must contain every requested byte within the mapping.
            if requested > size {
                return state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
            }
            let length = if requested == 0 { size } else { requested };
            let address = memory.map_heap_allocation(PROCESS_HEAP, length as usize);
            let Ok(address) = address else {
                return state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
            };
            // The view opens with the backing store's content, as shared
            // memory the process is only now joining would.
            {
                let backing = backing.lock().expect("file mapping backing");
                let count = backing.len().min(length as usize);
                if count > 0 && memory.write_bytes(address, &backing[..count]).is_err() {
                    memory.release_heap_allocation(PROCESS_HEAP, address);
                    return state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
                }
            }
            state
                .objects
                .lock()
                .expect("guest object state")
                .mapping_views
                .insert(address, (length, backing));
            Ok(address)
        });
    }

    // Paging-file-backed views have no file to write back, but the backing
    // store must observe the flush: copy the containing view out.
    let flush_state = Arc::clone(state);
    registry.register(KERNEL32, "FlushViewOfFile", move |args, cpu, memory| {
        let found = {
            let objects = flush_state.objects.lock().expect("guest object state");
            objects
                .mapping_views
                .range(..=args.rcx)
                .next_back()
                .and_then(|(&base, (length, backing))| {
                    let offset = args.rcx - base;
                    (offset < *length && args.rdx <= length - offset)
                        .then(|| (base, *length, Arc::clone(backing)))
                })
        };
        let Some((base, length, backing)) = found else {
            return flush_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
        };
        copy_view_to_backing(memory, base, length, &backing);
        Ok(1)
    });
    registry.set_status(
        KERNEL32,
        "FlushViewOfFile",
        crate::imports::ServiceStatus::Partial,
    );

    let state_unmap = Arc::clone(state);
    registry.register(KERNEL32, "UnmapViewOfFile", move |args, cpu, memory| {
        let address = args.rcx;
        let view = state_unmap
            .objects
            .lock()
            .expect("guest object state")
            .mapping_views
            .remove(&address);
        match view {
            Some((length, backing)) => {
                copy_view_to_backing(memory, address, length, &backing);
                memory.release_heap_allocation(PROCESS_HEAP, address);
                Ok(1)
            }
            None => state_unmap.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0),
        }
    });

    let query_state = Arc::clone(state);
    registry.register(KERNEL32, "VirtualQueryEx", move |args, cpu, memory| {
        let teb = cpu.gs_base;
        if args.rcx != CURRENT_PROCESS {
            let objects = query_state.objects.lock().expect("guest object state");
            // OpenProcess does not track granted rights. The runtime also
            // has no live address space to query for a finished child.
            let error = match objects.handles.get(&args.rcx) {
                Some(GuestObject::Process | GuestObject::ChildProcess { .. }) => {
                    ERROR_NOT_SUPPORTED
                }
                _ => ERROR_INVALID_HANDLE,
            };
            return query_state.fail(teb, error, 0);
        }
        let mut context = ServiceContext {
            state: &query_state,
            memory,
            teb,
        };
        handlers::virtual_query(&mut context, args.rdx, OutPtr::<48>(args.r8), args.r9)
    });

    for symbol in [
        "CreateFileMappingA",
        "CreateFileMappingW",
        "MapViewOfFile",
        "UnmapViewOfFile",
        "VirtualQueryEx",
    ] {
        registry.alias(MAPPING_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, crate::imports::ServiceStatus::Partial);
    }
}
