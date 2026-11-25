// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded soak test: one original guest workload run many times on the same
//! guest runtime, checking that nothing it uses accumulates.
//!
//! The workload is a hand-assembled x86-64 routine in a synthetic PE32+
//! image, bound to the standard service registry and run as translated guest
//! code under the guest thread scheduler. Each iteration it creates an event
//! and a worker thread, waits for the worker and reads its exit code, closes
//! both handles, and allocates, writes, reads back and frees a process-heap
//! block and a `VirtualAlloc` region. It calls `GlobalMemoryStatusEx` at the
//! start, while the worker is alive, while it holds both allocations, and at
//! the end.
//!
//! After every iteration the test checks that the open handle count, the
//! guest memory in use and the mapped guest address space are back at their
//! starting values (the first iteration may keep one errno cell for the
//! worker's thread slot; see [`ERRNO_CELL`]), that exactly one worker thread
//! existed, that memory held mid-iteration is exactly the two allocations,
//! that the translation cache stops
//! growing after warm-up, and that outputs are deterministic. The worker's
//! exit code is checked against [`worker_reference`], written here from the
//! routine's specification rather than taken from the runtime.
//!
//! Expected service results follow Microsoft's public documentation:
//! https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-createthread
//! https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-getexitcodethread
//! https://learn.microsoft.com/windows/win32/api/synchapi/nf-synchapi-waitforsingleobject
//! https://learn.microsoft.com/windows/win32/api/heapapi/nf-heapapi-heapalloc
//! https://learn.microsoft.com/windows/win32/api/memoryapi/nf-memoryapi-virtualalloc
//! https://learn.microsoft.com/windows/win32/api/sysinfoapi/ns-sysinfoapi-memorystatusex
//!
//! Scope: CPU-side guest runtime only. The clock is the existing `MockClock`,
//! and no GPU path runs: the guest graphics context has no resource release
//! and no public resource count yet, so GPU resource lifetime is not covered.
//!
//! The default run is part of `cargo test`. The long run is ignored by
//! default; run it with:
//!
//! ```sh
//! cargo test --locked --release -p xenora-loader --test soak_workload -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use xenora_loader::cpu::CpuState;
use xenora_loader::dbt::DbtEngine;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};
use xenora_loader::pe::ParsedPe;
use xenora_loader::scheduler::{DEFAULT_QUANTUM, Outcome, ScheduleReport, Scheduler};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};

const BASE: u64 = 0x1_4000_0000;
const TEXT_RVA: u32 = 0x1000;
const TEXT_RAW: usize = 0x200;
const TEXT_LEN: usize = 0x1000;
const IDATA_RVA: u32 = 0x8000;
const IDATA_RAW: usize = TEXT_RAW + TEXT_LEN;
const IDATA_LEN: usize = 0x1000;
const SHARED: u64 = 0x5_0000;
const STACK: u64 = 0x8_0000;
const STACK_LEN: usize = 0x1_0000;
const TEB: u64 = 0x7_0000;

/// Layout of the shared page the host and the guest routine exchange.
const SEED: u64 = SHARED;
const EXIT_CODE: u64 = SHARED + 8;
const RESULTS: u64 = SHARED + 0x10;
/// `MEMORYSTATUSEX` records, 64 bytes each: at the start, while the worker is
/// alive, while both allocations are held, and at the end.
const STATUS: [u64; 4] = [
    SHARED + 0x200,
    SHARED + 0x240,
    SHARED + 0x280,
    SHARED + 0x2C0,
];
const MEMORYSTATUSEX_LEN: u32 = 64;
/// The runtime keeps one 4-byte CRT errno cell per thread TEB address, so a
/// recycled thread slot starts with errno 0 (`services/threads.rs`,
/// `map_thread`). It is created the first time a slot is used and is bounded
/// by the number of thread slots, so only the first iteration may add it.
const ERRNO_CELL: u64 = 4;

const HEAP_BLOCK: u32 = 0x400;
const VIRTUAL_REGION: u32 = 0x1_0000;
const MEM_COMMIT_RESERVE: u32 = 0x3000;
const MEM_RELEASE: u32 = 0x8000;
const PAGE_READWRITE: u32 = 4;
const INFINITE: u32 = u32::MAX;
const SENTINEL: u64 = 0xDEAD_BEEF_DEAD_BEEF;

/// Distinct seeds cycled through the iterations; each repeats many times.
const SEEDS: [u64; 8] = [
    0,
    1,
    0x3F,
    0x40,
    0x55,
    0x1234_5678,
    0xFFFF_FFFF,
    0xABCD_EF01,
];

/// Budget for one scheduled iteration, in guest instructions and service
/// calls. One iteration takes well under a tenth of this.
const ITERATION_BUDGET: usize = 50_000;

/// Win64 registers by encoding number.
const RAX: u8 = 0;
const RCX: u8 = 1;
const RDX: u8 = 2;
const R8: u8 = 8;
const R9: u8 = 9;

/// Stack frame of the main routine: home area, two stack arguments, five
/// saved values. 0x58 keeps RSP 16-byte aligned at each call.
const FRAME: u8 = 0x58;
const SAVED_EVENT: u8 = 0x30;
const SAVED_THREAD: u8 = 0x38;
const SAVED_HEAP: u8 = 0x40;
const SAVED_BLOCK: u8 = 0x48;
const SAVED_REGION: u8 = 0x50;

/// A minimal x86-64 emitter for the few forms the routine needs. Encodings
/// follow the Intel SDM Vol. 2 instruction reference.
#[derive(Default)]
struct Asm {
    code: Vec<u8>,
    /// Offsets of `call [rip + disp32]` displacements and the import called.
    imports: Vec<(usize, &'static str)>,
}

impl Asm {
    fn bytes(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    /// `call qword ptr [rip + disp32]` through the import's IAT slot.
    fn call(&mut self, import: &'static str) {
        self.bytes(&[0xFF, 0x15]);
        self.imports.push((self.code.len(), import));
        self.bytes(&[0; 4]);
    }

    /// `mov r64, imm64` (REX.W B8+r io).
    fn mov_imm(&mut self, register: u8, value: u64) {
        self.bytes(&[0x48 | (register >> 3), 0xB8 + (register & 7)]);
        self.bytes(&value.to_le_bytes());
    }

    /// `xor r32, r32`, which zeroes the full 64-bit register.
    fn zero(&mut self, register: u8) {
        let rex = if register >= 8 { 0x45 } else { 0 };
        if rex != 0 {
            self.bytes(&[rex]);
        }
        let low = register & 7;
        self.bytes(&[0x31, 0xC0 | (low << 3) | low]);
    }

    /// `mov r64, [rsp + disp8]` (REX.W 8B /r with a SIB byte).
    fn load(&mut self, register: u8, slot: u8) {
        let rex = 0x48 | ((register >> 3) << 2);
        self.bytes(&[rex, 0x8B, 0x44 | ((register & 7) << 3), 0x24, slot]);
    }

    /// `mov [rsp + disp8], rax`.
    fn save_rax(&mut self, slot: u8) {
        self.bytes(&[0x48, 0x89, 0x44, 0x24, slot]);
    }

    /// `mov rcx, imm64; mov [rcx], rax`: record RAX in result `index`.
    fn record(&mut self, index: u64) {
        self.mov_imm(RCX, RESULTS + 8 * index);
        self.bytes(&[0x48, 0x89, 0x01]);
    }

    /// Store the seed at [RAX], read it back into RAX and record it.
    fn write_and_read_back(&mut self, index: u64) {
        self.mov_imm(RCX, SEED);
        self.bytes(&[0x48, 0x8B, 0x09]); // mov rcx, [rcx]
        self.bytes(&[0x48, 0x89, 0x08]); // mov [rax], rcx
        self.bytes(&[0x48, 0x8B, 0x00]); // mov rax, [rax]
        self.record(index);
    }

    fn memory_status(&mut self, record: u64, index: u64) {
        self.mov_imm(RCX, record);
        self.call("GlobalMemoryStatusEx");
        self.record(index);
    }
}

/// What each entry of the results array must hold after an iteration, given
/// the seed. `None` means only "nonzero" (handle values and addresses are
/// runtime choices, not part of any contract).
fn expected_results(seed: u64) -> [Option<u64>; RESULT_COUNT] {
    [
        Some(1),    // 0 GlobalMemoryStatusEx at start
        None,       // 1 CreateEventW handle
        None,       // 2 CreateThread handle
        Some(1),    // 3 GlobalMemoryStatusEx with the worker alive
        Some(0),    // 4 WaitForSingleObject(thread) = WAIT_OBJECT_0
        Some(1),    // 5 GetExitCodeThread
        Some(1),    // 6 CloseHandle(thread)
        None,       // 7 HeapAlloc block
        Some(seed), // 8 heap block read back
        None,       // 9 VirtualAlloc region
        Some(seed), // 10 region read back
        Some(1),    // 11 GlobalMemoryStatusEx with both held
        Some(1),    // 12 HeapFree
        Some(1),    // 13 VirtualFree
        Some(1),    // 14 SetEvent
        Some(0),    // 15 WaitForSingleObject(event, 0) = WAIT_OBJECT_0
        Some(1),    // 16 CloseHandle(event)
        Some(1),    // 17 GlobalMemoryStatusEx at end
    ]
}
const RESULT_COUNT: usize = 18;

/// The worker routine: `DWORD worker(LPVOID seed)`.
///
/// count = (seed & 63) + 1; acc = 0; for d = count down to 1:
/// acc = acc * 31 + d (32-bit wrapping); return acc.
fn worker(asm: &mut Asm) {
    asm.bytes(&[0x89, 0xCA]); // mov edx, ecx
    asm.bytes(&[0x83, 0xE2, 0x3F]); // and edx, 63
    asm.bytes(&[0xFF, 0xC2]); // inc edx
    asm.bytes(&[0x31, 0xC0]); // xor eax, eax
    asm.bytes(&[0x6B, 0xC0, 0x1F]); // loop: imul eax, eax, 31
    asm.bytes(&[0x01, 0xD0]); // add eax, edx
    asm.bytes(&[0xFF, 0xCA]); // dec edx
    asm.bytes(&[0x75, 0xF7]); // jnz loop
    asm.bytes(&[0xC3]); // ret
}

/// Independent model of [`worker`], from its specification above.
fn worker_reference(seed: u64) -> u32 {
    let count = (seed as u32 & 63) + 1;
    (1..=count)
        .rev()
        .fold(0u32, |acc, d| acc.wrapping_mul(31).wrapping_add(d))
}

/// The main routine. `worker_address` is the worker's guest address.
fn main_routine(asm: &mut Asm, worker_address: u64) {
    asm.bytes(&[0x48, 0x83, 0xEC, FRAME]); // sub rsp, FRAME
    asm.memory_status(STATUS[0], 0);

    // CreateEventW(NULL, FALSE, FALSE, NULL): auto-reset, not signaled.
    for register in [RCX, RDX, R8, R9] {
        asm.zero(register);
    }
    asm.call("CreateEventW");
    asm.save_rax(SAVED_EVENT);
    asm.record(1);

    // CreateThread(NULL, 0, worker, seed, 0, NULL).
    asm.zero(RCX);
    asm.zero(RDX);
    asm.mov_imm(R8, worker_address);
    asm.mov_imm(R9, SEED);
    asm.bytes(&[0x4D, 0x8B, 0x09]); // mov r9, [r9]
    asm.bytes(&[0x48, 0xC7, 0x44, 0x24, 0x20, 0, 0, 0, 0]); // mov qword [rsp+0x20], 0
    asm.bytes(&[0x48, 0xC7, 0x44, 0x24, 0x28, 0, 0, 0, 0]); // mov qword [rsp+0x28], 0
    asm.call("CreateThread");
    asm.save_rax(SAVED_THREAD);
    asm.record(2);
    asm.memory_status(STATUS[1], 3);

    asm.load(RCX, SAVED_THREAD);
    asm.mov_imm(RDX, u64::from(INFINITE));
    asm.call("WaitForSingleObject");
    asm.record(4);
    asm.load(RCX, SAVED_THREAD);
    asm.mov_imm(RDX, EXIT_CODE);
    asm.call("GetExitCodeThread");
    asm.record(5);
    asm.load(RCX, SAVED_THREAD);
    asm.call("CloseHandle");
    asm.record(6);

    // HeapAlloc(GetProcessHeap(), 0, HEAP_BLOCK).
    asm.call("GetProcessHeap");
    asm.save_rax(SAVED_HEAP);
    asm.bytes(&[0x48, 0x89, 0xC1]); // mov rcx, rax
    asm.zero(RDX);
    asm.mov_imm(R8, u64::from(HEAP_BLOCK));
    asm.call("HeapAlloc");
    asm.save_rax(SAVED_BLOCK);
    asm.record(7);
    asm.load(RAX, SAVED_BLOCK);
    asm.write_and_read_back(8);

    // VirtualAlloc(NULL, VIRTUAL_REGION, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE).
    asm.zero(RCX);
    asm.mov_imm(RDX, u64::from(VIRTUAL_REGION));
    asm.mov_imm(R8, u64::from(MEM_COMMIT_RESERVE));
    asm.mov_imm(R9, u64::from(PAGE_READWRITE));
    asm.call("VirtualAlloc");
    asm.save_rax(SAVED_REGION);
    asm.record(9);
    asm.load(RAX, SAVED_REGION);
    asm.write_and_read_back(10);
    asm.memory_status(STATUS[2], 11);

    asm.load(RCX, SAVED_HEAP);
    asm.zero(RDX);
    asm.load(R8, SAVED_BLOCK);
    asm.call("HeapFree");
    asm.record(12);
    asm.load(RCX, SAVED_REGION);
    asm.zero(RDX);
    asm.mov_imm(R8, u64::from(MEM_RELEASE));
    asm.call("VirtualFree");
    asm.record(13);

    asm.load(RCX, SAVED_EVENT);
    asm.call("SetEvent");
    asm.record(14);
    asm.load(RCX, SAVED_EVENT);
    asm.zero(RDX);
    asm.call("WaitForSingleObject");
    asm.record(15);
    asm.load(RCX, SAVED_EVENT);
    asm.call("CloseHandle");
    asm.record(16);
    asm.memory_status(STATUS[3], 17);

    asm.bytes(&[0x48, 0x83, 0xC4, FRAME]); // add rsp, FRAME
    asm.bytes(&[0xC3]); // ret
}

fn put(pe: &mut [u8], at: usize, bytes: &[u8]) {
    pe[at..at + bytes.len()].copy_from_slice(bytes);
}

/// Build the image. The entry point is `call main; hlt`, followed by main and
/// the worker; the entry point is the start of the text section.
fn image() -> Vec<u8> {
    let entry_len = 6;
    let main_start = entry_len;
    // The worker's address is needed inside main, so lay main out once to
    // learn its length; its length does not depend on the address.
    let main_len = {
        let mut probe = Asm::default();
        main_routine(&mut probe, 0);
        probe.code.len()
    };
    let worker_start = (main_start + main_len).next_multiple_of(16);
    let worker_address = BASE + u64::from(TEXT_RVA) + worker_start as u64;

    let mut asm = Asm::default();
    asm.bytes(&[0xE8]); // call main
    asm.bytes(&((main_start as i32 - 5).to_le_bytes()));
    asm.bytes(&[0xF4]); // hlt
    main_routine(&mut asm, worker_address);
    assert_eq!(asm.code.len(), main_start + main_len);
    asm.code.resize(worker_start, 0xCC);
    worker(&mut asm);
    assert!(asm.code.len() <= TEXT_LEN);

    let mut pe = vec![0u8; IDATA_RAW + IDATA_LEN];
    put(&mut pe, 0, b"MZ");
    put(&mut pe, 0x3C, &0x80u32.to_le_bytes());
    put(&mut pe, 0x80, b"PE\0\0");
    put(&mut pe, 0x84, &0x8664u16.to_le_bytes());
    put(&mut pe, 0x86, &2u16.to_le_bytes());
    put(&mut pe, 0x94, &240u16.to_le_bytes());
    let optional = 0x98;
    put(&mut pe, optional, &0x20Bu16.to_le_bytes());
    put(&mut pe, optional + 16, &TEXT_RVA.to_le_bytes());
    put(&mut pe, optional + 24, &BASE.to_le_bytes());
    put(&mut pe, optional + 32, &0x1000u32.to_le_bytes());
    put(&mut pe, optional + 36, &0x200u32.to_le_bytes());
    put(&mut pe, optional + 56, &0x10000u32.to_le_bytes());
    put(&mut pe, optional + 60, &0x200u32.to_le_bytes());
    put(&mut pe, optional + 108, &16u32.to_le_bytes());
    put(&mut pe, optional + 120, &IDATA_RVA.to_le_bytes());
    put(&mut pe, optional + 124, &0x100u32.to_le_bytes());
    for (index, (name, rva, raw, len, flags)) in [
        (b".text", TEXT_RVA, TEXT_RAW, TEXT_LEN, 0x6000_0020u32),
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
    put(&mut pe, TEXT_RAW, &asm.code);

    // One import descriptor, all from KERNEL32.dll.
    let mut names: Vec<&str> = asm.imports.iter().map(|(_, name)| *name).collect();
    names.sort_unstable();
    names.dedup();
    let file = |rva: u32| IDATA_RAW + (rva - IDATA_RVA) as usize;
    let mut next = IDATA_RVA + 40;
    let mut take = |len: usize| {
        let rva = next;
        next = (next + len as u32 + 7) & !7;
        rva
    };
    let module = b"KERNEL32.dll";
    let module_rva = take(module.len() + 1);
    put(&mut pe, file(module_rva), module);
    let table_len = 8 * (names.len() + 1);
    let (lookup, slots) = (take(table_len), take(table_len));
    let mut slot_of = BTreeMap::new();
    for (index, name) in names.iter().enumerate() {
        let hint = take(name.len() + 3);
        put(&mut pe, file(hint) + 2, name.as_bytes());
        let offset = 8 * index as u32;
        put(
            &mut pe,
            file(lookup + offset),
            &u64::from(hint).to_le_bytes(),
        );
        put(
            &mut pe,
            file(slots + offset),
            &u64::from(hint).to_le_bytes(),
        );
        slot_of.insert(*name, slots + offset);
    }
    assert!(next <= IDATA_RVA + IDATA_LEN as u32);
    let descriptor = file(IDATA_RVA);
    put(&mut pe, descriptor, &lookup.to_le_bytes());
    put(&mut pe, descriptor + 12, &module_rva.to_le_bytes());
    put(&mut pe, descriptor + 16, &slots.to_le_bytes());
    for (offset, name) in &asm.imports {
        let next_instruction = TEXT_RVA + *offset as u32 + 4;
        let displacement = slot_of[name].wrapping_sub(next_instruction);
        put(&mut pe, TEXT_RAW + offset, &displacement.to_le_bytes());
    }
    pe
}

struct Soak {
    engine: DbtEngine,
    memory: GuestMemory,
    state: Arc<ServicesState>,
    entry: u64,
}

impl Soak {
    fn new() -> Self {
        let bytes = image();
        let pe = ParsedPe::parse(&bytes).unwrap();
        let loaded = pe.load_into_memory(&bytes).unwrap();
        let entry = loaded.entry_point;
        let mut memory = loaded.memory;
        for (base, len) in [(SHARED, 0x1000), (STACK, STACK_LEN), (TEB, 0x2000)] {
            memory
                .map_region(base, len, MemoryPermissions::READ_WRITE)
                .unwrap();
        }
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(1_000_000))));
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, Arc::clone(&state));
        let bound = registry
            .bind_imports(&pe, &bytes, BASE, &mut memory)
            .unwrap();
        assert_eq!(bound, 12, "every import binds to a standard service");
        let mut engine = DbtEngine::new(1024 * 1024).unwrap();
        engine.set_import_registry(registry);
        Self {
            engine,
            memory,
            state,
            entry,
        }
    }

    fn iteration(&mut self, seed: u64) -> Iteration {
        self.memory.write_u64(SEED, seed).unwrap();
        self.memory.write_u64(EXIT_CODE, SENTINEL).unwrap();
        for index in 0..RESULT_COUNT as u64 {
            self.memory
                .write_u64(RESULTS + 8 * index, SENTINEL)
                .unwrap();
        }
        for record in STATUS {
            self.memory.write_bytes(record, &[0; 64]).unwrap();
            self.memory.write_u32(record, MEMORYSTATUSEX_LEN).unwrap();
        }
        let mut main = CpuState::new(self.entry, STACK + STACK_LEN as u64 - 0x100);
        main.gs_base = TEB;
        let report = Scheduler::new(Arc::clone(&self.state), DEFAULT_QUANTUM)
            .run(&mut self.engine, &mut self.memory, &main, ITERATION_BUDGET)
            .expect("the scheduler attaches");
        let results =
            std::array::from_fn(|index| self.memory.read_u64(RESULTS + 8 * index as u64).unwrap());
        Iteration {
            report,
            exit_code: self.memory.read_u32(EXIT_CODE).unwrap(),
            results,
            status: STATUS.map(|record| MemoryStatus::read(&self.memory, record)),
        }
    }
}

/// The fields of `MEMORYSTATUSEX` this test compares, as Xenora reports them:
/// guest memory in use is total minus available physical memory, and mapped
/// guest address space is total minus available virtual memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MemoryStatus {
    in_use: u64,
    mapped: u64,
}

impl MemoryStatus {
    fn read(memory: &GuestMemory, record: u64) -> Self {
        let field = |offset| memory.read_u64(record + offset).unwrap();
        Self {
            in_use: field(8) - field(16),
            mapped: field(40) - field(48),
        }
    }
}

struct Iteration {
    report: ScheduleReport,
    exit_code: u32,
    results: [u64; RESULT_COUNT],
    status: [MemoryStatus; 4],
}

/// What one seed must reproduce exactly on every later iteration.
#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    instructions: usize,
    turns: usize,
    exit_code: u32,
    status: [MemoryStatus; 4],
}

struct Summary {
    iterations: usize,
    elapsed: Duration,
    peak_in_use: u64,
    peak_mapped: u64,
    baseline: MemoryStatus,
    blocks_compiled: usize,
    native_bytes: usize,
    guest_instructions: usize,
}

impl Summary {
    fn report(&self) {
        println!(
            "soak: {} iterations in {:.3?}, {} guest instructions; \
             baseline in use {} B, mapped {} B; peak in use {} B, mapped {} B; \
             {} blocks translated, {} native bytes",
            self.iterations,
            self.elapsed,
            self.guest_instructions,
            self.baseline.in_use,
            self.baseline.mapped,
            self.peak_in_use,
            self.peak_mapped,
            self.blocks_compiled,
            self.native_bytes,
        );
    }
}

/// Run `iterations` iterations, or fewer if `time_limit` would be exceeded,
/// which fails. Every check runs after every iteration.
fn soak(iterations: usize, time_limit: Duration) -> Summary {
    let started = Instant::now();
    let mut soak = Soak::new();
    let handles_at_start = soak.state.open_handle_count();
    assert_eq!(handles_at_start, 0);
    let mut baseline = None;
    let mut warm_cache = None;
    let mut fingerprints: BTreeMap<u64, Fingerprint> = BTreeMap::new();
    let (mut peak_in_use, mut peak_mapped, mut guest_instructions) = (0, 0, 0);

    for index in 0..iterations {
        let seed = SEEDS[index % SEEDS.len()];
        let run = soak.iteration(seed);
        let at = format!("iteration {index}, seed {seed:#x}");
        assert!(
            started.elapsed() <= time_limit,
            "{at}: over the {time_limit:?} time limit"
        );
        assert_eq!(run.report.outcome, Outcome::Halted, "{at}: budget or fault");
        assert!(run.report.instructions < ITERATION_BUDGET, "{at}");
        guest_instructions += run.report.instructions;

        // Exactly one worker existed, and it is gone.
        assert_eq!(run.report.peak_threads, 2, "{at}: guest threads");

        for (index, (actual, expected)) in
            run.results.iter().zip(expected_results(seed)).enumerate()
        {
            match expected {
                Some(value) => assert_eq!(*actual, value, "{at}: result {index}"),
                None => assert!(*actual != 0 && *actual != SENTINEL, "{at}: result {index}"),
            }
        }
        assert_eq!(
            run.exit_code,
            worker_reference(seed),
            "{at}: worker exit code"
        );

        // Handles back at baseline.
        assert_eq!(
            soak.state.open_handle_count(),
            handles_at_start,
            "{at}: open handles"
        );

        // Memory back where the iteration started. The first iteration may
        // keep the worker slot's errno cell, and nothing else.
        let [start, worker_alive, held, end] = run.status;
        let retained = if index == 0 { ERRNO_CELL } else { 0 };
        assert_eq!(
            end,
            MemoryStatus {
                mapped: start.mapped + retained,
                ..start
            },
            "{at}: memory not returned"
        );
        // Each run ends where it started and nothing runs between runs, so
        // this also bounds growth across iterations.
        baseline.get_or_insert(end);
        assert!(worker_alive.mapped > start.mapped, "{at}: worker stack");
        assert_eq!(
            held.in_use,
            start.in_use + u64::from(HEAP_BLOCK + VIRTUAL_REGION),
            "{at}: allocations not counted exactly"
        );
        peak_in_use = peak_in_use.max(held.in_use).max(worker_alive.in_use);
        peak_mapped = peak_mapped.max(held.mapped).max(worker_alive.mapped);

        // Identical seeds give identical runs, compared from the second pass
        // over the seeds so the first iteration's errno cell is excluded.
        let fingerprint = Fingerprint {
            instructions: run.report.instructions,
            turns: run.report.turns,
            exit_code: run.exit_code,
            status: run.status,
        };
        if index >= SEEDS.len() {
            match fingerprints.get(&seed) {
                Some(first) => assert_eq!(&fingerprint, first, "{at}: not deterministic"),
                None => {
                    fingerprints.insert(seed, fingerprint);
                }
            }
        }

        // The translation cache stops growing once every seed has run once.
        let metrics = soak.engine.metrics;
        let cache = (metrics.blocks_compiled, metrics.native_bytes_emitted);
        if index + 1 == SEEDS.len() {
            warm_cache = Some(cache);
        } else if let Some(warm) = warm_cache {
            assert_eq!(cache, warm, "{at}: translation cache grew");
        }
    }

    let metrics = soak.engine.metrics;
    Summary {
        iterations,
        elapsed: started.elapsed(),
        peak_in_use,
        peak_mapped,
        baseline: baseline.expect("at least one iteration"),
        blocks_compiled: metrics.blocks_compiled,
        native_bytes: metrics.native_bytes_emitted,
        guest_instructions,
    }
}

#[test]
fn worker_reference_matches_hand_computed_values() {
    // count 1: 1. count 2: (0*31+2)*31+1 = 63. count 3: ((3*31)+2)*31+1 = 2946.
    assert_eq!(worker_reference(0), 1);
    assert_eq!(worker_reference(1), 63);
    assert_eq!(worker_reference(2), 2946);
    // Only the low six bits count.
    assert_eq!(worker_reference(0x40), worker_reference(0));
}

#[test]
fn bounded_soak_returns_every_resource_to_baseline() {
    let summary = soak(32 * SEEDS.len(), Duration::from_secs(60));
    summary.report();
}

/// Long variant; see the module documentation for the command.
#[test]
#[ignore = "long soak, run manually"]
fn long_soak_returns_every_resource_to_baseline() {
    let summary = soak(2_000_000, Duration::from_secs(600));
    summary.report();
}
