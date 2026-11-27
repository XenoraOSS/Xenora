// SPDX-License-Identifier: GPL-3.0-or-later

//! Explicitly opt-in synthetic COM class, for exercising host-backed vtables.
//! This is not a Windows class or an Xbox compatibility claim.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError};
use std::collections::BTreeMap;
use std::sync::Arc;

const MODULE: &str = "xenora_com_fixture.dll";
/// Private, synthetic fixture class ID in Windows GUID memory order.
pub const CLASS_ID: [u8; 16] = [
    0x51, 0x37, 0x71, 0x58, 0x90, 0x74, 0x4e, 0x41, 0xa0, 0x58, 0x63, 0x6f, 0x6d, 0x00, 0x00, 0x01,
];
const IID_UNKNOWN: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
const IID_FACTORY: [u8; 16] = [1, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;
const E_POINTER: i32 = 0x8000_4003u32 as i32;
const E_OUTOFMEMORY: i32 = 0x8007_000eu32 as i32;
const CLASS_E_NOAGGREGATION: i32 = 0x8004_0110u32 as i32;
const UNSUPPORTED: ExitReason = ExitReason::IllegalInstruction(0xfe);
const LIMIT: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Factory,
    Object,
}
struct Object {
    kind: Kind,
    references: u32,
}
#[derive(Default)]
pub(super) struct Fixture {
    methods: Option<[u64; 5]>,
    heap: Option<u64>,
    objects: BTreeMap<u64, Object>,
}

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}
fn read_guid(memory: &GuestMemory, address: u64) -> Result<[u8; 16], ExitReason> {
    let mut guid = [0; 16];
    memory.read_bytes(address, &mut guid).map_err(fault)?;
    Ok(guid)
}
fn supports(kind: Kind, iid: &[u8; 16]) -> bool {
    *iid == IID_UNKNOWN || (kind == Kind::Factory && *iid == IID_FACTORY)
}
impl Fixture {
    fn allocate(&mut self, memory: &mut GuestMemory, kind: Kind) -> Result<u64, ExitReason> {
        let methods = self.methods.ok_or(UNSUPPORTED)?;
        if self.objects.len() == LIMIT {
            return Ok(0);
        }
        let heap = match self.heap.filter(|h| memory.has_heap(*h)) {
            Some(heap) => heap,
            None => match memory.create_private_heap(0) {
                Some(heap) => {
                    self.heap = Some(heap);
                    heap
                }
                None => return Ok(0),
            },
        };
        // Object pointer followed by an immutable-by-contract guest vtable.
        let address = match memory.map_heap_allocation(heap, 48) {
            Ok(address) => address,
            Err(_) => return Ok(0),
        };
        let mut data = [0u8; 48];
        data[..8].copy_from_slice(&(address + 8).to_le_bytes());
        let count = if kind == Kind::Factory { 5 } else { 3 };
        for (i, method) in methods[..count].iter().enumerate() {
            data[8 + i * 8..16 + i * 8].copy_from_slice(&method.to_le_bytes());
        }
        if let Err(error) = memory.write_bytes(address, &data) {
            memory.release_heap_allocation(heap, address);
            return Err(fault(error));
        }
        self.objects.insert(
            address,
            Object {
                kind,
                references: 1,
            },
        );
        Ok(address)
    }
}

/// Returns `None` when the opt-in fixture does not own this class ID.
pub(super) struct Activation<'a> {
    pub clsid: &'a [u8; 16],
    pub iid: &'a [u8; 16],
    pub outer: u64,
    pub output: u64,
    pub factory: bool,
    pub class_context: u32,
}

pub(super) fn activate(
    state: &ServicesState,
    memory: &mut GuestMemory,
    request: Activation<'_>,
) -> Result<Option<i32>, ExitReason> {
    let mut fixture = state.com_fixture.lock().expect("COM fixture");
    if fixture.methods.is_none() || *request.clsid != CLASS_ID {
        return Ok(None);
    }
    if request.class_context & 1 == 0 {
        return Ok(Some(super::com::REGDB_E_CLASSNOTREG));
    }
    let kind = if request.factory {
        Kind::Factory
    } else {
        Kind::Object
    };
    if request.outer != 0 {
        return Ok(Some(CLASS_E_NOAGGREGATION));
    }
    if !supports(kind, request.iid) {
        return Ok(Some(E_NOINTERFACE));
    }
    let address = fixture.allocate(memory, kind)?;
    if address == 0 {
        return Ok(Some(E_OUTOFMEMORY));
    }
    memory.write_u64(request.output, address).map_err(fault)?;
    Ok(Some(0))
}

pub(super) struct MultiActivation<'a> {
    pub clsid: &'a [u8; 16],
    pub outer: u64,
    pub class_context: u32,
    pub output: u64,
    pub records: Vec<u8>,
    pub iids: Vec<[u8; 16]>,
}

pub(super) fn activate_multiple(
    state: &ServicesState,
    memory: &mut GuestMemory,
    mut request: MultiActivation<'_>,
) -> Result<Option<i32>, ExitReason> {
    let mut fixture = state.com_fixture.lock().expect("COM fixture");
    if fixture.methods.is_none() || *request.clsid != CLASS_ID {
        return Ok(None);
    }
    if request.class_context & 1 == 0 {
        return Ok(Some(super::com::REGDB_E_CLASSNOTREG));
    }
    if request.outer != 0 {
        return Ok(Some(CLASS_E_NOAGGREGATION));
    }
    let supported = request
        .iids
        .iter()
        .filter(|iid| supports(Kind::Object, iid))
        .count();
    let address = if supported == 0 {
        0
    } else {
        fixture.allocate(memory, Kind::Object)?
    };
    if supported != 0 && address == 0 {
        return Ok(Some(E_OUTOFMEMORY));
    }
    if address != 0 {
        fixture
            .objects
            .get_mut(&address)
            .expect("new fixture object")
            .references = supported as u32;
    }
    for (index, iid) in request.iids.iter().enumerate() {
        let offset = index * 24;
        let found = supports(Kind::Object, iid);
        request.records[offset + 8..offset + 16]
            .copy_from_slice(&(if found { address } else { 0 }).to_le_bytes());
        let hr = if found { 0 } else { E_NOINTERFACE as u32 };
        request.records[offset + 16..offset + 20].copy_from_slice(&hr.to_le_bytes());
    }
    if let Err(error) = memory.write_bytes(request.output, &request.records) {
        if address != 0 {
            if let Some(heap) = fixture.heap {
                memory.release_heap_allocation(heap, address);
            }
            fixture.objects.remove(&address);
        }
        return Err(fault(error));
    }
    Ok(Some(if supported == 0 {
        E_NOINTERFACE
    } else if supported == request.iids.len() {
        0
    } else {
        0x0008_0012
    }))
}

/// Installs a synthetic class for original fixture programs only. Standard
/// runtime registration does not enable this class.
pub fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let state_query = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "QueryInterface",
        ServiceStatus::Partial,
        move |args, _, memory| {
            if args.r8 == 0 {
                return Ok(E_POINTER as u32 as u64);
            }
            memory.write_u64(args.r8, 0).map_err(fault)?;
            let iid = read_guid(memory, args.rdx)?;
            let mut fixture = state_query.com_fixture.lock().expect("COM fixture");
            let object = fixture.objects.get_mut(&args.rcx).ok_or(UNSUPPORTED)?;
            if !supports(object.kind, &iid) {
                return Ok(E_NOINTERFACE as u32 as u64);
            }
            object.references = object.references.checked_add(1).ok_or(UNSUPPORTED)?;
            memory.write_u64(args.r8, args.rcx).map_err(fault)?;
            Ok(0)
        },
    );
    let state_add = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "AddRef",
        ServiceStatus::Partial,
        move |args, _, _| {
            let mut fixture = state_add.com_fixture.lock().expect("COM fixture");
            let object = fixture.objects.get_mut(&args.rcx).ok_or(UNSUPPORTED)?;
            object.references = object.references.checked_add(1).ok_or(UNSUPPORTED)?;
            Ok(u64::from(object.references))
        },
    );
    let state_release = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "Release",
        ServiceStatus::Partial,
        move |args, _, memory| {
            let mut fixture = state_release.com_fixture.lock().expect("COM fixture");
            let object = fixture.objects.get_mut(&args.rcx).ok_or(UNSUPPORTED)?;
            object.references -= 1;
            let remaining = object.references;
            if remaining == 0 {
                let heap = fixture.heap.ok_or(UNSUPPORTED)?;
                if !memory.release_heap_allocation(heap, args.rcx) {
                    return Err(UNSUPPORTED);
                }
                fixture.objects.remove(&args.rcx);
            }
            Ok(u64::from(remaining))
        },
    );
    let state_create = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "CreateInstance",
        ServiceStatus::Partial,
        move |args, _, memory| {
            if args.r9 == 0 {
                return Ok(E_POINTER as u32 as u64);
            }
            memory.write_u64(args.r9, 0).map_err(fault)?;
            let iid = read_guid(memory, args.r8)?;
            let mut fixture = state_create.com_fixture.lock().expect("COM fixture");
            if !matches!(
                fixture.objects.get(&args.rcx),
                Some(Object {
                    kind: Kind::Factory,
                    ..
                })
            ) {
                return Err(UNSUPPORTED);
            }
            if args.rdx != 0 {
                return Ok(CLASS_E_NOAGGREGATION as u32 as u64);
            }
            if !supports(Kind::Object, &iid) {
                return Ok(E_NOINTERFACE as u32 as u64);
            }
            let address = fixture.allocate(memory, Kind::Object)?;
            if address == 0 {
                return Ok(E_OUTOFMEMORY as u32 as u64);
            }
            memory.write_u64(args.r9, address).map_err(fault)?;
            Ok(0)
        },
    );
    registry.register_with_status(MODULE, "LockServer", ServiceStatus::Partial, |_, _, _| {
        Err(UNSUPPORTED)
    });
    let trampolines = registry.dynamic_trampolines();
    let names = [
        "QueryInterface",
        "AddRef",
        "Release",
        "CreateInstance",
        "LockServer",
    ];
    let methods = names.map(|name| {
        trampolines.expose(
            registry
                .service_index(MODULE, name)
                .expect("COM fixture method"),
        )
    });
    state.com_fixture.lock().expect("COM fixture").methods = Some(methods);
}
