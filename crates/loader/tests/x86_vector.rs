// SPDX-License-Identifier: GPL-3.0-or-later

//! Independent tests of the SSE and SSE2 moves and packed integer forms.
//!
//! `tests/x86/vector.txt` pairs LLVM encodings with Intel-syntax text. The expected
//! results come from `tests/x86/vector_reference.rs`, which reads only the text.

#[path = "x86/vector_reference.rs"]
mod vector_reference;

use vector_reference::{Bytes, FLAGS, Machine, reference, string_compare};
use xenora_loader::cpu::{CpuState, ExecutionResult, ExitReason, Interpreter, Mxcsr, RFlags};
use xenora_loader::dbt::{DbtEngine, is_native};
use xenora_loader::memory::{GuestMemory, MemoryError, MemoryPermissions};
use xenora_loader::x86_decode::{DecodeError, RmOperand, decode};
use xenora_loader::x86_table::Op;

const ENTRY: u64 = 0x1000;
const DATA: u64 = 0x2000;
/// Address of `[rsi + 4*rdi + 16]` with RSI = DATA and RDI = 4.
const OPERAND: u64 = DATA + 32;

struct Case {
    code: Vec<u8>,
    text: &'static str,
}

fn parse(fixture: &'static str) -> Vec<Case> {
    fixture
        .lines()
        .map(|line| {
            let (hex, text) = line.split_once(" | ").unwrap();
            let code = hex
                .split(' ')
                .map(|b| u8::from_str_radix(b, 16).unwrap())
                .collect();
            Case { code, text }
        })
        .collect()
}

fn cases() -> Vec<Case> {
    parse(include_str!("x86/vector.txt"))
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Bytes biased towards lane boundaries and, at times, small shift counts.
    fn vector(&mut self) -> Bytes {
        const EDGES: [u8; 8] = [0, 1, 0x7F, 0x80, 0x81, 0xFE, 0xFF, 0x10];
        let mode = self.next() % 4;
        let mut out = [0; 16];
        for byte in &mut out {
            *byte = match mode {
                0 => self.next() as u8,
                _ => EDGES[(self.next() % 8) as usize],
            };
        }
        if mode == 3 {
            out[..8].copy_from_slice(&(self.next() % 70).to_le_bytes());
        }
        out
    }

    fn machine(&mut self) -> Machine {
        let mut gpr = [0; 16];
        for register in &mut gpr {
            *register = self.next();
        }
        (gpr[6], gpr[7]) = (DATA, 4);
        // RAX and RDX are string lengths, of either sign.
        for length in [0, 2] {
            if !self.next().is_multiple_of(4) {
                gpr[length] = (self.next() % 41).wrapping_sub(20);
            }
        }
        let mut xmm = [[0; 16]; 16];
        for register in &mut xmm {
            *register = self.vector();
        }
        Machine {
            gpr,
            xmm,
            operand: self.vector(),
            mxcsr: (self.next() & 0xFFFF) as u32,
            flags: FLAGS,
        }
    }
}

// --- harness ---------------------------------------------------------------

fn load(machine: &Machine, code: &[u8], permissions: MemoryPermissions) -> (CpuState, GuestMemory) {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    let mut data = vec![0xEE; 0x100];
    data[32..48].copy_from_slice(&machine.operand);
    memory
        .map_region_with_data(DATA, 0x100, permissions, &data)
        .unwrap();
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr = machine.gpr;
    cpu.xmm = machine.xmm.map(u128::from_le_bytes);
    cpu.mxcsr = Mxcsr(machine.mxcsr);
    cpu.flags = machine.flags;
    (cpu, memory)
}

fn run(native: bool, cpu: &CpuState, memory: &mut GuestMemory, budget: usize) -> ExecutionResult {
    if native {
        DbtEngine::new(256 * 1024)
            .unwrap()
            .run(&mut cpu.clone(), memory, budget)
    } else {
        Interpreter::new(cpu.clone(), memory, budget).run()
    }
}

fn data(memory: &GuestMemory) -> Vec<u8> {
    let mut out = vec![0; 0x100];
    memory.read_bytes(DATA, &mut out).unwrap();
    out
}

#[test]
fn every_vector_fixture_matches_the_reference_in_both_engines() {
    let mut random = Random(0xA076_1D64_78BD_642F);
    let mut native_forms = 0;
    for case in cases() {
        let text = case.text;
        let insn = decode(ENTRY, |address| {
            case.code.get((address - ENTRY) as usize).copied().ok_or(())
        })
        .unwrap_or_else(|error| panic!("{text}: {error:?}"));
        let native = is_native(&insn);
        native_forms += usize::from(native);
        let mut engine = DbtEngine::new(64 * 1024).unwrap();
        for _ in 0..64 {
            let machine = random.machine();
            let mut expected = machine.clone();
            reference(&mut expected, text);
            let (mut wanted, _) = load(&expected, &case.code, MemoryPermissions::READ_WRITE);
            wanted.rip = ENTRY + case.code.len() as u64;
            let (cpu, mut memory) = load(&machine, &case.code, MemoryPermissions::READ_WRITE);
            let interpreted = Interpreter::new(cpu.clone(), &mut memory, 1).run();
            let (mut dbt_cpu, mut dbt_memory) =
                load(&machine, &case.code, MemoryPermissions::READ_WRITE);
            let translated = engine.run(&mut dbt_cpu, &mut dbt_memory, 1);
            for (result, memory) in [(interpreted, memory), (translated, dbt_memory)] {
                assert_eq!(result.exit_reason, ExitReason::StepLimitExceeded, "{text}");
                assert_eq!(result.final_state, wanted, "{text} from {cpu:x?}");
                let mut bytes = vec![0xEE; 0x100];
                bytes[32..48].copy_from_slice(&expected.operand);
                assert_eq!(data(&memory), bytes, "{text}");
            }
        }
        assert_eq!(engine.metrics.interpreter_fallbacks == 0, native, "{text}");
    }
    assert!(native_forms > 150, "{native_forms}");
}

/// Whether a 16-byte operand may have any alignment.
fn any_alignment(name: &str) -> bool {
    ["movups", "movupd", "movdqu"].contains(&name)
        || name.starts_with("pcmpestr")
        || name.starts_with("pcmpistr")
}

#[test]
fn aligned_operands_fault_and_unaligned_operands_do_not() {
    let mut random = Random(0x51);
    let mut checked = [0, 0];
    for case in cases()
        .into_iter()
        .filter(|case| case.text.contains("xmmword ptr"))
    {
        let name = case.text.split(' ').next().unwrap();
        let unaligned = any_alignment(name);
        checked[usize::from(unaligned)] += 1;
        for offset in [1, 4, 8, 15] {
            let mut machine = random.machine();
            machine.gpr[6] = DATA + offset;
            for native in [false, true] {
                let (cpu, mut memory) = load(&machine, &case.code, MemoryPermissions::READ_WRITE);
                let before = data(&memory);
                let result = run(native, &cpu, &mut memory, 1);
                if unaligned {
                    assert_eq!(
                        result.exit_reason,
                        ExitReason::StepLimitExceeded,
                        "{}",
                        case.text
                    );
                    let store = case.text.split(", ").next().unwrap().contains(" ptr ");
                    if !store {
                        let mut expected = machine.clone();
                        let at = 32 + offset as usize;
                        expected.operand.copy_from_slice(&before[at..at + 16]);
                        reference(&mut expected, case.text);
                        let state = &result.final_state;
                        assert_eq!(state.gpr, expected.gpr, "{}", case.text);
                        assert_eq!(state.xmm, expected.xmm.map(u128::from_le_bytes));
                        assert_eq!(state.flags, expected.flags, "{}", case.text);
                    }
                } else {
                    assert_eq!(
                        result.exit_reason,
                        ExitReason::GeneralProtection,
                        "{}",
                        case.text
                    );
                    assert_eq!(result.final_state, cpu, "{}", case.text);
                    assert_eq!(data(&memory), before, "{}", case.text);
                }
            }
        }
    }
    assert!(checked[0] > 60 && checked[1] == 38, "{checked:?}");
}

#[test]
fn faulting_vector_accesses_change_nothing() {
    for case in cases()
        .into_iter()
        .filter(|case| case.text.contains(" ptr "))
    {
        let store = case.text.split(", ").next().unwrap().contains(" ptr ");
        let mut machine = Random(0x77).machine();
        let mut runs = vec![(0x9000, MemoryPermissions::READ_WRITE)];
        if store {
            runs.push((DATA, MemoryPermissions::READ_ONLY));
        }
        for (base, permissions) in runs {
            machine.gpr[6] = base;
            for native in [false, true] {
                let (cpu, mut memory) = load(&machine, &case.code, permissions);
                let before = data(&memory);
                let result = run(native, &cpu, &mut memory, 1);
                assert!(
                    matches!(result.exit_reason, ExitReason::MemoryFault(_)),
                    "{}",
                    case.text
                );
                assert_eq!(result.final_state, cpu, "{}", case.text);
                assert_eq!(data(&memory), before, "{}", case.text);
            }
        }
    }
}

#[test]
fn only_the_operand_width_is_accessed_at_the_end_of_a_region() {
    // pmovsx and pmovzx, by opcode: bw, bd, bq, wd, wq and dq from [rbx].
    let widening: Vec<([u8; 5], u64)> = [0x20, 0x30]
        .into_iter()
        .flat_map(|base| {
            [8, 4, 2, 8, 4, 8]
                .into_iter()
                .zip(base..)
                .map(|(width, opcode)| ([0x66, 0x0F, 0x38, opcode, 0x03], width))
        })
        .collect();
    let fixed = [
        (&[0xF3u8, 0x0F, 0x10, 0x03][..], 4),       // movss xmm0, [rbx]
        (&[0xF2, 0x0F, 0x10, 0x03], 8),             // movsd xmm0, [rbx]
        (&[0xF3, 0x0F, 0x7E, 0x03], 8),             // movq xmm0, [rbx]
        (&[0x66, 0x0F, 0xC4, 0x03, 0x01], 2),       // pinsrw xmm0, [rbx], 1
        (&[0x66, 0x0F, 0x3A, 0x20, 0x03, 0x01], 1), // pinsrb xmm0, [rbx], 1
        (&[0x66, 0x0F, 0x3A, 0x21, 0x03, 0xD8], 4), // insertps xmm0, [rbx], 0xD8
        (&[0x66, 0x0F, 0x3A, 0x17, 0x03, 0x03], 4), // extractps [rbx], xmm0, 3
        (&[0x66, 0x48, 0x0F, 0x3A, 0x17, 0x03, 0x03], 4), // the same with REX.W
        (&[0x66, 0x0F, 0x3A, 0x0A, 0x03, 0x00], 4), // roundss xmm0, [rbx], 0
        (&[0x66, 0x0F, 0x3A, 0x0B, 0x03, 0x00], 8), // roundsd xmm0, [rbx], 0
        (&[0x66, 0x0F, 0x3A, 0x22, 0x03, 0x01], 4), // pinsrd xmm0, [rbx], 1
        (&[0x66, 0x48, 0x0F, 0x3A, 0x22, 0x03, 0x01], 8), // pinsrq xmm0, [rbx], 1
        (&[0x66, 0x0F, 0x3A, 0x14, 0x03, 0x01], 1), // pextrb [rbx], xmm0, 1
        (&[0x66, 0x0F, 0x3A, 0x15, 0x03, 0x01], 2), // pextrw [rbx], xmm0, 1
        (&[0x66, 0x0F, 0x3A, 0x16, 0x03, 0x01], 4), // pextrd [rbx], xmm0, 1
        (&[0x66, 0x48, 0x0F, 0x3A, 0x16, 0x03, 0x01], 8), // pextrq [rbx], xmm0, 1
        (&[0x0F, 0x10, 0x03], 16),                  // movups xmm0, [rbx]
        (&[0x0F, 0x11, 0x03], 16),                  // movups [rbx], xmm0
    ];
    let all = widening.iter().map(|(code, width)| (&code[..], *width));
    for (code, width) in all.chain(fixed) {
        let end = DATA + 0x100;
        for (address, ok) in [(end - width, true), (end - width + 1, false)] {
            let mut machine = Random(0x99).machine();
            machine.gpr[3] = address;
            for native in [false, true] {
                let (cpu, mut memory) = load(&machine, code, MemoryPermissions::READ_WRITE);
                let result = run(native, &cpu, &mut memory, 1);
                assert_eq!(
                    result.exit_reason == ExitReason::StepLimitExceeded,
                    ok,
                    "{code:02X?}"
                );
            }
        }
    }
}

#[test]
fn register_store_encodings_write_the_rm_operand() {
    // The store opcodes with a register operand, which assemblers rarely emit;
    // `objdump` (LLVM) disassembles each as the text given here.
    let cases: &[(&str, &[u8])] = &[
        ("movaps xmm3, xmm9", &[0x44, 0x0F, 0x29, 0xCB]),
        ("movups xmm3, xmm9", &[0x44, 0x0F, 0x11, 0xCB]),
        ("movdqa xmm3, xmm9", &[0x66, 0x44, 0x0F, 0x7F, 0xCB]),
        ("movdqu xmm3, xmm9", &[0xF3, 0x44, 0x0F, 0x7F, 0xCB]),
        ("movss xmm3, xmm9", &[0xF3, 0x44, 0x0F, 0x11, 0xCB]),
        ("movsd xmm3, xmm9", &[0xF2, 0x44, 0x0F, 0x11, 0xCB]),
        ("movq xmm3, xmm9", &[0x66, 0x44, 0x0F, 0xD6, 0xCB]),
        (
            "pextrw ebx, xmm9, 5",
            &[0x66, 0x44, 0x0F, 0x3A, 0x15, 0xCB, 5],
        ),
        ("pextrw ebp, xmm1, 10", &[0x66, 0x0F, 0x3A, 0x15, 0xCD, 10]),
    ];
    let mut random = Random(0x1234);
    for (text, code) in cases {
        for _ in 0..32 {
            let machine = random.machine();
            let mut expected = machine.clone();
            reference(&mut expected, text);
            for native in [false, true] {
                let (cpu, mut memory) = load(&machine, code, MemoryPermissions::READ_WRITE);
                let result = run(native, &cpu, &mut memory, 1);
                assert_eq!(
                    result.final_state.xmm,
                    expected.xmm.map(u128::from_le_bytes),
                    "{text}"
                );
                assert_eq!(result.final_state.gpr, expected.gpr, "{text}");
            }
        }
    }
}

#[test]
fn mxcsr_resets_loads_stores_and_rejects_reserved_bits() {
    assert_eq!(CpuState::new(0, 0).mxcsr, Mxcsr(0x1F80));
    assert_eq!(CpuState::default().mxcsr, Mxcsr(0x1F80));
    for (value, accepted) in [
        (0x1F80u32, true),
        (0, true),
        (0xFFFF, true),
        (0x1_0000, false),
        (0x8000_1F80, false),
    ] {
        // ldmxcsr dword ptr [rbx]; stmxcsr dword ptr [rbx+4]
        let code = [0x0F, 0xAE, 0x13, 0x0F, 0xAE, 0x5B, 0x04];
        let mut machine = Random(0x55).machine();
        machine.gpr[3] = OPERAND;
        machine.operand[..4].copy_from_slice(&value.to_le_bytes());
        for native in [false, true] {
            let (cpu, mut memory) = load(&machine, &code, MemoryPermissions::READ_WRITE);
            let result = run(native, &cpu, &mut memory, 2);
            if accepted {
                assert_eq!(result.exit_reason, ExitReason::StepLimitExceeded);
                assert_eq!(result.final_state.mxcsr, Mxcsr(value));
                assert_eq!(memory.read_u32(OPERAND + 4).unwrap(), value);
            } else {
                assert_eq!(result.exit_reason, ExitReason::GeneralProtection);
                assert_eq!(result.final_state, cpu);
            }
        }
    }
}

#[test]
fn unsupported_vector_encodings_are_rejected() {
    let rejected: &[&[u8]] = &[
        &[0x0F, 0xFC, 0xC1],                         // paddb mm0, mm1 (MMX)
        &[0x0F, 0x53, 0xC1],                         // rcpps xmm0, xmm1 (approximation)
        &[0xF3, 0x0F, 0x52, 0xC1],                   // rsqrtss xmm0, xmm1 (approximation)
        &[0x0F, 0x2D, 0xC1],                         // cvtps2pi mm0, xmm1 (MMX)
        &[0x0F, 0x38, 0x00, 0xC1],                   // pshufb mm0, mm1 (MMX)
        &[0x0F, 0x3A, 0x0F, 0xC1, 0x08],             // palignr mm0, mm1, 8 (MMX)
        &[0xF3, 0x66, 0x0F, 0x38, 0x00, 0xC1],       // F3 over the mandatory 66
        &[0x66, 0xF2, 0x0F, 0x38, 0x00, 0xC1],       // F2 over the mandatory 66
        &[0x66, 0xF3, 0x0F, 0x3A, 0x0F, 0xC1, 0x08], // the same in 0F 3A
        &[0xF0, 0x66, 0x0F, 0x38, 0x00, 0x06],       // LOCK
        &[0x66, 0x0F, 0x38, 0x0C, 0xC1],             // unassigned without VEX
        &[0x66, 0x0F, 0x38, 0x2A, 0xC1],             // movntdqa with a register
        &[0x0F, 0x3A, 0x08, 0xC1, 0x00],             // roundps without 66
        &[0xF3, 0x66, 0x0F, 0x3A, 0x0A, 0xC1, 0x00], // F3 over the 66 of roundss
        &[0xF2, 0x66, 0x0F, 0x38, 0x14, 0xC1],       // F2 over the 66 of blendvps
        &[0xF0, 0x66, 0x0F, 0x3A, 0x17, 0x06, 0x00], // LOCK extractps
        &[0x0F, 0x3A, 0x63, 0xC1, 0x00],             // pcmpistri without 66
        &[0xF2, 0x66, 0x0F, 0x3A, 0x61, 0xC1, 0x00], // F2 over the 66 of pcmpestri
        &[0xF0, 0x66, 0x0F, 0x3A, 0x60, 0x06, 0x00], // LOCK pcmpestrm
        &[0xF2, 0x66, 0x0F, 0x38, 0x37, 0xC1],       // F2 over the 66 of pcmpgtq
        &[0x66, 0x0F, 0x38, 0x13, 0xC1],             // unassigned without VEX
        &[0x66, 0x0F, 0x3A, 0x18, 0xC1, 0x00],       // unassigned without VEX
        &[0x66, 0x0F, 0x38, 0xDC, 0xC1],             // aesenc
        &[0x66, 0x0F, 0x3A, 0x44, 0xC1, 0x00],       // pclmulqdq
        &[0x66, 0x0F, 0x3A, 0x00, 0xC1, 0x00],       // unassigned without VEX
        &[0x66, 0x0F, 0x39, 0x00, 0xC1],             // 0F 39 is no escape
        &[0xC4, 0xE2, 0x75, 0x47, 0xC2],             // vpsllvd ymm (AVX2)
        &[0xF2, 0xF3, 0x0F, 0x10, 0xC1],             // both mandatory prefixes
        &[0x66, 0xF3, 0x0F, 0x10, 0xC1],             // 66 with a scalar move
        &[0x66, 0x0F, 0x12, 0xC1],                   // movlpd with a register source
        &[0x0F, 0x13, 0xC1],                         // movlps register store
        &[0x66, 0x0F, 0xE7, 0xC1],                   // movntdq with a register
        &[0x66, 0x0F, 0x71, 0x11, 0x01],             // psrlw with a memory operand
        &[0x66, 0x0F, 0x71, 0xC1, 0x01],             // 71 /0
        &[0x0F, 0xAE, 0xD1],                         // ldmxcsr with a register
        &[0x66, 0x0F, 0xAE, 0x13],                   // 66 with ldmxcsr
    ];
    for code in rejected {
        let padded = [code, &[0x90; 8][..]].concat();
        let machine = Random(0x31).machine();
        for native in [false, true] {
            let (cpu, mut memory) = load(&machine, &padded, MemoryPermissions::READ_WRITE);
            let result = run(native, &cpu, &mut memory, 1);
            assert!(
                matches!(result.exit_reason, ExitReason::IllegalInstruction(_)),
                "{code:02X?}: {:?}",
                result.exit_reason
            );
            assert_eq!(result.final_state, cpu, "{code:02X?}");
        }
    }
}

#[test]
fn three_byte_opcodes_decode_by_the_general_rules() {
    let at = |code: &[u8]| {
        decode(ENTRY, |address| {
            code.get((address - ENTRY) as usize).copied().ok_or(())
        })
    };
    // Extension bits name the high registers, REX.W is ignored, and a segment
    // override or a repeated mandatory prefix changes nothing else.
    for (code, reg, rm) in [
        (&[0x66, 0x0F, 0x38, 0x00, 0xC1][..], 0, 1),
        (&[0x66, 0x45, 0x0F, 0x3A, 0x62, 0xC1, 0x05], 8, 9),
        (&[0x66, 0x45, 0x0F, 0x38, 0x00, 0xC1], 8, 9),
        (&[0x66, 0x48, 0x0F, 0x38, 0x00, 0xC1], 0, 1),
        (&[0x66, 0x66, 0x0F, 0x38, 0x00, 0xC1], 0, 1),
        (&[0x65, 0x66, 0x0F, 0x38, 0x00, 0xC1], 0, 1),
        (&[0x66, 0x44, 0x0F, 0x3A, 0x0F, 0xC1, 0x05], 8, 1),
    ] {
        let insn = at(code).unwrap();
        assert_eq!(usize::from(insn.len), code.len(), "{code:02X?}");
        assert_eq!((insn.reg.index, insn.size), (reg, 16), "{code:02X?}");
        assert!(
            matches!(insn.rm, Some(RmOperand::Reg(operand)) if operand.index == rm),
            "{code:02X?}"
        );
        // Every byte belongs to the instruction.
        for length in 1..code.len() {
            assert!(
                matches!(at(&code[..length]), Err(DecodeError::Fetch(()))),
                "{code:02X?} cut to {length}"
            );
        }
    }
    // Nothing is fetched after an escape that cannot start a declared form.
    for code in [
        &[0x0F, 0x38][..],
        &[0xF3, 0x66, 0x0F, 0x3A],
        &[0xF3, 0x0F, 0x38],
        &[0xF2, 0x0F, 0x3A],
    ] {
        assert!(
            matches!(at(code), Err(DecodeError::Unsupported(_))),
            "{code:02X?}"
        );
    }
    // Without 0F these are byte compares, on which 66 has no effect.
    for code in [[0x66, 0x38, 0xC1], [0x66, 0x3A, 0xC1]] {
        let insn = at(&code).unwrap();
        assert_eq!((insn.op, insn.size, insn.len), (Op::Cmp, 1, 3));
    }
    // 15 bytes are allowed and 16 are not.
    let mut long = vec![0x66; 11];
    long.extend([0x0F, 0x38, 0x00, 0xC1]);
    assert_eq!(at(&long).unwrap().len, 15);
    long.insert(0, 0x66);
    assert!(matches!(at(&long), Err(DecodeError::TooLong)));
}

#[test]
fn a_rip_relative_operand_counts_the_immediate() {
    // palignr xmm1, [rip + displacement], 4: ten bytes, the last one immediate.
    let displacement = (OPERAND as i64 - (ENTRY as i64 + 10)) as i32;
    let mut code = vec![0x66, 0x0F, 0x3A, 0x0F, 0x0D];
    code.extend(displacement.to_le_bytes());
    code.push(4);
    let machine = Random(0x77).machine();
    let mut wanted = [0; 16];
    wanted[..12].copy_from_slice(&machine.operand[4..]);
    wanted[12..].copy_from_slice(&machine.xmm[1][..4]);
    for native in [false, true] {
        let (cpu, mut memory) = load(&machine, &code, MemoryPermissions::READ_WRITE);
        let result = run(native, &cpu, &mut memory, 1);
        assert_eq!(result.exit_reason, ExitReason::StepLimitExceeded);
        assert_eq!(result.final_state.xmm[1], u128::from_le_bytes(wanted));
    }
}

/// SDM, PTEST: ZF is set when the AND is zero, CF when the AND of the source
/// with the complement of the destination is, and the other four are cleared.
#[test]
fn ptest_sets_zero_and_carry_and_clears_the_rest() {
    let top = 1u128 << 127;
    for (target, source, zf, cf) in [
        (0, 0, true, true),
        (1, 0, true, true),
        (0, 1, true, false),
        (u128::MAX, u128::MAX, false, true),
        (u128::MAX, top, false, true),
        (top - 1, top, true, false),
        (top, u128::MAX, false, false),
        (1 << 64, 1 << 64, false, true),
        (0xF0, 0x3C, false, false),
    ] {
        let mut machine = Random(0x99).machine();
        machine.xmm[1] = target.to_le_bytes();
        machine.xmm[2] = source.to_le_bytes();
        for start in [
            RFlags::default(),
            RFlags {
                cf: true,
                zf: true,
                sf: true,
                of: true,
                pf: true,
                af: true,
            },
        ] {
            machine.flags = start;
            let code = [0x66, 0x0F, 0x38, 0x17, 0xCA]; // ptest xmm1, xmm2
            for native in [false, true] {
                let (cpu, mut memory) = load(&machine, &code, MemoryPermissions::READ_WRITE);
                let result = run(native, &cpu, &mut memory, 1);
                let wanted = RFlags {
                    zf,
                    cf,
                    ..RFlags::default()
                };
                assert_eq!(result.final_state.flags, wanted, "{target:x} {source:x}");
                assert_eq!(result.final_state.xmm, cpu.xmm);
            }
        }
    }
}

#[test]
fn flags_survive_a_ptest_that_faults() {
    // add eax, ecx; ptest xmm1, [rbx]
    let code = [0x01, 0xC8, 0x66, 0x0F, 0x38, 0x17, 0x0B];
    let mut machine = Random(0x9A).machine();
    (machine.gpr[0], machine.gpr[1]) = (0xFFFF_FFFF, 1);
    for (address, misaligned) in [(0x9000, false), (DATA + 8, true)] {
        machine.gpr[3] = address;
        for native in [false, true] {
            let (cpu, mut memory) = load(&machine, &code, MemoryPermissions::READ_WRITE);
            let result = run(native, &cpu, &mut memory, 2);
            if misaligned {
                assert_eq!(result.exit_reason, ExitReason::GeneralProtection);
            } else {
                assert!(matches!(result.exit_reason, ExitReason::MemoryFault(_)));
            }
            // FFFFFFFF + 1 carries out of every position and leaves zero.
            let wanted = RFlags {
                cf: true,
                zf: true,
                sf: false,
                of: false,
                pf: true,
                af: true,
            };
            assert_eq!(result.final_state.flags, wanted);
            assert_eq!(result.final_state.rip, ENTRY + 2);
        }
    }
}

/// A byte or word lane moves through a 32-bit register: without REX, register
/// field 4 to 7 is RSP to RDI and never AH to BH.
#[test]
fn lanes_do_not_use_the_legacy_high_byte_registers() {
    let mut machine = Random(0x9B).machine();
    machine.gpr[0] = 0xAAAA_AAAA_AAAA_AAAA;
    machine.gpr[4] = 0x1111_1111_1111_1144;
    machine.xmm[1] = [0x77; 16];
    // pinsrb xmm1, esp, 0, then pextrb esp, xmm1, 0
    let code = [
        0x66, 0x0F, 0x3A, 0x20, 0xCC, 0x00, 0x66, 0x0F, 0x3A, 0x14, 0xCC, 0x00,
    ];
    for native in [false, true] {
        let (cpu, mut memory) = load(&machine, &code, MemoryPermissions::READ_WRITE);
        let result = run(native, &cpu, &mut memory, 2);
        assert_eq!(result.final_state.xmm[1].to_le_bytes()[..2], [0x44, 0x77]);
        assert_eq!(result.final_state.gpr[4], 0x44);
        assert_eq!(result.final_state.gpr[0], machine.gpr[0]);
    }
}

/// Both vector fixtures, with the byte position after the legacy prefixes.
fn all_vector_cases() -> Vec<(Case, usize)> {
    cases()
        .into_iter()
        .chain(parse(include_str!("x86/float.txt")))
        .map(|case| {
            let prefixes = case
                .code
                .iter()
                .take_while(|byte| matches!(byte, 0x66 | 0xF2 | 0xF3))
                .count();
            (case, prefixes)
        })
        .collect()
}

/// Exit reason, state and data after one instruction, with the instruction
/// pointer relative to the end of the instruction.
fn outcome(
    native: bool,
    cpu: &CpuState,
    code: &[u8],
    operand: &Bytes,
) -> (ExitReason, CpuState, Vec<u8>) {
    let machine = Machine {
        gpr: cpu.gpr,
        xmm: [[0; 16]; 16],
        operand: *operand,
        mxcsr: 0,
        flags: FLAGS,
    };
    let (_, mut memory) = load(&machine, code, MemoryPermissions::READ_WRITE);
    let result = run(native, cpu, &mut memory, 1);
    let mut state = result.final_state;
    state.rip = state.rip.wrapping_sub(code.len() as u64);
    (result.exit_reason, state, data(&memory))
}

/// All exceptions masked, the other controls of a random machine kept.
fn masked(cpu: &mut CpuState) {
    cpu.mxcsr.0 |= 0x1F80;
}

/// The bits of an instruction that REX.W cannot change: SDM Vol. 2 lists no
/// REX.W form for these mnemonics.
#[test]
fn rex_w_changes_no_form_that_ignores_it() {
    let mut random = Random(0xD1CE);
    let mut checked = 0;
    for (case, prefixes) in all_vector_cases() {
        let name = case.text.split(' ').next().unwrap();
        let sized = name.starts_with("pcmpestr")
            || ["movd", "movq", "pextrd", "pextrq", "pinsrd", "pinsrq"].contains(&name)
            || name.contains("2si")
            || name.starts_with("cvtsi2");
        if sized {
            continue;
        }
        let mut code = case.code.clone();
        if code[prefixes] & 0xF0 == 0x40 {
            code[prefixes] |= 8;
        } else {
            code.insert(prefixes, 0x48);
        }
        let machine = random.machine();
        let (mut cpu, _) = load(&machine, &case.code, MemoryPermissions::READ_WRITE);
        masked(&mut cpu);
        for native in [false, true] {
            let plain = outcome(native, &cpu, &case.code, &machine.operand);
            assert_eq!(plain.0, ExitReason::StepLimitExceeded, "{}", case.text);
            let wide = outcome(native, &cpu, &code, &machine.operand);
            assert_eq!(wide, plain, "{} as {code:02X?}", case.text);
        }
        checked += 1;
    }
    assert!(checked > 900, "{checked}");
}

#[test]
fn segment_overrides_add_their_base_to_every_memory_operand() {
    let mut random = Random(0xF5);
    let mut checked = 0;
    for (case, _) in all_vector_cases() {
        if !case.text.contains(" ptr ") {
            continue;
        }
        let machine = random.machine();
        let (mut cpu, _) = load(&machine, &case.code, MemoryPermissions::READ_WRITE);
        masked(&mut cpu);
        // The other segment's base must not be used.
        (cpu.fs_base, cpu.gs_base) = (0x1_0000, 0x2_0000);
        for native in [false, true] {
            let plain = outcome(native, &cpu, &case.code, &machine.operand);
            assert_eq!(plain.0, ExitReason::StepLimitExceeded, "{}", case.text);
            for (prefix, base) in [(0x64, 0x1_0000), (0x65, 0x2_0000), (0x2E, 0)] {
                let code = [&[prefix][..], &case.code].concat();
                let mut moved = cpu.clone();
                // The sum wraps around the address space. Alignment is judged
                // on the linear address, so an odd base does not matter.
                moved.gpr[6] = DATA.wrapping_sub(base + 3);
                if base != 0 {
                    moved.fs_base += 3;
                    moved.gs_base += 3;
                } else {
                    moved.gpr[6] = DATA;
                }
                let mut result = outcome(native, &moved, &code, &machine.operand);
                assert_eq!(result.0, plain.0, "{prefix:02x} {}", case.text);
                result.1.gpr[6] = plain.1.gpr[6];
                (result.1.fs_base, result.1.gs_base) = (plain.1.fs_base, plain.1.gs_base);
                assert_eq!(result, plain, "{prefix:02x} {}", case.text);
            }
            // An aligned displacement on a misaligned base is misaligned.
            let name = case.text.split(' ').next().unwrap();
            if case.text.contains("xmmword ptr") && !any_alignment(name) {
                let mut moved = cpu.clone();
                moved.gs_base = 8;
                let code = [&[0x65][..], &case.code].concat();
                let (exit, mut state, _) = outcome(native, &moved, &code, &machine.operand);
                assert_eq!(exit, ExitReason::GeneralProtection, "{}", case.text);
                state.rip = state.rip.wrapping_add(code.len() as u64);
                assert_eq!(state, moved, "{}", case.text);
            }
        }
        checked += 1;
    }
    assert!(checked > 250, "{checked}");
}

const EXECUTE_ONLY: MemoryPermissions = MemoryPermissions {
    read: false,
    write: false,
    execute: true,
};

fn operand_width(text: &str) -> u64 {
    let place = text.split(" ptr ").next().unwrap();
    match place.rsplit([' ', ',']).next().unwrap() {
        "xmmword" => 16,
        "qword" => 8,
        "dword" => 4,
        "word" => 2,
        "byte" => 1,
        other => panic!("width {other}"),
    }
}

#[test]
fn vector_faults_name_the_access_and_the_first_bad_byte() {
    let end = DATA + 0x100;
    let mut crossing = 0;
    for (case, _) in all_vector_cases() {
        if !case.text.contains(" ptr ") {
            continue;
        }
        let text = case.text;
        let name = text.split(' ').next().unwrap();
        let store = text.split(", ").next().unwrap().contains(" ptr ");
        let width = operand_width(text);
        let unaligned = width < 16 || any_alignment(name);
        let mut tries = vec![
            (0x9000, MemoryPermissions::READ_WRITE, Some(0x9000)),
            (DATA + 0x40, MemoryPermissions::READ_ONLY, None),
            (DATA + 0x40, EXECUTE_ONLY, None),
        ];
        if unaligned && width > 1 {
            // All but the last byte are mapped.
            tries.push((end - width + 1, MemoryPermissions::READ_WRITE, Some(end)));
            crossing += 1;
        }
        for (address, permissions, unmapped) in tries {
            let mut machine = Random(0x3C).machine();
            machine.gpr[6] = address - 32;
            for native in [false, true] {
                let (mut cpu, mut memory) = load(&machine, &case.code, permissions);
                masked(&mut cpu);
                let result = run(native, &cpu, &mut memory, 1);
                let wanted = match unmapped {
                    Some(at) => MemoryError::UnmappedAddress(at),
                    None if store || permissions == EXECUTE_ONLY => {
                        MemoryError::PermissionViolation {
                            address,
                            required: if store { "write" } else { "read" },
                        }
                    }
                    None => continue, // a load from a readable region
                };
                assert_eq!(
                    result.exit_reason,
                    ExitReason::MemoryFault(wanted),
                    "{text}"
                );
                assert_eq!(result.final_state, cpu, "{text}");
                assert_eq!(result.instructions_executed, 0, "{text}");
            }
        }
    }
    assert!(crossing > 100, "{crossing}");
}

/// A non-canonical address is a general-protection fault, not a page fault, also
/// when only the end of the operand is non-canonical.
#[test]
fn non_canonical_vector_operands_are_general_protection_faults() {
    const LIMIT: u64 = 0x0000_8000_0000_0000;
    let mut straddling = 0;
    for (case, _) in all_vector_cases() {
        if !case.text.contains(" ptr ") {
            continue;
        }
        let width = operand_width(case.text);
        let mut addresses = vec![LIMIT, 0xFFFF_7FFF_FFFF_FFF0, 1 << 63, 0x1234_5678_0000_0000];
        if width > 1 {
            addresses.push(LIMIT - width + 1);
            straddling += 1;
        }
        for address in addresses {
            let mut machine = Random(0x3D).machine();
            machine.gpr[6] = address.wrapping_sub(32);
            for native in [false, true] {
                let (mut cpu, mut memory) =
                    load(&machine, &case.code, MemoryPermissions::READ_WRITE);
                masked(&mut cpu);
                let result = run(native, &cpu, &mut memory, 1);
                assert_eq!(
                    result.exit_reason,
                    ExitReason::GeneralProtection,
                    "{} at {address:#x}",
                    case.text
                );
                assert_eq!(result.final_state, cpu, "{}", case.text);
            }
        }
        // The last canonical operand is only unmapped.
        let mut machine = Random(0x3D).machine();
        machine.gpr[6] = LIMIT - 16 - 32;
        for native in [false, true] {
            let (cpu, mut memory) = load(&machine, &case.code, MemoryPermissions::READ_WRITE);
            let result = run(native, &cpu, &mut memory, 1);
            assert_eq!(
                result.exit_reason,
                ExitReason::MemoryFault(MemoryError::UnmappedAddress(LIMIT - 16)),
                "{}",
                case.text
            );
        }
    }
    assert!(straddling > 200, "{straddling}");
}

/// The DBT may drop the flags of an instruction only if the next one replaces
/// them all, so each vector form runs between an ADD and the end of the block.
#[test]
fn flags_of_an_earlier_instruction_survive_every_form_that_keeps_them() {
    // FFFFFFFF + 1 carries out of every position and leaves zero.
    let added = RFlags {
        cf: true,
        zf: true,
        sf: false,
        of: false,
        pf: true,
        af: true,
    };
    let mut replaced = 0;
    for (case, _) in all_vector_cases() {
        let name = case.text.split(' ').next().unwrap();
        // add eax, ecx; the form; mov edx, edx; hlt
        let code = [&[0x01, 0xC8][..], &case.code, &[0x89, 0xD2, 0xF4]].concat();
        let mut machine = Random(0xADD).machine();
        (machine.gpr[0], machine.gpr[1]) = (0xFFFF_FFFF, 1);
        let mut expected = machine.clone();
        expected.gpr[0] = 0; // the sum, which is also a string length
        let replaces =
            name == "ptest" || name.starts_with("pcmpestr") || name.starts_with("pcmpistr");
        if replaces {
            reference(&mut expected, case.text);
            replaced += 1;
        }
        for budget in [2, 3, 4] {
            let mut states = Vec::new();
            for native in [false, true] {
                let (mut cpu, mut memory) = load(&machine, &code, MemoryPermissions::READ_WRITE);
                masked(&mut cpu);
                let result = run(native, &cpu, &mut memory, budget);
                if replaces {
                    assert_eq!(result.final_state.flags, expected.flags, "{}", case.text);
                } else if !name.contains("comi") {
                    assert_eq!(result.final_state.flags, added, "{}", case.text);
                }
                states.push((result.exit_reason, result.final_state));
            }
            assert_eq!(states[0], states[1], "{}", case.text);
        }
    }
    assert_eq!(replaced, 3 + 4 * 139);
}

/// SETB, SETZ and a branch read what a register PTEST wrote, and flags before it
/// are dead.
#[test]
fn flags_around_a_register_ptest_are_exact() {
    // add eax, ecx; ptest xmm1, xmm2; setb bl; setz dl; jz +1; nop; hlt
    let code = [
        0x01, 0xC8, 0x66, 0x0F, 0x38, 0x17, 0xCA, 0x0F, 0x92, 0xC3, 0x0F, 0x94, 0xC2, 0x74, 0x01,
        0x90, 0xF4,
    ];
    for (target, source, zf, cf) in [
        (1u128, 2u128, true, false),
        (3, 1, false, true),
        (1, 3, false, false),
        (1, 0, true, true),
    ] {
        let mut machine = Random(0x9C).machine();
        // Leaves CF and ZF both set or, for the second value, both clear.
        for addend in [(0xFFFF_FFFF, 1), (1, 1)] {
            (machine.gpr[0], machine.gpr[1]) = addend;
            (machine.gpr[2], machine.gpr[3]) = (0, 0);
            machine.xmm[1] = target.to_le_bytes();
            machine.xmm[2] = source.to_le_bytes();
            for native in [false, true] {
                let (cpu, mut memory) = load(&machine, &code, MemoryPermissions::READ_WRITE);
                let result = run(native, &cpu, &mut memory, 16);
                assert_eq!(result.exit_reason, ExitReason::Halt);
                assert_eq!(result.instructions_executed, if zf { 6 } else { 7 });
                assert_eq!(result.final_state.gpr[3], u64::from(cf));
                assert_eq!(result.final_state.gpr[2], u64::from(zf));
                assert_eq!(
                    result.final_state.flags,
                    RFlags {
                        zf,
                        cf,
                        ..RFlags::default()
                    }
                );
            }
        }
    }
    // Only the register form lets the flags before it go: the memory form can
    // fault and leave the block with them.
    let size = |modrm: u8| {
        // add eax, ecx; ptest xmm1, xmm2 or [rdx]; hlt
        let code = [0x01, 0xC8, 0x66, 0x0F, 0x38, 0x17, modrm, 0xF4];
        let mut machine = Random(0x9E).machine();
        machine.gpr[2] = OPERAND;
        let (mut cpu, mut memory) = load(&machine, &code, MemoryPermissions::READ_WRITE);
        let mut engine = DbtEngine::new(64 * 1024).unwrap();
        let result = engine.run(&mut cpu, &mut memory, 16);
        assert_eq!(result.exit_reason, ExitReason::Halt);
        assert_eq!(engine.metrics.interpreter_fallbacks, 1);
        engine.metrics.native_bytes_emitted
    };
    assert!(size(0xCA) < size(0x0A), "{} {}", size(0xCA), size(0x0A));
    // ptest xmm1, xmm2; add eax, ecx; hlt: the flags of PTEST are dead.
    let code = [0x66, 0x0F, 0x38, 0x17, 0xCA, 0x01, 0xC8, 0xF4];
    let mut machine = Random(0x9D).machine();
    (machine.gpr[0], machine.gpr[1]) = (0x7FFF_FFFF, 1);
    for native in [false, true] {
        let (cpu, mut memory) = load(&machine, &code, MemoryPermissions::READ_WRITE);
        let result = run(native, &cpu, &mut memory, 16);
        let wanted = RFlags {
            cf: false,
            zf: false,
            sf: true,
            of: true,
            pf: true,
            af: true,
        };
        assert_eq!(result.final_state.flags, wanted);
    }
}

fn padded(text: &str) -> Bytes {
    let mut out = [0; 16];
    out[..text.len()].copy_from_slice(text.as_bytes());
    out
}

/// Hand-worked results of the idioms the instructions were made for.
#[test]
fn string_compares_give_hand_worked_results() {
    // (opcode, immediate, RAX, RDX, first, second, index or mask, CF ZF SF OF)
    let sixteen = "0123456789abcdef";
    type Row = (
        u8,
        u8,
        u64,
        u64,
        &'static str,
        &'static str,
        u128,
        [bool; 4],
    );
    let rows: &[Row] = &[
        // The first vowel of "hello world" is at 1, and the last at 7.
        (
            0x63,
            0x00,
            0,
            0,
            "aeiou",
            "hello world",
            1,
            [true, true, true, false],
        ),
        (
            0x63,
            0x40,
            0,
            0,
            "aeiou",
            "hello world",
            7,
            [true, true, true, false],
        ),
        // None of them: the index is the number of elements, and CF is clear.
        (
            0x63,
            0x00,
            0,
            0,
            "xyz",
            "hello",
            16,
            [false, true, true, false],
        ),
        // The first character outside a to z: the space. With plain negation the
        // unused elements count too, so from the end it is the last of them.
        (
            0x63,
            0x14,
            0,
            0,
            "az",
            "hello world",
            5,
            [true, true, true, false],
        ),
        (
            0x63,
            0x54,
            0,
            0,
            "az",
            "hello world",
            15,
            [true, true, true, false],
        ),
        (
            0x63,
            0x74,
            0,
            0,
            "az",
            "hello world",
            5,
            [true, true, true, false],
        ),
        // Substring search.
        (
            0x63,
            0x0C,
            0,
            0,
            "wor",
            "hello world",
            6,
            [true, true, true, false],
        ),
        // A needle cut off by the end of a full register still matches there.
        (
            0x63,
            0x0C,
            0,
            0,
            "efgh",
            sixteen,
            14,
            [true, false, true, false],
        ),
        (
            0x63,
            0x0C,
            0,
            0,
            "efgh",
            "0123456789abcde",
            16,
            [false, true, true, false],
        ),
        // An empty needle is everywhere.
        (0x63, 0x0C, 0, 0, "", "x", 0, [true, true, true, true]),
        // strcmp: the first difference, and none between equal strings.
        (
            0x63,
            0x18,
            0,
            0,
            "abcd",
            "abcx",
            3,
            [true, true, true, false],
        ),
        (
            0x63,
            0x18,
            0,
            0,
            "abcd",
            "abcd",
            16,
            [false, true, true, false],
        ),
        (
            0x63,
            0x18,
            0,
            0,
            "abcd",
            "abc",
            3,
            [true, true, true, false],
        ),
        // Masked negation leaves the elements past both ends set, not only the
        // difference at 3.
        (
            0x63,
            0x38,
            0,
            0,
            "abcd",
            "abc",
            4,
            [true, true, true, false],
        ),
        (
            0x63,
            0x18,
            0,
            0,
            sixteen,
            sixteen,
            16,
            [false, false, false, false],
        ),
        // Masks: bits, then bytes.
        (
            0x62,
            0x00,
            0,
            0,
            "l",
            "hello",
            0b01100,
            [true, true, true, false],
        ),
        (
            0x62,
            0x40,
            0,
            0,
            "l",
            "hello",
            0xFFFF_0000,
            [true, true, true, false],
        ),
        // Word elements: "he" is the word 6568h, the second word of "xxhe".
        (0x63, 0x01, 0, 0, "he", "xxhe", 1, [true, true, true, false]),
        (
            0x62,
            0x41,
            0,
            0,
            "he",
            "xxhe",
            0xFFFF_0000,
            [true, true, true, false],
        ),
        // Explicit lengths: the sign is dropped, large ones saturate, and zeros
        // are characters.
        (
            0x61,
            0x00,
            5,
            11,
            "aeiou",
            "hello world",
            1,
            [true, true, true, false],
        ),
        (
            0x61,
            0x00,
            (-5i64) as u64,
            (-11i64) as u64,
            "aeiou",
            "hello world",
            1,
            [true, true, true, false],
        ),
        (
            0x61,
            0x00,
            1,
            11,
            "aeiou",
            "hello world",
            16,
            [false, true, true, false],
        ),
        (
            0x61,
            0x00,
            16,
            100,
            "",
            "hello",
            5,
            [true, false, false, false],
        ),
        (
            0x61,
            0x00,
            0x8000_0000,
            0x8000_0000,
            "",
            "hello",
            5,
            [true, false, false, false],
        ),
        (
            0x61,
            0x00,
            0,
            11,
            "aeiou",
            "hello world",
            16,
            [false, true, true, false],
        ),
        // Without REX.W only EAX and EDX count.
        (
            0x61,
            0x00,
            0xFFFF_FFFF_0000_0005,
            0x1_0000_000B,
            "aeiou",
            "hello world",
            1,
            [true, true, true, false],
        ),
        (
            0x60,
            0x08,
            4,
            4,
            "abcd",
            "abxd",
            0b1111_1111_1111_1011,
            [true, true, true, true],
        ),
        (
            0x60,
            0x18,
            4,
            4,
            "abcd",
            "abxd",
            0b100,
            [true, true, true, false],
        ),
    ];
    for &(opcode, imm, rax, rdx, first, second, result, [cf, zf, sf, of]) in rows {
        let code = [0x66, 0x0F, 0x3A, opcode, 0xCA, imm]; // xmm1, xmm2
        let mut machine = Random(0x42).machine();
        (machine.gpr[0], machine.gpr[2]) = (rax, rdx);
        machine.xmm[1] = padded(first);
        machine.xmm[2] = padded(second);
        for native in [false, true] {
            let (cpu, mut memory) = load(&machine, &code, MemoryPermissions::READ_WRITE);
            let run = run(native, &cpu, &mut memory, 1);
            let mut wanted = cpu.clone();
            wanted.rip += 6;
            if opcode & 1 == 1 {
                wanted.gpr[1] = result as u64;
            } else {
                wanted.xmm[0] = result;
            }
            wanted.flags = RFlags {
                cf,
                zf,
                sf,
                of,
                ..RFlags::default()
            };
            let what = format!("{opcode:x} {imm:#x} {first:?} {second:?}");
            assert_eq!(run.exit_reason, ExitReason::StepLimitExceeded, "{what}");
            assert_eq!(run.final_state, wanted, "{what}");
        }
    }
}

/// Short alphabets, so that all aggregations find something, at every immediate
/// including the reserved bit 7, with both widths of the length registers.
#[test]
fn string_compares_match_the_reference_on_related_strings() {
    let mut random = Random(0x57A1);
    let mut found = [0u32; 4];
    for imm in 0..=255u8 {
        for opcode in 0x60..=0x63u8 {
            let name =
                ["pcmpestrm", "pcmpestri", "pcmpistrm", "pcmpistri"][usize::from(opcode - 0x60)];
            for wide in [false, true] {
                let mut code = vec![0x66, 0x48, 0x0F, 0x3A, opcode, 0xCA, imm];
                if !wide {
                    code.remove(1);
                }
                let mut engine = DbtEngine::new(16 * 1024).unwrap();
                for _ in 0..24 {
                    let mut machine = random.machine();
                    const ALPHABET: [u8; 8] = [0, 1, 1, 2, 2, 0x7F, 0x80, 0xFF];
                    for register in [1, 2] {
                        for byte in &mut machine.xmm[register] {
                            *byte = ALPHABET[(random.next() % 8) as usize];
                        }
                    }
                    if random.next().is_multiple_of(2) {
                        // The second operand contains the start of the first.
                        let at = (random.next() % 14) as usize;
                        let prefix = machine.xmm[1];
                        machine.xmm[2][at..].copy_from_slice(&prefix[..16 - at]);
                    }
                    if random.next().is_multiple_of(4) {
                        // Only a full-width read sees these as large, or as zero.
                        machine.gpr[0] = (1 << (32 + random.next() % 32)) | (random.next() % 9);
                        machine.gpr[2] = (random.next() % 9).wrapping_neg() & 0xFFFF_FFFF;
                    }
                    let mut expected = machine.clone();
                    let (first, second) = (machine.xmm[1], machine.xmm[2]);
                    string_compare(
                        &mut expected,
                        name,
                        first,
                        second,
                        usize::from(imm % 128),
                        wide,
                    );
                    found[usize::from(imm / 4 % 4)] += u32::from(expected.flags.cf);
                    let (mut wanted, _) = load(&expected, &code, MemoryPermissions::READ_WRITE);
                    wanted.rip += code.len() as u64;
                    let (cpu, mut memory) = load(&machine, &code, MemoryPermissions::READ_WRITE);
                    let interpreted = Interpreter::new(cpu.clone(), &mut memory, 1).run();
                    let translated = engine.run(&mut cpu.clone(), &mut memory, 1);
                    for result in [interpreted, translated] {
                        assert_eq!(
                            result.final_state, wanted,
                            "{name} {imm:#x} {first:x?} {second:x?}"
                        );
                    }
                }
            }
        }
    }
    assert!(found.iter().all(|count| *count > 2000), "{found:?}");
}

#[test]
fn mixed_vector_and_integer_sequences_agree() {
    let vectors = cases();
    let integers: Vec<Case> = parse(include_str!("x86/integer.txt"))
        .into_iter()
        .filter(|case| {
            !["push", "pop", "leave", "div", "idiv"]
                .iter()
                .any(|name| case.text.starts_with(name))
                && case.text != "mov sil, dil"
        })
        .collect();
    let mut random = Random(0xBEEF);
    for round in 0..3000 {
        let mut code = Vec::new();
        let mut text = Vec::new();
        for _ in 0..2 + random.next() % 30 {
            let pool = if random.next().is_multiple_of(4) {
                &integers
            } else {
                &vectors
            };
            let case = &pool[(random.next() % pool.len() as u64) as usize];
            code.extend_from_slice(&case.code);
            text.push(case.text);
        }
        code.push(0xF4);
        let mut machine = random.machine();
        if random.next().is_multiple_of(8) {
            machine.gpr[6] += 1 + random.next() % 15; // misaligned operand
        }
        let budget = 1 + (random.next() % 40) as usize;
        let (cpu, mut first) = load(&machine, &code, MemoryPermissions::READ_WRITE);
        let interpreted = run(false, &cpu, &mut first, budget);
        let (_, mut second) = load(&machine, &code, MemoryPermissions::READ_WRITE);
        let translated = run(true, &cpu, &mut second, budget);
        assert_eq!(
            translated.exit_reason, interpreted.exit_reason,
            "{round}: {text:#?}"
        );
        assert_eq!(
            translated.instructions_executed, interpreted.instructions_executed,
            "{round}: {text:#?}"
        );
        assert_eq!(
            translated.final_state, interpreted.final_state,
            "{round}: {text:#?}"
        );
        assert_eq!(data(&first), data(&second), "{round}: {text:#?}");
    }
}

#[test]
fn vector_shift_counts_use_all_64_low_bits() {
    // The count is the low quadword of the source; its high quadword is ignored.
    for case in cases() {
        let name = case.text.split(' ').next().unwrap();
        let is_shift = ["psll", "psrl", "psra"]
            .iter()
            .any(|prefix| name.starts_with(prefix));
        if !is_shift || !case.text.ends_with("xmm3, xmm9") {
            continue;
        }
        for (low, high) in [
            (1u64 << 32 | 1, 0u64),
            (1 << 63, 0),
            (u64::MAX, 0),
            (3, u64::MAX),
        ] {
            let mut machine = Random(0x41).machine();
            machine.xmm[9][..8].copy_from_slice(&low.to_le_bytes());
            machine.xmm[9][8..].copy_from_slice(&high.to_le_bytes());
            let mut expected = machine.clone();
            reference(&mut expected, case.text);
            for native in [false, true] {
                let (cpu, mut memory) = load(&machine, &case.code, MemoryPermissions::READ_WRITE);
                let result = run(native, &cpu, &mut memory, 1);
                let wanted = expected.xmm.map(u128::from_le_bytes);
                assert_eq!(result.final_state.xmm, wanted, "{} by {low:#x}", case.text);
            }
        }
    }
}
