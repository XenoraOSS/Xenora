// SPDX-License-Identifier: GPL-3.0-or-later

//! C-locale CRT services. Only the `"C"` locale is supported; other locale
//! names fail closed. `localeconv` returns a guest `lconv` that describes the
//! C locale and stays valid until the next successful `setlocale`.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError, PROCESS_HEAP};
use std::sync::Arc;

const UCRT: &str = "ucrtbase.dll";
const LOCALE_API: &str = "api-ms-win-crt-locale-l1-1-0.dll";
const EINVAL: u32 = 22;
const LC_ALL: i32 = 0;
const LC_TIME: i32 = 5;
/// Windows `CHAR_MAX` used for "not available" monetary fields.
const CHAR_MAX: u8 = 127;
/// Pointers (10) + char fields (8) in the MSVC x64 `lconv` we expose.
const LCONV_BYTES: usize = 10 * 8 + 8;
const STRING_BYTES: usize = 16;

#[derive(Default)]
pub(super) struct LocaleState {
    /// Guest VA of the packed C-locale blob (strings then `lconv`), or 0.
    block: u64,
    /// Guest VA of the durable `"C"` name string.
    name: u64,
}

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

fn set_errno(memory: &mut GuestMemory, teb: u64, error: u32) -> Result<(), ExitReason> {
    let address = memory.crt_errno_address(teb).map_err(fault)?;
    memory.write_u32(address, error).map_err(fault)
}

fn read_c_string(memory: &GuestMemory, address: u64, limit: usize) -> Result<String, ExitReason> {
    if address == 0 {
        return Ok(String::new());
    }
    let mut bytes = Vec::new();
    for index in 0..limit {
        let byte = memory.read_u8(address + index as u64).map_err(fault)?;
        if byte == 0 {
            return String::from_utf8(bytes).map_err(|_| ExitReason::IllegalInstruction(0xFE));
        }
        bytes.push(byte);
    }
    Err(ExitReason::IllegalInstruction(0xFE))
}

fn is_c_locale(name: &str) -> bool {
    name.is_empty()
        || name.eq_ignore_ascii_case("C")
        || name.eq_ignore_ascii_case("POSIX")
        || name.eq_ignore_ascii_case("C.UTF-8")
        || name.eq_ignore_ascii_case("C.utf8")
}

fn valid_category(category: i32) -> bool {
    (LC_ALL..=LC_TIME).contains(&category)
}

fn ensure_c_locale(
    state: &Arc<ServicesState>,
    memory: &mut GuestMemory,
) -> Result<(u64, u64), ExitReason> {
    let mut locale = state.locale.lock().expect("locale state");
    if locale.block != 0 && locale.name != 0 {
        return Ok((locale.block, locale.name));
    }
    let total = STRING_BYTES + LCONV_BYTES;
    let block = memory
        .map_heap_allocation(PROCESS_HEAP, total)
        .map_err(fault)?;
    // Layout: [0]=".\0", [2]="" (empty at the extra NUL), [3]="C\0"; lconv at 16.
    memory.write_bytes(block, b".\0\0C\0").map_err(fault)?;
    let decimal = block;
    let empty = block + 2;
    let name = block + 3;
    let lconv = block + STRING_BYTES as u64;
    let mut bytes = [0u8; LCONV_BYTES];
    let write_ptr = |bytes: &mut [u8], index: usize, value: u64| {
        bytes[index * 8..(index + 1) * 8].copy_from_slice(&value.to_le_bytes());
    };
    write_ptr(&mut bytes, 0, decimal); // decimal_point
    write_ptr(&mut bytes, 1, empty); // thousands_sep
    write_ptr(&mut bytes, 2, empty); // grouping
    write_ptr(&mut bytes, 3, empty); // int_curr_symbol
    write_ptr(&mut bytes, 4, empty); // currency_symbol
    write_ptr(&mut bytes, 5, empty); // mon_decimal_point
    write_ptr(&mut bytes, 6, empty); // mon_thousands_sep
    write_ptr(&mut bytes, 7, empty); // mon_grouping
    write_ptr(&mut bytes, 8, empty); // positive_sign
    write_ptr(&mut bytes, 9, empty); // negative_sign
    for field in 0..8 {
        bytes[80 + field] = CHAR_MAX;
    }
    memory.write_bytes(lconv, &bytes).map_err(fault)?;
    locale.block = block;
    locale.name = name;
    Ok((block, name))
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let setlocale = Arc::clone(state);
    registry.register(UCRT, "setlocale", move |args, cpu, memory| {
        let category = args.rcx as i32;
        if !valid_category(category) {
            set_errno(memory, cpu.gs_base, EINVAL)?;
            return Ok(0);
        }
        if args.rdx == 0 {
            let (_, name) = ensure_c_locale(&setlocale, memory)?;
            return Ok(name);
        }
        let requested = read_c_string(memory, args.rdx, 64)?;
        if !is_c_locale(&requested) {
            set_errno(memory, cpu.gs_base, EINVAL)?;
            return Ok(0);
        }
        let (_, name) = ensure_c_locale(&setlocale, memory)?;
        Ok(name)
    });

    let localeconv = Arc::clone(state);
    registry.register(UCRT, "localeconv", move |_, _, memory| {
        let (block, _) = ensure_c_locale(&localeconv, memory)?;
        Ok(block + STRING_BYTES as u64)
    });

    for symbol in ["setlocale", "localeconv"] {
        registry.alias(LOCALE_API, UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::CpuState;
    use crate::imports::Win64Args;
    use crate::services::MockClock;
    use crate::services::ServicesState;

    #[test]
    fn setlocale_accepts_only_the_c_locale() {
        use crate::cpu::Register;
        use crate::import_table::ImportName;
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register(&mut registry, &state);
        let mut memory = GuestMemory::new();
        let mut cpu = CpuState {
            gs_base: 0x10000,
            ..CpuState::default()
        };
        let call = |symbol: &str, cpu: &mut CpuState, memory: &mut GuestMemory| {
            let service = registry
                .resolve(UCRT, &ImportName::Name(symbol.to_owned()))
                .unwrap();
            (service.handler)(&Win64Args::new(cpu), cpu, memory).unwrap()
        };
        let name = memory.map_heap_allocation(PROCESS_HEAP, 8).unwrap();
        memory.write_bytes(name, b"C\0").unwrap();
        cpu.set(Register::Rcx, LC_ALL as u64);
        cpu.set(Register::Rdx, name);
        let returned = call("setlocale", &mut cpu, &mut memory);
        assert_ne!(returned, 0);
        let mut label = [0u8; 2];
        memory.read_bytes(returned, &mut label).unwrap();
        assert_eq!(&label, b"C\0");

        let bad = memory.map_heap_allocation(PROCESS_HEAP, 16).unwrap();
        memory.write_bytes(bad, b"en_US\0").unwrap();
        cpu.set(Register::Rdx, bad);
        assert_eq!(call("setlocale", &mut cpu, &mut memory), 0);
        let lconv = call("localeconv", &mut cpu, &mut memory);
        assert_ne!(lconv, 0);
        let decimal = memory.read_u64(lconv).unwrap();
        let mut point = [0u8; 2];
        memory.read_bytes(decimal, &mut point).unwrap();
        assert_eq!(&point, b".\0");
    }
}
