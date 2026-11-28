// SPDX-License-Identifier: GPL-3.0-or-later

//! MOVS, STOS, REP and the direction flag (issue #129).
//!
//! Expected results come from a byte-array model written from the operation
//! sections of the Intel SDM Vol. 2 for MOVS, STOS, REP, CLD and STD. The
//! model never decodes or calls the production semantics. Encodings were
//! assembled with `clang -target x86_64-apple-macos` and read with `objdump`.
//! Nothing was compared with x86 hardware.
//!
//! A REP form runs one element per step and stays on the instruction until RCX
//! is zero. The tests check that rule through the step counts, so a repeat can
//! never outrun the step budget.

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
const RSI: usize = 6;
const RDI: usize = 7;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Movs,
    Stos,
}

/// `[rep] movs/stos` for an element of `size` bytes, in the assembler's order.
fn encode(kind: Kind, size: u64, rep: bool) -> Vec<u8> {
    let mut code = Vec::new();
    if rep {
        code.push(0xF3);
    }
    match size {
        2 => code.push(0x66),
        8 => code.push(0x48),
        _ => {}
    }
    code.push(match (kind, size) {
        (Kind::Movs, 1) => 0xA4,
        (Kind::Movs, _) => 0xA5,
        (Kind::Stos, 1) => 0xAA,
        (Kind::Stos, _) => 0xAB,
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

/// The architectural effect of `count` elements on a byte array: each element
/// is read whole, then written, then the pointers move by its size.
fn model(
    kind: Kind,
    size: u64,
    backward: bool,
    count: u64,
    data: &mut [u8],
    rax: u64,
    (mut rsi, mut rdi): (u64, u64),
) -> (u64, u64) {
    let size_bytes = size as usize;
    for _ in 0..count {
        let element: Vec<u8> = match kind {
            Kind::Movs => {
                let from = (rsi - DATA) as usize;
                data[from..from + size_bytes].to_vec()
            }
            Kind::Stos => rax.to_le_bytes()[..size_bytes].to_vec(),
        };
        let to = (rdi - DATA) as usize;
        data[to..to + size_bytes].copy_from_slice(&element);
        if kind == Kind::Movs {
            rsi = if backward { rsi - size } else { rsi + size };
        }
        rdi = if backward { rdi - size } else { rdi + size };
    }
    (rsi, rdi)
}

const FLAGS: RFlags = RFlags {
    cf: true,
    zf: false,
    sf: true,
    of: false,
    pf: true,
    af: false,
};

#[test]
fn every_form_matches_a_byte_array_model_and_counts_one_step_per_element() {
    let mut random = Random(0x1234_5678_9ABC_DEF1);
    for kind in [Kind::Movs, Kind::Stos] {
        for size in [1, 2, 4, 8] {
            for rep in [false, true] {
                for backward in [false, true] {
                    for round in 0..40 {
                        // Counts 0 to 9. Source and destination start within 24
                        // bytes of each other, so the ranges often overlap.
                        let count = round % 10;
                        let rsi = DATA + 0x400 + random.next() % 0x100;
                        let rdi = rsi + random.next() % 49 - 24;
                        let rax = random.next();
                        let data: Vec<u8> = (0..PAGE).map(|_| random.next() as u8).collect();

                        let mut code = encode(kind, size, rep);
                        code.push(0xF4); // hlt
                        let memory = image(&code, &data);
                        let mut cpu = CpuState::new(ENTRY, 0);
                        for register in &mut cpu.gpr {
                            *register = random.next();
                        }
                        cpu.gpr[RAX] = rax;
                        cpu.gpr[RCX] = count;
                        cpu.gpr[RSI] = rsi;
                        cpu.gpr[RDI] = rdi;
                        cpu.df = backward;
                        cpu.flags = FLAGS;

                        // Without REP exactly one element moves and RCX is not
                        // read or written.
                        let elements = if rep { count } else { 1 };
                        let mut wanted_data = data.clone();
                        let (wanted_rsi, wanted_rdi) = model(
                            kind,
                            size,
                            backward,
                            elements,
                            &mut wanted_data,
                            rax,
                            (rsi, rdi),
                        );
                        let mut wanted = cpu.clone();
                        wanted.rip = ENTRY + code.len() as u64;
                        wanted.gpr[RSI] = wanted_rsi;
                        wanted.gpr[RDI] = wanted_rdi;
                        if rep {
                            wanted.gpr[RCX] = 0;
                        }
                        // One step per element, one for a REP with a zero
                        // count, and one for the HLT.
                        let steps = elements.max(1) as usize + 1;

                        for run in both(&cpu, &memory, 64) {
                            let what = format!(
                                "{kind:?} size {size} rep={rep} backward={backward} count={count} \
                                 on the {}",
                                run.name
                            );
                            assert_eq!(run.result.exit_reason, ExitReason::Halt, "{what}");
                            assert_eq!(run.result.final_state, wanted, "{what}");
                            assert!(bytes(&run.memory) == wanted_data, "{what}: memory");
                            assert_eq!(run.result.instructions_executed, steps, "{what}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn step_budget_stops_a_repeat_part_way_and_it_resumes() {
    // rep stosb; hlt
    let code = [0xF3, 0xAA, 0xF4];
    let memory = image(&code, &[0; PAGE]);
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr[RAX] = 0x5A;
    cpu.gpr[RCX] = 100;
    cpu.gpr[RDI] = DATA + 0x200;
    for run in both(&cpu, &memory, 10) {
        // Ten steps store ten bytes and leave RIP on the instruction.
        assert_eq!(
            run.result.exit_reason,
            ExitReason::StepLimitExceeded,
            "{}",
            run.name
        );
        assert_eq!(run.result.instructions_executed, 10, "{}", run.name);
        let state = &run.result.final_state;
        assert_eq!(
            (state.rip, state.gpr[RCX], state.gpr[RDI]),
            (ENTRY, 90, DATA + 0x20A),
            "{}",
            run.name
        );
        let written = bytes(&run.memory);
        assert_eq!(written[0x200..0x20A], [0x5A; 10], "{}", run.name);
        assert_eq!(written[0x20A..0x264], [0; 90], "{}", run.name);
        if run.name == "DBT" {
            assert_eq!(run.helpers, 10);
        }

        // Resuming from that state finishes the remaining ninety and halts.
        let [rest, _] = both(state, &run.memory, 1000);
        assert_eq!(rest.result.exit_reason, ExitReason::Halt, "{}", run.name);
        assert_eq!(rest.result.instructions_executed, 91, "{}", run.name);
        assert_eq!(rest.result.final_state.gpr[RCX], 0, "{}", run.name);
        assert_eq!(
            bytes(&rest.memory)[0x200..0x264],
            [0x5A; 100],
            "{}",
            run.name
        );
    }
}

#[test]
fn the_count_is_all_of_rcx() {
    // rep stosb with a count above 32 bits: two steps take exactly two off it.
    let memory = image(&[0xF3, 0xAA, 0xF4], &[0; PAGE]);
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr[RCX] = 0x0000_0001_0000_0001;
    cpu.gpr[RDI] = DATA;
    for run in both(&cpu, &memory, 2) {
        assert_eq!(
            run.result.exit_reason,
            ExitReason::StepLimitExceeded,
            "{}",
            run.name
        );
        assert_eq!(
            run.result.final_state.gpr[RCX], 0x0000_0000_FFFF_FFFF,
            "{}",
            run.name
        );
        assert_eq!(run.result.final_state.rip, ENTRY, "{}", run.name);
    }
}

#[test]
fn a_fault_keeps_the_elements_already_done() {
    // (code, RSI, RDI, backward, elements that complete before the fault)
    let cases: &[(&[u8], u64, u64, bool, u64)] = &[
        // rep movsq up to the end of the page: 19 bytes hold two elements, and
        // the third would end past the page.
        (&[0xF3, 0x48, 0xA5], DATA, END - 19, false, 2),
        // rep stosd down to the start of the page: elements at offsets 5 and 1
        // fit, the next would start before the page.
        (&[0xF3, 0xAB], 0, DATA + 5, true, 2),
        // rep movsw from a source that ends: three elements can be read.
        (&[0xF3, 0x66, 0xA5], END - 6, DATA + 0x100, false, 3),
        // rep movsb with no readable source at all.
        (&[0xF3, 0xA4], END, DATA + 0x100, false, 0),
    ];
    let mut random = Random(77);
    for &(code, rsi, rdi, backward, done) in cases {
        let data: Vec<u8> = (0..PAGE).map(|_| random.next() as u8).collect();
        let memory = image(code, &data);
        let insn = decode(ENTRY, |at| {
            code.get((at - ENTRY) as usize).copied().ok_or(())
        })
        .unwrap();
        let (kind, size) = (
            if insn.op == Op::Movs {
                Kind::Movs
            } else {
                Kind::Stos
            },
            u64::from(insn.size),
        );
        let mut cpu = CpuState::new(ENTRY, 0);
        cpu.gpr[RAX] = 0x1122_3344_5566_7788;
        cpu.gpr[RCX] = 9;
        cpu.gpr[RSI] = rsi;
        cpu.gpr[RDI] = rdi;
        cpu.df = backward;
        cpu.flags = FLAGS;

        // The completed elements took effect. The faulting one changed
        // nothing, and RIP stays on the instruction so it can be restarted.
        let mut wanted_data = data.clone();
        let (wanted_rsi, wanted_rdi) = model(
            kind,
            size,
            backward,
            done,
            &mut wanted_data,
            cpu.gpr[RAX],
            (rsi, rdi),
        );
        let mut wanted = cpu.clone();
        wanted.gpr[RCX] = 9 - done;
        wanted.gpr[RSI] = wanted_rsi;
        wanted.gpr[RDI] = wanted_rdi;

        for run in both(&cpu, &memory, 64) {
            let what = format!("{code:02X?} on the {}", run.name);
            assert!(
                matches!(run.result.exit_reason, ExitReason::MemoryFault(_)),
                "{what}: {:?}",
                run.result.exit_reason
            );
            assert_eq!(run.result.final_state, wanted, "{what}");
            assert!(bytes(&run.memory) == wanted_data, "{what}: memory");
            assert_eq!(run.result.instructions_executed, done as usize, "{what}");
        }
    }
}

#[test]
fn direction_flag_is_set_by_std_and_cleared_by_cld() {
    // std; stosb; cld; stosb; hlt
    let code = [0xFD, 0xAA, 0xFC, 0xAA, 0xF4];
    let memory = image(&code, &[0; PAGE]);
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr[RAX] = 0xC3;
    cpu.gpr[RDI] = DATA + 0x80;
    cpu.flags = FLAGS;
    for run in both(&cpu, &memory, 16) {
        assert_eq!(run.result.exit_reason, ExitReason::Halt, "{}", run.name);
        // The first store steps down to 0x7F, the second back up to 0x80.
        assert_eq!(run.result.final_state.gpr[RDI], DATA + 0x80, "{}", run.name);
        let written = bytes(&run.memory);
        assert_eq!(written[0x7F..0x81], [0xC3, 0xC3], "{}", run.name);
        assert!(!run.result.final_state.df, "{}", run.name);
        // Neither instruction touches a status flag.
        assert_eq!(run.result.final_state.flags, FLAGS, "{}", run.name);
    }
    // The flag outlives the run that set it.
    for run in both(&cpu, &image(&[0xFD, 0xF4], &[0; PAGE]), 4) {
        assert!(run.result.final_state.df, "{}", run.name);
        assert_eq!(run.result.final_state.flags, FLAGS, "{}", run.name);
    }
}

#[test]
fn a_segment_override_moves_the_source_only() {
    const BASE: u64 = 0x300;
    let mut data = vec![0u8; PAGE];
    data[0x10] = 0x11; // DATA + 0x10, what [RSI] names without a base
    data[0x310] = 0x22; // DATA + 0x10 + BASE, the same RSI under the FS or GS base
    for (code, uses_base) in [
        (&[0xA4][..], false),      // movsb
        (&[0x64, 0xA4][..], true), // movsb with the source under FS
        (&[0x65, 0xA4][..], true), // movsb with the source under GS
    ] {
        let memory = image(code, &data);
        let mut cpu = CpuState::new(ENTRY, 0);
        cpu.fs_base = BASE;
        cpu.gs_base = BASE;
        cpu.gpr[RSI] = DATA + 0x10;
        cpu.gpr[RDI] = DATA + 0x800;
        for run in both(&cpu, &memory, 1) {
            let what = format!("{code:02X?} on the {}", run.name);
            // The destination is [RDI] with no base either way.
            let wanted = if uses_base { 0x22 } else { 0x11 };
            assert_eq!(bytes(&run.memory)[0x800], wanted, "{what}");
            assert_eq!(
                run.result.final_state.gpr[RSI],
                DATA + 0x11,
                "{what}: RSI moves by the element, not by the base"
            );
        }
    }
    // STOS has no source, and its destination takes no override.
    let memory = image(&[0x64, 0xAA], &data);
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.fs_base = BASE;
    cpu.gpr[RAX] = 0x77;
    cpu.gpr[RDI] = DATA + 0x800;
    for run in both(&cpu, &memory, 1) {
        let written = bytes(&run.memory);
        assert_eq!((written[0x800], written[0xB00]), (0x77, 0), "{}", run.name);
    }
}

#[test]
fn exactly_the_declared_string_forms_decode() {
    let decode_bytes = |code: &[u8]| {
        decode(ENTRY, |at| {
            code.get((at - ENTRY) as usize).copied().ok_or(())
        })
    };
    let mut seen = BTreeSet::new();
    for kind in [Kind::Movs, Kind::Stos] {
        for size in [1, 2, 4, 8] {
            for rep in [false, true] {
                let code = encode(kind, size, rep);
                let insn =
                    decode_bytes(&code).unwrap_or_else(|error| panic!("{code:02X?}: {error:?}"));
                assert_eq!(usize::from(insn.len), code.len(), "{code:02X?}");
                assert_eq!((u64::from(insn.size), insn.rep), (size, rep), "{code:02X?}");
                assert!(!is_native(&insn), "{code:02X?}");
                seen.insert(usize::from(insn.form));
            }
        }
    }
    for code in [[0xFC], [0xFD]] {
        seen.insert(usize::from(decode_bytes(&code).unwrap().form));
    }
    // Every declared form of these operations was reached above.
    for (index, form) in FORMS.iter().enumerate() {
        if matches!(form.op, Op::Movs | Op::Stos | Op::Cld | Op::Std) {
            assert!(seen.contains(&index), "{:?} {:?}", form.op, form.size);
        }
    }

    let rejected: &[&[u8]] = &[
        &[0xF2, 0xA4],       // repne movsb
        &[0xF2, 0xAB],       // repne stosd
        &[0xF3, 0xF2, 0xA4], // both repeat prefixes
        &[0x67, 0xF3, 0xA4], // rep movsb with 32-bit addresses
        &[0xF0, 0xF3, 0xAA], // lock rep stosb
        &[0xF3, 0xFC],       // rep cld
        &[0xF2, 0xFD],       // repne std
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
            assert!(bytes(&run.memory) == vec![0; PAGE], "{code:02X?}");
        }
    }
}
