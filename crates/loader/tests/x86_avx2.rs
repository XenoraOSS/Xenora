// SPDX-License-Identifier: GPL-3.0-or-later

//! AVX2 cross-lane permutations, broadcasts, blends and per-lane shifts.
//!
//! `tests/x86/avx2.txt` pairs LLVM encodings with Intel-syntax text. The
//! reference below reads only the text and works on byte arrays, following the
//! operation sections of Intel SDM Vol. 2. It shares nothing with the registry,
//! the decoder or the production semantics.
//!
//! The two operand roles of a variable shift are the ones the manual gives:
//! `SRC1`, the register named by VEX.vvvv, holds the value and `SRC2`, the
//! ModRM operand, holds the count. The last test uses asymmetric operands so a
//! swapped pair cannot pass.

use std::collections::BTreeMap;
use xenora_loader::cpu::{CpuState, ExecutionResult, ExitReason, Interpreter, Mxcsr, RFlags};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};

const ENTRY: u64 = 0x1000;
const DATA: u64 = 0x2000;
/// Address of `[rsi + 4*rdi + 16]` with RSI = DATA and RDI = 4.
const OPERAND: u64 = DATA + 32;

type Wide = [u8; 32];

fn fixture() -> BTreeMap<&'static str, Vec<u8>> {
    include_str!("x86/avx2.txt")
        .lines()
        .map(|line| {
            let (hex, text) = line.split_once(" | ").unwrap();
            let code = hex
                .split(' ')
                .map(|byte| u8::from_str_radix(byte, 16).unwrap())
                .collect();
            (text, code)
        })
        .collect()
}

/// One operand of an instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operand {
    /// A vector register and whether the text named it as `ymm`.
    Register(u8, bool),
    Memory(usize),
    Immediate(u8),
}

fn operand(text: &str) -> Operand {
    // A memory operand names its width first, so `xmmword` is not `xmm`.
    let register = |prefix: &str, wide: bool| {
        text.strip_prefix(prefix)
            .filter(|name| name.chars().all(|digit| digit.is_ascii_digit()))
            .map(|name| Operand::Register(name.parse().unwrap(), wide))
    };
    if let Some(register) = register("ymm", true).or_else(|| register("xmm", false)) {
        return register;
    }
    let size = if text.starts_with("xmmword") {
        16
    } else if text.starts_with("ymmword") {
        32
    } else if text.starts_with("byte") {
        1
    } else if text.starts_with("word") {
        2
    } else if text.starts_with("dword") {
        4
    } else if text.starts_with("qword") {
        8
    } else {
        return Operand::Immediate(text.parse().unwrap());
    };
    Operand::Memory(size)
}

fn split(text: &str) -> (&str, Vec<Operand>) {
    let (name, rest) = text.split_once(' ').unwrap();
    (name, rest.split(", ").map(operand).collect())
}

/// Little-endian lane of a byte array.
fn lane(bytes: &[u8], size: usize, index: usize) -> u64 {
    let mut value = 0;
    for offset in (0..size).rev() {
        value = value << 8 | u64::from(bytes[index * size + offset]);
    }
    value
}

fn put(bytes: &mut [u8], size: usize, index: usize, value: u64) {
    for offset in 0..size {
        bytes[index * size + offset] = (value >> (8 * offset)) as u8;
    }
}

/// `SELECT4` of the manual: two selector bits, and a third that zeroes.
fn select4(first: &Wide, second: &Wide, control: u8) -> [u8; 16] {
    let chosen = match control & 0b1011 {
        0 => &first[..16],
        1 => &first[16..],
        2 => &second[..16],
        3 => &second[16..],
        _ => return [0; 16],
    };
    chosen.try_into().unwrap()
}

/// The vector length of an instruction, from its destination.
fn length(name: &str, operands: &[Operand]) -> usize {
    match name {
        "vpermd" | "vpermps" | "vpermq" | "vpermpd" | "vperm2i128" | "vinserti128"
        | "vbroadcasti128" => 32,
        "vextracti128" => 16,
        "vpmaskmovd" | "vpmaskmovq" => operands
            .iter()
            .find_map(|operand| match operand {
                Operand::Memory(size) => Some(*size),
                _ => None,
            })
            .unwrap(),
        _ => match operands[0] {
            Operand::Register(_, wide) if wide => 32,
            _ => 16,
        },
    }
}

/// The result of one instruction from the manual's operation section: the
/// destination bytes and, for a store, the bytes written to memory.
fn reference(
    name: &str,
    operands: &[Operand],
    registers: &[Wide; 16],
    memory: &[u8; 32],
    length: usize,
) -> (Wide, Option<Wide>) {
    let source_of = |operand: Operand| -> Wide {
        match operand {
            Operand::Register(index, _) => registers[usize::from(index)],
            Operand::Memory(size) => {
                let mut value = [0; 32];
                value[..size].copy_from_slice(&memory[..size]);
                value
            }
            Operand::Immediate(_) => unreachable!(),
        }
    };
    let immediate = |operand: Operand| match operand {
        Operand::Immediate(value) => value,
        _ => unreachable!(),
    };
    let mut out = [0; 32];
    match name {
        // DEST[i] := SRC2[SRC1[i] & 7], the index in VEX.vvvv.
        "vpermd" | "vpermps" => {
            let indices = source_of(operands[1]);
            let data = source_of(operands[2]);
            for index in 0..length / 4 {
                let selected = (lane(&indices, 4, index) & 7) as usize;
                out[4 * index..4 * index + 4]
                    .copy_from_slice(&data[4 * selected..4 * selected + 4]);
            }
        }
        // DEST[i] := SRC[(imm8 >> 2*i) & 3], a 64-bit lane.
        "vpermq" | "vpermpd" => {
            let data = source_of(operands[1]);
            let imm = immediate(operands[2]);
            for index in 0..4 {
                let selected = (imm >> (2 * index)) & 3;
                out[8 * index..8 * index + 8]
                    .copy_from_slice(&data[8 * selected as usize..8 * selected as usize + 8]);
            }
        }
        "vperm2i128" => {
            let first = source_of(operands[1]);
            let second = source_of(operands[2]);
            let imm = immediate(operands[3]);
            out[..16].copy_from_slice(&select4(&first, &second, imm));
            out[16..].copy_from_slice(&select4(&first, &second, imm >> 4));
        }
        "vinserti128" => {
            let first = source_of(operands[1]);
            let second = source_of(operands[2]);
            let imm = immediate(operands[3]);
            out = first;
            let at = 16 * usize::from(imm & 1);
            out[at..at + 16].copy_from_slice(&second[..16]);
        }
        "vextracti128" => {
            let source = source_of(operands[1]);
            let imm = immediate(operands[2]);
            let at = 16 * usize::from(imm & 1);
            let half = &source[at..at + 16];
            match operands[0] {
                Operand::Register(_, _) => out[..16].copy_from_slice(half),
                Operand::Memory(_) => {
                    let mut stored = *memory;
                    stored[..16].copy_from_slice(half);
                    return (out, Some(stored));
                }
                Operand::Immediate(_) => unreachable!(),
            }
        }
        // Every element of the destination is element 0 of the source.
        "vpbroadcastb" | "vpbroadcastw" | "vpbroadcastd" | "vpbroadcastq" | "vbroadcastss"
        | "vbroadcastsd" => {
            let size = match name {
                "vpbroadcastb" => 1,
                "vpbroadcastw" => 2,
                "vpbroadcastd" | "vbroadcastss" => 4,
                _ => 8,
            };
            let source = source_of(operands[1]);
            let value = lane(&source, size, 0);
            for index in 0..length / size {
                put(&mut out, size, index, value);
            }
        }
        "vbroadcasti128" => {
            let source = source_of(operands[1]);
            out[..16].copy_from_slice(&source[..16]);
            out[16..].copy_from_slice(&source[..16]);
        }
        "vpblendd" => {
            let first = source_of(operands[1]);
            let second = source_of(operands[2]);
            let imm = immediate(operands[3]);
            for index in 0..length / 4 {
                let chosen = if imm >> index & 1 == 1 {
                    &second
                } else {
                    &first
                };
                out[4 * index..4 * index + 4].copy_from_slice(&chosen[4 * index..4 * index + 4]);
            }
        }
        "vpsllvd" | "vpsrlvd" | "vpsravd" | "vpsllvq" | "vpsrlvq" => {
            let size = if name.ends_with('q') { 8 } else { 4 };
            let bits = 8 * size as u32;
            // SRC1, in VEX.vvvv, is the value; SRC2, the ModRM operand, the count.
            let values = source_of(operands[1]);
            let counts = source_of(operands[2]);
            for index in 0..length / size {
                let value = lane(&values, size, index);
                let count = lane(&counts, size, index);
                let shifted = if count >= u64::from(bits) {
                    let negative = value >> (bits - 1) & 1 == 1;
                    if name == "vpsravd" && negative {
                        if size == 8 {
                            u64::MAX
                        } else {
                            u64::from(u32::MAX)
                        }
                    } else {
                        0
                    }
                } else {
                    let count = count as u32;
                    match name {
                        "vpsllvd" | "vpsllvq" => value << count,
                        "vpsrlvd" | "vpsrlvq" => value >> count,
                        _ => {
                            let signed = (value << (64 - bits)) as i64 >> (64 - bits);
                            (signed >> count) as u64
                        }
                    }
                };
                put(&mut out, size, index, shifted);
            }
        }
        // DEST or memory lane i is written only when the sign bit of mask
        // lane i is set: the load zeroes masked lanes, the store leaves
        // their memory bytes alone. The mask is the VEX.vvvv operand.
        "vpmaskmovd" | "vpmaskmovq" => {
            let size = if name.ends_with('q') { 8 } else { 4 };
            let sign = 8 * size - 1;
            let mask = source_of(operands[1]);
            match operands[0] {
                Operand::Memory(_) => {
                    let values = source_of(operands[2]);
                    let mut stored = *memory;
                    for index in 0..length / size {
                        if lane(&mask, size, index) >> sign & 1 == 1 {
                            put(&mut stored, size, index, lane(&values, size, index));
                        }
                    }
                    return (out, Some(stored));
                }
                _ => {
                    let values = source_of(operands[2]);
                    for index in 0..length / size {
                        if lane(&mask, size, index) >> sign & 1 == 1 {
                            put(&mut out, size, index, lane(&values, size, index));
                        }
                    }
                }
            }
        }
        _ => panic!("{name}"),
    }
    (out, None)
}

struct Machine {
    registers: [Wide; 16],
    memory: [u8; 32],
    mxcsr: u32,
    flags: RFlags,
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn wide(&mut self) -> Wide {
        let mut out = [0; 32];
        for chunk in out.chunks_mut(8) {
            chunk.copy_from_slice(&self.next().to_le_bytes());
        }
        out
    }

    fn machine(&mut self) -> Machine {
        Machine {
            registers: std::array::from_fn(|_| self.wide()),
            memory: self.wide(),
            mxcsr: 0x1F80,
            flags: RFlags {
                cf: true,
                zf: false,
                sf: true,
                of: true,
                pf: false,
                af: true,
            },
        }
    }
}

fn load(machine: &Machine, code: &[u8]) -> (CpuState, GuestMemory) {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    let mut data = vec![0xEE; 0x100];
    data[32..64].copy_from_slice(&machine.memory);
    memory
        .map_region_with_data(DATA, 0x100, MemoryPermissions::READ_WRITE, &data)
        .unwrap();
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr[6] = DATA;
    cpu.gpr[7] = 4;
    for (index, register) in machine.registers.iter().enumerate() {
        cpu.xmm[index] = u128::from_le_bytes(register[..16].try_into().unwrap());
        cpu.ymm_high[index] = u128::from_le_bytes(register[16..].try_into().unwrap());
    }
    cpu.mxcsr = Mxcsr(machine.mxcsr);
    cpu.flags = machine.flags;
    (cpu, memory)
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

fn data(memory: &GuestMemory) -> [u8; 32] {
    let mut out = [0; 32];
    memory.read_bytes(OPERAND, &mut out).unwrap();
    out
}

#[test]
fn every_avx2_form_matches_the_reference_in_both_engines() {
    let code = fixture();
    let mut random = Random(0x5D2C_9A47_1E63_B8F0);
    let mut checked = 0;
    for (text, bytes) in &code {
        let (name, operands) = split(text);
        let length = length(name, &operands);
        for _ in 0..8 {
            let machine = random.machine();
            let (cpu, mut memory) = load(&machine, bytes);
            let (wanted, stored) =
                reference(name, &operands, &machine.registers, &machine.memory, length);
            let mut after = cpu.clone();
            after.rip = ENTRY + bytes.len() as u64;
            if let Operand::Register(index, _) = operands[0] {
                let index = usize::from(index);
                after.xmm[index] = u128::from_le_bytes(wanted[..16].try_into().unwrap());
                after.ymm_high[index] = if length == 32 {
                    u128::from_le_bytes(wanted[16..].try_into().unwrap())
                } else {
                    0
                };
            }
            for native in [false, true] {
                let engine = if native { "DBT" } else { "interpreter" };
                let result = run(native, &cpu, &mut memory);
                assert_eq!(result.exit_reason, ExitReason::StepLimitExceeded, "{text}");
                assert_eq!(result.final_state.xmm, after.xmm, "{text} on the {engine}");
                assert_eq!(
                    result.final_state.ymm_high, after.ymm_high,
                    "{text} on the {engine}"
                );
                assert_eq!(
                    result.final_state.mxcsr, cpu.mxcsr,
                    "{text} on the {engine}"
                );
                assert_eq!(
                    result.final_state.flags, cpu.flags,
                    "{text} on the {engine}"
                );
                match stored {
                    Some(stored) => assert_eq!(data(&memory), stored, "{text} on the {engine}"),
                    None => assert_eq!(data(&memory), machine.memory, "{text} on the {engine}"),
                }
            }
            checked += 1;
        }
    }
    assert!(checked > 900, "{checked}");
}

/// The value and the count of a variable shift are not interchangeable, so an
/// implementation that read them the other way round would pass none of these.
#[test]
fn a_variable_shift_takes_the_value_from_vvvv_and_the_count_from_modrm() {
    let code = fixture();
    let text = "vpsllvd ymm3, ymm9, ymm10";
    let bytes = &code[text];
    let mut machine = Machine {
        registers: [[0; 32]; 16],
        memory: [0; 32],
        mxcsr: 0x1F80,
        flags: RFlags::default(),
    };
    // The value in VEX.vvvv and the counts in the ModRM operand.
    for index in 0..8 {
        put(&mut machine.registers[9], 4, index, 1 << index);
        put(&mut machine.registers[10], 4, index, index as u64);
    }
    let (cpu, mut memory) = load(&machine, bytes);
    for native in [false, true] {
        let result = run(native, &cpu, &mut memory);
        let engine = if native { "DBT" } else { "interpreter" };
        let mut wanted = [0; 32];
        for index in 0..8 {
            put(&mut wanted, 4, index, (1u64 << index) << index);
        }
        assert_eq!(
            result.final_state.xmm[3],
            u128::from_le_bytes(wanted[..16].try_into().unwrap()),
            "the low half on the {engine}"
        );
        assert_eq!(
            result.final_state.ymm_high[3],
            u128::from_le_bytes(wanted[16..].try_into().unwrap()),
            "the high half on the {engine}"
        );
        // Swapping the roles gives a different vector, so the check above can
        // only pass with the operands the manual names.
        let mut swapped = [0; 32];
        for index in 0..8 {
            let count = 1u64 << index;
            put(
                &mut swapped,
                4,
                index,
                if count > 31 {
                    0
                } else {
                    (index as u64) << count
                },
            );
        }
        assert_ne!(wanted, swapped, "the operands are interchangeable");
    }
}
