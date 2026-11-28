// SPDX-License-Identifier: GPL-3.0-or-later

//! MASKMOVDQU and VMASKMOVDQU in both engines, against an independent
//! byte-array reference.
//!
//! The vector fixture files pair LLVM encodings with Intel-syntax text. The
//! reference here reads only the text: the memory address is implicit in RDI,
//! the sign bit of each mask byte decides whether the corresponding source
//! byte is stored, and a masked byte is never accessed, so its fault is
//! suppressed. It shares nothing with the registry, the decoder or the
//! production semantics. Every byte position is masked off in some case, and
//! the boundary test places RDI so the enabled lanes end exactly at the edge
//! of mapped memory: one more enabled lane would fault.

use std::collections::BTreeMap;
use xenora_loader::cpu::{CpuState, ExecutionResult, ExitReason, Interpreter, RFlags};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};

const ENTRY: u64 = 0x1000;
const DATA: u64 = 0x4000;
const PAGE: u64 = 0x1000;

fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    let mut out = BTreeMap::new();
    for file in [include_str!("x86/vector.txt"), include_str!("x86/avx.txt")] {
        for line in file.lines() {
            let (hex, text) = line.split_once(" | ").unwrap();
            if text.contains("maskmovdqu") {
                let code = hex
                    .split(' ')
                    .map(|byte| u8::from_str_radix(byte, 16).unwrap())
                    .collect();
                out.insert(text, code);
            }
        }
    }
    out.into_iter().collect()
}

/// The data and mask register numbers of a form: `maskmovdqu xmm3, xmm9`.
fn parse(text: &str) -> (usize, usize) {
    let parts: Vec<&str> = text.split_once(' ').unwrap().1.split(", ").collect();
    let register = |text: &str| text.trim_start_matches("xmm").parse().unwrap();
    (register(parts[0]), register(parts[1]))
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

fn run(native: bool, cpu: &CpuState, memory: &mut GuestMemory) -> ExecutionResult {
    if native {
        DbtEngine::new(64 * 1024)
            .unwrap()
            .run(&mut cpu.clone(), memory, 1)
    } else {
        Interpreter::new(cpu.clone(), memory, 1).run()
    }
}

fn machine(code: &[u8], rdi: u64) -> (CpuState, GuestMemory) {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    memory
        .map_region(DATA, (2 * PAGE) as usize, MemoryPermissions::READ_WRITE)
        .unwrap();
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr[7] = rdi;
    cpu.flags = RFlags::default();
    (cpu, memory)
}

#[test]
fn masked_stores_match_the_byte_reference_in_both_engines() {
    let mut random = Random(0x00A5_C38D_21E7_B490);
    let mut checked = 0;
    for (text, code) in fixtures() {
        let (data_register, mask_register) = parse(text);
        for _ in 0..32 {
            let mut data = [0u8; 16];
            let mut mask = [0u8; 16];
            let mut before = [0u8; 16];
            for byte in 0..16 {
                data[byte] = random.next() as u8;
                // Roughly half the lanes enabled, every position over the rounds.
                mask[byte] = if random.next().is_multiple_of(2) {
                    0x80
                } else {
                    0x7F
                };
                before[byte] = random.next() as u8;
            }
            // Aliasing decides the effective register contents, exactly as the
            // hardware reads them.
            let effective_data = data;
            let effective_mask = if mask_register == data_register {
                data
            } else {
                mask
            };
            // The reference: byte i is stored only when its mask sign bit is set.
            let mut wanted = before;
            let mut any = false;
            for byte in 0..16 {
                if effective_mask[byte] & 0x80 != 0 {
                    wanted[byte] = effective_data[byte];
                    any = true;
                }
            }
            let _ = any;
            let (mut cpu, mut memory) = machine(&code, DATA + PAGE);
            cpu.xmm[data_register] = u128::from_le_bytes(data);
            if mask_register != data_register {
                cpu.xmm[mask_register] = u128::from_le_bytes(mask);
            }
            memory.write_bytes(DATA + PAGE, &before).unwrap();
            for native in [false, true] {
                let (cpu, mut memory) = (cpu.clone(), memory.clone());
                let result = run(native, &cpu, &mut memory);
                assert_eq!(result.exit_reason, ExitReason::StepLimitExceeded, "{text}");
                // The source registers are unchanged: the instruction writes memory only.
                assert_eq!(result.final_state.xmm, cpu.xmm, "{text} registers");
                let mut stored = [0u8; 16];
                memory.read_bytes(DATA + PAGE, &mut stored).unwrap();
                assert_eq!(stored, wanted, "{text} memory");
            }
            checked += 1;
        }
    }
    assert!(checked > 150, "{checked}");
}

#[test]
fn masked_bytes_past_the_end_of_memory_never_fault() {
    for (text, code) in fixtures() {
        let (data_register, mask_register) = parse(text);
        for native in [false, true] {
            // RDI four bytes before the end of mapped memory: lanes 0-3 in
            // bounds, lanes 4-15 out. Only lanes 0-3 enabled: no fault.
            let (mut cpu, mut memory) = machine(&code, DATA + 2 * PAGE - 4);
            let mut mask = [0u8; 16];
            for byte in mask.iter_mut().take(4) {
                *byte = 0x80;
            }
            cpu.xmm[data_register] = 0xA5A5_A5A5_A5A5_A5A5_A5A5_A5A5_A5A5_A5A5;
            if mask_register != data_register {
                cpu.xmm[mask_register] = u128::from_le_bytes(mask);
            } else {
                // Aliased: the data is the mask, so use bytes whose low lanes
                // are negative and high lanes positive.
                cpu.xmm[data_register] = u128::from_le_bytes([
                    0x80, 0x80, 0x80, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                ]);
            }
            let result = run(native, &cpu, &mut memory);
            assert_eq!(
                result.exit_reason,
                ExitReason::StepLimitExceeded,
                "{text} masked lanes must not fault"
            );
            // Now lane 4 enabled: the store faults.
            let (mut cpu, mut memory) = machine(&code, DATA + 2 * PAGE - 4);
            let mut mask = [0u8; 16];
            for byte in mask.iter_mut().take(5) {
                *byte = 0x80;
            }
            cpu.xmm[data_register] = 0xA5A5_A5A5_A5A5_A5A5_A5A5_A5A5_A5A5_A5A5;
            if mask_register != data_register {
                cpu.xmm[mask_register] = u128::from_le_bytes(mask);
            } else {
                cpu.xmm[data_register] = u128::from_le_bytes([
                    0x80, 0x80, 0x80, 0x80, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                ]);
            }
            let result = run(native, &cpu, &mut memory);
            assert!(
                matches!(result.exit_reason, ExitReason::MemoryFault(_)),
                "{text} an enabled lane out of memory must fault, got {:?}",
                result.exit_reason
            );
        }
    }
}
