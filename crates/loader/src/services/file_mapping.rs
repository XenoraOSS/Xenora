// SPDX-License-Identifier: GPL-3.0-or-later

//! Memory-mapped files, honestly scoped to shared memory.
//!
//! Documented (Microsoft Learn, "CreateFileMappingA", "MapViewOfFile",
//! "UnmapViewOfFile"): a file mapping object describes a region of memory
//! backed by a file or by the paging file, a view of it is memory the
//! program can use, and closing the object's last handle destroys it once
//! every view is gone. A fresh view of paging-file-backed memory reads as
//! zero.
//!
//! Xenora's choice, because there is no host file content to share
//! coherently: only paging-file-backed mappings - `INVALID_HANDLE_VALUE`
//! as the file - are supported, and they are shared memory in name only,
//! as nothing else can open them in this process-only world. Mapping a
//! real file fails honestly with ERROR_NOT_SUPPORTED rather than serving
//! content that writes could not reach. Views are zero-filled memory, as
//! documented for fresh paging-file-backed pages.

use super::{GuestObject, KERNEL32, ServicesState};
use crate::cpu::ExitReason;
use crate::imports::ImportRegistry;
use crate::memory::PROCESS_HEAP;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const MAPPING_API: &str = "api-ms-win-core-memory-l1-1-0.dll";
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_FILE_INVALID: u32 = 1006;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const INVALID_HANDLE_VALUE_FILE: u64 = u64::MAX;
const PAGE_PROTECTIONS: u32 = 0x2 | 0x4 | 0x8 | 0x10 | 0x20 | 0x40;

/// The live views: their base address, size and owning object, so that
/// unmapping releases the memory and closing the object stays honest.
#[derive(Default)]
struct Views {
    by_address: BTreeMap<u64, u64>,
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let views = Arc::new(Mutex::new(Views::default()));

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
            let mut objects = state.objects.lock().expect("guest object state");
            let Some(handle) = objects.next_handle() else {
                return state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
            };
            objects.insert_handle(handle, GuestObject::FileMapping { size });
            Ok(handle)
        });
    }

    for (symbol, all_access) in [("MapViewOfFile", false), ("MapViewOfFileEx", true)] {
        let state = Arc::clone(state);
        let views = Arc::clone(&views);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let (object, _access, offset_high, offset_low) =
                (args.rcx, args.rdx, args.r8 as u32, args.r9 as u32);
            let requested = args.get(4, memory).map_err(ExitReason::MemoryFault)?;
            let size = {
                let objects = state.objects.lock().expect("guest object state");
                match objects.handles.get(&object) {
                    Some(GuestObject::FileMapping { size }) => *size,
                    _ => return state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0),
                }
            };
            let offset = (u64::from(offset_high) << 32) | u64::from(offset_low);
            if offset != 0 || (all_access && args.get(5, memory).map_err(ExitReason::MemoryFault)? != 0) {
                // Offset views and placed views are not modelled.
                return state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            let length = if requested == 0 { size } else { requested.min(size) };
            let address = memory.map_heap_allocation(PROCESS_HEAP, length as usize);
            let Ok(address) = address else {
                return state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
            };
            views
                .lock()
                .expect("mapped views")
                .by_address
                .insert(address, length);
            Ok(address)
        });
    }

    let state_unmap = Arc::clone(state);
    let views_unmap = Arc::clone(&views);
    registry.register(KERNEL32, "UnmapViewOfFile", move |args, cpu, memory| {
        let address = args.rcx;
        let length = views_unmap
            .lock()
            .expect("mapped views")
            .by_address
            .remove(&address);
        match length {
            Some(_) if memory.release_heap_allocation(PROCESS_HEAP, address) => Ok(1),
            Some(_) => Ok(1),
            None => state_unmap.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0),
        }
    });

    for symbol in [
        "CreateFileMappingA",
        "CreateFileMappingW",
        "MapViewOfFile",
        "UnmapViewOfFile",
    ] {
        registry.alias(MAPPING_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, crate::imports::ServiceStatus::Partial);
    }
}
