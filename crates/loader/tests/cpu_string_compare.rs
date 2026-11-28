// SPDX-License-Identifier: GPL-3.0-or-later

//! LODS, SCAS and CMPS with REP, REPE and REPNE (issue #988).
//!
//! The expected results come from a byte-array model written from the
//! operation sections of Intel SDM Vol. 2A and 2B: LODS, SCAS, CMPS and
//! REP/REPE/REPZ/REPNE/REPNZ. The model never decodes an instruction or calls
//! the production semantics, and it derives the flags from 128-bit
//! arithmetic. The encodings were assembled with LLVM `llvm-mc` for x86-64.
//! Nothing was compared with x86 hardware.
//!
//! As for MOVS and STOS (`tests/cpu_rep.rs`), a repeated form runs one element
//! per step and stays on the instruction while it repeats. REPE and REPNE
//! also stop on the ZF that the element just produced.

use std::collections::BTreeSet;
use xenora_loader::cpu::{CpuState, ExecutionResult, ExitReason, Interpreter, RFlags};
use xenora_loader::dbt::{DbtEngine, is_native};
use xenora_loader::memory::{GuestMemory, MemoryPermissions};
use xenora_loader::x86_decode::decode;
use xenora_loader::x86_table::{FORMS, Op};

const ENTRY: u64 = 0x1000;
/// One writable page with nothing mapped on either side.
const DATA: u64 = 0x8000;
const PAGE: usize = 0x1000;
const END: u64 = DATA + PAGE as u64;

const RAX: usize = 0;
const RCX: usize = 1;
const RDX: usize = 2;
const RSI: usize = 6;
const RDI: usize = 7;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Lods,
    Scas,
    Cmps,
}

/// The repeat prefix: none, F3 (REP, or REPE on a compare) or F2 (REPNE).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Repeat {
    Once,
    F3,
    F2,
}

/// The instruction for an element of `size` bytes, in the assembler's order.
fn encode(kind: Kind, size: u64, repeat: Repeat) -> Vec<u8> {
    let mut code = Vec::new();
    match repeat {
        Repeat::Once => {}
        Repeat::F3 => code.push(0xF3),
        Repeat::F2 => code.push(0xF2),
    }
    match size {
        2 => code.push(0x66),
        8 => code.push(0x48),
        _ => {}
    }
    let byte = size == 1;
    code.push(match kind {
        Kind::Lods if byte => 0xAC,
        Kind::Lods => 0xAD,
        Kind::Scas if byte => 0xAE,
        Kind::Scas => 0xAF,
        Kind::Cmps if byte => 0xA6,
        Kind::Cmps => 0xA7,
    });
    code
}

fn image(code: &[u8], data: &[u8]) -> GuestMemory {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    memory
        .map_region_with_data(DATA, PAGE, MemoryPermissions::READ_WRITE, data)
        .unwrap();
    memory
}

fn bytes(memory: &GuestMemory) -> Vec<u8> {
    let mut out = vec![0; PAGE];
    memory.read_bytes(DATA, &mut out).unwrap();
    out
}

struct Run {
    name: &'static str,
    result: ExecutionResult,
    memory: GuestMemory,
    /// Instructions the DBT ran on the shared semantics.
    helpers: usize,
}

/// The interpreter and the DBT, each from its own copy of the state.
fn both(cpu: &CpuState, memory: &GuestMemory, budget: usize) -> [Run; 2] {
    let mut interpreted = memory.clone();
    let result = Interpreter::new(cpu.clone(), &mut interpreted, budget).run();
    let mut translated = memory.clone();
    let mut engine = DbtEngine::new(64 * 1024).unwrap();
    let mut state = cpu.clone();
    let native = engine.run(&mut state, &mut translated, budget);
    [
        Run {
            name: "interpreter",
            result,
            memory: interpreted,
            helpers: 0,
        },
        Run {
            name: "DBT",
            result: native,
            memory: translated,
            helpers: engine.metrics.interpreter_fallbacks,
        },
    ]
}

/// Deterministic generator (xorshift64*).
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// The six status flags of `a - b` on `bytes`-byte operands, as CMP sets them
/// (Intel SDM Vol. 1, appendix A, and Vol. 2A, CMP), from 128-bit arithmetic.
fn compare(a: u64, b: u64, bytes: u64) -> RFlags {
    let bits = 8 * bytes as u32;
    let modulus = 1i128 << bits;
    let (a, b) = (i128::from(a) % modulus, i128::from(b) % modulus);
    let signed = |value: i128| {
        if value >= modulus / 2 {
            value - modulus
        } else {
            value
        }
    };
    let result = (a - b).rem_euclid(modulus);
    let exact = signed(a) - signed(b);
    RFlags {
        cf: a < b,
        zf: result == 0,
        sf: result >= modulus / 2,
        of: exact < -modulus / 2 || exact >= modulus / 2,
        pf: (result & 0xFF).count_ones() % 2 == 0,
        af: (a & 0xF) < (b & 0xF),
    }
}

/// An element of `size` bytes at `address`, read little-endian from `data`.
fn element(data: &[u8], address: u64, size: u64) -> u64 {
    let at = (address - DATA) as usize;
    let mut value = [0; 8];
    value[..size as usize].copy_from_slice(&data[at..at + size as usize]);
    u64::from_le_bytes(value)
}

/// The architectural effect of the instruction on `cpu`, and how many steps
/// it takes. Each element is read whole, then the accumulator or the flags
/// change, then the pointers move by its size. With a repeat prefix a zero
/// RCX ends it before any element, and so does a decrement to zero or, on a
/// compare, the ZF of the element against the prefix.
fn model(kind: Kind, size: u64, repeat: Repeat, data: &[u8], cpu: &mut CpuState) -> usize {
    let step = if cpu.df { size.wrapping_neg() } else { size };
    let mut elements = 0;
    loop {
        if repeat != Repeat::Once && cpu.gpr[RCX] == 0 {
            break;
        }
        let accumulator = cpu.gpr[RAX] & (u64::MAX >> (64 - 8 * size));
        match kind {
            Kind::Lods => {
                let value = element(data, cpu.gpr[RSI], size);
                cpu.gpr[RAX] = match size {
                    1 => cpu.gpr[RAX] & !0xFF | value,
                    2 => cpu.gpr[RAX] & !0xFFFF | value,
                    // A 32-bit destination clears bits 63:32 (Vol. 1, 3.4.1.1).
                    _ => value,
                };
            }
            Kind::Scas => cpu.flags = compare(accumulator, element(data, cpu.gpr[RDI], size), size),
            Kind::Cmps => {
                cpu.flags = compare(
                    element(data, cpu.gpr[RSI], size),
                    element(data, cpu.gpr[RDI], size),
                    size,
                );
            }
        }
        if kind != Kind::Scas {
            cpu.gpr[RSI] = cpu.gpr[RSI].wrapping_add(step);
        }
        if kind != Kind::Lods {
            cpu.gpr[RDI] = cpu.gpr[RDI].wrapping_add(step);
        }
        elements += 1;
        if repeat == Repeat::Once {
            break;
        }
        cpu.gpr[RCX] -= 1;
        let stop = match (kind, repeat) {
            (Kind::Lods, _) => false,
            (_, Repeat::F3) => !cpu.flags.zf,
            _ => cpu.flags.zf,
        };
        if cpu.gpr[RCX] == 0 || stop {
            break;
        }
    }
    elements.max(1)
}

fn kinds() -> impl Iterator<Item = (Kind, Repeat)> {
    [Kind::Lods, Kind::Scas, Kind::Cmps]
        .into_iter()
        .flat_map(|kind| [Repeat::Once, Repeat::F3, Repeat::F2].map(|repeat| (kind, repeat)))
        // REPNE is not defined for LODS.
        .filter(|&(kind, repeat)| !(kind == Kind::Lods && repeat == Repeat::F2))
}

#[test]
fn every_form_matches_a_byte_array_model_and_counts_one_step_per_element() {
    let mut random = Random(0x0DDB_A11C_AFE5_F00D);
    for (kind, repeat) in kinds() {
        for size in [1u64, 2, 4, 8] {
            for backward in [false, true] {
                for round in 0..48u64 {
                    // Counts 0 to 11. The source runs from DATA + 0x400 and
                    // the destination from DATA + 0xA00, in either direction.
                    let count = round % 12;
                    let rsi = DATA + 0x400 + random.next() % 8;
                    let rdi = DATA + 0xA00 + random.next() % 8;
                    let mut data: Vec<u8> = (0..PAGE).map(|_| random.next() as u8).collect();
                    let mut cpu = CpuState::new(ENTRY, 0);
                    for register in &mut cpu.gpr {
                        *register = random.next();
                    }
                    cpu.gpr[RCX] = count;
                    cpu.gpr[RSI] = rsi;
                    cpu.gpr[RDI] = rdi;
                    cpu.df = backward;
                    let flag = |random: &mut Random| random.next() & 1 != 0;
                    cpu.flags = RFlags {
                        cf: flag(&mut random),
                        zf: flag(&mut random),
                        sf: flag(&mut random),
                        of: flag(&mut random),
                        pf: flag(&mut random),
                        af: flag(&mut random),
                    };

                    // Random bytes would almost never compare equal. Up to a
                    // chosen element the compared values are equal for REPE
                    // and different for REPNE; that element is the opposite,
                    // so a repeat stops there or runs out of count first.
                    let turn = random.next() % (count + 2);
                    let step = if backward { size.wrapping_neg() } else { size };
                    let accumulator = cpu.gpr[RAX];
                    for index in 0..=count {
                        let offset = step.wrapping_mul(index);
                        let (source, target) = (rsi.wrapping_add(offset), rdi.wrapping_add(offset));
                        let equal = (index < turn) == (repeat != Repeat::F2);
                        let wanted = match kind {
                            Kind::Scas => accumulator,
                            _ => element(&data, source, size),
                        };
                        let value = if equal {
                            wanted
                        } else {
                            wanted ^ 1 << (random.next() % (8 * size))
                        };
                        let at = (target - DATA) as usize;
                        if kind != Kind::Lods {
                            data[at..at + size as usize]
                                .copy_from_slice(&value.to_le_bytes()[..size as usize]);
                        }
                    }

                    let mut code = encode(kind, size, repeat);
                    code.push(0xF4); // hlt
                    let memory = image(&code, &data);
                    let mut wanted = cpu.clone();
                    let steps = model(kind, size, repeat, &data, &mut wanted) + 1;
                    wanted.rip = ENTRY + code.len() as u64;

                    for run in both(&cpu, &memory, 64) {
                        let what = format!(
                            "{kind:?} size {size} {repeat:?} backward={backward} count={count} \
                             turn={turn} on the {}",
                            run.name
                        );
                        assert_eq!(run.result.exit_reason, ExitReason::Halt, "{what}");
                        assert_eq!(run.result.final_state, wanted, "{what}");
                        assert!(bytes(&run.memory) == data, "{what}: memory");
                        assert_eq!(run.result.instructions_executed, steps, "{what}");
                    }
                }
            }
        }
    }
}

#[test]
fn a_step_budget_stops_a_repeat_part_way_and_it_resumes() {
    // repne scasb; hlt, over 40 bytes of 0x11 then the 0x22 it looks for.
    let code = [0xF2, 0xAE, 0xF4];
    let mut data = vec![0x11; PAGE];
    data[0x140] = 0x22;
    let memory = image(&code, &data);
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr[RAX] = 0x22;
    cpu.gpr[RCX] = 100;
    cpu.gpr[RDI] = DATA + 0x118;
    for run in both(&cpu, &memory, 10) {
        let state = &run.result.final_state;
        assert_eq!(
            run.result.exit_reason,
            ExitReason::StepLimitExceeded,
            "{}",
            run.name
        );
        // Ten elements, none of them a match: RIP stays on the instruction.
        assert_eq!(
            (state.rip, state.gpr[RCX], state.gpr[RDI], state.flags.zf),
            (ENTRY, 90, DATA + 0x122, false),
            "{}",
            run.name
        );
        if run.name == "DBT" {
            assert_eq!(run.helpers, 10);
        }
        // Resuming finds the byte at the 41st element and halts.
        let [rest, _] = both(state, &run.memory, 1000);
        let state = &rest.result.final_state;
        assert_eq!(rest.result.exit_reason, ExitReason::Halt, "{}", run.name);
        assert_eq!(rest.result.instructions_executed, 32, "{}", run.name);
        assert_eq!(
            (state.gpr[RCX], state.gpr[RDI], state.flags.zf),
            (59, DATA + 0x141, true),
            "{}",
            run.name
        );
    }
}

#[test]
fn a_fault_keeps_the_elements_done_and_restores_the_flags() {
    // (code, RSI, RDI, backward, elements that complete before the fault)
    let cases: &[(&[u8], u64, u64, bool, u64)] = &[
        // repe cmpsw towards the end of the page: [RDI] has three whole words.
        (&[0xF3, 0x66, 0xA7], DATA + 0x100, END - 7, false, 3),
        // repne scasd down to the start of the page: two doublewords fit.
        (&[0xF2, 0xAF], 0, DATA + 5, true, 2),
        // rep lodsq from a source that ends after four quadwords.
        (&[0xF3, 0x48, 0xAD], END - 32, 0, false, 4),
        // repe cmpsb with no readable source at all.
        (&[0xF3, 0xA6], END, DATA + 0x100, false, 0),
        // cmpsd whose destination is unreadable: no element completes.
        (&[0xA7], DATA + 0x100, END - 2, false, 0),
    ];
    for &(code, rsi, rdi, backward, done) in cases {
        // Every compared pair is equal, so REPE runs on and, with REPNE's
        // accumulator never matching, so does REPNE. Each completed compare
        // therefore leaves flags unlike the ones below.
        let data = vec![0x5A; PAGE];
        let memory = image(code, &data);
        let insn = decode(ENTRY, |at| {
            code.get((at - ENTRY) as usize).copied().ok_or(())
        })
        .unwrap();
        let kind = match insn.op {
            Op::Lods => Kind::Lods,
            Op::Scas => Kind::Scas,
            _ => Kind::Cmps,
        };
        let repeat = match (insn.rep, insn.repne) {
            (false, _) => Repeat::Once,
            (true, false) => Repeat::F3,
            (true, true) => Repeat::F2,
        };
        let mut cpu = CpuState::new(ENTRY, 0);
        cpu.gpr[RAX] = 0x1122_3344_5566_7788;
        cpu.gpr[RCX] = 9;
        cpu.gpr[RSI] = rsi;
        cpu.gpr[RDI] = rdi;
        cpu.df = backward;
        cpu.flags = RFlags {
            cf: true,
            zf: false,
            sf: true,
            of: true,
            pf: false,
            af: true,
        };

        // The completed elements took effect and the faulting one changed
        // nothing: RIP stays on the instruction, and RSI, RDI and RCX are
        // those the last completed element left. The flags are those the
        // instruction began with (Intel SDM Vol. 2B, REP: a fault in a CMPS or
        // SCAS under REPE or REPNE restores them).
        let mut wanted = cpu.clone();
        if done > 0 {
            wanted.gpr[RCX] = done;
            model(kind, u64::from(insn.size), repeat, &data, &mut wanted);
            wanted.gpr[RCX] = if repeat == Repeat::Once { 9 } else { 9 - done };
            wanted.flags = cpu.flags;
        }

        for run in both(&cpu, &memory, 64) {
            let what = format!("{code:02X?} on the {}", run.name);
            assert!(
                matches!(run.result.exit_reason, ExitReason::MemoryFault(_)),
                "{what}: {:?}",
                run.result.exit_reason
            );
            assert_eq!(run.result.final_state, wanted, "{what}");
            assert!(bytes(&run.memory) == data, "{what}: memory");
            assert_eq!(run.result.instructions_executed, done as usize, "{what}");
        }
    }
}

#[test]
fn a_repeat_split_by_the_step_budget_still_restores_its_first_flags() {
    // repe cmpsw towards the end of the page, as above: the fourth element
    // faults. Stopping after one or two elements and resuming must give the
    // same result as one uninterrupted run.
    let code = [0xF3, 0x66, 0xA7];
    let data = vec![0x5A; PAGE];
    let memory = image(&code, &data);
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr[RCX] = 9;
    cpu.gpr[RSI] = DATA + 0x100;
    cpu.gpr[RDI] = END - 7;
    cpu.flags = RFlags {
        cf: true,
        zf: false,
        sf: false,
        of: true,
        pf: false,
        af: true,
    };
    let [whole, _] = both(&cpu, &memory, 64);
    assert!(matches!(
        whole.result.exit_reason,
        ExitReason::MemoryFault(_)
    ));
    assert_eq!(whole.result.final_state.flags, cpu.flags);
    for first in [1, 2] {
        for run in both(&cpu, &memory, first) {
            let what = format!("{first} then the rest on the {}", run.name);
            let part = &run.result.final_state;
            assert_eq!(
                run.result.exit_reason,
                ExitReason::StepLimitExceeded,
                "{what}"
            );
            // Equal words: the completed elements set ZF.
            assert!(part.flags.zf, "{what}");
            let [rest, _] = both(part, &run.memory, 64);
            assert!(
                matches!(rest.result.exit_reason, ExitReason::MemoryFault(_)),
                "{what}"
            );
            assert_eq!(rest.result.final_state, whole.result.final_state, "{what}");
        }
    }
}

#[test]
fn a_segment_override_moves_the_source_only() {
    const BASE: u64 = 0x300;
    let mut data = vec![0u8; PAGE];
    data[0x10] = 0x11; // DATA + 0x10, what [RSI] names without a base
    data[0x310] = 0x22; // the same RSI under the FS or GS base
    data[0x800] = 0x22; // [RDI], which never takes a base
    data[0xB00] = 0x11; // [RDI] if it did
    // (code, whether the source uses the base)
    for (code, uses_base) in [
        (&[0xAC][..], false),      // lodsb
        (&[0x64, 0xAC][..], true), // lodsb al, fs:[rsi]
        (&[0x65, 0xAC][..], true), // lodsb al, gs:[rsi]
        (&[0xA6][..], false),      // cmpsb
        (&[0x64, 0xA6][..], true), // cmpsb fs:[rsi], es:[rdi]
        (&[0x65, 0xA6][..], true), // cmpsb gs:[rsi], es:[rdi]
    ] {
        let memory = image(code, &data);
        let mut cpu = CpuState::new(ENTRY, 0);
        cpu.fs_base = BASE;
        cpu.gs_base = BASE;
        cpu.gpr[RSI] = DATA + 0x10;
        cpu.gpr[RDI] = DATA + 0x800;
        for run in both(&cpu, &memory, 1) {
            let what = format!("{code:02X?} on the {}", run.name);
            let state = &run.result.final_state;
            assert_eq!(
                run.result.exit_reason,
                ExitReason::StepLimitExceeded,
                "{what}"
            );
            if code.ends_with(&[0xAC]) {
                let wanted = if uses_base { 0x22 } else { 0x11 };
                assert_eq!(state.gpr[RAX], wanted, "{what}");
            } else {
                // 0x22 - 0x22 is zero; 0x11 - 0x22 is not.
                assert_eq!(state.flags.zf, uses_base, "{what}");
            }
            assert_eq!(
                state.gpr[RSI],
                DATA + 0x11,
                "{what}: RSI moves by the element, not by the base"
            );
        }
    }
    // SCAS has no source, and its [RDI] takes no override.
    for code in [&[0x64, 0xAE][..], &[0x65, 0xAE]] {
        let memory = image(code, &data);
        let mut cpu = CpuState::new(ENTRY, 0);
        cpu.fs_base = BASE;
        cpu.gs_base = BASE;
        cpu.gpr[RAX] = 0x22;
        cpu.gpr[RDI] = DATA + 0x800;
        for run in both(&cpu, &memory, 1) {
            assert!(run.result.final_state.flags.zf, "{code:02X?} {}", run.name);
        }
    }
}

#[test]
fn exactly_the_declared_forms_and_prefixes_decode() {
    let decode_bytes = |code: &[u8]| {
        decode(ENTRY, |at| {
            code.get((at - ENTRY) as usize).copied().ok_or(())
        })
    };
    let mut seen = BTreeSet::new();
    for (kind, repeat) in kinds() {
        for size in [1, 2, 4, 8] {
            let code = encode(kind, size, repeat);
            let insn = decode_bytes(&code).unwrap_or_else(|error| panic!("{code:02X?}: {error:?}"));
            assert_eq!(usize::from(insn.len), code.len(), "{code:02X?}");
            assert_eq!(
                (u64::from(insn.size), insn.rep, insn.repne),
                (size, repeat != Repeat::Once, repeat == Repeat::F2),
                "{code:02X?}"
            );
            assert!(!is_native(&insn), "{code:02X?}");
            seen.insert(usize::from(insn.form));
        }
    }
    for (index, form) in FORMS.iter().enumerate() {
        if matches!(form.op, Op::Lods | Op::Scas | Op::Cmps) {
            assert!(seen.contains(&index), "{:?} {:?}", form.op, form.size);
        }
    }

    let rejected: &[&[u8]] = &[
        &[0xF2, 0xAC],             // repne lodsb
        &[0xF2, 0x48, 0xAD],       // repne lodsq
        &[0xF3, 0xF2, 0xA6],       // both repeat prefixes on cmpsb
        &[0xF2, 0xF3, 0xAE],       // both repeat prefixes on scasb
        &[0x67, 0xF2, 0xAE],       // repne scasb with 32-bit addresses
        &[0x67, 0xAC],             // lodsb with 32-bit addresses
        &[0xF0, 0xA6],             // lock cmpsb
        &[0xF0, 0xF3, 0xAF],       // lock repe scasd
        &[0xF2, 0x90],             // F2 is no PAUSE
        &[0xF2, 0x0F, 0xC1, 0xC8], // F2 on a form that takes no prefix
    ];
    for code in rejected {
        let mut padded = code.to_vec();
        padded.extend([0x90; 8]);
        let memory = image(&padded, &[0; PAGE]);
        let mut cpu = CpuState::new(ENTRY, 0);
        cpu.gpr[RCX] = 3;
        cpu.gpr[RSI] = DATA;
        cpu.gpr[RDI] = DATA + 0x100;
        for run in both(&cpu, &memory, 1) {
            assert!(
                matches!(run.result.exit_reason, ExitReason::IllegalInstruction(_)),
                "{code:02X?} on the {}: {:?}",
                run.name,
                run.result.exit_reason
            );
            assert_eq!(run.result.final_state, cpu, "{code:02X?}");
        }
    }
}

#[test]
fn strlen_and_memcmp_idioms_run_in_both_engines() {
    //   xor eax, eax
    //   mov rcx, -1
    //   repne scasb
    //   not rcx
    //   dec rcx
    //   hlt
    let strlen = [
        0x31, 0xC0, 0x48, 0xC7, 0xC1, 0xFF, 0xFF, 0xFF, 0xFF, 0xF2, 0xAE, 0x48, 0xF7, 0xD1, 0x48,
        0xFF, 0xC9, 0xF4,
    ];
    let mut data = vec![0xEE; PAGE];
    data[0x20..0x2D].copy_from_slice(b"hello, world\0");
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr[RDI] = DATA + 0x20;
    for run in both(&cpu, &image(&strlen, &data), 64) {
        let state = &run.result.final_state;
        assert_eq!(run.result.exit_reason, ExitReason::Halt, "{}", run.name);
        // Twelve characters, and RDI one past the terminator.
        assert_eq!(state.gpr[RCX], 12, "{}", run.name);
        assert_eq!(state.gpr[RDI], DATA + 0x2D, "{}", run.name);
        // XOR and MOV, thirteen elements, NOT and DEC, then HLT.
        assert_eq!(run.result.instructions_executed, 18, "{}", run.name);
        if run.name == "DBT" {
            assert_eq!(run.helpers, 13);
        }
    }

    //   repe cmpsb
    //   seta al
    //   setb dl
    //   hlt
    let memcmp = [0xF3, 0xA6, 0x0F, 0x97, 0xC0, 0x0F, 0x92, 0xC2, 0xF4];
    let mut data = vec![0; PAGE];
    data[0x40..0x46].copy_from_slice(b"abcdef");
    data[0x80..0x86].copy_from_slice(b"abcxef");
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr[RAX] = 0xFF;
    cpu.gpr[RDX] = 0xFF;
    cpu.gpr[RCX] = 6;
    cpu.gpr[RSI] = DATA + 0x40;
    cpu.gpr[RDI] = DATA + 0x80;
    for run in both(&cpu, &image(&memcmp, &data), 64) {
        let state = &run.result.final_state;
        assert_eq!(run.result.exit_reason, ExitReason::Halt, "{}", run.name);
        // 'd' (0x64) is below 'x' (0x78): the fourth element stops it with
        // CF set, so SETA stores 0 and SETB 1.
        assert_eq!(
            (state.gpr[RAX], state.gpr[RDX], state.gpr[RCX]),
            (0, 1, 2),
            "{}",
            run.name
        );
        assert_eq!(
            (state.gpr[RSI], state.gpr[RDI]),
            (DATA + 0x44, DATA + 0x84),
            "{}",
            run.name
        );
    }
}
