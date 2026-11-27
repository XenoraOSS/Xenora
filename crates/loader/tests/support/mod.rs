// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared harness for runtime service tests: an original PE32+ image whose
//! imports are bound to the standard service registry and called from
//! translated guest code.

#![allow(dead_code)]

pub mod robustness;

use std::sync::Arc;
use xenora_loader::cpu::{CpuState, ExitReason, Register};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};
use xenora_loader::pe::ParsedPe;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};

pub const BASE: u64 = 0x1_4000_0000;
pub const CODE_RVA: u32 = 0x1000;
const IDATA_RVA: u32 = 0x10000;
const IDATA_RAW: usize = 0x2200;
const IDATA_LEN: usize = 0x4000;
pub const SCRATCH: u64 = 0x5_0000;
pub const SCRATCH_LEN: u64 = 0x1000;
pub const STACK: u64 = 0x8_0000;
pub const STACK_TOP: u64 = STACK + 0x800;
pub const THREAD_A: u64 = 0x7_0000;
pub const THREAD_B: u64 = 0x7_2000;
/// Guest routines start at this RVA in the text section, 64 bytes apart.
pub const ROUTINES_RVA: u32 = 0x1800;
pub const ROUTINE_SIZE: usize = 64;
pub const RUNTIME_LIMIT: ExitReason = ExitReason::IllegalInstruction(0xFE);

/// Guest address of routine `index`.
pub fn routine(index: usize) -> u64 {
    BASE + u64::from(ROUTINES_RVA) + (ROUTINE_SIZE * index) as u64
}

/// Guest address of the import address table slot for `imports[index]`, as
/// laid out by the image builder.
pub fn import_slot(imports: &[(&str, &str)], index: usize) -> u64 {
    BASE + u64::from(build_image(imports, &[]).1[index])
}

#[derive(Clone, Copy)]
enum ImageSymbol<'a> {
    Name(&'a str),
    Ordinal(u16),
}

/// The code for import `n`, at RVA 0x1000 + 16n, is `call [its slot]; hlt`.
/// Each import has its own descriptor.
pub fn build_image(imports: &[(&str, &str)], routines: &[Vec<u8>]) -> (Vec<u8>, Vec<u32>) {
    let symbols: Vec<_> = imports
        .iter()
        .map(|(module, symbol)| (*module, ImageSymbol::Name(symbol)))
        .collect();
    build_image_symbols(&symbols, routines)
}

/// Build the same kind of original PE32+ image with ordinal import entries.
pub fn build_ordinal_image(imports: &[(&str, u16)]) -> (Vec<u8>, Vec<u32>) {
    let symbols: Vec<_> = imports
        .iter()
        .map(|(module, ordinal)| (*module, ImageSymbol::Ordinal(*ordinal)))
        .collect();
    build_image_symbols(&symbols, &[])
}

fn build_image_symbols(
    imports: &[(&str, ImageSymbol<'_>)],
    routines: &[Vec<u8>],
) -> (Vec<u8>, Vec<u32>) {
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

    assert!(16 * imports.len() <= (ROUTINES_RVA - CODE_RVA) as usize);
    let file = |rva: u32| IDATA_RAW + (rva - IDATA_RVA) as usize;
    let descriptor_size = 20 * (imports.len() as u32 + 1);
    put(&mut pe, optional + 124, &descriptor_size.to_le_bytes());
    let mut next = IDATA_RVA + descriptor_size;
    let mut take = |len: usize| {
        let rva = next;
        next = (next + len as u32 + 7) & !7;
        rva
    };
    let mut slots = Vec::with_capacity(imports.len());
    for (index, (module, symbol)) in imports.iter().enumerate() {
        let module_rva = take(module.len() + 1);
        put(&mut pe, file(module_rva), module.as_bytes());
        let lookup_value = match *symbol {
            ImageSymbol::Name(symbol) => {
                let hint = take(symbol.len() + 3);
                put(&mut pe, file(hint) + 2, symbol.as_bytes());
                u64::from(hint)
            }
            ImageSymbol::Ordinal(ordinal) => (1u64 << 63) | u64::from(ordinal),
        };
        let (lookup, slot) = (take(16), take(16));
        put(&mut pe, file(lookup), &lookup_value.to_le_bytes());
        put(&mut pe, file(slot), &lookup_value.to_le_bytes());
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
    for (index, code) in routines.iter().enumerate() {
        assert!(code.len() <= ROUTINE_SIZE);
        let at = 0x200 + (ROUTINES_RVA - CODE_RVA) as usize + ROUTINE_SIZE * index;
        put(&mut pe, at, code);
    }
    (pe, slots)
}

pub struct Guest {
    engine: DbtEngine,
    pub memory: GuestMemory,
    pub state: Arc<ServicesState>,
    imports: &'static [(&'static str, &'static str)],
    teb: u64,
}

impl Guest {
    pub fn new(imports: &'static [(&'static str, &'static str)], routines: &[Vec<u8>]) -> Self {
        let state = ServicesState::new(Arc::new(MockClock::new(0)));
        Self::with_state(imports, routines, state)
    }

    pub fn with_state(
        imports: &'static [(&'static str, &'static str)],
        routines: &[Vec<u8>],
        state: ServicesState,
    ) -> Self {
        let (bytes, _) = build_image(imports, routines);
        let pe = ParsedPe::parse(&bytes).unwrap();
        let mut memory = pe.load_into_memory(&bytes).unwrap().memory;
        memory
            .map_region(SCRATCH, SCRATCH_LEN as usize, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory
            .map_region(STACK, 0x1000, MemoryPermissions::READ_WRITE)
            .unwrap();
        let state = Arc::new(state);
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, Arc::clone(&state));
        let bound = registry
            .bind_imports(&pe, &bytes, BASE, &mut memory)
            .unwrap();
        assert_eq!(bound, imports.len());
        let mut engine = DbtEngine::new(256 * 1024).unwrap();
        engine.set_import_registry(registry);
        Self {
            engine,
            memory,
            state,
            imports,
            teb: THREAD_A,
        }
    }

    pub fn on(&mut self, teb: u64) -> &mut Self {
        self.teb = teb;
        self
    }

    /// Call an import with Win64 arguments. Arguments after the fourth go on
    /// the stack after the return address and the home area.
    pub fn run(&mut self, symbol: &str, args: &[u64]) -> (u64, ExitReason) {
        let index = self
            .imports
            .iter()
            .position(|(_, name)| *name == symbol)
            .unwrap_or_else(|| panic!("{symbol} is not imported"));
        let mut cpu = CpuState::new(BASE + u64::from(CODE_RVA) + 16 * index as u64, STACK_TOP);
        cpu.gs_base = self.teb;
        for (register, value) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
            .into_iter()
            .zip(args)
        {
            cpu.set(register, *value);
        }
        for (slot, value) in args.iter().skip(4).enumerate() {
            let address = STACK_TOP + 32 + 8 * slot as u64;
            self.memory.write_u64(address, *value).unwrap();
        }
        use Register::{R12, R13, R14, R15, Rbp, Rbx, Rdi, Rsi};
        let nonvolatile = [Rbx, Rbp, Rdi, Rsi, R12, R13, R14, R15];
        for (index, register) in nonvolatile.into_iter().enumerate() {
            cpu.set(register, 0x5EED_0000 + index as u64);
        }
        let result = self.engine.run(&mut cpu, &mut self.memory, 100_000);
        if result.exit_reason == ExitReason::Halt {
            // The service returned to the instruction after its CALL.
            assert_eq!(cpu.get(Register::Rsp), STACK_TOP, "{symbol}");
            for (index, register) in nonvolatile.into_iter().enumerate() {
                assert_eq!(cpu.get(register), 0x5EED_0000 + index as u64, "{symbol}");
            }
        }
        (cpu.get(Register::Rax), result.exit_reason)
    }

    /// Call an import as the main thread of a scheduled run: the engine runs
    /// in slices of `quantum` instructions under the guest thread scheduler.
    pub fn scheduled(
        &mut self,
        symbol: &str,
        args: &[u64],
        quantum: usize,
        budget: usize,
    ) -> xenora_loader::scheduler::ScheduleReport {
        let index = self
            .imports
            .iter()
            .position(|(_, name)| *name == symbol)
            .unwrap_or_else(|| panic!("{symbol} is not imported"));
        let mut cpu = CpuState::new(BASE + u64::from(CODE_RVA) + 16 * index as u64, STACK_TOP);
        cpu.gs_base = self.teb;
        for (register, value) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
            .into_iter()
            .zip(args)
        {
            cpu.set(register, *value);
        }
        xenora_loader::scheduler::Scheduler::new(Arc::clone(&self.state), quantum)
            .run(&mut self.engine, &mut self.memory, &cpu, budget)
            .expect("the scheduler attaches")
    }

    /// Run guest code from `entry` on the calling thread's TEB until it stops.
    pub fn execute(&mut self, entry: u64, max_instructions: usize) -> (CpuState, ExitReason) {
        let mut cpu = CpuState::new(entry, STACK_TOP);
        cpu.gs_base = self.teb;
        let result = self
            .engine
            .run(&mut cpu, &mut self.memory, max_instructions);
        (cpu, result.exit_reason)
    }

    pub fn call(&mut self, symbol: &str, args: &[u64]) -> u64 {
        let (rax, exit) = self.run(symbol, args);
        assert_eq!(exit, ExitReason::Halt, "{symbol}");
        rax
    }

    pub fn fails(&mut self, symbol: &str, args: &[u64]) -> ExitReason {
        let (_, exit) = self.run(symbol, args);
        assert_ne!(exit, ExitReason::Halt, "{symbol} should stop the guest");
        exit
    }

    pub fn error(&mut self) -> u64 {
        self.call("GetLastError", &[])
    }

    pub fn u16_at(&self, address: u64) -> u16 {
        self.memory.read_u16(address).unwrap()
    }

    pub fn u32_at(&self, address: u64) -> u32 {
        self.memory.read_u32(address).unwrap()
    }

    pub fn u64_at(&self, address: u64) -> u64 {
        self.memory.read_u64(address).unwrap()
    }
}
