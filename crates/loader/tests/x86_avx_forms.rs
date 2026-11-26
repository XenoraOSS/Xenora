// SPDX-License-Identifier: GPL-3.0-or-later

//! Results of the VEX forms of SSE to SSE4.2 instructions.
//!
//! `tests/x86/avx_forms.txt` pairs LLVM encodings with Intel-syntax text. Expected
//! values come from the same references as those of the legacy forms, which share
//! nothing with the implementation: `vector_reference.rs` for the integer forms, and
//! the exact fractions of `float_reference.py` for floating point. The rules added
//! here are those of the Intel SDM Vol. 2 operation sections: the first source is
//! VEX.vvvv, the halves of a 256-bit vector are computed alike, and the register is
//! cleared above the result. Compare predicates are judged by their SDM names, which
//! the assembler turned into immediates. `x86_avx.rs` covers prefixes, faults,
//! alignment and the other rules of encodings.

#[path = "x86/vector_reference.rs"]
mod vector_reference;

use std::collections::BTreeMap;
use vector_reference::{Bytes, FLAGS, Machine, reference, string_compare};
use xenora_loader::cpu::{CpuState, ExecutionResult, ExitReason, Interpreter, Mxcsr, RFlags};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};

const ENTRY: u64 = 0x1000;
const DATA: u64 = 0x2000;
/// Address of `[rsi + 4*rdi + 16]` with RSI = DATA and RDI = 4.
const OPERAND: u64 = DATA + 32;
/// Signaling NaNs, as single and as double precision, wherever no lane is read.
const POISON: u128 = 0x7FF0_0001_7FA0_0001_7FF0_0001_7FA0_0001;

type Wide = [u8; 32];

fn fixture() -> BTreeMap<&'static str, Vec<u8>> {
    include_str!("x86/avx_forms.txt")
        .lines()
        .map(|line| {
            let (hex, text) = line.split_once(" | ").unwrap();
            let code = hex
                .split(' ')
                .map(|b| u8::from_str_radix(b, 16).unwrap())
                .collect();
            (text, code)
        })
        .collect()
}

fn is_float(name: &str) -> bool {
    [
        "vadd", "vsub", "vmul", "vdiv", "vmin", "vmax", "vsqrt", "vcmp", "vround", "vcvt", "vcomi",
        "vucomi", "vdp",
    ]
    .iter()
    .any(|stem| name.starts_with(stem))
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
    fn half(&mut self) -> Bytes {
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

    fn wide(&mut self) -> Wide {
        let mut out = [0; 32];
        out[..16].copy_from_slice(&self.half());
        out[16..].copy_from_slice(&self.half());
        // Vectors with nothing set, and with all the same lanes, decide VPTEST.
        match self.next() % 8 {
            0 => out.fill(0),
            1 => out[16..].fill(0),
            2 => out[..16].fill(0),
            3 => out[..16].fill(0xFF),
            4 => out[16..].fill(0xFF),
            5 => out.fill(0xFF),
            _ => {}
        }
        out
    }

    fn state(&mut self) -> State {
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
        let mut ymm = [[0; 32]; 16];
        for register in &mut ymm {
            *register = self.wide();
        }
        State {
            gpr,
            ymm,
            operand: self.wide(),
            flags: FLAGS,
        }
    }
}

#[derive(Clone)]
struct State {
    gpr: [u64; 16],
    ymm: [Wide; 16],
    operand: Wide,
    flags: RFlags,
}

fn halves(value: &Wide) -> [Bytes; 2] {
    [0, 16].map(|at| value[at..at + 16].try_into().unwrap())
}

fn load(state: &State, code: &[u8], mxcsr: u32) -> (CpuState, GuestMemory) {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    memory
        .map_region(DATA, 0x100, MemoryPermissions::READ_WRITE)
        .unwrap();
    memory.write_bytes(OPERAND, &state.operand).unwrap();
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr = state.gpr;
    for (index, register) in state.ymm.iter().enumerate() {
        let [low, high] = halves(register).map(u128::from_le_bytes);
        (cpu.xmm[index], cpu.ymm_high[index]) = (low, high);
    }
    cpu.mxcsr = Mxcsr(mxcsr);
    cpu.flags = state.flags;
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

/// Run in both engines, expecting the instruction to finish in `wanted`.
fn expect(state: &State, wanted: &State, code: &[u8], mxcsr: (u32, u32), what: &str) {
    let (mut after, _) = load(wanted, code, mxcsr.1);
    after.rip += code.len() as u64;
    for native in [false, true] {
        let (cpu, mut memory) = load(state, code, mxcsr.0);
        let result = run(native, &cpu, &mut memory);
        assert_eq!(result.exit_reason, ExitReason::StepLimitExceeded, "{what}");
        assert_eq!(result.final_state, after, "{what} from {cpu:x?}");
        let mut operand = [0; 32];
        memory.read_bytes(OPERAND, &mut operand).unwrap();
        assert_eq!(operand, wanted.operand, "{what}");
    }
}

// --- integer forms ---------------------------------------------------------

fn vector(text: &str) -> Option<(usize, usize)> {
    let (digits, count) = match text.strip_prefix("ymm") {
        Some(digits) => (digits, 2),
        None => (text.strip_prefix("xmm")?, 1),
    };
    Some((digits.parse().ok()?, count))
}

/// The legacy machine for one half of a VEX instruction: XMM3 holds the first source,
/// XMM9 the second and XMM0 the selecting one.
fn machine(state: &State, half: usize, sources: [Option<usize>; 3]) -> Machine {
    let register = |index: Option<usize>| match index {
        Some(index) => halves(&state.ymm[index])[half],
        None => [0xAA; 16],
    };
    let mut xmm = [[0xAA; 16]; 16];
    (xmm[3], xmm[9], xmm[0]) = (
        register(sources[0]),
        register(sources[1]),
        register(sources[2]),
    );
    Machine {
        gpr: state.gpr,
        xmm,
        operand: halves(&state.operand)[half],
        mxcsr: 0,
        flags: state.flags,
    }
}

/// The state after an integer VEX instruction, from the reference of its legacy form.
fn integer(state: &State, text: &str) -> State {
    let mut wanted = state.clone();
    let (name, rest) = text.split_once(' ').unwrap();
    let parts: Vec<&str> = rest.split(", ").collect();
    let legacy = &name[1..];
    let Some((target, count)) = vector(parts[0]) else {
        // A general register or memory from a vector: the same instruction.
        let mut m = machine(state, 0, [None; 3]);
        m.xmm = state.ymm.map(|register| halves(&register)[0]);
        reference(&mut m, &text[1..]);
        wanted.gpr = m.gpr;
        wanted.operand[..16].copy_from_slice(&m.operand);
        return wanted;
    };
    let index = |text: &str| vector(text).map(|(index, _)| index);
    let is_string = legacy.starts_with("pcmpestr") || legacy.starts_with("pcmpistr");
    let registers: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|text| text.parse::<u32>().is_err())
        .collect();
    let imm = parts.last().unwrap().parse::<u32>().ok();
    // Forms that compare the first operand write no vector register themselves.
    let compares = is_string || legacy == "ptest";
    let (first, second) = match registers.len() {
        2 if compares => (parts[0], parts[1]),
        2 => ("", parts[1]),
        _ => (parts[1], parts[2]),
    };
    let mut out = [0; 32];
    let mut flags = Vec::new();
    for half in 0..count {
        let control = registers.get(3).and_then(|text| index(text));
        let by_count = legacy.starts_with("psll") || legacy.starts_with("psr");
        let shifts = by_count && registers.len() == 2;
        let sources = if shifts {
            [index(second), None, None]
        } else {
            [index(first), index(second), control]
        };
        let mut m = machine(state, half, sources);
        // Blends and VSHUFPD use further immediate bits for the high half.
        let imm = imm.map(|imm| match legacy {
            "blendps" => imm >> (4 * half),
            "blendpd" | "shufpd" => imm >> (2 * half),
            _ => imm,
        });
        let source = match index(second) {
            Some(_) => "xmm9".to_string(),
            None => second.replace("ymmword", "xmmword"),
        };
        let line = match imm {
            Some(imm) if shifts => format!("{legacy} xmm3, {imm}"),
            Some(imm) => format!("{legacy} xmm3, {source}, {imm}"),
            None => format!("{legacy} xmm3, {source}"),
        };
        reference(&mut m, &line);
        flags.push(m.flags);
        if is_string {
            wanted.gpr = m.gpr;
            out[..16].copy_from_slice(&m.xmm[0]);
        } else {
            out[16 * half..16 * half + 16].copy_from_slice(&m.xmm[3]);
        }
    }
    if compares {
        wanted.flags = flags[0];
        // VPTEST is decided by the whole vector.
        wanted.flags.zf &= flags[count - 1].zf;
        wanted.flags.cf &= flags[count - 1].cf;
        if legacy.ends_with('m') {
            wanted.ymm[0] = out;
        }
    } else {
        wanted.ymm[target] = out;
    }
    wanted
}

#[test]
fn integer_and_shuffle_forms_match_the_legacy_reference() {
    let mut random = Random(0x1F2E_3D4C_5B6A_7988);
    let (mut checked, mut flags) = (0, std::collections::BTreeSet::new());
    for (text, code) in fixture() {
        let name = text.split(' ').next().unwrap();
        if is_float(name) {
            continue;
        }
        let rounds = if name == "vptest" { 256 } else { 12 };
        for _ in 0..rounds {
            let state = random.state();
            let wanted = integer(&state, text);
            expect(&state, &wanted, &code, (0x1F80, 0x1F80), text);
            if name == "vptest" {
                flags.insert((text.contains("ymm"), wanted.flags.zf, wanted.flags.cf));
            }
        }
        checked += 1;
    }
    assert!(checked > 1400, "{checked}");
    assert_eq!(flags.len(), 8, "{flags:?}");
}

#[test]
fn avx2_signed_multiply_endpoints_wrap_in_both_halves() {
    let fixtures = fixture();
    let mut state = Random(0x711).state();
    // Signed words -32768 fill the low half and +1 fill the high half.
    let input = joined(
        0x8000_8000_8000_8000_8000_8000_8000_8000,
        0x0001_0001_0001_0001_0001_0001_0001_0001,
    );
    state.ymm[9] = input;
    state.ymm[10] = input;
    for (text, result) in [
        (
            "vpmaddwd ymm3, ymm9, ymm10",
            joined(
                0x8000_0000_8000_0000_8000_0000_8000_0000,
                0x0000_0002_0000_0002_0000_0002_0000_0002,
            ),
        ),
        (
            "vpmulhrsw ymm3, ymm9, ymm10",
            joined(0x8000_8000_8000_8000_8000_8000_8000_8000, 0),
        ),
    ] {
        let mut wanted = state.clone();
        wanted.ymm[3] = result;
        expect(&state, &wanted, &fixtures[text], (0x1F80, 0x1F80), text);
    }
}

/// VEX.W selects RAX and RDX over EAX and EDX as the lengths.
#[test]
fn string_lengths_are_64_bit_with_w() {
    let mut random = Random(0x57);
    let code = fixture();
    let (mut shorter, mut checked) = (0, 0);
    for name in ["pcmpestri", "pcmpestrm"] {
        for imm in [0u8, 1, 12, 13, 24, 56, 68, 70, 75, 121] {
            let mut code = code[format!("v{name} xmm3, xmm9, {imm}").as_str()].clone();
            assert_eq!((code[0], code[2] & 0x80), (0xC4, 0));
            code[2] |= 0x80;
            for _ in 0..64 {
                let mut state = random.state();
                for length in [0, 2] {
                    state.gpr[length] = match random.next() % 3 {
                        0 => state.gpr[length] | 1 << 32,
                        1 => (random.next() % 20) | 1 << 40,
                        _ => random.next() % 20,
                    };
                }
                let mut m = machine(&state, 0, [Some(3), Some(9), None]);
                let (first, second) = (m.xmm[3], m.xmm[9]);
                string_compare(&mut m, name, first, second, imm.into(), true);
                let mut wanted = state.clone();
                (wanted.gpr, wanted.flags) = (m.gpr, m.flags);
                if name.ends_with('m') {
                    wanted.ymm[0] = [0; 32];
                    wanted.ymm[0][..16].copy_from_slice(&m.xmm[0]);
                }
                expect(&state, &wanted, &code, (0x1F80, 0x1F80), name);
                let mut short = machine(&state, 0, [Some(3), Some(9), None]);
                string_compare(&mut short, name, first, second, imm.into(), false);
                shorter += usize::from((short.gpr, short.flags) != (m.gpr, m.flags));
                checked += 1;
            }
        }
    }
    assert!(shorter > checked / 10, "{shorter} of {checked}");
}

/// The fourth register is bits 7:4 of the immediate. Bits 3:0 are ignored.
#[test]
fn the_low_bits_of_a_register_immediate_are_ignored() {
    let mut random = Random(0x54);
    let mut checked = 0;
    for (text, mut code) in fixture() {
        if !text.starts_with("vblendv") && !text.starts_with("vpblendvb") {
            continue;
        }
        for low in [1, 8, 15] {
            *code.last_mut().unwrap() |= low;
            let state = random.state();
            expect(&state, &integer(&state, text), &code, (0, 0), text);
            checked += 1;
        }
    }
    assert!(checked > 150, "{checked}");
}

// --- floating point --------------------------------------------------------

/// The compare predicates in immediate order, as SDM Vol. 2A names them in CMPPD.
const PREDICATES: &str = "eq_oq lt_os le_os unord_q neq_uq nlt_us nle_us ord_q eq_uq nge_us \
    ngt_us false_oq neq_oq ge_os gt_os true_uq eq_os lt_oq le_oq unord_s neq_us nlt_uq nle_uq \
    ord_s eq_us nge_uq ngt_uq false_os neq_os ge_oq gt_oq true_us";

#[derive(Clone, Copy, Debug)]
struct Vector {
    name: &'static str,
    /// Compare predicate or rounding immediate.
    code: Option<u8>,
    bits: u32,
    mxcsr: u32,
    a: u64,
    b: u64,
    result: u64,
    flags: u32,
}

/// Committed vectors. The immediate is split off the name.
fn committed() -> Vec<Vector> {
    include_str!("x86/float_vectors.txt")
        .lines()
        .map(|line| {
            let parts: Vec<&'static str> = line.split(' ').collect();
            let hex = |index: usize| u64::from_str_radix(parts[index], 16).unwrap();
            let name = parts[0].trim_end_matches(|c: char| c.is_ascii_digit());
            let source = name.split('2').next().unwrap();
            let bits = match name {
                "cvtsi" | "cvtsi2ssd" | "cvtsi2sdd" => 32,
                "cvtsi2ssq" | "cvtsi2sdq" => 64,
                _ if source.contains("ss") => 32,
                _ => 64,
            };
            Vector {
                name,
                code: parts[0][name.len()..].parse().ok(),
                bits,
                mxcsr: hex(1) as u32,
                a: hex(2),
                b: hex(3),
                result: hex(4),
                flags: hex(5) as u32,
            }
        })
        .collect()
}

/// A compare by the name of its predicate: a relation, then whether it holds for
/// unordered operands, then whether a quiet NaN signals.
fn compared(vector: &Vector) -> Vector {
    let code = usize::from(vector.code.unwrap());
    let name = PREDICATES.split_whitespace().nth(code).unwrap();
    let (relation, kind) = name.split_once('_').unwrap();
    let (top, fraction) = match vector.bits {
        32 => (1u64 << 31, 23),
        _ => (1 << 63, 52),
    };
    let infinity = (top - 1) >> fraction << fraction;
    let zeroed = vector.mxcsr & 1 << 6 != 0;
    let mut flags = 0;
    // The sign and magnitude of each operand, as an integer that orders like it.
    let mut unordered = false;
    let mut invalid = false;
    let values = [vector.a, vector.b].map(|value| {
        let mut magnitude = value % top;
        if magnitude > infinity {
            unordered = true;
            invalid |= magnitude >> (fraction - 1) & 1 == 0 || kind.ends_with('s');
        } else if 0 < magnitude && magnitude < 1 << fraction {
            if zeroed {
                magnitude = 0;
            } else {
                flags = 2;
            }
        }
        let order = i128::from(magnitude);
        if value >= top { -order } else { order }
    });
    if unordered {
        flags = u32::from(invalid);
    }
    let [a, b] = values;
    let holds = match relation {
        "unord" => unordered,
        "ord" => !unordered,
        "true" => true,
        "false" => false,
        _ if unordered => kind.starts_with('u'),
        "eq" => a == b,
        "neq" => a != b,
        "lt" | "nge" => a < b,
        "le" | "ngt" => a <= b,
        "gt" | "nle" => a > b,
        "ge" | "nlt" => a >= b,
        other => panic!("{other}"),
    };
    Vector {
        result: if holds { top | (top - 1) } else { 0 },
        flags,
        ..*vector
    }
}

/// Every vector: the committed ones, with compares for all 32 predicates in place of
/// the eight legacy ones.
fn vectors() -> Vec<Vector> {
    let mut all = Vec::new();
    for vector in committed() {
        if vector.name.starts_with("cmp") {
            for step in 0..4 {
                let code = vector.code.map(|code| code + 8 * step);
                all.push(compared(&Vector { code, ..vector }));
            }
        } else {
            all.push(vector);
        }
    }
    all
}

#[test]
fn the_predicate_reference_agrees_with_the_committed_vectors() {
    let mut checked = 0;
    for vector in committed() {
        if vector.name.starts_with("cmp") {
            let wanted = compared(&vector);
            assert_eq!(
                (wanted.result, wanted.flags),
                (vector.result, vector.flags),
                "{vector:x?}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 640);
}

fn merge(target: &mut Wide, value: u64, bits: u32, index: usize) {
    let bytes = bits as usize / 8;
    target[index * bytes..(index + 1) * bytes].copy_from_slice(&value.to_le_bytes()[..bytes]);
}

fn poisoned() -> State {
    let mut wide = [0; 32];
    wide[..16].copy_from_slice(&POISON.to_le_bytes());
    wide[16..].copy_from_slice(&POISON.to_le_bytes());
    let mut gpr = [0x5A5A_5A5A_5A5A_5A5A; 16];
    (gpr[6], gpr[7]) = (DATA, 4);
    State {
        gpr,
        ymm: [wide; 16],
        operand: wide,
        flags: FLAGS,
    }
}

/// Put `value` where an operand is read from.
fn place(state: &mut State, operand: &str, value: &Wide) {
    match vector(operand) {
        Some((index, _)) => state.ymm[index] = *value,
        None => state.operand = *value,
    }
}

fn memory(bits: u32) -> &'static str {
    match bits {
        32 => "dword ptr [rsi + 4*rdi + 16]",
        64 => "qword ptr [rsi + 4*rdi + 16]",
        128 => "xmmword ptr [rsi + 4*rdi + 16]",
        _ => "ymmword ptr [rsi + 4*rdi + 16]",
    }
}

/// The VEX mnemonic of a scalar vector with its trailing immediate.
fn mnemonic(vector: &Vector) -> (String, String) {
    match vector.code {
        Some(code) if vector.name.starts_with("cmp") => {
            let predicate = PREDICATES.split_whitespace().nth(code.into()).unwrap();
            (
                format!("vcmp{predicate}{}", &vector.name[3..]),
                String::new(),
            )
        }
        Some(code) => (format!("v{}", vector.name), format!(", {code}")),
        None => (format!("v{}", vector.name), String::new()),
    }
}

/// Every scalar form of a vector: encoding, and the states before and after.
fn scalars(fixture: &BTreeMap<&str, Vec<u8>>, vector: &Vector) -> Vec<(Vec<u8>, State, State)> {
    let (name, tail) = mnemonic(vector);
    let base = poisoned();
    let (mut a, mut b) = (base.ymm[0], base.ymm[0]);
    merge(&mut a, vector.a, vector.bits, 0);
    merge(&mut b, vector.b, vector.bits, 0);
    let memory = memory(vector.bits);
    let mut out = Vec::new();
    if vector.name.contains("comi") {
        for (first, second) in [("xmm3", "xmm9"), ("xmm12", memory)] {
            let mut state = base.clone();
            place(&mut state, first, &a);
            place(&mut state, second, &b);
            let mut wanted = state.clone();
            wanted.flags = RFlags {
                zf: vector.result & 4 != 0,
                pf: vector.result & 2 != 0,
                cf: vector.result & 1 != 0,
                ..RFlags::default()
            };
            let text = format!("{name} {first}, {second}");
            out.push((fixture[text.as_str()].clone(), state, wanted));
        }
        return out;
    }
    if vector.name.contains("2si") {
        let (stem, size) = name.split_at(name.len() - 1);
        let register = if size == "d" { "r9d" } else { "r9" };
        for second in ["xmm9", memory] {
            let mut state = base.clone();
            place(&mut state, second, &b);
            let mut wanted = state.clone();
            wanted.gpr[9] = vector.result;
            let text = format!("{stem} {register}, {second}");
            out.push((fixture[text.as_str()].clone(), state, wanted));
        }
        return out;
    }
    // Three operands. The result has the width of the destination format.
    let result = match vector.name {
        "cvtss2sd" | "cvtsi2sdd" | "cvtsi2sdq" => 64,
        "cvtsd2ss" | "cvtsi2ssd" | "cvtsi2ssq" => 32,
        _ => vector.bits,
    };
    let (name, second) = match vector.name.strip_prefix("cvtsi2") {
        Some(_) => {
            let register = if vector.bits == 32 { "r9d" } else { "r9" };
            (name[..name.len() - 1].to_string(), register)
        }
        None => (name, "xmm10"),
    };
    for (target, first, second) in [(3, 9, second), (3, 9, "xmm3"), (12, 1, memory)] {
        let text = format!("{name} xmm{target}, xmm{first}, {second}{tail}");
        let Some(code) = fixture.get(text.as_str()) else {
            assert!(second == "xmm3" && name.starts_with("vcvtsi2"), "{text}");
            continue;
        };
        let mut state = base.clone();
        state.ymm[first] = a;
        if second.starts_with('r') {
            state.gpr[9] = vector.b;
            if vector.bits == 32 {
                state.gpr[9] |= 0xABCD_EF01 << 32;
            }
        } else {
            place(&mut state, second, &b);
        }
        let mut wanted = state.clone();
        let mut value = a;
        value[16..].fill(0);
        merge(&mut value, vector.result, result, 0);
        wanted.ymm[target] = value;
        out.push((code.clone(), state, wanted));
    }
    out
}

#[test]
fn scalar_forms_match_the_vectors_and_keep_the_first_source() {
    let fixture = fixture();
    let mut checked = 0;
    for vector in vectors() {
        let mxcsr = (vector.mxcsr, vector.mxcsr | vector.flags);
        for (mut code, state, wanted) in scalars(&fixture, &vector) {
            let what = format!("{code:02X?} {vector:x?}");
            expect(&state, &wanted, &code, mxcsr, &what);
            if vector.name.starts_with("cmp") {
                // Reserved immediate bits are ignored, which is a local rule.
                *code.last_mut().unwrap() |= 0xE0;
                expect(&state, &wanted, &code, mxcsr, &what);
            }
            checked += 1;
        }
    }
    assert!(checked > 25000, "{checked}");
}

/// Unmask exception `bit` of a vector and expect it to stop the instruction with no
/// effect but the flags. False where the vectors leave its outcome open.
fn stops(state: &State, code: &[u8], mxcsr: u32, flags: u32, bit: u32, what: &str) -> bool {
    // Flush-to-zero applies only while underflow is masked.
    if flags & 1 << bit == 0 || bit == 4 && mxcsr & 1 << 15 != 0 {
        return false;
    }
    let unmasked = mxcsr & !(1 << (bit + 7));
    // Pre-computation exceptions stop before the others are raised.
    let raised = if bit < 3 { flags & 7 } else { flags };
    // The scaled result of an unmasked overflow or underflow may be exact.
    let exact = if bit == 3 || bit == 4 { 1 << 5 } else { 0 };
    for native in [false, true] {
        let (cpu, mut memory) = load(state, code, unmasked);
        let result = run(native, &cpu, &mut memory);
        assert_eq!(result.exit_reason, ExitReason::SimdException, "{what}");
        assert_eq!(result.instructions_executed, 0, "{what}");
        let after = result.final_state.mxcsr.0;
        assert_eq!(after | exact, unmasked | raised | exact, "{what}");
        let mut wanted = cpu.clone();
        wanted.mxcsr = Mxcsr(after);
        assert_eq!(result.final_state, wanted, "{what}");
    }
    true
}

#[test]
fn an_unmasked_exception_stops_every_scalar_form() {
    let fixture = fixture();
    let mut stopped: BTreeMap<&str, u32> = BTreeMap::new();
    for vector in vectors().iter().filter(|vector| vector.flags != 0) {
        for (code, state, _) in scalars(&fixture, vector) {
            for bit in 0..6 {
                let what = format!("{bit} {code:02X?} {vector:x?}");
                if stops(&state, &code, vector.mxcsr, vector.flags, bit, &what) {
                    *stopped.entry(vector.name).or_default() |= 1 << bit;
                }
            }
        }
    }
    let seen = stopped.values().fold(0, |all, bits| all | bits);
    assert_eq!(seen, 0x3F);
    // Conversions of 32-bit integers to double precision are always exact.
    assert_eq!(stopped.len(), 35, "{stopped:x?}");
}

/// A packed instruction and the lanes of its vectors.
struct Packed {
    text: String,
    first: &'static str,
    second: String,
    target: usize,
    source: u32,
    result: u32,
    lanes: usize,
}

/// Every packed form of a scalar vector's operation, for one vector length.
fn packed(vector: &Vector, long: bool) -> Vec<Packed> {
    let (length, v) = if long { (256, "ymm") } else { (128, "xmm") };
    // Name, source and result lane width, and the register names of both.
    let converted = match vector.name {
        "cvtss2sd" => Some(("vcvtps2pd", 32, 64)),
        "cvtsd2ss" => Some(("vcvtpd2ps", 64, 32)),
        "cvtss2sid" => Some(("vcvtps2dq", 32, 32)),
        "cvttss2sid" => Some(("vcvttps2dq", 32, 32)),
        "cvtsd2sid" => Some(("vcvtpd2dq", 64, 32)),
        "cvttsd2sid" => Some(("vcvttpd2dq", 64, 32)),
        "cvtsi2ssd" => Some(("vcvtdq2ps", 32, 32)),
        "cvtsi2sdd" => Some(("vcvtdq2pd", 32, 64)),
        _ => None,
    };
    if let Some((name, source, result)) = converted {
        // The wider side has the vector length.
        let lanes = length / source.max(result);
        let register = |bits: u32| if bits * lanes == 256 { "ymm" } else { "xmm" };
        return [
            (3, format!("{}9", register(source))),
            (12, memory(source * lanes).to_string()),
            (2, format!("{}2", register(source))),
        ]
        .into_iter()
        .map(|(target, second)| Packed {
            text: format!("{name} {}{target}, {second}", register(result)),
            first: "",
            second,
            target,
            source,
            result,
            lanes: lanes as usize,
        })
        .collect();
    }
    if vector.name.contains("cvt") || vector.name.contains("comi") {
        return Vec::new();
    }
    let (name, tail) = mnemonic(vector);
    let stem = &name[..name.len() - 2];
    let kind = if vector.bits == 32 { "ps" } else { "pd" };
    let single = stem == "vsqrt" || stem == "vround";
    let forms: &[(usize, &str, &str)] = if single {
        &[(3, "", "9"), (12, "", ""), (2, "", "2")]
    } else {
        &[(3, "9", "10"), (3, "9", "3"), (3, "3", "5"), (12, "1", "")]
    };
    forms
        .iter()
        .map(|(target, first, second)| {
            let second = match *second {
                "" => memory(length).to_string(),
                index => format!("{v}{index}"),
            };
            let first: &'static str = match *first {
                "" => "",
                "1" => "xmm1",
                "3" => "xmm3",
                _ => "xmm9",
            };
            let middle = match first {
                "" => String::new(),
                name => format!("{v}{}, ", &name[3..]),
            };
            Packed {
                text: format!("{stem}{kind} {v}{target}, {middle}{second}{tail}"),
                first,
                second,
                target: *target,
                source: vector.bits,
                result: vector.bits,
                lanes: (length / vector.bits) as usize,
            }
        })
        .collect()
}

type Groups = BTreeMap<(&'static str, Option<u8>, u32), Vec<Vector>>;

/// Vectors of one operation, immediate and MXCSR.
fn groups() -> Groups {
    let mut groups = Groups::new();
    for vector in vectors() {
        groups
            .entry((vector.name, vector.code, vector.mxcsr))
            .or_default()
            .push(vector);
    }
    groups
}

/// The state before and after a packed form, with one vector in each lane, and the
/// flags of all of them.
fn lanes_of(form: &Packed, chunk: &[Vector]) -> (State, State, u32) {
    let mut state = poisoned();
    let (mut a, mut b, mut out) = (state.ymm[0], state.ymm[0], [0; 32]);
    let mut flags = 0;
    for (index, vector) in chunk.iter().enumerate() {
        merge(&mut a, vector.a, form.source, index);
        merge(&mut b, vector.b, form.source, index);
        merge(&mut out, vector.result, form.result, index);
        flags |= vector.flags;
    }
    if !form.first.is_empty() {
        place(&mut state, form.first, &a);
    }
    place(&mut state, &form.second, &b);
    let mut wanted = state.clone();
    wanted.ymm[form.target] = out;
    (state, wanted, flags)
}

#[test]
fn packed_forms_match_the_vectors_in_every_lane_of_both_lengths() {
    let code = fixture();
    let mut checked = [0, 0];
    for ((_, _, mxcsr), group) in groups() {
        for long in [false, true] {
            for form in packed(&group[0], long) {
                let code = &code[form.text.as_str()];
                // Short groups are repeated to fill the register.
                let count = group.len().div_ceil(form.lanes) * form.lanes;
                let all: Vec<Vector> = group.iter().copied().cycle().take(count).collect();
                for chunk in all.chunks(form.lanes) {
                    let (state, wanted, flags) = lanes_of(&form, chunk);
                    let what = format!("{} {chunk:x?}", form.text);
                    expect(&state, &wanted, code, (mxcsr, mxcsr | flags), &what);
                    checked[usize::from(long)] += 1;
                }
            }
        }
    }
    assert!(checked[0] > 8000 && checked[1] > 4000, "{checked:?}");
}

/// An unmasked exception in any lane leaves the whole destination alone.
#[test]
fn an_unmasked_exception_in_any_lane_stops_every_packed_form() {
    let fixture = fixture();
    let (mut seen, mut lanes_seen) = (0, [0u32; 2]);
    for ((_, _, mxcsr), group) in groups() {
        let quiet = group.iter().find(|vector| vector.flags == 0);
        let loud: Vec<&Vector> = group.iter().filter(|vector| vector.flags != 0).collect();
        let Some(quiet) = quiet else { continue };
        for (at, vector) in loud.iter().enumerate().step_by(3) {
            for long in [false, true] {
                for form in packed(vector, long) {
                    let lane = at % form.lanes;
                    let mut chunk = vec![*quiet; form.lanes];
                    chunk[lane] = **vector;
                    let (state, _, flags) = lanes_of(&form, &chunk);
                    for bit in 0..6 {
                        let what = format!("{bit} {} {chunk:x?}", form.text);
                        let code = &fixture[form.text.as_str()];
                        if stops(&state, code, mxcsr, flags, bit, &what) {
                            seen |= 1 << bit;
                            lanes_seen[usize::from(long)] |= 1 << lane;
                        }
                    }
                }
            }
        }
    }
    assert_eq!((seen, lanes_seen), (0x3F, [0xF, 0xFF]));
}

/// The exceptions of both halves are judged together: those found before computing come
/// first wherever they are, and the flags of both halves are recorded.
#[test]
fn both_halves_raise_their_exceptions_together() {
    let fixture = fixture();
    let code = &fixture["vaddps ymm3, ymm9, ymm10"];
    // 1 + 2^-30 is inexact, infinity minus infinity invalid, 1 + 2^-140 has a denormal
    // operand and, like the first, an inexact sum.
    let inexact = (0x3F80_0000u128, 0x3080_0000u128);
    let invalid = (0x7F80_0000, 0xFF80_0000);
    let denormal = (0x3F80_0000, 0x0000_0200);
    for (low, high, unmasked, flags) in [
        (inexact, invalid, 1 << 7, 1),
        (invalid, inexact, 1 << 7, 1),
        (inexact, invalid, 1 << 12, 0x21),
        (invalid, inexact, 1 << 12, 0x21),
        (inexact, invalid, 1 << 7 | 1 << 12, 1),
        (invalid, denormal, 1 << 7, 3),
        (denormal, invalid, 1 << 8, 3),
        (denormal, invalid, 1 << 7, 3),
        (inexact, denormal, 1 << 8, 2),
        (denormal, inexact, 1 << 12, 0x22),
    ] {
        let mut state = poisoned();
        state.ymm[9] = joined(low.0, high.0);
        state.ymm[10] = joined(low.1, high.1);
        for native in [false, true] {
            let (cpu, mut memory) = load(&state, code, 0x1F80 & !unmasked);
            let result = run(native, &cpu, &mut memory);
            assert_eq!(result.exit_reason, ExitReason::SimdException);
            let mut wanted = cpu.clone();
            wanted.mxcsr = Mxcsr(0x1F80 & !unmasked | flags);
            assert_eq!(
                result.final_state, wanted,
                "{low:x?} {high:x?} {unmasked:x}"
            );
        }
    }
}

struct Dot {
    name: &'static str,
    imm: u8,
    mxcsr: u32,
    a: u128,
    b: u128,
    result: Option<u128>,
    flags: u32,
}

fn dots() -> Vec<Dot> {
    include_str!("x86/dot_vectors.txt")
        .lines()
        .map(|line| {
            let parts: Vec<&str> = line.split(' ').collect();
            let hex = |index: usize| u128::from_str_radix(parts[index], 16).unwrap();
            Dot {
                name: parts[0],
                imm: hex(1) as u8,
                mxcsr: hex(2) as u32,
                a: hex(3),
                b: hex(4),
                result: (parts[5] != "-").then(|| hex(5)),
                flags: hex(6) as u32,
            }
        })
        .collect()
}

fn joined(low: u128, high: u128) -> Wide {
    let mut out = [0; 32];
    out[..16].copy_from_slice(&low.to_le_bytes());
    out[16..].copy_from_slice(&high.to_le_bytes());
    out
}

/// Dot products of both halves, the low one first. An unmasked exception in it stops
/// before the high one raises any.
#[test]
fn dot_products_match_in_both_halves() {
    let fixture = fixture();
    let all = dots();
    let (mut stopped, mut pairs) = ([0; 3], 0);
    let mut check = |name: &str, low: &Dot, high: Option<&Dot>| {
        let v = if high.is_some() { "ymm" } else { "xmm" };
        let (bits, poison) = (if high.is_some() { 256 } else { 128 }, POISON);
        for (target, first, second) in [
            (3, 9, format!("{v}10")),
            (3, 9, format!("{v}3")),
            (12, 1, memory(bits).to_string()),
        ] {
            let text = format!("v{name} {v}{target}, {v}{first}, {second}, 255");
            let mut code = fixture[text.as_str()].clone();
            *code.last_mut().unwrap() = low.imm;
            let mut state = poisoned();
            state.ymm[first] = joined(low.a, high.map_or(poison, |dot| dot.a));
            place(
                &mut state,
                &second,
                &joined(low.b, high.map_or(poison, |dot| dot.b)),
            );
            let flags = match (low.result, high) {
                (Some(_), Some(high)) => low.flags | high.flags,
                _ => low.flags,
            };
            let result = match (low.result, high) {
                (Some(low), Some(high)) => high.result.map(|high| joined(low, high)),
                (low, _) => low.map(|low| joined(low, 0)),
            };
            let what = format!("{text} {:#x}", low.imm);
            if let Some(value) = result {
                let mut wanted = state.clone();
                wanted.ymm[target] = value;
                expect(
                    &state,
                    &wanted,
                    &code,
                    (low.mxcsr, low.mxcsr | flags),
                    &what,
                );
                stopped[0] += 1;
            } else {
                for native in [false, true] {
                    let (cpu, mut memory) = load(&state, &code, low.mxcsr);
                    let result = run(native, &cpu, &mut memory);
                    assert_eq!(result.exit_reason, ExitReason::SimdException, "{what}");
                    let mut wanted = cpu.clone();
                    wanted.mxcsr = Mxcsr(low.mxcsr | flags);
                    assert_eq!(result.final_state, wanted, "{what}");
                }
                stopped[if low.result.is_none() { 1 } else { 2 }] += 1;
            }
        }
    };
    let mut groups: BTreeMap<(u8, u32), Vec<&Dot>> = BTreeMap::new();
    for dot in &all {
        check(dot.name, dot, None);
        if dot.name == "dpps" {
            groups.entry((dot.imm, dot.mxcsr)).or_default().push(dot);
        }
    }
    for group in groups.values() {
        for low in group {
            for high in group {
                check("dpps", low, Some(high));
                pairs += usize::from(!std::ptr::eq(*low, *high));
            }
        }
    }
    assert!(pairs > 500, "{pairs}");
    assert!(stopped.iter().all(|count| *count > 40), "{stopped:?}");
}
