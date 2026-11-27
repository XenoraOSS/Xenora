// SPDX-License-Identifier: GPL-3.0-or-later

//! The COM base surface: the apartment a guest thread initializes, the GUID
//! constructors and parsers, the task allocator and the `PROPVARIANT` clear.
//!
//! Sources: Microsoft Learn and the public headers `combaseapi.h` and
//! `objbase.h` (`CoInitializeEx`, `CoUninitialize`, `CoCreateGuid`,
//! `StringFromGUID2`, `CLSIDFromString`, `CoCreateInstance`, `CoTaskMemAlloc`,
//! `CoTaskMemFree`, `CoTaskMemRealloc`), the `COINIT` enumeration, and
//! `propidl.h` for the `PROPVARIANT` layout and `PropVariantClear`.
//!
//! The normal runtime has no class registry. An explicitly enabled synthetic
//! fixture supplies one class for local host-vtable tests. `PropVariantClear`
//! serves value types and task-allocated strings and refuses interface and
//! BSTR ownership, whose release paths are not modelled.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::memory::{GuestMemory, MemoryError};
use crate::runtime::{OutPtr, ServiceContext};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// `S_OK`.
pub(crate) const S_OK: i32 = 0;
/// `S_FALSE`: the apartment was already initialized on this thread.
pub(crate) const S_FALSE: i32 = 1;
/// `E_INVALIDARG`.
pub(crate) const E_INVALIDARG: i32 = 0x8007_0057u32 as i32;
/// `E_POINTER`.
pub(crate) const E_POINTER: i32 = 0x8000_4003u32 as i32;
/// `E_UNEXPECTED`: the host had no entropy for a new GUID.
pub(crate) const E_UNEXPECTED: i32 = 0x8000_FFFFu32 as i32;
/// `RPC_E_CHANGED_MODE`: the thread already chose the other apartment model.
pub(crate) const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;
/// `CO_E_CLASSSTRING`: the text is not a CLSID string.
pub(crate) const CO_E_CLASSSTRING: i32 = 0x8004_01F3u32 as i32;
/// `REGDB_E_CLASSNOTREG`: no class is registered under the CLSID.
pub(crate) const REGDB_E_CLASSNOTREG: i32 = 0x8004_0154u32 as i32;
/// COM has not been initialized on the calling thread.
pub(crate) const CO_E_NOTINITIALIZED: i32 = 0x8004_01f0u32 as i32;

const COINIT_APARTMENTTHREADED: u32 = 0x2;
const COINIT_DISABLE_OLE1DDE: u32 = 0x4;
const COINIT_SPEED_OVER_MEMORY: u32 = 0x8;
/// The `COINIT` bits `CoInitializeEx` accepts. `COINIT_MULTITHREADED` is zero,
/// so the apartment bit alone selects the model.
const COINIT_ACCEPTED: u32 =
    COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE | COINIT_SPEED_OVER_MEMORY;

/// `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` is 38 units; with the terminator
/// it is 39, the documented minimum for `StringFromGUID2`.
const GUID_TEXT_UNITS: i32 = 39;
const GUID_TEXT_BYTES: usize = GUID_TEXT_UNITS as usize * 2;
/// A CLSID string is never longer than its braced form; anything longer is not
/// one, and bounding the scan keeps a missing terminator from running away.
const MAX_GUID_TEXT_UNITS: usize = 64;

/// `PROPVARIANT` on x64: the type word, three reserved words, then a 16-byte
/// union.
const PROPVARIANT_BYTES: usize = 24;
const VT_EMPTY: u16 = 0;
const VT_NULL: u16 = 1;
const VT_I2: u16 = 2;
const VT_I4: u16 = 3;
const VT_R4: u16 = 4;
const VT_R8: u16 = 5;
const VT_CY: u16 = 6;
const VT_DATE: u16 = 7;
const VT_ERROR: u16 = 10;
const VT_BOOL: u16 = 11;
const VT_I1: u16 = 16;
const VT_UI1: u16 = 17;
const VT_UI2: u16 = 18;
const VT_UI4: u16 = 19;
const VT_I8: u16 = 20;
const VT_UI8: u16 = 21;
const VT_INT: u16 = 22;
const VT_UINT: u16 = 23;
const VT_LPSTR: u16 = 30;
const VT_LPWSTR: u16 = 31;
const VT_FILETIME: u16 = 64;
const VT_TYPEMASK: u16 = 0x0fff;

/// The exit for a request the runtime does not model.
const NOT_MODELLED: ExitReason = ExitReason::IllegalInstruction(0xFE);

type Outcome<T> = Result<T, ExitReason>;

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

/// One guest thread's COM apartment.
#[derive(Clone, Copy)]
struct Apartment {
    /// True for a single-threaded apartment (`COINIT_APARTMENTTHREADED`).
    single_threaded: bool,
    /// Outstanding successful initializations on the thread.
    references: u32,
}

/// The COM state the services share.
#[derive(Default)]
pub(crate) struct Com {
    /// The apartment of each initialized guest thread, keyed by TEB.
    apartments: BTreeMap<u64, Apartment>,
    /// The private heap the task allocator hands blocks out of. It is separate
    /// from the process heap so that a block freed with the wrong free cannot
    /// appear to succeed.
    task_heap: Option<u64>,
}

impl Com {
    pub(super) fn retire_thread(&mut self, teb: u64) {
        self.apartments.remove(&teb);
    }

    fn task_heap(&self, memory: &GuestMemory) -> Option<u64> {
        self.task_heap.filter(|heap| memory.has_heap(*heap))
    }
}

/// `CoInitializeEx`: record the apartment for the calling thread.
pub(crate) fn co_initialize_ex(
    context: &mut ServiceContext<'_>,
    reserved: u64,
    model: u32,
) -> Outcome<i32> {
    // The documented contract: `pvReserved` must be NULL.
    if reserved != 0 || model & !COINIT_ACCEPTED != 0 {
        return Ok(E_INVALIDARG);
    }
    let single_threaded = model & COINIT_APARTMENTTHREADED != 0;
    let mut com = context.state.com();
    match com.apartments.get_mut(&context.teb) {
        Some(apartment) if apartment.single_threaded == single_threaded => {
            apartment.references = apartment.references.checked_add(1).ok_or(NOT_MODELLED)?;
            Ok(S_FALSE)
        }
        Some(_) => Ok(RPC_E_CHANGED_MODE),
        None => {
            com.apartments.insert(
                context.teb,
                Apartment {
                    single_threaded,
                    references: 1,
                },
            );
            Ok(S_OK)
        }
    }
}

pub(crate) fn co_initialize(context: &mut ServiceContext<'_>, reserved: u64) -> Outcome<i32> {
    co_initialize_ex(context, reserved, COINIT_APARTMENTTHREADED)
}

/// `CoUninitialize`: release one initialization of the calling thread.
pub(crate) fn co_uninitialize(context: &mut ServiceContext<'_>) -> Outcome<()> {
    let mut com = context.state.com();
    if let Some(apartment) = com.apartments.get_mut(&context.teb) {
        apartment.references = apartment.references.saturating_sub(1);
        if apartment.references == 0 {
            com.apartments.remove(&context.teb);
        }
    }
    Ok(())
}

/// `CoCreateGuid`: a version 4 (random) GUID.
///
/// Xenora chooses the version-4 layout and obtains randomness from the host.
pub(crate) fn co_create_guid(context: &mut ServiceContext<'_>, guid: OutPtr<16>) -> Outcome<i32> {
    let mut bytes = [0u8; 16];
    if !host_entropy(&mut bytes) {
        return Ok(E_UNEXPECTED);
    }
    bytes[7] = (bytes[7] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    guid.store(context.memory, bytes).map_err(fault)?;
    Ok(S_OK)
}

/// Fill `bytes` from the host's cryptographic entropy source.
fn host_entropy(bytes: &mut [u8]) -> bool {
    #[cfg(unix)]
    {
        // `getentropy` accepts at most 256 bytes; every caller here asks for
        // sixteen.
        debug_assert!(bytes.len() <= 256);
        // SAFETY: `bytes` is a live, writable slice of the length passed, and
        // the buffer does not outlive the call.
        unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = bytes;
        false
    }
}

/// `StringFromGUID2`: render a GUID as `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`.
pub(crate) fn string_from_guid2(
    context: &mut ServiceContext<'_>,
    guid: u64,
    text: u64,
    capacity: i32,
) -> Outcome<i32> {
    // The documented answer when the buffer cannot hold the text and its
    // terminator: nothing is written and zero is returned.
    if capacity < GUID_TEXT_UNITS {
        return Ok(0);
    }
    let mut bytes = [0u8; 16];
    context.memory.read_bytes(guid, &mut bytes).map_err(fault)?;
    let mut encoded = [0u8; GUID_TEXT_BYTES];
    for (index, unit) in format_guid(&bytes).encode_utf16().enumerate() {
        encoded[index * 2..index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
    }
    context.memory.write_bytes(text, &encoded).map_err(fault)?;
    Ok(GUID_TEXT_UNITS)
}

/// `CLSIDFromString`: parse the braced or bare CLSID text form.
pub(crate) fn clsid_from_string(
    context: &mut ServiceContext<'_>,
    text: u64,
    clsid: OutPtr<16>,
) -> Outcome<i32> {
    if text == 0 {
        clsid.store(context.memory, [0; 16]).map_err(fault)?;
        return Ok(S_OK);
    }
    let mut units = Vec::new();
    let mut at = text;
    loop {
        let unit = context.memory.read_u16(at).map_err(fault)?;
        if unit == 0 {
            break;
        }
        if units.len() >= MAX_GUID_TEXT_UNITS {
            return Ok(CO_E_CLASSSTRING);
        }
        units.push(unit);
        at = at
            .checked_add(2)
            .ok_or_else(|| fault(MemoryError::Overflow))?;
    }
    match parse_guid(&units) {
        Some(bytes) => {
            clsid.store(context.memory, bytes).map_err(fault)?;
            Ok(S_OK)
        }
        // The documented answer for text that is not a CLSID string; the
        // output is left alone.
        None => Ok(CO_E_CLASSSTRING),
    }
}

/// Parse IID text using the same bounded GUID decoder; malformed text returns
/// E_INVALIDARG as a Xenora policy pending independent Windows comparison.
pub(crate) fn iid_from_string(
    context: &mut ServiceContext<'_>,
    text: u64,
    iid: OutPtr<16>,
) -> Outcome<i32> {
    let result = clsid_from_string(context, text, iid)?;
    Ok(if result == CO_E_CLASSSTRING {
        E_INVALIDARG
    } else {
        result
    })
}

/// `CoTaskMemAlloc`: a block from the task allocator.
pub(crate) fn co_task_mem_alloc(context: &mut ServiceContext<'_>, size: u64) -> Outcome<u64> {
    let Some(heap) = task_heap(context.state, context.memory) else {
        return Ok(0);
    };
    Ok(context
        .memory
        .map_heap_allocation(heap, size.max(1) as usize)
        .unwrap_or(0))
}

/// `CoTaskMemFree`: release a block the task allocator handed out.
///
/// Freeing the null pointer is the documented no-op. A block the task
/// allocator does not own ends the guest as a Xenora policy; behavior for a
/// mismatched free is not claimed to match Windows.
pub(crate) fn co_task_mem_free(context: &mut ServiceContext<'_>, address: u64) -> Outcome<()> {
    if address == 0 {
        return Ok(());
    }
    let Some(heap) = context.state.com().task_heap(context.memory) else {
        return Err(fault(MemoryError::UnmappedAddress(address)));
    };
    if !context.memory.release_heap_allocation(heap, address) {
        return Err(fault(MemoryError::UnmappedAddress(address)));
    }
    Ok(())
}

/// `CoTaskMemRealloc`: resize a task block, keeping its contents.
pub(crate) fn co_task_mem_realloc(
    context: &mut ServiceContext<'_>,
    address: u64,
    size: u64,
) -> Outcome<u64> {
    if address == 0 {
        return co_task_mem_alloc(context, size);
    }
    let Some(heap) = context.state.com().task_heap(context.memory) else {
        return Err(fault(MemoryError::UnmappedAddress(address)));
    };
    // The documented answer for a zero size: the block is freed and NULL is
    // returned.
    if size == 0 {
        if !context.memory.release_heap_allocation(heap, address) {
            return Err(fault(MemoryError::UnmappedAddress(address)));
        }
        return Ok(0);
    }
    match context
        .memory
        .resize_heap_allocation(heap, address, size as usize, false)
    {
        Ok(base) => Ok(base),
        Err(MemoryError::UnmappedAddress(_)) => Err(fault(MemoryError::UnmappedAddress(address))),
        // A failed grow leaves the original block in place and answers NULL.
        Err(_) => Ok(0),
    }
}

/// `CoCreateInstance`: no class is registered, so the documented
/// `REGDB_E_CLASSNOTREG` is the honest answer.
pub(crate) fn co_create_instance(
    context: &mut ServiceContext<'_>,
    clsid: u64,
    outer: u64,
    class_context: u32,
    iid: u64,
    object: u64,
) -> Outcome<i32> {
    if object == 0 {
        return Ok(E_POINTER);
    }
    // The documented contract: `*ppv` is NULL whenever the call fails.
    context.memory.write_u64(object, 0).map_err(fault)?;
    if clsid == 0 || iid == 0 {
        return Ok(E_INVALIDARG);
    }
    if !context.state.com().apartments.contains_key(&context.teb) {
        return Ok(CO_E_NOTINITIALIZED);
    }
    let mut class = [0u8; 16];
    let mut interface = [0u8; 16];
    context
        .memory
        .read_bytes(clsid, &mut class)
        .map_err(fault)?;
    context
        .memory
        .read_bytes(iid, &mut interface)
        .map_err(fault)?;
    if let Some(result) = super::com_fixture::activate(
        context.state,
        context.memory,
        super::com_fixture::Activation {
            clsid: &class,
            iid: &interface,
            outer,
            output: object,
            factory: false,
            class_context,
        },
    )? {
        return Ok(result);
    }
    // An outer object is refused only after a class is found.
    Ok(REGDB_E_CLASSNOTREG)
}

/// Class lookup has the same local class-registry boundary as activation.
pub(crate) fn co_get_class_object(
    context: &mut ServiceContext<'_>,
    clsid: u64,
    class_context: u32,
    server_info: u64,
    iid: u64,
    object: u64,
) -> Outcome<i32> {
    if server_info != 0 {
        return Err(NOT_MODELLED);
    }
    if object == 0 {
        return Ok(E_POINTER);
    }
    context.memory.write_u64(object, 0).map_err(fault)?;
    if clsid == 0 || iid == 0 {
        return Ok(E_INVALIDARG);
    }
    if !context.state.com().apartments.contains_key(&context.teb) {
        return Ok(CO_E_NOTINITIALIZED);
    }
    let mut class = [0u8; 16];
    let mut interface = [0u8; 16];
    context
        .memory
        .read_bytes(clsid, &mut class)
        .map_err(fault)?;
    context
        .memory
        .read_bytes(iid, &mut interface)
        .map_err(fault)?;
    if let Some(result) = super::com_fixture::activate(
        context.state,
        context.memory,
        super::com_fixture::Activation {
            clsid: &class,
            iid: &interface,
            outer: 0,
            output: object,
            factory: true,
            class_context,
        },
    )? {
        return Ok(result);
    }
    Ok(REGDB_E_CLASSNOTREG)
}

/// Local activation is limited to the opt-in fixture class. Remote activation
/// and requests larger than the bounded guest interface array are unsupported.
pub(crate) fn co_create_instance_ex(
    context: &mut ServiceContext<'_>,
    clsid: u64,
    outer: u64,
    class_context: u32,
    server_info: u64,
    count: u32,
    results: u64,
) -> Outcome<i32> {
    if clsid == 0 || count == 0 || results == 0 {
        return Ok(E_INVALIDARG);
    }
    if server_info != 0 || count > 64 {
        return Err(NOT_MODELLED);
    }
    if !context.state.com().apartments.contains_key(&context.teb) {
        return Ok(CO_E_NOTINITIALIZED);
    }
    let mut class = [0u8; 16];
    context
        .memory
        .read_bytes(clsid, &mut class)
        .map_err(fault)?;
    let length = count as usize * 24;
    context.memory.check_write(results, length).map_err(fault)?;
    let mut records = vec![0; length];
    context
        .memory
        .read_bytes(results, &mut records)
        .map_err(fault)?;
    let mut iids = Vec::with_capacity(count as usize);
    for index in 0..count {
        let entry = results
            .checked_add(u64::from(index) * 24)
            .ok_or_else(|| fault(MemoryError::Overflow))?;
        let iid = context.memory.read_u64(entry).map_err(fault)?;
        let interface = context.memory.read_u64(entry + 8).map_err(fault)?;
        let result = context.memory.read_u32(entry + 16).map_err(fault)?;
        if iid == 0 || interface != 0 || result != 0 {
            return Ok(E_INVALIDARG);
        }
        let mut guid = [0u8; 16];
        context.memory.read_bytes(iid, &mut guid).map_err(fault)?;
        iids.push(guid);
    }
    if let Some(result) = super::com_fixture::activate_multiple(
        context.state,
        context.memory,
        super::com_fixture::MultiActivation {
            clsid: &class,
            outer,
            class_context,
            output: results,
            records,
            iids,
        },
    )? {
        return Ok(result);
    }
    // No QueryInterface occurs when class lookup fails. The exact Windows
    // output-array behavior for this failure has not been established.
    Ok(REGDB_E_CLASSNOTREG)
}

/// `PropVariantClear`: release what the variant owns and clear it to
/// `VT_EMPTY`.
pub(crate) fn prop_variant_clear(context: &mut ServiceContext<'_>, variant: u64) -> Outcome<i32> {
    if variant == 0 {
        // The public contract requires a pointer and does not specify the
        // result for NULL. Do not invent a successful clear.
        return Err(NOT_MODELLED);
    }
    let vt = context.memory.read_u16(variant).map_err(fault)?;
    // Arrays, vectors and by-reference forms own storage this runtime does not
    // model; refuse rather than leak or free something else's memory.
    if vt & !VT_TYPEMASK != 0 {
        return Err(NOT_MODELLED);
    }
    // Do not release owned storage if the result cannot be written back.
    context
        .memory
        .check_write(variant, PROPVARIANT_BYTES)
        .map_err(fault)?;
    match vt & VT_TYPEMASK {
        VT_EMPTY | VT_NULL => {}
        VT_LPSTR | VT_LPWSTR => {
            let pointer_address = variant
                .checked_add(8)
                .ok_or_else(|| fault(MemoryError::Overflow))?;
            let pointer = context.memory.read_u64(pointer_address).map_err(fault)?;
            if pointer != 0 {
                let Some(heap) = context.state.com().task_heap(context.memory) else {
                    return Err(fault(MemoryError::UnmappedAddress(pointer)));
                };
                if !context.memory.release_heap_allocation(heap, pointer) {
                    return Err(fault(MemoryError::UnmappedAddress(pointer)));
                }
            }
        }
        // Value types carry no owned storage.
        VT_I1 | VT_UI1 | VT_I2 | VT_UI2 | VT_I4 | VT_UI4 | VT_I8 | VT_UI8 | VT_INT | VT_UINT
        | VT_R4 | VT_R8 | VT_BOOL | VT_ERROR | VT_CY | VT_DATE | VT_FILETIME => {}
        // `VT_BSTR` needs `SysFreeString`, and `VT_UNKNOWN`/`VT_DISPATCH` need
        // `Release`; neither exists here, so the variant is left untouched.
        _ => return Err(NOT_MODELLED),
    }
    context
        .memory
        .write_bytes(variant, &[0u8; PROPVARIANT_BYTES])
        .map_err(fault)?;
    Ok(S_OK)
}

/// The task allocator's heap, created on first use.
fn task_heap(state: &ServicesState, memory: &mut GuestMemory) -> Option<u64> {
    let stored = state.com().task_heap;
    if let Some(heap) = stored
        && memory.has_heap(heap)
    {
        return Some(heap);
    }
    let heap = memory.create_private_heap(0)?;
    state.com().task_heap = Some(heap);
    Some(heap)
}

/// `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`, uppercase as Windows renders it.
fn format_guid(bytes: &[u8; 16]) -> String {
    let data1 = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let data2 = u16::from_le_bytes([bytes[4], bytes[5]]);
    let data3 = u16::from_le_bytes([bytes[6], bytes[7]]);
    let mut text = format!("{{{data1:08X}-{data2:04X}-{data3:04X}-");
    push_hex(&mut text, &bytes[8..10]);
    text.push('-');
    push_hex(&mut text, &bytes[10..16]);
    text.push('}');
    text
}

fn push_hex(text: &mut String, bytes: &[u8]) {
    for byte in bytes {
        let _ = write!(text, "{byte:02X}");
    }
}

fn hex_digit(unit: u16) -> Option<u8> {
    match unit {
        0x30..=0x39 => Some((unit - 0x30) as u8),
        0x41..=0x46 => Some((unit - 0x41 + 10) as u8),
        0x61..=0x66 => Some((unit - 0x61 + 10) as u8),
        _ => None,
    }
}

/// Parse `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` or the bare form. The first
/// three fields are little-endian in memory, as the GUID layout documents.
fn parse_guid(units: &[u16]) -> Option<[u8; 16]> {
    let inner = match units {
        [open, rest @ .., close] if *open == u16::from(b'{') && *close == u16::from(b'}') => rest,
        _ => units,
    };
    if inner.len() != 36 {
        return None;
    }
    let mut digits = [0u8; 32];
    let mut count = 0;
    for (index, unit) in inner.iter().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            if *unit != u16::from(b'-') {
                return None;
            }
            continue;
        }
        digits[count] = hex_digit(*unit)?;
        count += 1;
    }
    if count != 32 {
        return None;
    }
    let mut fields = [0u8; 16];
    for (index, pair) in digits.as_chunks::<2>().0.iter().enumerate() {
        fields[index] = (pair[0] << 4) | pair[1];
    }
    let mut guid = [0u8; 16];
    guid[0] = fields[3];
    guid[1] = fields[2];
    guid[2] = fields[1];
    guid[3] = fields[0];
    guid[4] = fields[5];
    guid[5] = fields[4];
    guid[6] = fields[7];
    guid[7] = fields[6];
    guid[8..].copy_from_slice(&fields[8..]);
    Some(guid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_text_round_trips_through_the_memory_layout() {
        let bytes: [u8; 16] = [
            0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        assert_eq!(
            format_guid(&bytes),
            "{00112233-4455-6677-8899-AABBCCDDEEFF}"
        );
        let units: Vec<u16> = format_guid(&bytes).encode_utf16().collect();
        assert_eq!(parse_guid(&units), Some(bytes));
        let bare: Vec<u16> = "00112233-4455-6677-8899-aabbccddeeff"
            .encode_utf16()
            .collect();
        assert_eq!(parse_guid(&bare), Some(bytes));
    }

    #[test]
    fn malformed_guid_text_is_refused() {
        for text in [
            "",
            "{00112233-4455-6677-8899-aabbccddeef}",
            "{00112233-4455-6677-8899-aabbccddeefff}",
            "00112233445566778899aabbccddeeff",
            "{0011223g-4455-6677-8899-aabbccddeeff}",
            "{00112233-4455-6677-8899-aabbccddeefg}",
        ] {
            let units: Vec<u16> = text.encode_utf16().collect();
            assert_eq!(parse_guid(&units), None, "{text}");
        }
    }
}
