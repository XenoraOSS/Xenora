// SPDX-License-Identifier: GPL-3.0-or-later

//! Drives sequences of standard runtime services with adversarial arguments.
//!
//! Shared by the `runtime_services` libFuzzer target and the stable-toolchain
//! regression test. Writable guest memory is filled from a deterministic
//! pattern, not from the fuzz input, so short corpora still reach the dispatch
//! loop. Each call takes its arguments from the remaining input: zero, all
//! ones, pointers into writable, read-only and unmapped memory, raw values,
//! small integers or an earlier result (so handles flow between calls). After
//! every call the driver checks that read-only and code memory are unchanged
//! and that the Win64 nonvolatile registers are preserved. A panic, a broken
//! invariant or a hang is a bug.

use std::collections::BTreeSet;
use std::sync::Arc;
use xenora_loader::cpu::{CpuState, Register};
use xenora_loader::imports::{ImportRegistry, Win64Args};
use xenora_loader::memory::{GuestMemory, MemoryPermissions};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};

const WRITABLE: u64 = 0x5_0000;
const WRITABLE_LEN: usize = 0x1_0000;
const READ_ONLY: u64 = 0x7_0000;
const CODE: u64 = 0x7_8000;
const REGION_LEN: usize = 0x1000;
const STACK: u64 = 0x9_0000;
const STACK_LEN: usize = 0x2000;
const UNMAPPED: u64 = 0xDEAD_0000;
const MAX_CALLS: usize = 48;

/// Argument encoding mode that reuses an earlier handler result (handles).
#[allow(dead_code)] // used by the stable-toolchain regression test
pub const ARG_REUSE_RESULT: u8 = 8;
/// Argument encoding mode that supplies zero.
#[allow(dead_code)] // used by the stable-toolchain regression test
pub const ARG_ZERO: u8 = 0;

/// Services that wait on real time, run guest code or reach native Metal are
/// left out, so each input runs quickly and deterministically.
fn excluded(module: &str, symbol: &str) -> bool {
    module.eq_ignore_ascii_case("xenora_fixture.dll")
        || module.eq_ignore_ascii_case("xenora_graphics.dll")
        || matches!(symbol, "WSAPoll" | "XTaskQueueDispatch")
}

/// Outcome of one `Driver::run` against a single corpus input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunStats {
    pub calls: usize,
    pub distinct_services: usize,
}

pub struct Driver {
    services: Vec<(String, String)>,
}

impl Default for Driver {
    fn default() -> Self {
        Self::new()
    }
}

impl Driver {
    pub fn new() -> Self {
        let mut registry = ImportRegistry::new();
        register_standard_services(
            &mut registry,
            Arc::new(ServicesState::new(Arc::new(MockClock::new(0)))),
        );
        let mut services: Vec<(String, String)> = registry
            .iter_registered()
            .filter(|(module, symbol, _)| !excluded(module, symbol))
            .map(|(module, symbol, _)| (module.to_owned(), symbol.to_owned()))
            .collect();
        services.sort();
        Self { services }
    }

    #[allow(dead_code)] // used by the stable-toolchain regression test
    pub fn service_count(&self) -> usize {
        self.services.len()
    }

    /// Index of `module!symbol` in the driver's sorted service table.
    #[allow(dead_code)] // used by the stable-toolchain regression test
    pub fn service_index(&self, module: &str, symbol: &str) -> Option<usize> {
        self.services
            .iter()
            .position(|(entry_module, entry_symbol)| {
                entry_module.eq_ignore_ascii_case(module) && entry_symbol == symbol
            })
    }

    /// Encode a deterministic multi-call corpus.
    ///
    /// Each entry is `(service_index, [arg_mode; 7])`. Argument offsets are
    /// zero; a trailing byte per call supplies the GS-base choice.
    #[allow(dead_code)] // used by the stable-toolchain regression test
    pub fn encode_sequence(&self, calls: &[(usize, [u8; 7])]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(calls.len() * (2 + 7 * 3 + 1));
        for &(index, modes) in calls {
            bytes.extend_from_slice(&(index as u16).to_le_bytes());
            for mode in modes {
                bytes.extend_from_slice(&0u16.to_le_bytes());
                bytes.push(mode);
            }
            bytes.push(0); // gs_base selector
        }
        bytes
    }

    /// Run one input against fresh service state and fresh guest memory.
    pub fn run(&self, data: &[u8]) -> RunStats {
        let mut registry = ImportRegistry::new();
        let clock = Arc::new(MockClock::new(0x01DA_0000_0000_0000));
        register_standard_services(&mut registry, Arc::new(ServicesState::new(clock)));

        let mut input = Input(data);
        let mut memory = GuestMemory::new();
        memory
            .map_region(WRITABLE, WRITABLE_LEN, MemoryPermissions::READ_WRITE)
            .unwrap();
        memory
            .map_region(STACK, STACK_LEN, MemoryPermissions::READ_WRITE)
            .unwrap();
        // Deterministic fill: do not consume fuzz/corpus bytes here, or short
        // inputs never reach the dispatch loop (WRITABLE_LEN is 65,536).
        let fill: Vec<u8> = (0..WRITABLE_LEN)
            .map(|i| (i.wrapping_mul(131).wrapping_add(17)) as u8)
            .collect();
        memory.write_bytes(WRITABLE, &fill).unwrap();
        let read_only: Vec<u8> = (0..REGION_LEN).map(|i| (i * 7) as u8).collect();
        memory
            .map_region_with_data(
                READ_ONLY,
                REGION_LEN,
                MemoryPermissions::READ_ONLY,
                &read_only,
            )
            .unwrap();
        memory
            .map_region_with_data(
                CODE,
                REGION_LEN,
                MemoryPermissions::READ_EXECUTE,
                &[0xC3; REGION_LEN],
            )
            .unwrap();

        let mut results = vec![0u64];
        let mut seen = BTreeSet::new();
        let mut calls = 0usize;
        for _ in 0..MAX_CALLS {
            let Some(selector) = input.u16() else { break };
            let index = selector as usize % self.services.len();
            let (module, symbol) = &self.services[index];
            let mut args = [0u64; 7];
            for arg in &mut args {
                *arg = input.argument(&results);
            }
            let rsp = STACK + STACK_LEN as u64 / 2;
            let mut cpu = CpuState::new(CODE, rsp);
            cpu.gs_base = [0x7_F000, 0x7_F800][input.byte_or(0) as usize % 2];
            for (register, value) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
                .into_iter()
                .zip(args)
            {
                cpu.set(register, value);
            }
            for (slot, value) in args[4..].iter().enumerate() {
                memory
                    .write_u64(rsp + 40 + 8 * slot as u64, *value)
                    .unwrap();
            }
            use Register::{R12, R13, R14, R15, Rbp, Rbx, Rdi, Rsi, Rsp};
            let preserved = [Rbx, Rbp, Rdi, Rsi, Rsp, R12, R13, R14, R15];
            let before = preserved.map(|register| cpu.get(register));

            let service = registry
                .binding(module, symbol)
                .expect("registered service");
            let outcome = (service.handler)(&Win64Args::new(&cpu), &mut cpu, &mut memory);

            assert_eq!(
                preserved.map(|register| cpu.get(register)),
                before,
                "{module}!{symbol} changed a nonvolatile register"
            );
            let mut check = vec![0; REGION_LEN];
            memory.read_bytes(READ_ONLY, &mut check).unwrap();
            assert_eq!(check, read_only, "{module}!{symbol} wrote read-only memory");
            memory.read_bytes(CODE, &mut check).unwrap();
            assert!(
                check.iter().all(|&byte| byte == 0xC3),
                "{module}!{symbol} wrote code memory"
            );
            calls += 1;
            seen.insert((module.clone(), symbol.clone()));
            if let Ok(value) = outcome {
                results.push(value);
            }
        }
        RunStats {
            calls,
            distinct_services: seen.len(),
        }
    }
}

struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }

    fn byte_or(&mut self, default: u8) -> u8 {
        self.take::<1>().map_or(default, |[byte]| byte)
    }

    fn u16(&mut self) -> Option<u16> {
        self.take::<2>().map(u16::from_le_bytes)
    }

    fn argument(&mut self, results: &[u64]) -> u64 {
        let offset = u64::from(self.u16().unwrap_or(0));
        match self.byte_or(0) % 9 {
            0 => 0,
            1 => u64::MAX,
            2 => WRITABLE + offset % WRITABLE_LEN as u64,
            3 => READ_ONLY + offset % REGION_LEN as u64,
            4 => CODE + offset % REGION_LEN as u64,
            5 => UNMAPPED + offset,
            6 => self.take::<8>().map_or(offset, u64::from_le_bytes),
            7 => offset % 512,
            _ => results[offset as usize % results.len()],
        }
    }
}
