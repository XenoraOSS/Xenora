// SPDX-License-Identifier: GPL-3.0-or-later

//! AVX2 gathers in both engines, against an independent byte-array reference.
//!
//! `tests/x86/avx2.txt` pairs LLVM encodings with Intel-syntax text. The
//! reference here reads only the text: it computes each lane's address from
//! the base register, the sign-extended index lane and the scale, reads memory
//! as bytes, and applies the merging and mask-zeroing rules of Intel SDM
//! Vol. 2. It shares nothing with the registry, the decoder or the production
//! semantics. Indices are small and mixed in sign so sign extension decides,
//! and every lane position is masked off in some case.

use std::collections::BTreeMap;
use xenora_loader::cpu::{CpuState, ExecutionResult, ExitReason, Interpreter, RFlags};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};

const ENTRY: u64 = 0x1000;
const DATA: u64 = 0x2000;
/// Index vectors address this far into the data region, so the negative ones
/// still land in memory.
const CENTER: u64 = 0x800;

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

/// A gather form parsed from its fixture text.
struct Form {
    name: &'static str,
    dest: usize,
    mask: usize,
    indices: usize,
    wide_indices: bool,
    base: bool,
    scale: u64,
    disp: i64,
}

fn parse(text: &'static str) -> Form {
    let (name, rest) = text.split_once(' ').unwrap();
    let parts: Vec<&str> = rest.split(", ").collect();
    let register = |text: &str| {
        text.trim_start_matches("ymm")
            .trim_start_matches("xmm")
            .parse()
            .unwrap()
    };
    let memory = parts[1];
    let brackets = memory.split_once('[').unwrap().1.split_once(']').unwrap().0;
    let wide_indices = brackets.contains("ymm");
    let base = brackets.contains("rsi");
    let scale = memory
        .split('*')
        .nth(1)
        .and_then(|tail| tail.split([' ', ']']).next().unwrap().parse().ok())
        .unwrap_or(1);
    let disp = brackets
        .rsplit('+')
        .next()
        .and_then(|tail| tail.trim().parse::<i64>().ok())
        .unwrap_or(0);
    Form {
        name,
        dest: register(parts[0]),
        mask: register(parts[2]),
        indices: register(
            brackets
                .split('+')
                .find(|piece| piece.contains("xmm") || piece.contains("ymm"))
                .unwrap()
                .split('*')
                .next()
                .unwrap()
                .trim(),
        ),
        wide_indices,
        base,
        scale,
        disp,
    }
}

/// The data lane width of a form, and whether its indices are doublewords.
fn widths(name: &str) -> (usize, usize) {
    let data = if name.ends_with('q') || name.ends_with("pd") || name == "vpgatherqq" {
        8
    } else {
        4
    };
    // Doubleword-index families are the 90/92 opcodes: those whose data width
    // matches their name's first index letter, for both integer and float.
    let index = if matches!(
        name,
        "vpgatherdd" | "vpgatherdq" | "vgatherdps" | "vgatherdpd"
    ) {
        4
    } else {
        8
    };
    (data, index)
}

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

#[test]
fn gathers_match_the_independent_reference() {
    let fixture = fixture();
    let mut random = Random(0x006A_7448_4552_4345);
    let mut checked = 0;
    for (text, code) in fixture.iter().filter(|(text, _)| text.contains("gather")) {
        let form = parse(text);
        // A form aliasing the mask onto the destination is encodable but
        // degenerate: the mask zeroing and the lane writes target the same
        // register. The sweep exercises its decode; the semantic validation
        // here needs independent registers.
        if form.mask == form.dest {
            continue;
        }
        let (data, index) = widths(form.name);
        // The lane count follows the encoded length and the wider of the two
        // widths; the index vector and the destination widths follow the lanes.
        let length_bits = if form.wide_indices { 256 } else { 128 };
        // The dq/dpd forms keep xmm indices at either length: their lane count
        // comes from the destination width, which the wide bit of the encoding
        // decides — read it back off the VEX byte instead of guessing.
        let lanes = if matches!(form.name, "vpgatherdd" | "vgatherdps") {
            length_bits / 32
        } else if matches!(
            form.name,
            "vpgatherqq" | "vgatherqpd" | "vpgatherqd" | "vgatherqps"
        ) {
            // Quadword-index forms: the index width decides the lanes.
            length_bits / 64
        } else {
            // dq/dpd: indices are always xmm; the lane count is the number of
            // doubleword index lanes in use, which the qword destination count
            // matches. The VEX.L bit of the encoding decides.
            let l = code[2] & 4 != 0;
            if l { 4 } else { 2 }
        };
        for round in 0..40 {
            let _ = round;
            // The data region is a byte pattern every read can be checked
            // against; indices are small and mixed in sign.
            let mut memory_bytes = [0u8; 0x1000];
            for (offset, byte) in memory_bytes.iter_mut().enumerate() {
                *byte = (offset as u8).wrapping_mul(37).wrapping_add(11);
            }
            let mut index_bytes = [0u8; 32];
            for lane_index in 0..lanes {
                // Small magnitudes, sign mixed by lane and round.
                let magnitude = (random.next() % 12) as i64;
                let slot = if (random.next() + lane_index as u64).is_multiple_of(3) {
                    -magnitude
                } else {
                    magnitude
                };
                put(&mut index_bytes, index, lane_index, slot as u64);
            }
            let mut mask_bytes = [0u8; 32];
            for lane_index in 0..lanes {
                if !random.next().is_multiple_of(3) {
                    put(&mut mask_bytes, data, lane_index, 1u64 << (8 * data - 1));
                }
            }
            let dest_before: u64 = random.next();
            let mut dest_bytes = [0u8; 32];
            for lane_index in 0..lanes.max(1) {
                put(
                    &mut dest_bytes,
                    data,
                    lane_index,
                    dest_before.rotate_left(lane_index as u32),
                );
            }
            // Aliasing decides the effective register contents: a register the
            // text names twice holds the last array written for it, and the
            // reference must read its indices, mask lanes and merging values
            // from those contents rather than the generated arrays.
            let effective = |register: usize, generated: &[u8; 32]| -> [u8; 32] {
                if register == form.dest {
                    dest_bytes
                } else if register == form.mask {
                    mask_bytes
                } else {
                    *generated
                }
            };
            let index_content = effective(form.indices, &index_bytes);
            let mask_content = effective(form.mask, &mask_bytes);
            let indices: Vec<i64> = (0..lanes)
                .map(|lane_index| {
                    let raw = lane(&index_content, index, lane_index);
                    if index == 4 {
                        i64::from(raw as u32 as i32)
                    } else {
                        raw as i64
                    }
                })
                .collect();
            let enabled: Vec<bool> = (0..lanes)
                .map(|lane_index| lane(&mask_content, data, lane_index) >> (8 * data - 1) & 1 == 1)
                .collect();
            // The reference: each enabled lane reads memory at its own address.
            let base = if form.base { DATA + CENTER } else { 0 };
            let mut wanted = dest_bytes;
            let mut valid = true;
            for lane_index in 0..lanes {
                if !enabled[lane_index] {
                    continue;
                }
                let address = base as i64 + indices[lane_index] * form.scale as i64 + form.disp;
                if !(DATA..DATA + 0x1000 - data as u64).contains(&(address as u64)) {
                    valid = false;
                    break;
                }
                let offset = (address as u64 - DATA) as usize;
                let mut bytes = [0u8; 8];
                bytes[..data].copy_from_slice(&memory_bytes[offset..offset + data]);
                put(&mut wanted, data, lane_index, u64::from_le_bytes(bytes));
            }
            if !valid {
                continue;
            }
            // Machine setup: registers from the byte arrays, memory the pattern.
            let mut memory = GuestMemory::new();
            memory
                .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
                .unwrap();
            memory
                .map_region(DATA, 0x1000, MemoryPermissions::READ_WRITE)
                .unwrap();
            memory.write_bytes(DATA, &memory_bytes).unwrap();
            let mut cpu = CpuState::new(ENTRY, 0);
            cpu.gpr[6] = DATA + CENTER;
            cpu.gpr[7] = 4;
            cpu.flags = RFlags::default();
            let place = |cpu: &mut CpuState, register: usize, bytes: &[u8; 32]| {
                cpu.xmm[register] = u128::from_le_bytes(bytes[..16].try_into().unwrap());
                cpu.ymm_high[register] = u128::from_le_bytes(bytes[16..].try_into().unwrap());
            };
            place(&mut cpu, form.indices, &index_bytes);
            place(&mut cpu, form.mask, &mask_bytes);
            place(&mut cpu, form.dest, &dest_bytes);
            for native in [false, true] {
                let (cpu, mut memory) = (cpu.clone(), memory.clone());
                let result = run(native, &cpu, &mut memory);
                assert_eq!(result.exit_reason, ExitReason::StepLimitExceeded, "{text}");
                let after = result.final_state;
                let low = after.xmm[form.dest].to_le_bytes();
                let high = after.ymm_high[form.dest].to_le_bytes();
                let mut got = [0u8; 32];
                got[..16].copy_from_slice(&low);
                got[16..].copy_from_slice(&high);
                let width = lanes * data;
                assert_eq!(
                    &got[..width],
                    &wanted[..width],
                    "{text} lanes {indices:?} mask {enabled:?}"
                );
                if width < 32 {
                    assert!(got[width..].iter().all(|byte| *byte == 0), "{text} upper");
                }
                assert_eq!(after.xmm[form.mask], 0, "{text} mask low");
                assert_eq!(after.ymm_high[form.mask], 0, "{text} mask high");
            }
            checked += 1;
        }
    }
    assert!(checked > 500, "{checked}");
}
