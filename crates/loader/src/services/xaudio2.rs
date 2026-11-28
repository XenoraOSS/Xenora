// SPDX-License-Identifier: GPL-3.0-or-later

//! The guest-visible XAudio2 objects of the loader.
//!
//! The loader owns the guest side of the XAudio2 API: the objects a guest
//! holds a pointer to, the vtables it calls through, and the checks every
//! guest pointer passes before the bounded core model in `crates/xaudio2`
//! sees it. The core model owns the rules; this module owns the boundary.
//!
//! The private module name `xenora_xaudio2.dll` holds the interface
//! methods. It is not a Windows module and not a compatibility claim: the
//! guest never names it. The public module `xaudio2_9.dll` holds the two
//! exports the guest imports.

use crate::cpu::{CpuState, ExitReason};
use crate::imports::{ImportRegistry, ServiceStatus, Win64Args};
use crate::memory::{GuestMemory, MemoryError};
use crate::services::ServicesState;
use std::collections::BTreeMap;
use std::sync::Arc;
use xenora_xaudio2::hresult::{E_POINTER, S_OK};
use xenora_xaudio2::{EngineId, Runtime, VoiceId};

/// The private module the interface methods are registered in.
pub const MODULE: &str = "xenora_xaudio2.dll";

/// E_NOINTERFACE: the interface identifier is not supported.
pub(crate) const E_NOINTERFACE: u32 = 0x8000_4002;

/// IID_IUnknown in the memory order of a Windows GUID.
pub(crate) const IID_IUNKNOWN: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
/// IID_IXAudio2 of XAudio2 2.9 in the memory order of a Windows GUID.
pub(crate) const IID_IXAUDIO2: [u8; 16] = [
    0xcf, 0xe3, 0x02, 0x2b, 0x0b, 0x2e, 0xc3, 0x4e, 0xbe, 0x45, 0x1b, 0x2a, 0x3f, 0xe7, 0x21, 0x0d,
];

/// What a guest object is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// The engine object of IXAudio2.
    Engine,
    /// A source voice.
    Source,
    /// A mastering voice.
    Mastering,
}

/// One guest object: the engine or one of its voices.
pub(crate) struct Object {
    /// What the object is.
    pub kind: Kind,
    /// The engine the object belongs to.
    pub engine: EngineId,
    /// The voice the object is, when it is a voice.
    pub voice: Option<VoiceId>,
    /// The number of references the guest holds.
    pub references: u32,
    /// Whether the guest destroyed the object.
    pub destroyed: bool,
    /// The version argument of XAudio2CreateWithVersionInfo, zero when the
    /// guest called XAudio2Create.
    pub version: u32,
}

/// The counters of the calls the loader refused.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    /// Calls that reported E_NOTIMPL.
    pub not_implemented: u64,
    /// Calls that reported E_POINTER.
    pub pointer: u64,
    /// Calls that reported XAUDIO2_E_INVALID_CALL.
    pub invalid_call: u64,
    /// Calls that wrote nothing because they cannot report an error.
    pub silent: u64,
}

/// The XAudio2 objects one guest process owns.
pub(crate) struct XAudio2 {
    /// The bounded core model.
    pub runtime: Runtime,
    /// The guest objects, by guest address.
    pub objects: BTreeMap<u64, Object>,
    /// The private heap the guest objects live in.
    pub heap: Option<u64>,
    /// The guest addresses of the IXAudio2 methods.
    pub engine_vtable: [u64; 13],
    /// The guest addresses of the IXAudio2Voice methods.
    pub voice_vtable: [u64; 19],
    /// The guest addresses of the IXAudio2SourceVoice methods.
    pub source_vtable: [u64; 29],
    /// The guest addresses of the IXAudio2MasteringVoice methods.
    pub mastering_vtable: [u64; 20],
    /// The counters of the calls the loader refused.
    pub diagnostics: Diagnostics,
}

impl Default for XAudio2 {
    fn default() -> Self {
        Self {
            runtime: Runtime::new(),
            objects: BTreeMap::new(),
            heap: None,
            engine_vtable: [0; 13],
            voice_vtable: [0; 19],
            source_vtable: [0; 29],
            mastering_vtable: [0; 20],
            diagnostics: Diagnostics::default(),
        }
    }
}

impl XAudio2 {
    /// The engine a guest object belongs to, when the guest owns one.
    pub(crate) fn engine_of(&self, address: u64) -> Option<EngineId> {
        let object = self.objects.get(&address)?;
        if object.destroyed || object.kind != Kind::Engine {
            return None;
        }
        Some(object.engine)
    }

    /// The voice a guest object is, when the guest owns a live voice there.
    pub(crate) fn voice_of(&self, address: u64, kind: Kind) -> Option<VoiceId> {
        let object = self.objects.get(&address)?;
        if object.destroyed || object.kind != kind {
            return None;
        }
        object.voice
    }

    /// The voice a guest object is, when it is a voice of any kind.
    pub(crate) fn any_voice_of(&self, address: u64) -> Option<VoiceId> {
        let object = self.objects.get(&address)?;
        if object.destroyed || object.kind == Kind::Engine {
            return None;
        }
        object.voice
    }

    /// Allocates a guest object with its vtable.
    ///
    /// The object pointer the guest holds points at the first word of the
    /// allocation, and that word holds the address of the vtable, which
    /// follows it in the same allocation. The guest never sees the vtable
    /// as a separate object.
    pub(crate) fn allocate(&mut self, memory: &mut GuestMemory, vtable: &[u64]) -> Option<u64> {
        let heap = match self.heap.filter(|heap| memory.has_heap(*heap)) {
            Some(heap) => heap,
            None => {
                let heap = memory.create_private_heap(0)?;
                self.heap = Some(heap);
                heap
            }
        };
        let size = 8 + vtable.len() * 8;
        let address = memory.map_heap_allocation(heap, size).ok()?;
        let mut data = vec![0u8; size];
        data[..8].copy_from_slice(&(address + 8).to_le_bytes());
        for (index, entry) in vtable.iter().enumerate() {
            let at = 8 + index * 8;
            data[at..at + 8].copy_from_slice(&entry.to_le_bytes());
        }
        if memory.write_bytes(address, &data).is_err() {
            memory.release_heap_allocation(heap, address);
            return None;
        }
        Some(address)
    }

    /// Frees the guest objects of one engine.
    pub(crate) fn free_objects(&mut self, memory: &mut GuestMemory, engine: EngineId) {
        let addresses: Vec<u64> = self
            .objects
            .iter()
            .filter(|(_, object)| object.engine == engine)
            .map(|(address, _)| *address)
            .collect();
        for address in addresses {
            if let Some(heap) = self.heap {
                memory.release_heap_allocation(heap, address);
            }
            self.objects.remove(&address);
        }
    }

    /// Frees one guest object.
    pub(crate) fn free(&mut self, memory: &mut GuestMemory, address: u64) {
        if let Some(heap) = self.heap {
            memory.release_heap_allocation(heap, address);
        }
    }
}

/// A guest method of one of the XAudio2 interfaces.
pub(crate) type Method =
    fn(&ServicesState, &mut GuestMemory, &Win64Args, &CpuState) -> Result<u64, ExitReason>;

/// Reads a fixed size structure from the guest.
pub(crate) fn read_struct<const N: usize>(memory: &GuestMemory, address: u64) -> Option<[u8; N]> {
    let mut bytes = [0u8; N];
    memory.read_bytes(address, &mut bytes).ok()?;
    Some(bytes)
}

/// Reads an interface identifier from the guest.
pub(crate) fn read_guid(memory: &GuestMemory, address: u64) -> Option<[u8; 16]> {
    read_struct::<16>(memory, address)
}

/// Writes a guest pointer, and reports whether the guest could read it.
pub(crate) fn write_pointer(memory: &mut GuestMemory, address: u64, value: u64) -> bool {
    memory.write_u64(address, value).is_ok()
}

/// Registers the XAudio2 services and exposes their guest addresses.
pub(crate) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let engine = expose(
        registry,
        state,
        &crate::services::xaudio2_engine::ENGINE_METHODS,
    );
    let voice = expose(
        registry,
        state,
        &crate::services::xaudio2_voice::VOICE_METHODS,
    );
    let source = expose(
        registry,
        state,
        &crate::services::xaudio2_source::SOURCE_METHODS,
    );
    let mastering = expose(
        registry,
        state,
        &crate::services::xaudio2_source::MASTERING_METHODS,
    );
    let mut xaudio2 = state.xaudio2();
    xaudio2.engine_vtable = engine.try_into().expect("the IXAudio2 vtable");
    xaudio2.voice_vtable = voice.clone().try_into().expect("the voice vtable");
    let mut source_vtable = voice.clone();
    source_vtable.extend(source);
    xaudio2.source_vtable = source_vtable.try_into().expect("the source voice vtable");
    let mut mastering_vtable = voice.clone();
    mastering_vtable.extend(mastering);
    xaudio2.mastering_vtable = mastering_vtable.try_into().expect("the mastering vtable");
}

/// Registers the methods and returns the guest address of each of them.
fn expose(
    registry: &mut ImportRegistry,
    state: &Arc<ServicesState>,
    methods: &[(&str, Method)],
) -> Vec<u64> {
    let mut table = Vec::with_capacity(methods.len());
    for (name, method) in methods {
        let method = *method;
        let owned = Arc::clone(state);
        registry.register_with_status(
            MODULE,
            name,
            ServiceStatus::Partial,
            move |args, cpu, memory| method(&owned, memory, args, cpu),
        );
        let index = registry
            .service_index(MODULE, name)
            .expect("the XAudio2 method");
        table.push(registry.dynamic_trampolines().expose(index));
    }
    table
}

/// Reports a guest memory failure as a host fault.
pub(crate) fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

/// Creates the engine a guest asked for and hands the guest its pointer.
pub(crate) fn create_engine(
    state: &ServicesState,
    memory: &mut GuestMemory,
    out: u64,
    flags: u32,
    processor: u32,
    version: u32,
) -> u64 {
    if out == 0 {
        return E_POINTER as u64;
    }
    let mut xaudio2 = state.xaudio2();
    if !write_pointer(memory, out, 0) {
        xaudio2.diagnostics.pointer += 1;
        return E_POINTER as u64;
    }
    let engine = match xaudio2.runtime.create_engine(flags, processor) {
        Ok(engine) => engine,
        Err(error) => return error.hresult() as u64,
    };
    let vtable = xaudio2.engine_vtable;
    let Some(address) = xaudio2.allocate(memory, &vtable) else {
        let _ = xaudio2.runtime.release_engine(engine);
        return xenora_xaudio2::hresult::E_OUTOFMEMORY as u64;
    };
    xaudio2.objects.insert(
        address,
        Object {
            kind: Kind::Engine,
            engine,
            voice: None,
            references: 1,
            destroyed: false,
            version,
        },
    );
    if !write_pointer(memory, out, address) {
        xaudio2.objects.remove(&address);
        if let Some(heap) = xaudio2.heap {
            memory.release_heap_allocation(heap, address);
        }
        let _ = xaudio2.runtime.release_engine(engine);
        xaudio2.diagnostics.pointer += 1;
        return E_POINTER as u64;
    }
    S_OK as u64
}
