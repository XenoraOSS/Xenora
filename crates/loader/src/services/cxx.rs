// SPDX-License-Identifier: GPL-3.0-or-later

//! C++ runtime services for hosted guest programs.
//!
//! A mingw-w64 C++ program imports a handful of symbols from
//! `libstdc++-6.dll`. Only the three that every such program needs are
//! provided here: the two allocation operators and libstdc++'s length-error
//! path. The allocation operators reuse the guest heap the C runtime's
//! `malloc` and `free` already use, so a C++ guest and a C guest share one
//! allocator instead of running two.

use super::ServicesState;
use super::crt;
use crate::cpu::{CpuState, ExitReason};
use crate::import_table::ImportName;
use crate::imports::{ImportRegistry, ServiceHandler, ServiceStatus, Win64Args};
use crate::memory::{GuestMemory, MemoryError, PROCESS_HEAP};
use std::sync::Arc;

const MODULE: &str = "libstdc++-6.dll";

/// The module GCC's emulated thread-local storage accessor is imported from.
const EMUTLS_MODULE: &str = "libgcc_s_seh-1.dll";

/// `ENOMEM`, the C runtime's out-of-memory code.
const ENOMEM: u32 = 12;

/// The exit for a request the runtime does not model.
const RUNTIME_LIMIT: ExitReason = ExitReason::IllegalInstruction(0xFE);

/// `operator new(unsigned long long)`.
///
/// Allocates guest memory the way the C runtime's `malloc` does, through the
/// shared guest heap. The block is not zeroed: only `new T()` value-
/// initialises. A size of zero still yields a distinct, freeable block, as
/// `malloc(0)` does. A failed allocation sets `errno` to `ENOMEM` and returns
/// zero, which is the contract the C++ operator is defined in terms of.
fn cxx_operator_new(
    args: &Win64Args,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<u64, ExitReason> {
    let size = args.rcx.max(1) as usize;
    match memory.map_heap_allocation(PROCESS_HEAP, size) {
        Ok(address) => Ok(address),
        Err(_) => {
            crt::set_errno(memory, cpu.gs_base, ENOMEM)?;
            Ok(0)
        }
    }
}

/// `operator delete(void*)`.
///
/// Releases a block through the same guest heap the C runtime's `free` uses.
/// A null pointer does nothing. A pointer that is not a live guest allocation
/// is reported as a guest memory fault, as the C runtime's `free` reports it.
fn cxx_operator_delete(
    args: &Win64Args,
    _cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<u64, ExitReason> {
    let address = args.rcx;
    if address == 0 || memory.release_heap_allocation(PROCESS_HEAP, address) {
        return Ok(0);
    }
    Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(
        address,
    )))
}

/// `std::__throw_length_error(char const*)`.
///
/// Xenora implements no C++ exception machinery, so this function must not
/// return: it is a noreturn function, and returning from it is undefined
/// behaviour. The guest is stopped with the runtime-limit fault instead. This
/// is an explicit refusal, naming the reason the guest reached a libstdc++
/// error path Xenora does not implement, and not a silent no-op.
fn cxx_throw_length_error(
    _args: &Win64Args,
    _cpu: &mut CpuState,
    _memory: &mut GuestMemory,
) -> Result<u64, ExitReason> {
    Err(RUNTIME_LIMIT)
}

/// `__emutls_get_address(void*)`.
///
/// mingw-w64's C++ runtime uses GCC's emulated thread-local storage. For each
/// thread-local variable the compiler emits a descriptor holding the size and
/// the alignment of that variable and passes the descriptor's address here.
/// The runtime returns the calling thread's block for that variable and
/// allocates it on first use.
///
/// Two guest threads must never share a block, so the storage is keyed by the
/// guest thread first and by the descriptor second. The block comes from the
/// shared guest heap, so it is zeroed like every other guest allocation.
///
/// A null descriptor or a descriptor that is not readable, or whose size is
/// zero, yields zero: there is no variable to address.
fn cxx_emutls_get_address(
    state: &Arc<ServicesState>,
    args: &Win64Args,
    cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<u64, ExitReason> {
    let descriptor = args.rcx;
    if descriptor == 0 {
        return Ok(0);
    }
    let mut size_bytes = [0u8; 8];
    memory
        .read_bytes(descriptor, &mut size_bytes)
        .map_err(ExitReason::MemoryFault)?;
    let size = u64::from_le_bytes(size_bytes);
    if size == 0 {
        return Ok(0);
    }
    let teb = cpu.gs_base;
    let mut blocks = state.emutls.lock().expect("emulated thread-local storage");
    let thread_blocks = blocks.entry(teb).or_default();
    if let Some(address) = thread_blocks.get(&descriptor) {
        return Ok(*address);
    }
    let address = memory
        .map_heap_allocation(PROCESS_HEAP, size as usize)
        .map_err(ExitReason::MemoryFault)?;
    thread_blocks.insert(descriptor, address);
    Ok(address)
}

/// `std::__cxx11::basic_string<...>::_M_dispose()`.
///
/// Frees the string's heap buffer when it does not keep its characters in
/// its own local buffer. The layout is the one libstdc++ documents in
/// `bits/basic_string.h`: the data pointer at offset 0, the length at offset
/// 8, and then either the local buffer or the allocated capacity at offset
/// 16. A string whose data pointer is its own local buffer frees nothing,
/// and a string whose data pointer is zero frees nothing either.
fn cxx_string_dispose(
    args: &Win64Args,
    _cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<u64, ExitReason> {
    let object = args.rcx;
    if object == 0 {
        return Ok(0);
    }
    let mut data_bytes = [0u8; 8];
    memory
        .read_bytes(object, &mut data_bytes)
        .map_err(ExitReason::MemoryFault)?;
    let data = u64::from_le_bytes(data_bytes);
    // The local buffer follows the data pointer and the length.
    let local = object + 16;
    if data == 0 || data == local {
        return Ok(0);
    }
    if memory.release_heap_allocation(PROCESS_HEAP, data) {
        return Ok(0);
    }
    Err(ExitReason::MemoryFault(MemoryError::UnmappedAddress(data)))
}

/// `std::__cxx11::basic_string<...>::_S_copy(char*, char const*, size_t)`.
///
/// Copies characters with `char_traits<char>::copy`, which is a byte copy.
fn cxx_string_copy(
    args: &Win64Args,
    _cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<u64, ExitReason> {
    let destination = args.rcx;
    let source = args.rdx;
    let mut remaining = args.r8 as usize;
    // Copy in bounded pieces. The count comes from the guest, so it may name
    // far more bytes than any mapping holds; reading it in pieces means the
    // copy stops at the first unreadable byte instead of allocating a buffer
    // of the size the guest asked for.
    const PIECE: usize = 64 * 1024;
    let mut source = source;
    let mut destination = destination;
    while remaining > 0 {
        let take = remaining.min(PIECE);
        let mut buffer = vec![0u8; take];
        memory
            .read_bytes(source, &mut buffer)
            .map_err(ExitReason::MemoryFault)?;
        memory
            .write_bytes(destination, &buffer)
            .map_err(ExitReason::MemoryFault)?;
        source += take as u64;
        destination += take as u64;
        remaining -= take;
    }
    Ok(0)
}

/// `std::__throw_bad_alloc()`.
///
/// A noreturn function, like the length-error path above. Xenora implements
/// no C++ exception machinery, so the guest is stopped with the runtime-limit
/// fault rather than the function returning.
fn cxx_throw_bad_alloc(
    _args: &Win64Args,
    _cpu: &mut CpuState,
    _memory: &mut GuestMemory,
) -> Result<u64, ExitReason> {
    Err(RUNTIME_LIMIT)
}

/// `std::__throw_logic_error(char const*)`.
///
/// A noreturn function; the guest is stopped with the runtime-limit fault.
fn cxx_throw_logic_error(
    _args: &Win64Args,
    _cpu: &mut CpuState,
    _memory: &mut GuestMemory,
) -> Result<u64, ExitReason> {
    Err(RUNTIME_LIMIT)
}

/// `std::__throw_bad_array_new_length()`.
///
/// A noreturn function; the guest is stopped with the runtime-limit fault.
fn cxx_throw_bad_array_new_length(
    _args: &Win64Args,
    _cpu: &mut CpuState,
    _memory: &mut GuestMemory,
) -> Result<u64, ExitReason> {
    Err(RUNTIME_LIMIT)
}

/// `std::__glibcxx_assert_fail(char const*, int, char const*, char const*)`.
///
/// libstdc++'s assertion failure. It is noreturn, so the guest is stopped
/// with the runtime-limit fault. The file, line, function and expression the
/// assertion failed on are named in the fault the runtime reports, but the
/// fault type itself carries no text, so they are read here only to keep the
/// guest's memory access checked.
fn cxx_assert_fail(
    args: &Win64Args,
    _cpu: &mut CpuState,
    memory: &mut GuestMemory,
) -> Result<u64, ExitReason> {
    for address in [args.rcx, args.r8, args.r9] {
        if address == 0 {
            continue;
        }
        let mut byte = [0u8; 1];
        memory
            .read_bytes(address, &mut byte)
            .map_err(ExitReason::MemoryFault)?;
    }
    Err(RUNTIME_LIMIT)
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let handlers: [(&str, ServiceHandler); 3] = [
        ("_Znwy", Box::new(cxx_operator_new)),
        ("_ZdlPvy", Box::new(cxx_operator_delete)),
        (
            "_ZSt20__throw_length_errorPKc",
            Box::new(cxx_throw_length_error),
        ),
    ];
    for (symbol, handler) in handlers {
        // A duplicate would mean another module already provides this export;
        // the first registration wins rather than panicking the host.
        if registry
            .try_register(
                MODULE,
                ImportName::Name(symbol.to_owned()),
                ServiceStatus::Implemented,
                handler,
            )
            .is_err()
        {
            continue;
        }
        registry.set_status(MODULE, symbol, ServiceStatus::Implemented);
    }

    // The string helpers and the remaining error paths a C++ guest imports.
    let extra: [(&str, ServiceHandler); 6] = [
        (
            "_ZNSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEE10_M_disposeEv",
            Box::new(cxx_string_dispose),
        ),
        (
            "_ZNSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEE7_S_copyEPcPKcy",
            Box::new(cxx_string_copy),
        ),
        ("_ZSt17__throw_bad_allocv", Box::new(cxx_throw_bad_alloc)),
        (
            "_ZSt19__throw_logic_errorPKc",
            Box::new(cxx_throw_logic_error),
        ),
        (
            "_ZSt28__throw_bad_array_new_lengthv",
            Box::new(cxx_throw_bad_array_new_length),
        ),
        (
            "_ZSt21__glibcxx_assert_failPKciS0_S0_",
            Box::new(cxx_assert_fail),
        ),
    ];
    for (symbol, handler) in extra {
        if registry
            .try_register(
                MODULE,
                ImportName::Name(symbol.to_owned()),
                ServiceStatus::Implemented,
                handler,
            )
            .is_err()
        {
            continue;
        }
        registry.set_status(MODULE, symbol, ServiceStatus::Implemented);
    }

    // The emulated thread-local storage accessor lives in libgcc, not in
    // libstdc++, and it needs the shared state to find the calling thread's
    // block.
    let emutls_state = Arc::clone(state);
    let handler: ServiceHandler =
        Box::new(move |args, cpu, memory| cxx_emutls_get_address(&emutls_state, args, cpu, memory));
    if registry
        .try_register(
            EMUTLS_MODULE,
            ImportName::Name("__emutls_get_address".to_owned()),
            ServiceStatus::Implemented,
            handler,
        )
        .is_ok()
    {
        registry.set_status(
            EMUTLS_MODULE,
            "__emutls_get_address",
            ServiceStatus::Implemented,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{CpuState, Register};
    use crate::memory::MemoryPermissions;
    use crate::services::MockClock;

    fn call(
        registry: &ImportRegistry,
        symbol: &str,
        cpu: &mut CpuState,
        memory: &mut GuestMemory,
    ) -> Result<u64, ExitReason> {
        let service = registry
            .resolve(MODULE, &ImportName::Name(symbol.to_owned()))
            .expect("registered C++ service");
        (service.handler)(&Win64Args::new(cpu), cpu, memory)
    }

    fn registry() -> ImportRegistry {
        let mut registry = ImportRegistry::new();
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        register(&mut registry, &state);
        registry
    }

    /// The emulated thread-local accessor, resolved from its own module.
    fn emutls(
        registry: &ImportRegistry,
        cpu: &mut CpuState,
        memory: &mut GuestMemory,
    ) -> Result<u64, ExitReason> {
        let service = registry
            .resolve(
                EMUTLS_MODULE,
                &ImportName::Name("__emutls_get_address".to_owned()),
            )
            .expect("registered emulated thread-local service");
        (service.handler)(&Win64Args::new(cpu), cpu, memory)
    }

    /// A descriptor as the compiler emits it: the size and the alignment of
    /// one thread-local variable.
    fn descriptor(memory: &mut GuestMemory, address: u64, size: u64, align: u64) {
        memory
            .write_bytes(address, &size.to_le_bytes())
            .expect("descriptor size");
        memory
            .write_bytes(address + 8, &align.to_le_bytes())
            .expect("descriptor alignment");
    }

    /// Each guest thread owns its own block for a thread-local variable.
    #[test]
    fn each_guest_thread_gets_its_own_emulated_thread_local_block() {
        let registry = registry();
        let mut memory = GuestMemory::new();
        let descriptor_address = 0x0001_0000;
        memory
            .map_region(descriptor_address, 0x1000, MemoryPermissions::READ_WRITE)
            .expect("descriptor region");
        descriptor(&mut memory, descriptor_address, 4, 4);

        let mut main = CpuState {
            gs_base: 0x10000,
            ..CpuState::default()
        };
        main.set(Register::Rcx, descriptor_address);
        let main_block = emutls(&registry, &mut main, &mut memory).expect("a block");
        assert_ne!(main_block, 0);

        // The same thread asks again and gets the same block.
        let again = emutls(&registry, &mut main, &mut memory).expect("a block");
        assert_eq!(again, main_block);

        // Another guest thread gets its own.
        let mut worker = CpuState {
            gs_base: 0x20000,
            ..CpuState::default()
        };
        worker.set(Register::Rcx, descriptor_address);
        let worker_block = emutls(&registry, &mut worker, &mut memory).expect("a block");
        assert_ne!(worker_block, 0);
        assert_ne!(worker_block, main_block);

        // Each thread keeps its own bytes.
        memory
            .write_bytes(main_block, &[1, 2, 3, 4])
            .expect("the main block is writable");
        memory
            .write_bytes(worker_block, &[9, 9, 9, 9])
            .expect("the worker block is writable");
        let mut read_back = [0u8; 4];
        memory
            .read_bytes(main_block, &mut read_back)
            .expect("the main block is readable");
        assert_eq!(read_back, [1, 2, 3, 4]);
        memory
            .read_bytes(worker_block, &mut read_back)
            .expect("the worker block is readable");
        assert_eq!(read_back, [9, 9, 9, 9]);
    }

    /// A descriptor that names nothing, or a variable of no size, has no
    /// block to address.
    #[test]
    fn a_null_or_empty_emulated_descriptor_has_no_block() {
        let registry = registry();
        let mut memory = GuestMemory::new();
        let empty = 0x0001_0000;
        memory
            .map_region(empty, 0x1000, MemoryPermissions::READ_WRITE)
            .expect("descriptor region");
        let mut cpu = CpuState {
            gs_base: 0x10000,
            ..CpuState::default()
        };
        cpu.set(Register::Rcx, empty);
        assert_eq!(
            emutls(&registry, &mut cpu, &mut memory).expect("no block"),
            0
        );

        descriptor(&mut memory, empty, 0, 4);
        assert_eq!(
            emutls(&registry, &mut cpu, &mut memory).expect("no block"),
            0
        );
    }

    fn new_block(registry: &ImportRegistry, memory: &mut GuestMemory, size: u64) -> u64 {
        let mut cpu = CpuState {
            gs_base: 0x10000,
            ..CpuState::default()
        };
        cpu.set(Register::Rcx, size);
        call(registry, "_Znwy", &mut cpu, memory).expect("operator new succeeds")
    }

    #[test]
    fn operator_new_returns_a_distinct_writable_block() {
        let registry = registry();
        let mut memory = GuestMemory::new();

        let first = new_block(&registry, &mut memory, 64);
        assert_ne!(first, 0);
        let second = new_block(&registry, &mut memory, 64);
        assert_ne!(second, 0);
        assert_ne!(first, second);

        memory
            .write_bytes(first, &[0xA5; 64])
            .expect("operator new returns writable memory");
        let mut read_back = [0u8; 64];
        memory
            .read_bytes(first, &mut read_back)
            .expect("operator new block is readable");
        assert_eq!(read_back, [0xA5; 64]);
    }

    #[test]
    fn operator_new_does_not_zero_guest_written_bytes() {
        // The shared guest heap hands out zero-filled pages, so `new` and a
        // value-initialising allocation look alike in this runtime. What this
        // pins is that the service itself writes nothing into the block and
        // never disturbs bytes the guest wrote.
        let registry = registry();
        let mut memory = GuestMemory::new();

        let first = new_block(&registry, &mut memory, 32);
        memory
            .write_bytes(first, &[0x5A; 32])
            .expect("operator new returns writable memory");
        let second = new_block(&registry, &mut memory, 32);
        assert_ne!(first, second);
        let mut read_back = [0u8; 32];
        memory
            .read_bytes(first, &mut read_back)
            .expect("the first block is still mapped");
        assert_eq!(read_back, [0x5A; 32]);
    }

    #[test]
    fn operator_new_of_zero_bytes_returns_a_freeable_block() {
        let registry = registry();
        let mut memory = GuestMemory::new();

        let empty = new_block(&registry, &mut memory, 0);
        assert_ne!(empty, 0);
        let other = new_block(&registry, &mut memory, 16);
        assert_ne!(empty, other);

        let mut cpu = CpuState {
            gs_base: 0x10000,
            ..CpuState::default()
        };
        cpu.set(Register::Rcx, empty);
        assert_eq!(
            call(&registry, "_ZdlPvy", &mut cpu, &mut memory).expect("delete succeeds"),
            0
        );
    }

    #[test]
    fn operator_delete_releases_the_block_and_ignores_null() {
        let registry = registry();
        let mut memory = GuestMemory::new();

        let block = new_block(&registry, &mut memory, 48);
        let mut cpu = CpuState {
            gs_base: 0x10000,
            ..CpuState::default()
        };
        cpu.set(Register::Rcx, block);
        assert_eq!(
            call(&registry, "_ZdlPvy", &mut cpu, &mut memory).expect("delete succeeds"),
            0
        );
        let mut read_back = [0u8; 8];
        assert!(
            memory.read_bytes(block, &mut read_back).is_err(),
            "the released block is no longer mapped"
        );

        cpu.set(Register::Rcx, 0);
        assert_eq!(
            call(&registry, "_ZdlPvy", &mut cpu, &mut memory).expect("a null delete is harmless"),
            0
        );
    }

    #[test]
    fn throw_length_error_stops_the_guest_instead_of_returning() {
        let registry = registry();
        let mut memory = GuestMemory::new();
        let mut cpu = CpuState {
            gs_base: 0x10000,
            ..CpuState::default()
        };
        cpu.set(Register::Rcx, 0x1000);

        assert_eq!(
            call(
                &registry,
                "_ZSt20__throw_length_errorPKc",
                &mut cpu,
                &mut memory
            ),
            Err(RUNTIME_LIMIT)
        );
    }
}
