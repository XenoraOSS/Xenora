// SPDX-License-Identifier: GPL-3.0-or-later

//! Typed runtime services, called by translated guest code through a bound
//! import table, and the import parser and registry behind them.
//!
//! Expected values come from the public Win32, ISO C and Microsoft x64 calling
//! convention documentation, not from the registry or the handlers.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use xenora_loader::cpu::{CpuState, ExitReason, Register};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::import_table::{ImportName, ImportTableError, MAX_IMPORTS, parse_imports};
use xenora_loader::imports::{
    ImportError, ImportRegistry, RegistryError, ServiceStatus, Win64Args,
};
use xenora_loader::memory::{GuestMemory, MemoryPermissions};
use xenora_loader::pe::ParsedPe;
use xenora_loader::probe::{ProbeConfig, ProbeReport, probe_pe_bytes};
use xenora_loader::runtime::{Call, Float32, Float64};
use xenora_loader::runtime_table::SERVICES;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};

const BASE: u64 = 0x1_4000_0000;
const CODE_RVA: u32 = 0x1000;
const IDATA_RVA: u32 = 0x10000;
const IDATA_RAW: usize = 0x2200;
const IDATA_LEN: usize = 0x4000;
const SCRATCH: u64 = 0x5_0000;
const STACK: u64 = 0x8_0000;
const STACK_TOP: u64 = STACK + 0x800;

#[derive(Clone)]
enum Symbol {
    Name(&'static str),
    Ordinal(u16),
}

/// Original PE32+ image importing `imports`, one descriptor each. The code for
/// import `n`, at RVA 0x1000 + 16n, is `call [its slot]; hlt`.
struct Image {
    bytes: Vec<u8>,
    slots: Vec<u32>,
}

fn build(imports: &[(&str, Symbol)]) -> Image {
    let mut pe = vec![0u8; IDATA_RAW + IDATA_LEN];
    fn put(pe: &mut [u8], at: usize, bytes: &[u8]) {
        pe[at..at + bytes.len()].copy_from_slice(bytes);
    }
    put(&mut pe, 0, b"MZ");
    put(&mut pe, 0x3C, &0x80u32.to_le_bytes());
    put(&mut pe, 0x80, b"PE\0\0");
    put(&mut pe, 0x84, &0x8664u16.to_le_bytes());
    put(&mut pe, 0x86, &2u16.to_le_bytes());
    put(&mut pe, 0x94, &240u16.to_le_bytes());
    let optional = 0x98;
    put(&mut pe, optional, &0x20Bu16.to_le_bytes());
    put(&mut pe, optional + 16, &CODE_RVA.to_le_bytes());
    put(&mut pe, optional + 24, &BASE.to_le_bytes());
    put(&mut pe, optional + 32, &0x1000u32.to_le_bytes());
    put(&mut pe, optional + 36, &0x200u32.to_le_bytes());
    put(&mut pe, optional + 56, &0x20000u32.to_le_bytes());
    put(&mut pe, optional + 60, &0x200u32.to_le_bytes());
    put(&mut pe, optional + 108, &16u32.to_le_bytes());
    put(&mut pe, optional + 120, &IDATA_RVA.to_le_bytes());
    put(&mut pe, optional + 124, &0x100u32.to_le_bytes());
    for (index, (name, rva, raw, len, flags)) in [
        (b".text", CODE_RVA, 0x200, 0x2000, 0x6000_0020u32),
        (b".idat", IDATA_RVA, IDATA_RAW, IDATA_LEN, 0xC000_0040),
    ]
    .into_iter()
    .enumerate()
    {
        let section = optional + 240 + 40 * index;
        put(&mut pe, section, name);
        put(&mut pe, section + 8, &(len as u32).to_le_bytes());
        put(&mut pe, section + 12, &rva.to_le_bytes());
        put(&mut pe, section + 16, &(len as u32).to_le_bytes());
        put(&mut pe, section + 20, &(raw as u32).to_le_bytes());
        put(&mut pe, section + 36, &flags.to_le_bytes());
    }

    let file = |rva: u32| IDATA_RAW + (rva - IDATA_RVA) as usize;
    let mut next = IDATA_RVA + 20 * (imports.len() as u32 + 1);
    let mut take = |len: usize| {
        let rva = next;
        next = (next + len as u32 + 7) & !7;
        rva
    };
    let mut slots = Vec::new();
    for (index, (module, symbol)) in imports.iter().enumerate() {
        let module_rva = take(module.len() + 1);
        put(&mut pe, file(module_rva), module.as_bytes());
        let entry = match symbol {
            Symbol::Ordinal(ordinal) => 1 << 63 | u64::from(*ordinal),
            Symbol::Name(name) => {
                let hint = take(name.len() + 3);
                put(&mut pe, file(hint) + 2, name.as_bytes());
                u64::from(hint)
            }
        };
        let (lookup, slot) = (take(16), take(16));
        put(&mut pe, file(lookup), &entry.to_le_bytes());
        put(&mut pe, file(slot), &entry.to_le_bytes());
        let descriptor = file(IDATA_RVA) + 20 * index;
        put(&mut pe, descriptor, &lookup.to_le_bytes());
        put(&mut pe, descriptor + 12, &module_rva.to_le_bytes());
        put(&mut pe, descriptor + 16, &slot.to_le_bytes());
        slots.push(slot);

        let code_rva = CODE_RVA + 16 * index as u32;
        let displacement = slot.wrapping_sub(code_rva + 6);
        let at = 0x200 + 16 * index;
        put(&mut pe, at, &[0xFF, 0x15]); // call qword ptr [rip + displacement]
        put(&mut pe, at + 2, &displacement.to_le_bytes());
        put(&mut pe, at + 6, &[0xF4]); // hlt
    }
    Image { bytes: pe, slots }
}

fn standard(clock: u64) -> (ImportRegistry, Arc<MockClock>) {
    let (registry, clock, _) = standard_with_state(clock);
    (registry, clock)
}

fn standard_with_state(clock: u64) -> (ImportRegistry, Arc<MockClock>, Arc<ServicesState>) {
    let clock = Arc::new(MockClock::new(clock));
    let state = Arc::new(ServicesState::new(clock.clone()));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state.clone());
    (registry, clock, state)
}

/// A guest whose image imports every generated service from its own library.
struct Guest {
    engine: DbtEngine,
    memory: GuestMemory,
    clock: Arc<MockClock>,
    state: Arc<ServicesState>,
    teb: u64,
    /// Upper halves and XMM lanes of unrelated arguments hold this pattern.
    xmm: [u128; 4],
}

impl Guest {
    fn new() -> Self {
        let imports: Vec<(&str, Symbol)> = SERVICES
            .iter()
            .map(|service| (service.library, Symbol::Name(service.symbol)))
            .collect();
        let image = build(&imports);
        let pe = ParsedPe::parse(&image.bytes).unwrap();
        let mut memory = pe.load_into_memory(&image.bytes).unwrap().memory;
        memory
            .map_region(SCRATCH, 0x100, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory
            .map_region(STACK, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();
        let (mut registry, clock, state) = standard_with_state(0x01DA_0000_1234_5678);
        let bound = registry
            .bind_imports(&pe, &image.bytes, BASE, &mut memory)
            .unwrap();
        assert_eq!(bound, SERVICES.len());
        let mut engine = DbtEngine::new(256 * 1024).unwrap();
        engine.set_import_registry(registry);
        Self {
            engine,
            memory,
            clock,
            state,
            teb: 0,
            xmm: [0xAAAA_BBBB_CCCC_DDDD_EEEE_FFFF_0123_4567; 4],
        }
    }

    fn run(&mut self, symbol: &str, args: &[u64]) -> (CpuState, ExitReason) {
        let index = SERVICES
            .iter()
            .position(|service| service.symbol == symbol)
            .unwrap();
        let mut cpu = CpuState::new(BASE + u64::from(CODE_RVA) + 16 * index as u64, STACK_TOP);
        cpu.gs_base = self.teb;
        for (index, register) in cpu.gpr.iter_mut().enumerate() {
            if index != Register::Rsp as usize {
                *register = 0x0101_0101_0101_0101 * (index as u64 + 1);
            }
        }
        for (register, value) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
            .into_iter()
            .zip(args)
        {
            cpu.set(register, *value);
        }
        for (index, register) in cpu.xmm.iter_mut().enumerate() {
            *register = 0x1111_2222_3333_4444_5555_6666_7777_0000 + index as u128;
        }
        cpu.xmm[..4].copy_from_slice(&self.xmm);
        let before = cpu.clone();
        let result = self.engine.run(&mut cpu, &mut self.memory, 8);
        if result.exit_reason == ExitReason::Halt {
            use Register::{R12, R13, R14, R15, Rbp, Rbx, Rdi, Rsi, Rsp};
            for register in [Rbx, Rbp, Rdi, Rsi, Rsp, R12, R13, R14, R15] {
                assert_eq!(
                    cpu.get(register),
                    before.get(register),
                    "{symbol} {register:?}"
                );
            }
            assert_eq!(cpu.xmm[6..], before.xmm[6..], "{symbol}");
            assert_eq!(cpu.mxcsr, before.mxcsr, "{symbol}");
        }
        (cpu, result.exit_reason)
    }

    fn call(&mut self, symbol: &str, args: &[u64]) -> u64 {
        let (cpu, exit) = self.run(symbol, args);
        assert_eq!(exit, ExitReason::Halt, "{symbol}");
        cpu.get(Register::Rax)
    }

    fn last_error(&mut self) -> u64 {
        self.call("GetLastError", &[])
    }
}

const PROCESS: u64 = u64::MAX;
const THREAD: u64 = u64::MAX - 1;

#[test]
fn identity_services_return_documented_pseudo_handles_and_ids() {
    let mut guest = Guest::new();
    assert_eq!(guest.call("GetCurrentProcess", &[]), PROCESS);
    assert_eq!(guest.call("GetCurrentThread", &[]), THREAD);
    let id = guest.call("GetCurrentProcessId", &[]);
    assert!(id != 0 && id <= u64::from(u32::MAX));
    assert_ne!(guest.call("GetCurrentThreadId", &[]), id);
    assert_eq!(guest.call("GetProcessId", &[PROCESS]), id);
    assert_eq!(guest.call("GetProcessId", &[THREAD]), 0);
    assert_eq!(guest.last_error(), 6); // ERROR_INVALID_HANDLE
}

#[test]
fn priority_boost_round_trips_and_checks_its_handle() {
    let mut guest = Guest::new();
    let read = |guest: &mut Guest| {
        guest.memory.write_u64(SCRATCH, u64::MAX).unwrap();
        assert_eq!(
            guest.call("GetProcessPriorityBoost", &[PROCESS, SCRATCH]),
            1
        );
        // A BOOL is four bytes; the next four stay untouched.
        assert_eq!(guest.memory.read_u32(SCRATCH + 4).unwrap(), u32::MAX);
        guest.memory.read_u32(SCRATCH).unwrap()
    };
    assert_eq!(read(&mut guest), 0);
    // Any nonzero BOOL disables; bits above 32 are not part of the argument.
    assert_eq!(
        guest.call("SetProcessPriorityBoost", &[PROCESS, 0xFFFF_FFFF_0000_0002]),
        1
    );
    assert_eq!(read(&mut guest), 1);
    assert_eq!(
        guest.call("SetProcessPriorityBoost", &[PROCESS, 0xFFFF_FFFF_0000_0000]),
        1
    );
    assert_eq!(read(&mut guest), 0);
    assert_eq!(guest.call("SetProcessPriorityBoost", &[THREAD, 1]), 0);
    assert_eq!(guest.last_error(), 6);
    assert_eq!(
        guest.call("GetProcessPriorityBoost", &[PROCESS, 0x9000_0000]),
        0
    );
    assert_eq!(guest.last_error(), 998); // ERROR_NOACCESS
}

#[test]
fn thread_handles_check_access_and_are_rejected_once_closed() {
    let mut guest = Guest::new();
    let id = guest.call("GetCurrentThreadId", &[]);
    // THREAD_QUERY_INFORMATION only.
    let query = guest.call("OpenThread", &[0x40, 0, id]);
    assert_ne!(query, 0);
    assert_eq!(guest.call("GetThreadPriority", &[query]), 0);
    assert_eq!(guest.call("SetThreadPriority", &[query, 1]), 0);
    assert_eq!(guest.last_error(), 5); // ERROR_ACCESS_DENIED
    assert_eq!(guest.call("GetThreadPriority", &[query]), 0);
    // THREAD_SET_INFORMATION | THREAD_QUERY_INFORMATION.
    let full = guest.call("OpenThread", &[0x60, 0, id]);
    assert_ne!(full, query);
    // A negative int is returned in EAX with the upper half clear.
    assert_eq!(
        guest.call("SetThreadPriority", &[full, 0xABCD_0000_FFFF_FFFE]),
        1
    );
    assert_eq!(guest.call("GetThreadPriority", &[full]), 0xFFFF_FFFE);
    assert_eq!(guest.call("SetThreadPriority", &[full, 3]), 0);
    assert_eq!(guest.last_error(), 87); // ERROR_INVALID_PARAMETER
    assert_eq!(guest.call("CloseHandle", &[full]), 1);
    assert_eq!(guest.call("GetThreadPriority", &[full]), 0x7FFF_FFFF);
    assert_eq!(guest.last_error(), 6);
    assert_eq!(guest.call("CloseHandle", &[full]), 0);
    assert_eq!(guest.call("OpenThread", &[0x40, 0, id + 1]), 0);
    assert_eq!(guest.last_error(), 87);
    assert_eq!(guest.call("OpenThread", &[0x40, 1, id]), 0);
    assert_eq!(guest.last_error(), 50); // ERROR_NOT_SUPPORTED
    assert_ne!(
        guest.call("OpenThread", &[0x40, 0, id]),
        full,
        "handles are not reused"
    );
}

#[test]
fn system_time_comes_from_the_injected_clock() {
    let mut guest = Guest::new();
    guest.call("GetSystemTimeAsFileTime", &[SCRATCH + 3]);
    assert_eq!(
        guest.memory.read_u64(SCRATCH + 3).unwrap(),
        0x01DA_0000_1234_5678
    );
    guest.clock.advance(10);
    guest.call("GetSystemTimeAsFileTime", &[SCRATCH]);
    assert_eq!(
        guest.memory.read_u64(SCRATCH).unwrap(),
        0x01DA_0000_1234_5682
    );
    for pointer in [0, SCRATCH + 0xFC] {
        let before = guest.memory.read_u32(SCRATCH + 0xFC).unwrap();
        let (_, exit) = guest.run("GetSystemTimeAsFileTime", &[pointer]);
        assert!(matches!(exit, ExitReason::MemoryFault(_)), "{exit:?}");
        assert_eq!(guest.memory.read_u32(SCRATCH + 0xFC).unwrap(), before);
    }
}

fn tokenization_guest(symbol: &'static str) -> (DbtEngine, GuestMemory) {
    let image = build(&[
        ("ucrtbase.dll", Symbol::Name(symbol)),
        ("api-ms-win-crt-string-l1-1-0.dll", Symbol::Name(symbol)),
        ("ucrtbase.dll", Symbol::Name("_errno")),
    ]);
    let pe = ParsedPe::parse(&image.bytes).unwrap();
    let mut memory = pe.load_into_memory(&image.bytes).unwrap().memory;
    memory
        .map_region(SCRATCH, 4096, MemoryPermissions::READ_WRITE)
        .unwrap();
    memory
        .map_region(STACK, 4096, MemoryPermissions::READ_WRITE)
        .unwrap();
    let (mut registry, _) = standard(0);
    assert_eq!(
        registry
            .bind_imports(&pe, &image.bytes, BASE, &mut memory)
            .unwrap(),
        3
    );
    let mut engine = DbtEngine::new(256 * 1024).unwrap();
    engine.set_import_registry(registry);
    (engine, memory)
}

fn call_tokenization_import(
    engine: &mut DbtEngine,
    memory: &mut GuestMemory,
    slot: u32,
    [input, separators, context]: [u64; 3],
) -> u64 {
    let entry = BASE + u64::from(CODE_RVA + 16 * slot);
    let mut cpu = CpuState::new(entry, STACK_TOP);
    cpu.set(Register::Rcx, input);
    cpu.set(Register::Rdx, separators);
    cpu.set(Register::R8, context);
    let result = engine.run(&mut cpu, memory, 8);
    assert_eq!(result.exit_reason, ExitReason::Halt);
    cpu.get(Register::Rax)
}

#[test]
fn guest_wcstok_s_resumes_in_wide_units_and_sets_errno() {
    let (mut engine, mut memory) = tokenization_guest("wcstok_s");
    let input = SCRATCH;
    let delimiters = SCRATCH + 32;
    let context = SCRATCH + 64;
    for (index, value) in [0x2603, 0x03bb, 0x2603, 0x03bc, 0].into_iter().enumerate() {
        memory.write_u16(input + 2 * index as u64, value).unwrap();
    }
    memory.write_u16(delimiters, 0x2603).unwrap();

    let mut call = |memory: &mut GuestMemory, slot, args| {
        call_tokenization_import(&mut engine, memory, slot, args)
    };
    let errno = call(&mut memory, 2, [0; 3]);
    assert_eq!(call(&mut memory, 1, [0, delimiters, context]), 0);
    assert_eq!(memory.read_u32(errno).unwrap(), 22);
    memory.write_u32(errno, 0).unwrap();

    assert_eq!(
        call(&mut memory, 0, [input, delimiters, context]),
        input + 2
    );
    assert_eq!(memory.read_u16(input + 4).unwrap(), 0);
    assert_eq!(memory.read_u64(context).unwrap(), input + 6);
    assert_eq!(call(&mut memory, 1, [0, delimiters, context]), input + 6);
    assert_eq!(call(&mut memory, 1, [0, delimiters, context]), 0);
    assert_eq!(memory.read_u64(context).unwrap(), input + 8);
    assert_eq!(memory.read_u32(errno).unwrap(), 0);
}

#[test]
fn guest_wcstok_resumes_from_its_win64_context_argument() {
    let (mut engine, mut memory) = tokenization_guest("wcstok");
    let input = SCRATCH;
    let separators = SCRATCH + 32;
    let context = SCRATCH + 64;
    let write = |memory: &mut GuestMemory, at, text: &str| {
        let data: Vec<_> = text
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        memory.write_bytes(at, &data).unwrap();
    };
    write(&mut memory, input, "☃λ☃μ;ν");
    write(&mut memory, separators, "☃");
    let mut call = |memory: &mut GuestMemory, slot, text| {
        call_tokenization_import(&mut engine, memory, slot, [text, separators, context])
    };
    assert_eq!(call(&mut memory, 0, input), input + 2);
    assert_eq!(memory.read_u16(input + 4).unwrap(), 0);
    assert_eq!(memory.read_u64(context).unwrap(), input + 6);
    write(&mut memory, separators, ";");
    assert_eq!(call(&mut memory, 1, 0), input + 6);
    assert_eq!(memory.read_u16(input + 8).unwrap(), 0);
    assert_eq!(call(&mut memory, 0, 0), input + 10);
    assert_eq!(memory.read_u64(context).unwrap(), 0);
    assert_eq!(call(&mut memory, 1, 0), 0);
}

#[test]
fn guest_strtok_s_shares_context_across_win64_import_aliases() {
    let (mut engine, mut memory) = tokenization_guest("strtok_s");
    let input = SCRATCH;
    let delimiters = SCRATCH + 32;
    let context = SCRATCH + 64;
    memory.write_bytes(input, b",a,b;c\0").unwrap();
    memory.write_bytes(delimiters, b",\0;\0").unwrap();

    let mut call = |memory: &mut GuestMemory, slot, args| {
        call_tokenization_import(&mut engine, memory, slot, args)
    };
    let errno = call(&mut memory, 2, [0; 3]);
    assert_eq!(call(&mut memory, 1, [0, delimiters, context]), 0);
    assert_eq!(memory.read_u32(errno).unwrap(), 22);
    memory.write_u32(errno, 0).unwrap();

    assert_eq!(
        call(&mut memory, 0, [input, delimiters, context]),
        input + 1
    );
    assert_eq!(memory.read_u8(input + 2).unwrap(), 0);
    assert_eq!(memory.read_u64(context).unwrap(), input + 3);
    assert_eq!(
        call(&mut memory, 1, [0, delimiters + 2, context]),
        input + 3
    );
    assert_eq!(memory.read_u8(input + 4).unwrap(), 0);
    assert_eq!(call(&mut memory, 0, [0, delimiters, context]), input + 5);
    assert_eq!(call(&mut memory, 1, [0, delimiters, context]), 0);
    assert_eq!(memory.read_u64(context).unwrap(), input + 6);
    assert_eq!(memory.read_u32(errno).unwrap(), 0);
}

#[test]
fn guest_udp_loopback_delivers_datagrams_through_bound_imports() {
    let symbols = [
        "WSAStartup",
        "socket",
        "bind",
        "connect",
        "send",
        "recv",
        "WSAGetLastError",
        "WSACleanup",
        "WSAPoll",
        "getsockname",
        "getpeername",
        "getsockopt",
        "inet_pton",
        "inet_ntop",
        "htons",
    ];
    let imports = symbols.map(|symbol| ("ws2_32.dll", Symbol::Name(symbol)));
    let image = build(&imports);
    let pe = ParsedPe::parse(&image.bytes).unwrap();
    let mut memory = pe.load_into_memory(&image.bytes).unwrap().memory;
    memory
        .map_region(SCRATCH, 4096, MemoryPermissions::READ_WRITE)
        .unwrap();
    memory
        .map_region(STACK, 4096, MemoryPermissions::READ_WRITE)
        .unwrap();
    let (mut registry, _) = standard(0);
    assert_eq!(
        registry
            .bind_imports(&pe, &image.bytes, BASE, &mut memory)
            .unwrap(),
        symbols.len()
    );
    let mut engine = DbtEngine::new(256 * 1024).unwrap();
    engine.set_import_registry(registry);
    let mut call = |memory: &mut GuestMemory, symbol, args: &[u64]| {
        let index = symbols.iter().position(|&name| symbol == name).unwrap() as u64;
        let mut cpu = CpuState::new(BASE + u64::from(CODE_RVA) + 16 * index, STACK_TOP);
        for (register, &value) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
            .into_iter()
            .zip(args)
        {
            cpu.set(register, value);
        }
        if let Some(&fifth) = args.get(4) {
            memory.write_u64(STACK_TOP + 32, fifth).unwrap();
        }
        let result = engine.run(&mut cpu, memory, 8);
        assert_eq!(result.exit_reason, ExitReason::Halt, "{symbol}");
        cpu.get(Register::Rax)
    };
    let address = SCRATCH + 0x200;
    let outgoing = SCRATCH + 0x220;
    let incoming = SCRATCH + 0x240;
    let text = SCRATCH + 0x260;
    let poll = SCRATCH + 0x280;
    let formatted = SCRATCH + 0x2a0;
    let address_length = SCRATCH + 0x2b0;
    let peer_address = SCRATCH + 0x2c0;
    memory.write_u16(address, 2).unwrap();
    let port = call(&mut memory, "htons", &[5000]);
    assert_eq!(port, 0x8813);
    memory.write_bytes(text, b"127.0.0.1\0").unwrap();
    assert_eq!(call(&mut memory, "inet_pton", &[2, text, address + 4]), 1);
    assert_eq!(
        memory.read_u32(address + 4).unwrap().to_le_bytes(),
        [127, 0, 0, 1]
    );
    memory.write_bytes(text, b"127.0.0.999\0").unwrap();
    assert_eq!(call(&mut memory, "inet_pton", &[2, text, address + 4]), 0);
    assert_eq!(
        memory.read_u32(address + 4).unwrap().to_le_bytes(),
        [127, 0, 0, 1]
    );
    assert_eq!(
        call(&mut memory, "inet_pton", &[23, text, address + 4]),
        u64::from(u32::MAX)
    );
    assert_eq!(call(&mut memory, "WSAGetLastError", &[]), 10047);
    memory.write_u8(formatted, 0xaa).unwrap();
    assert_eq!(
        call(&mut memory, "inet_ntop", &[2, address + 4, formatted, 9]),
        0
    );
    assert_eq!(call(&mut memory, "WSAGetLastError", &[]), 87);
    assert_eq!(memory.read_u8(formatted).unwrap(), 0xaa);
    assert_eq!(
        call(&mut memory, "inet_ntop", &[2, address + 4, formatted, 10]),
        formatted
    );
    let mut formatted_address = [0; 10];
    memory
        .read_bytes(formatted, &mut formatted_address)
        .unwrap();
    assert_eq!(&formatted_address, b"127.0.0.1\0");
    assert_eq!(call(&mut memory, "WSAStartup", &[0x0202, SCRATCH]), 0);
    let receiver = call(&mut memory, "socket", &[2, 2, 17]);
    let sender = call(&mut memory, "socket", &[2, 2, 17]);
    assert_ne!(receiver, u64::MAX);
    assert_ne!(sender, u64::MAX);
    assert_ne!(sender, receiver);
    for (option, expected) in [(0x1008, 2), (0x2003, 65_507)] {
        memory.write_u32(address_length, 8).unwrap();
        assert_eq!(
            call(
                &mut memory,
                "getsockopt",
                &[sender, 0xffff, option, incoming, address_length]
            ),
            0
        );
        assert_eq!(memory.read_u32(incoming).unwrap(), expected);
        assert_eq!(memory.read_u32(address_length).unwrap(), 4);
    }
    memory.write_u32(address_length, 3).unwrap();
    assert_eq!(
        call(
            &mut memory,
            "getsockopt",
            &[sender, 0xffff, 0x1008, incoming, address_length]
        ),
        u64::from(u32::MAX)
    );
    assert_eq!(call(&mut memory, "WSAGetLastError", &[]), 10014);
    assert_eq!(memory.read_u32(incoming).unwrap(), 65_507);
    assert_eq!(call(&mut memory, "bind", &[receiver, address, 16]), 0);
    memory.write_u32(address_length, 15).unwrap();
    assert_eq!(
        call(
            &mut memory,
            "getsockname",
            &[receiver, address, address_length]
        ),
        u64::from(u32::MAX)
    );
    assert_eq!(call(&mut memory, "WSAGetLastError", &[]), 10014);
    assert_eq!(memory.read_u16(address + 2).unwrap(), 0);
    memory.write_u32(address_length, 32).unwrap();
    assert_eq!(
        call(
            &mut memory,
            "getsockname",
            &[receiver, address, address_length]
        ),
        0
    );
    assert_eq!(memory.read_u32(address_length).unwrap(), 16);
    assert_eq!(memory.read_u16(address + 2).unwrap().swap_bytes(), 49152);
    assert_eq!(call(&mut memory, "connect", &[sender, address, 16]), 0);
    assert_eq!(
        call(
            &mut memory,
            "getpeername",
            &[sender, peer_address, address_length]
        ),
        0
    );
    assert_eq!(
        memory.read_u64(peer_address).unwrap(),
        memory.read_u64(address).unwrap()
    );
    memory.write_u64(poll, receiver).unwrap();
    memory.write_u16(poll + 8, 0x0100).unwrap();
    memory.write_u64(poll + 16, sender).unwrap();
    memory.write_u16(poll + 24, 0x0010).unwrap();
    assert_eq!(call(&mut memory, "WSAPoll", &[poll, 2, 0]), 1);
    assert_eq!(memory.read_u16(poll + 10).unwrap(), 0);
    assert_eq!(memory.read_u16(poll + 26).unwrap(), 0x0010);
    memory.write_bytes(outgoing, b"loop").unwrap();
    for _ in 0..16 {
        assert_eq!(call(&mut memory, "send", &[sender, outgoing, 4, 0]), 4);
    }
    assert_eq!(
        call(&mut memory, "send", &[sender, outgoing, 4, 0]),
        u64::from(u32::MAX)
    );
    assert_eq!(call(&mut memory, "WSAGetLastError", &[]), 10055);
    assert_eq!(call(&mut memory, "WSAPoll", &[poll, 2, 0]), 1);
    assert_eq!(memory.read_u16(poll + 10).unwrap(), 0x0100);
    assert_eq!(memory.read_u16(poll + 26).unwrap(), 0);
    for _ in 0..16 {
        assert_eq!(call(&mut memory, "recv", &[receiver, incoming, 4, 0]), 4);
        let mut received = [0; 4];
        memory.read_bytes(incoming, &mut received).unwrap();
        assert_eq!(&received, b"loop");
        memory.write_u32(incoming, 0).unwrap();
    }
    assert_eq!(
        call(&mut memory, "recv", &[receiver, incoming, 4, 0]),
        u64::from(u32::MAX)
    );
    assert_eq!(call(&mut memory, "WSAGetLastError", &[]), 10035);
    assert_eq!(call(&mut memory, "WSAPoll", &[poll, 2, 0]), 1);
    assert_eq!(memory.read_u16(poll + 10).unwrap(), 0);
    assert_eq!(memory.read_u16(poll + 26).unwrap(), 0x0010);
    let started = std::time::Instant::now();
    assert_eq!(call(&mut memory, "WSAPoll", &[poll, 1, 10]), 0);
    assert!(started.elapsed() >= std::time::Duration::from_millis(10));
    assert_eq!(memory.read_u16(poll + 10).unwrap(), 0);
    assert_eq!(
        call(&mut memory, "WSAPoll", &[poll, 1, 1001]),
        u64::from(u32::MAX)
    );
    assert_eq!(call(&mut memory, "WSAGetLastError", &[]), 10045);
    assert_eq!(call(&mut memory, "WSACleanup", &[]), 0);
}

#[test]
fn last_error_isolated_across_guest_thread_switches() {
    let mut guest = Guest::new();
    guest.teb = 0x10000;
    guest.call("SetLastError", &[123]);
    guest.teb = 0x20000;
    assert_eq!(guest.last_error(), 0);
    assert_eq!(guest.call("CloseHandle", &[123]), 0);
    assert_eq!(guest.last_error(), 6);
    guest.teb = 0x10000;
    guest.call("GetCurrentProcessId", &[]);
    assert_eq!(guest.last_error(), 123);
    guest.call("SetLastError", &[0]);
    assert_eq!(guest.last_error(), 0);
    guest.teb = 0x20000;
    assert_eq!(guest.last_error(), 6);
}

/// The last-error code is documented as kept per thread, and most functions
/// set it only when they fail (errhandlingapi.h GetLastError).
#[test]
fn a_successful_call_keeps_another_threads_error_and_its_own() {
    let mut guest = Guest::new();
    guest.teb = 0x10000;
    assert_eq!(guest.call("VirtualAlloc", &[0, 0, 0x3000, 4]), 0);
    assert_eq!(guest.last_error(), 87); // ERROR_INVALID_PARAMETER
    guest.teb = 0x20000;
    guest.call("SetLastError", &[0xFFFF_FFFF]);
    assert_eq!(guest.call("GetCurrentProcess", &[]), PROCESS);
    assert_eq!(guest.call("GetCurrentThreadId", &[]), 0x4104);
    assert_eq!(
        guest.last_error(),
        0xFFFF_FFFF,
        "success keeps a nonzero error"
    );
    guest.teb = 0x10000;
    assert_eq!(guest.last_error(), 87);
}

#[test]
fn a_retired_thread_starts_clean_when_its_teb_address_is_reused() {
    let mut guest = Guest::new();
    guest.teb = 0x10000;
    guest.call("SetLastError", &[998]);
    guest.teb = 0x20000;
    guest.call("CloseHandle", &[0x1234]);

    assert!(guest.state.retire_thread_last_error(0x10000));
    assert!(!guest.state.retire_thread_last_error(0x10000));
    assert!(
        !guest.state.retire_thread_last_error(0x30000),
        "a thread that never failed holds no value"
    );
    assert_eq!(guest.last_error(), 6, "the other thread is untouched");
    guest.teb = 0x10000;
    assert_eq!(guest.last_error(), 0);
    guest.call("SetLastError", &[5]);
    assert_eq!(guest.last_error(), 5);
}

#[test]
fn last_error_is_set_by_failures_and_by_the_guest() {
    let mut guest = Guest::new();
    assert_eq!(guest.last_error(), 0);
    guest.call("SetLastError", &[0xFFFF_FFFF_8000_0001]);
    assert_eq!(guest.last_error(), 0x8000_0001);
    assert_eq!(guest.call("GetCurrentProcess", &[]), PROCESS);
    assert_eq!(
        guest.last_error(),
        0x8000_0001,
        "success leaves it unchanged"
    );
    guest.call("CloseHandle", &[0x1234]);
    assert_eq!(guest.last_error(), 6);
}

#[test]
fn virtual_memory_is_mapped_released_and_bounded() {
    let mut guest = Guest::new();
    const COMMIT_RESERVE: u64 = 0x3000;
    const RELEASE: u64 = 0x8000;
    let address = guest.call("VirtualAlloc", &[0, 5000, COMMIT_RESERVE, 0x04]);
    assert!(address != 0 && address.is_multiple_of(0x1_0000));
    assert_eq!(
        guest.memory.read_u64(address + 8184).unwrap(),
        0,
        "zeroed, whole pages"
    );
    assert!(guest.memory.read_u8(address + 8192).is_err());
    guest.memory.write_u64(address, 7).unwrap();
    for (args, error) in [
        ([0, 0, COMMIT_RESERVE, 0x04], 87),
        ([0, 4096, 0x1000, 0x04], 50),
        ([0, 4096, COMMIT_RESERVE, 0x40], 87),
        ([address, 4096, COMMIT_RESERVE, 0x04], 487),
        ([0, u64::MAX, COMMIT_RESERVE, 0x04], 87),
        ([0, 1 << 40, COMMIT_RESERVE, 0x04], 8),
    ] {
        assert_eq!(guest.call("VirtualAlloc", &args), 0, "{args:x?}");
        assert_eq!(guest.last_error(), error, "{args:x?}");
    }
    for (args, error) in [
        ([address, 4096, RELEASE], 87),
        ([address, 0, 0x4000], 50),
        ([address + 4096, 0, RELEASE], 487),
    ] {
        assert_eq!(guest.call("VirtualFree", &args), 0, "{args:x?}");
        assert_eq!(guest.last_error(), error, "{args:x?}");
    }
    assert_eq!(guest.call("VirtualFree", &[address, 0, RELEASE]), 1);
    assert!(guest.memory.read_u8(address).is_err());
    assert_eq!(guest.call("VirtualFree", &[address, 0, RELEASE]), 0);
}

#[test]
fn floating_arguments_and_results_use_xmm_registers() {
    let mut guest = Guest::new();
    let mut float = |symbol: &str, bits: u64| {
        // Only the low lane is the argument, and RCX must be ignored.
        guest.xmm[0] = 0xFFFF_FFFF_FFFF_FFFF << 64 | u128::from(bits);
        let (cpu, exit) = guest.run(symbol, &[0x4022_0000_0000_0000]);
        assert_eq!(exit, ExitReason::Halt);
        cpu.xmm[0]
    };
    assert_eq!(
        float("sqrt", 0x4030_0000_0000_0000) as u64,
        0x4010_0000_0000_0000
    ); // 16 -> 4
    assert_eq!(
        float("sqrt", 0x4000_0000_0000_0000) as u64,
        0x3FF6_A09E_667F_3BCD
    ); // sqrt 2
    assert_eq!(
        float("sqrt", 0x8000_0000_0000_0000) as u64,
        0x8000_0000_0000_0000
    ); // -0
    assert_eq!(
        float("sqrt", 0x7FF0_0000_0000_0000) as u64,
        0x7FF0_0000_0000_0000
    );
    let nan = float("sqrt", 0xBFF0_0000_0000_0000) as u64;
    assert!(f64::from_bits(nan).is_nan());
    assert_eq!(
        float("fabs", 0xC004_0000_0000_0000) as u64,
        0x4004_0000_0000_0000
    ); // -2.5
    assert_eq!(float("fabs", 0x8000_0000_0000_0000) as u64, 0);
    let garbage = 0xDEAD_BEEF_0000_0000;
    assert_eq!(float("sqrtf", garbage | 0x4110_0000) as u32, 0x4040_0000); // 9 -> 3
    assert_eq!(float("fabsf", garbage | 0xBF80_0000) as u32, 0x3F80_0000); // -1
}

#[test]
fn arguments_are_assigned_by_position_and_read_the_stack_after_the_fourth() {
    let mut memory = GuestMemory::new();
    memory
        .map_region(STACK, 0x1000, MemoryPermissions::READ_WRITE)
        .unwrap();
    // [RSP] return address, 32 bytes of shadow space, then arguments 4 and 5.
    memory
        .write_u64(STACK_TOP + 40, 0x1122_3344_5566_7788)
        .unwrap();
    memory
        .write_u64(STACK_TOP + 48, 0x4008_0000_0000_0000)
        .unwrap();
    let mut cpu = CpuState::new(0, STACK_TOP);
    for (index, register) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
        .into_iter()
        .enumerate()
    {
        cpu.set(register, 0xFFFF_FFFF_0000_0010 + index as u64);
        cpu.xmm[index] = (1 << 100) + 0xAAAA_AAAA_0000_0020 + index as u128;
    }
    let args = Win64Args::new(&cpu);
    let call = Call::new(&args, &cpu, &memory);
    assert_eq!(call.u32(0), Ok(0x10));
    assert_eq!(call.f64(1), Ok(Float64(0xAAAA_AAAA_0000_0021)));
    assert_eq!(call.f32(2), Ok(Float32(0x22)));
    assert_eq!(call.u64(3), Ok(0xFFFF_FFFF_0000_0013));
    assert_eq!(call.i32(4), Ok(0x5566_7788));
    assert_eq!(call.f64(5), Ok(Float64(0x4008_0000_0000_0000)));
    cpu.set(Register::Rsp, STACK + 0xFF0);
    let args = Win64Args::new(&cpu);
    let call = Call::new(&args, &cpu, &memory);
    assert!(matches!(call.u64(4), Err(ExitReason::MemoryFault(_))));
    cpu.set(Register::Rsp, u64::MAX - 8);
    let args = Win64Args::new(&cpu);
    assert!(Call::new(&args, &cpu, &memory).f64(4).is_err());
}

// --- registry ----------------------------------------------------------------

fn named(name: &str) -> ImportName {
    ImportName::Name(name.to_owned())
}

#[test]
fn generated_metadata_matches_the_live_registry() {
    let (registry, _) = standard(0);
    for service in &SERVICES {
        let descriptor = registry
            .resolve(service.library, &named(service.symbol))
            .unwrap();
        assert_eq!(descriptor.status, service.status, "{}", service.symbol);
        assert!(!service.tests.is_empty());
        for alias in service.aliases {
            let upper = alias.to_ascii_uppercase();
            let aliased = registry.resolve(&upper, &named(service.symbol)).unwrap();
            assert!(
                std::ptr::eq(aliased, descriptor),
                "{alias}!{}",
                service.symbol
            );
        }
        // Symbol names are exact, and unlisted API sets do not resolve.
        assert!(
            registry
                .resolve(
                    service.library,
                    &named(&service.symbol.to_ascii_uppercase())
                )
                .is_none()
                || service.symbol == service.symbol.to_ascii_uppercase()
        );
        assert!(
            registry
                .resolve("api-ms-win-core-file-l1-1-0.dll", &named(service.symbol))
                .is_none()
        );
    }
}

#[test]
fn conflicting_or_invalid_registrations_are_refused() {
    let handler = || Box::new(|_: &Win64Args, _: &mut CpuState, _: &mut GuestMemory| Ok(0));
    let mut registry = ImportRegistry::new();
    let mut add = |module: &str, name: ImportName| {
        registry.try_register(module, name, ServiceStatus::Stub, handler())
    };
    assert_eq!(add("a.dll", named("F")), Ok(()));
    assert_eq!(
        add("a.dll", named("f")),
        Ok(()),
        "symbols are case-sensitive"
    );
    assert_eq!(add("b.dll", named("F")), Ok(()));
    assert!(matches!(
        add("A.DLL", named("F")),
        Err(RegistryError::Duplicate { .. })
    ));
    assert_eq!(add("a.dll", ImportName::Ordinal(7)), Ok(()));
    assert!(matches!(
        add("a.dll", ImportName::Ordinal(7)),
        Err(RegistryError::Duplicate { .. })
    ));
    assert_eq!(
        add("a.dll", named("Ordinal#7")),
        Ok(()),
        "a name is never an ordinal"
    );
    for (module, symbol) in [("", "F"), ("a.dll", ""), ("a b.dll", "F"), ("a.dll", "F\n")] {
        assert!(matches!(
            add(module, named(symbol)),
            Err(RegistryError::InvalidName { .. })
        ));
    }
    assert_eq!(registry.try_alias("set.dll", "a.dll", "F"), Ok(()));
    assert_eq!(
        registry.try_alias("set.dll", "a.dll", "F"),
        Ok(()),
        "repeating is harmless"
    );
    assert!(matches!(
        registry.try_alias("set.dll", "b.dll", "F"),
        Err(RegistryError::Duplicate { .. })
    ));
    assert!(matches!(
        registry.try_alias("b.dll", "a.dll", "F"),
        Err(RegistryError::Duplicate { .. })
    ));
    assert!(matches!(
        registry.try_alias("x.dll", "a.dll", "G"),
        Err(RegistryError::UnknownTarget { .. })
    ));
}

#[test]
#[should_panic(expected = "kernel32.dll!CloseHandle is already registered")]
fn a_host_module_cannot_replace_a_standard_service() {
    let (mut registry, _) = standard(0);
    registry.register("KERNEL32.dll", "CloseHandle", |_, _, _| Ok(1));
}

fn bind(registry: &mut ImportRegistry, image: &Image) -> (Result<usize, ImportError>, GuestMemory) {
    let pe = ParsedPe::parse(&image.bytes).unwrap();
    let mut memory = pe.load_into_memory(&image.bytes).unwrap().memory;
    let result = registry.bind_imports(&pe, &image.bytes, BASE, &mut memory);
    (result, memory)
}

#[test]
fn an_unresolved_import_leaves_every_slot_unbound() {
    let image = build(&[
        ("kernel32.dll", Symbol::Name("GetLastError")),
        ("kernel32.dll", Symbol::Name("NoSuchFunction")),
    ]);
    let (mut registry, _) = standard(0);
    let (result, memory) = bind(&mut registry, &image);
    let expected = ImportError::UnresolvedImport {
        module: "kernel32.dll".into(),
        symbol: "NoSuchFunction".into(),
    };
    assert_eq!(result, Err(expected));
    for slot in &image.slots {
        let file = IDATA_RAW + (slot - IDATA_RVA) as usize;
        let original = u64::from_le_bytes(image.bytes[file..file + 8].try_into().unwrap());
        assert_eq!(memory.read_u64(BASE + u64::from(*slot)).unwrap(), original);
    }
}

#[test]
fn aliases_share_a_trampoline_and_ordinals_bind_only_ordinals() {
    let (mut registry, _) = standard(0);
    registry
        .try_register(
            "o.dll",
            ImportName::Ordinal(7),
            ServiceStatus::Partial,
            Box::new(|_, _, _| Ok(77)),
        )
        .unwrap();
    let image = build(&[
        ("KERNEL32.dll", Symbol::Name("GetLastError")),
        (
            "API-MS-Win-Core-ErrorHandling-L1-1-0.dll",
            Symbol::Name("GetLastError"),
        ),
        ("O.DLL", Symbol::Ordinal(7)),
    ]);
    let (result, memory) = bind(&mut registry, &image);
    assert_eq!(result, Ok(3));
    let slot = |index: usize| {
        memory
            .read_u64(BASE + u64::from(image.slots[index]))
            .unwrap()
    };
    assert_eq!(slot(0), slot(1));
    assert_ne!(slot(0), slot(2));
    for symbol in [Symbol::Name("Ordinal#7"), Symbol::Ordinal(8)] {
        let (result, _) = bind(&mut registry, &build(&[("o.dll", symbol)]));
        assert!(
            matches!(result, Err(ImportError::UnresolvedImport { .. })),
            "{result:?}"
        );
    }
}

/// A registry with one service bound, and its trampoline address.
fn bound_service<F>(handler: F) -> (ImportRegistry, GuestMemory, u64)
where
    F: Fn(&Win64Args, &mut CpuState, &mut GuestMemory) -> Result<u64, ExitReason>
        + Send
        + Sync
        + 'static,
{
    let mut registry = ImportRegistry::new();
    registry.register("t.dll", "F", handler);
    let image = build(&[("t.dll", Symbol::Name("F"))]);
    let (result, mut memory) = bind(&mut registry, &image);
    assert_eq!(result, Ok(1));
    memory
        .map_region(STACK, 0x1000, MemoryPermissions::READ_WRITE)
        .unwrap();
    memory.write_u64(STACK_TOP, 0x1234).unwrap();
    let trampoline = memory.read_u64(BASE + u64::from(image.slots[0])).unwrap();
    (registry, memory, trampoline)
}

#[test]
fn a_service_that_changes_a_nonvolatile_register_stops_the_guest() {
    use Register::{R8, R9, R10, R11, R12, R13, R14, R15, Rbp, Rbx, Rcx, Rdi, Rdx, Rsi, Rsp};
    let dispatch = |change: fn(&mut CpuState, usize), which: usize| {
        let (registry, mut memory, trampoline) = bound_service(move |_, cpu, _| {
            change(cpu, which);
            Ok(5)
        });
        let mut cpu = CpuState::new(trampoline, STACK_TOP);
        let result = registry.dispatch(trampoline, &mut cpu, &mut memory);
        (result, cpu)
    };
    for register in [Rbx, Rbp, Rdi, Rsi, Rsp, R12, R13, R14, R15] {
        let (result, _) = dispatch(|cpu, index| cpu.gpr[index] ^= 8, register as usize);
        assert_eq!(result, Err(ExitReason::ServiceAbiViolation), "{register:?}");
    }
    for register in [Rcx, Rdx, R8, R9, R10, R11] {
        let (result, cpu) = dispatch(|cpu, index| cpu.gpr[index] ^= 8, register as usize);
        assert_eq!(result, Ok(()), "{register:?}");
        assert_eq!(
            (cpu.rip, cpu.get(Rsp), cpu.get(Register::Rax)),
            (0x1234, STACK_TOP + 8, 5)
        );
    }
    for index in 0..16 {
        let (result, _) = dispatch(|cpu, index| cpu.xmm[index] ^= 1 << 127, index);
        let expected = if index < 6 {
            Ok(())
        } else {
            Err(ExitReason::ServiceAbiViolation)
        };
        assert_eq!(result, expected, "xmm{index}");
    }
}

#[test]
fn a_call_that_cannot_return_runs_no_service() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let (registry, mut memory, trampoline) = bound_service(move |_, _, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(0)
    });
    for rsp in [0x9000_0000, STACK + 0xFFC, u64::MAX - 7] {
        let mut cpu = CpuState::new(trampoline, rsp);
        let before = cpu.clone();
        assert!(
            registry
                .dispatch(trampoline, &mut cpu, &mut memory)
                .is_err()
        );
        assert_eq!(cpu, before);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let mut cpu = CpuState::new(trampoline, STACK_TOP);
    assert_eq!(registry.dispatch(trampoline, &mut cpu, &mut memory), Ok(()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// --- import table --------------------------------------------------------------

fn inspect(image: &Image) -> ProbeReport {
    probe_pe_bytes(&image.bytes, &ProbeConfig::default()).unwrap()
}

/// File offset of an RVA in the import section.
fn file(rva: u32) -> usize {
    IDATA_RAW + (rva - IDATA_RVA) as usize
}

fn u32_at(image: &Image, at: usize) -> u32 {
    u32::from_le_bytes(image.bytes[at..at + 4].try_into().unwrap())
}

#[test]
fn malformed_import_tables_are_reported_and_never_bound() {
    let base = || {
        build(&[
            ("kernel32.dll", Symbol::Name("GetLastError")),
            ("kernel32.dll", Symbol::Name("CloseHandle")),
        ])
    };
    let end = IDATA_RVA + IDATA_LEN as u32;
    let second = IDATA_RAW + 20;
    type Damage = fn(&mut Image, usize, u32);
    type Expected = fn(&ImportTableError) -> bool;
    // (damage to the second descriptor, expected error)
    let cases: &[(&str, Damage, Expected)] = &[
        (
            "name runs off the section (issue #270)",
            |image, second, end| {
                let lookup = u32_at(image, second);
                image.bytes[file(lookup)..file(lookup) + 8]
                    .copy_from_slice(&u64::from(end - 6).to_le_bytes());
                let at = file(end - 4);
                image.bytes[at..at + 4].copy_from_slice(b"Name");
            },
            |error| matches!(error, ImportTableError::MalformedName { .. }),
        ),
        (
            "name pointer at the last byte",
            |image, second, end| {
                let lookup = u32_at(image, second);
                image.bytes[file(lookup)..file(lookup) + 8]
                    .copy_from_slice(&u64::from(end - 1).to_le_bytes());
            },
            |error| matches!(error, ImportTableError::MalformedName { .. }),
        ),
        (
            "empty name",
            |image, second, _| {
                let lookup = u32_at(image, second);
                let hint = u32_at(image, file(lookup));
                image.bytes[file(hint) + 2] = 0;
            },
            |error| matches!(error, ImportTableError::MalformedName { .. }),
        ),
        (
            "non-ASCII name",
            |image, second, _| {
                let lookup = u32_at(image, second);
                let hint = u32_at(image, file(lookup));
                image.bytes[file(hint) + 4] = 0xC3;
            },
            |error| matches!(error, ImportTableError::MalformedName { .. }),
        ),
        (
            "module name outside every section",
            |image, second, _| {
                image.bytes[second + 12..second + 16].copy_from_slice(&0x7000_0000u32.to_le_bytes())
            },
            |error| matches!(error, ImportTableError::MalformedName { .. }),
        ),
        (
            "lookup table outside every section",
            |image, second, _| {
                image.bytes[second..second + 4].copy_from_slice(&u32::MAX.to_le_bytes())
            },
            |error| matches!(error, ImportTableError::OutOfBounds { .. }),
        ),
        (
            "reserved bits in a name entry",
            |image, second, _| {
                let lookup = u32_at(image, second);
                image.bytes[file(lookup) + 5] = 1;
            },
            |error| matches!(error, ImportTableError::ReservedBits { .. }),
        ),
        (
            "bit 31 in a name entry",
            |image, second, _| {
                let lookup = u32_at(image, second);
                image.bytes[file(lookup) + 3] |= 0x80;
            },
            |error| matches!(error, ImportTableError::ReservedBits { .. }),
        ),
        (
            "name continues in bytes after the section",
            |image, second, end| {
                let lookup = u32_at(image, second);
                image.bytes[file(lookup)..file(lookup) + 8]
                    .copy_from_slice(&u64::from(end - 6).to_le_bytes());
                let at = file(end - 4);
                image.bytes[at..at + 4].copy_from_slice(b"Name");
                image.bytes.extend_from_slice(b"Tail\0");
            },
            |error| matches!(error, ImportTableError::MalformedName { .. }),
        ),
        (
            "reserved bits in an ordinal entry",
            |image, second, _| {
                let lookup = u32_at(image, second);
                image.bytes[file(lookup)..file(lookup) + 8]
                    .copy_from_slice(&(1u64 << 63 | 0x1_0000).to_le_bytes());
            },
            |error| matches!(error, ImportTableError::ReservedBits { .. }),
        ),
    ];
    for (what, damage, expected) in cases {
        let mut image = base();
        damage(&mut image, second, end);
        let (mut registry, _) = standard(0);
        let (result, _) = bind(&mut registry, &image);
        match &result {
            Err(ImportError::Table(error)) => assert!(expected(error), "{what}: {error:?}"),
            other => panic!("{what}: {other:?}"),
        }
        let report = inspect(&image);
        assert_eq!(
            report.import_error.map(ImportError::Table),
            result.err(),
            "{what}"
        );
        assert_eq!(
            report.imports.len(),
            1,
            "{what}: the first import is still listed"
        );
        assert_eq!(report.imports[0].symbol, "GetLastError");
    }
}

#[test]
fn endless_tables_are_bounded() {
    // A descriptor at 0, a module name at 0x40, a hint and name at 0x60 and a
    // lookup entry at 0x80. The mapping repeats one structure without end.
    let mut bytes = vec![0u8; 0x88];
    bytes[0..4].copy_from_slice(&0x80u32.to_le_bytes());
    bytes[12..16].copy_from_slice(&0x40u32.to_le_bytes());
    bytes[16..20].copy_from_slice(&0x80u32.to_le_bytes());
    bytes[0x40..0x46].copy_from_slice(b"m.dll\0");
    bytes[0x62..0x64].copy_from_slice(b"F\0");
    bytes[0x80] = 0x60;

    // Lookup entries repeat from 0x80 on.
    let (entries, error) = parse_imports(&bytes, 0, |rva| {
        let rva = rva as usize;
        Some(if rva < 0x80 { rva } else { 0x80 + rva % 8 })
    });
    assert_eq!(error, Some(ImportTableError::TooManyImports));
    assert_eq!(entries.len(), MAX_IMPORTS);

    // Descriptors repeat from 0x1000 on; each has one symbol.
    bytes.extend([0; 8]);
    let (entries, error) = parse_imports(&bytes, 0x1000, |rva| {
        let rva = rva as usize;
        Some(if rva < 0x1000 {
            rva
        } else {
            (rva - 0x1000) % 20
        })
    });
    assert_eq!(error, Some(ImportTableError::TooManyDescriptors));
    assert_eq!(entries.len(), 1024);
}

#[test]
fn inspection_and_binding_agree_on_damaged_tables() {
    let base = build(&[
        ("kernel32.dll", Symbol::Name("GetLastError")),
        (
            "api-ms-win-core-handle-l1-1-0.dll",
            Symbol::Name("CloseHandle"),
        ),
        ("ucrtbase.dll", Symbol::Name("sqrt")),
        ("o.dll", Symbol::Ordinal(3)),
    ]);
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    };
    let (mut bound, mut malformed, mut unresolved) = (0, 0, 0);
    for _ in 0..6000 {
        let mut image = Image {
            bytes: base.bytes.clone(),
            slots: base.slots.clone(),
        };
        for _ in 0..1 + next() % 3 {
            let at = IDATA_RAW + (next() % 0x220) as usize;
            image.bytes[at] = match next() % 4 {
                0 => 0,
                1 => 0xFF,
                _ => next() as u8,
            };
        }
        let (mut registry, _) = standard(0);
        registry
            .try_register(
                "o.dll",
                ImportName::Ordinal(3),
                ServiceStatus::Stub,
                Box::new(|_, _, _| Ok(0)),
            )
            .unwrap();
        let pe = ParsedPe::parse(&image.bytes).unwrap();
        let mut memory = pe.load_into_memory(&image.bytes).unwrap().memory;
        let result = registry.bind_imports(&pe, &image.bytes, BASE, &mut memory);
        let report = inspect(&image);
        match result {
            Ok(count) => {
                bound += 1;
                assert_eq!(report.import_error, None);
                assert_eq!(report.imports.len(), count);
            }
            Err(ImportError::Table(error)) => {
                malformed += 1;
                assert_eq!(report.import_error, Some(error));
            }
            Err(ImportError::UnresolvedImport { module, symbol }) => {
                unresolved += 1;
                assert_eq!(report.import_error, None);
                assert!(
                    report
                        .imports
                        .iter()
                        .any(|import| import.dll == module && import.symbol == symbol)
                );
            }
            Err(ImportError::Memory(_)) => {}
        }
    }
    assert!(
        bound > 100 && malformed > 100 && unresolved > 100,
        "{bound} {malformed} {unresolved}"
    );
}

#[test]
fn inspection_reports_exactly_what_binding_resolves() {
    let image = build(&[
        ("KERNEL32.dll", Symbol::Name("GetLastError")),
        ("kernel32.dll", Symbol::Name("getlasterror")),
        (
            "api-ms-win-core-errorhandling-l1-1-0.dll",
            Symbol::Name("SetLastError"),
        ),
        (
            "api-ms-win-core-errorhandling-l1-1-3.dll",
            Symbol::Name("SetLastError"),
        ),
        ("kernel32.dll", Symbol::Ordinal(1)),
    ]);
    let report = inspect(&image);
    let supported: Vec<bool> = report
        .imports
        .iter()
        .map(|import| import.is_supported)
        .collect();
    assert_eq!(supported, [true, false, true, false, false]);
    assert_eq!(
        report.imports[2].canonical_module.as_deref(),
        Some("kernel32.dll")
    );
    assert_eq!(report.imports[4].symbol, "Ordinal#1");
}
