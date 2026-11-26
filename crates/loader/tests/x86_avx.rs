// SPDX-License-Identifier: GPL-3.0-or-later

//! Independent tests of VEX-encoded instructions.
//!
//! `tests/x86/avx.txt` pairs LLVM encodings with Intel-syntax text. The reference
//! below reads only the text and works on byte arrays, following the operation
//! sections of the Intel SDM Vol. 2. It shares nothing with the registry, decoder or
//! production semantics. The forms of `tests/x86/avx_forms.txt` have their results
//! checked in `x86_avx_forms.rs` and take part here in the rules of encodings. Which encodings are invalid is written here from the SDM's
//! opcode tables and Vol. 2A, 2.3 and 2.4.

use std::collections::BTreeSet;
use xenora_loader::cpu::{CpuState, ExecutionResult, ExitReason, Interpreter, Mxcsr, RFlags};
use xenora_loader::dbt::{DbtEngine, is_native};
use xenora_loader::memory::{GuestMemory, MemoryError, MemoryPermissions};
use xenora_loader::x86_decode::{DecodeError, Insn, decode};

const ENTRY: u64 = 0x1000;
const DATA: u64 = 0x2000;
/// Address of `[rsi + 4*rdi + 16]` with RSI = DATA and RDI = 4; 32-byte aligned.
const OPERAND: u64 = DATA + 32;
const FLAGS: RFlags = RFlags {
    cf: true,
    zf: false,
    sf: true,
    of: false,
    pf: true,
    af: false,
};

struct Case {
    code: Vec<u8>,
    text: &'static str,
}

impl Case {
    fn name(&self) -> &'static str {
        self.text.split(' ').next().unwrap()
    }

    fn operands(&self) -> Vec<&'static str> {
        match self.text.split_once(' ') {
            Some((_, rest)) => rest.split(", ").collect(),
            None => Vec::new(),
        }
    }

    fn has_memory(&self) -> bool {
        self.text.contains(" ptr ")
    }

    fn is_store(&self) -> bool {
        self.name() != "vldmxcsr"
            && self
                .operands()
                .first()
                .is_some_and(|text| text.contains(" ptr "))
    }

    fn width(&self) -> u64 {
        let place = self
            .operands()
            .into_iter()
            .find(|text| text.contains(" ptr "));
        width(place.unwrap()) as u64
    }
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
    parse(include_str!("x86/avx.txt"))
}

/// [`cases`] and one case of each mnemonic, operand kinds and vector length of the VEX
/// forms of legacy instructions.
fn all_cases() -> Vec<Case> {
    let mut seen = BTreeSet::new();
    let mut all = cases();
    for case in parse(include_str!("x86/avx_forms.txt")) {
        let kinds: Vec<&str> = case
            .operands()
            .into_iter()
            .map(|text| {
                let end = text.find(|c: char| c.is_ascii_digit() || c == ' ');
                &text[..end.unwrap_or(text.len())]
            })
            .collect();
        // VEXTRACTPS and its like use ebx and r9d.
        if seen.insert((case.name(), kinds)) {
            all.push(case);
        }
    }
    assert!(all.len() > 1000, "{}", all.len());
    all
}

fn at(code: &[u8]) -> Result<Insn, DecodeError<()>> {
    decode(ENTRY, |address| {
        code.get((address - ENTRY) as usize).copied().ok_or(())
    })
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn bytes(&mut self) -> [u8; 32] {
        let mut out = [0; 32];
        for byte in &mut out {
            *byte = self.next() as u8;
        }
        out
    }

    fn machine(&mut self) -> Machine {
        let mut gpr = [0; 16];
        for register in &mut gpr {
            *register = self.next();
        }
        (gpr[6], gpr[7]) = (DATA, 4);
        let mut ymm = [[0; 32]; 16];
        for register in &mut ymm {
            *register = self.bytes();
        }
        let mut operand = self.bytes();
        // Reserved MXCSR bits of a load are covered by a test of their own.
        operand[2..4].fill(0);
        Machine {
            gpr,
            ymm,
            operand,
            // Every exception is masked.
            mxcsr: (self.next() & 0xFFFF) as u32 | 0x1F80,
        }
    }
}

#[derive(Clone)]
struct Machine {
    gpr: [u64; 16],
    ymm: [[u8; 32]; 16],
    operand: [u8; 32],
    mxcsr: u32,
}

// --- reference -------------------------------------------------------------

fn width(text: &str) -> usize {
    match text.split(' ').next().unwrap() {
        "ymmword" => 32,
        "xmmword" => 16,
        "qword" => 8,
        "dword" => 4,
        "word" => 2,
        "byte" => 1,
        other => panic!("width {other}"),
    }
}

/// Index and length of a vector register.
fn vector(text: &str) -> Option<(usize, usize)> {
    let (digits, length) = match text.strip_prefix("ymm") {
        Some(digits) => (digits, 32),
        None => (text.strip_prefix("xmm")?, 16),
    };
    Some((digits.parse().ok()?, length))
}

fn general(text: &str) -> Option<(usize, usize)> {
    Some(match text {
        "eax" => (0, 4),
        "ebx" => (3, 4),
        "r9d" => (9, 4),
        "r10d" => (10, 4),
        "r9" => (9, 8),
        _ => return None,
    })
}

/// The bytes of a vector register, memory or general register operand.
fn bytes(m: &Machine, text: &str) -> Vec<u8> {
    if let Some((index, length)) = vector(text) {
        m.ymm[index][..length].to_vec()
    } else if let Some((index, length)) = general(text) {
        m.gpr[index].to_le_bytes()[..length].to_vec()
    } else {
        m.operand[..width(text)].to_vec()
    }
}

/// Write a destination. A vector register is cleared above what is written, a
/// general register above its 32 bits, and memory keeps the rest.
fn store(m: &mut Machine, text: &str, value: &[u8]) {
    if let Some((index, _)) = vector(text) {
        m.ymm[index] = [0; 32];
        m.ymm[index][..value.len()].copy_from_slice(value);
    } else if let Some((index, _)) = general(text) {
        let mut wide = [0; 8];
        wide[..value.len()].copy_from_slice(value);
        m.gpr[index] = u64::from_le_bytes(wide);
    } else {
        m.operand[..value.len()].copy_from_slice(value);
    }
}

fn bitwise(a: &[u8], b: &[u8], f: impl Fn(u8, u8) -> u8) -> Vec<u8> {
    a.iter().zip(b).map(|(x, y)| f(*x, *y)).collect()
}

/// Elements of `size` bytes of `source`, picked by position for each result.
fn pick(source: &[u8], size: usize, positions: impl IntoIterator<Item = usize>) -> Vec<u8> {
    positions
        .into_iter()
        .flat_map(|position| source[position * size..(position + 1) * size].to_vec())
        .collect()
}

fn reference(m: &mut Machine, text: &str) {
    let Some((name, rest)) = text.split_once(' ') else {
        // VZEROUPPER keeps the low halves, VZEROALL nothing.
        for register in &mut m.ymm {
            register[if text == "vzeroall" { 0 } else { 16 }..].fill(0);
        }
        return;
    };
    let parts: Vec<&str> = rest.split(", ").collect();
    let destination = parts[0];
    let imm = parts.last().unwrap().parse::<usize>().ok();
    let sources: Vec<Vec<u8>> = parts[1..]
        .iter()
        .filter(|text| text.parse::<usize>().is_err())
        .map(|text| bytes(m, text))
        .collect();
    let length = vector(destination).map_or(0, |(_, length)| length);
    let lanes = length / 16;
    let value = match name {
        "vldmxcsr" => {
            m.mxcsr = u32::from_le_bytes(m.operand[..4].try_into().unwrap());
            return;
        }
        "vstmxcsr" => m.mxcsr.to_le_bytes().to_vec(),
        "vandps" | "vandpd" | "vpand" => bitwise(&sources[0], &sources[1], |x, y| x & y),
        "vandnps" | "vandnpd" | "vpandn" => bitwise(&sources[0], &sources[1], |x, y| (255 - x) & y),
        "vorps" | "vorpd" | "vpor" => bitwise(&sources[0], &sources[1], |x, y| x | y),
        "vxorps" | "vxorpd" | "vpxor" => bitwise(&sources[0], &sources[1], |x, y| x ^ y),
        "vmovss" | "vmovsd" => {
            let size = if name == "vmovss" { 4 } else { 8 };
            if sources.len() == 2 {
                [&sources[1][..size], &sources[0][size..]].concat()
            } else if destination.contains(" ptr ") {
                sources[0][..size].to_vec()
            } else {
                sources[0].clone()
            }
        }
        "vmovd" | "vmovq" => {
            let size = if name == "vmovd" { 4 } else { 8 };
            sources[0][..size].to_vec()
        }
        "vmovlps" | "vmovlpd" | "vmovhps" | "vmovhpd" => {
            let high = name.starts_with("vmovh");
            if sources.len() == 1 {
                sources[0][if high { 8..16 } else { 0..8 }].to_vec()
            } else if high {
                [&sources[0][..8], &sources[1][..8]].concat()
            } else {
                [&sources[1][..8], &sources[0][8..]].concat()
            }
        }
        "vmovhlps" => [&sources[1][8..], &sources[0][8..]].concat(),
        "vmovlhps" => [&sources[0][..8], &sources[1][..8]].concat(),
        "vmovmskps" | "vmovmskpd" => {
            let size = if name == "vmovmskps" { 4 } else { 8 };
            let mask: u32 = sources[0]
                .chunks(size)
                .enumerate()
                .filter(|(_, lane)| lane[size - 1] >= 128)
                .map(|(index, _)| 1 << index)
                .sum();
            mask.to_le_bytes().to_vec()
        }
        "vmovsldup" => pick(&sources[0], 4, (0..length / 4).map(|lane| lane / 2 * 2)),
        "vmovshdup" => pick(&sources[0], 4, (0..length / 4).map(|lane| lane / 2 * 2 + 1)),
        "vmovddup" => pick(&sources[0], 8, (0..length / 8).map(|lane| lane / 2 * 2)),
        "vbroadcastss" | "vbroadcastsd" | "vbroadcastf128" => {
            sources[0].repeat(length / sources[0].len())
        }
        "vinsertf128" => {
            let (low, high) = sources[0].split_at(16);
            match imm.unwrap() % 2 {
                0 => [&sources[1][..], high].concat(),
                _ => [low, &sources[1][..]].concat(),
            }
        }
        "vextractf128" => sources[0]
            .chunks(16)
            .nth(imm.unwrap() % 2)
            .unwrap()
            .to_vec(),
        "vperm2f128" => {
            let halves = [sources[0].chunks(16), sources[1].chunks(16)]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            [imm.unwrap() % 16, imm.unwrap() / 16]
                .into_iter()
                .flat_map(|control| match control >= 8 {
                    true => vec![0; 16],
                    false => halves[control % 4].to_vec(),
                })
                .collect()
        }
        "vpermilps" => {
            let per = |index: usize| match imm {
                Some(imm) => imm / 4usize.pow(index as u32 % 4) % 4,
                None => usize::from(sources[1][4 * index]) % 4,
            };
            pick(
                &sources[0],
                4,
                (0..4 * lanes).map(|index| index / 4 * 4 + per(index)),
            )
        }
        "vpermilpd" => {
            let per = |index: usize| match imm {
                Some(imm) => imm / 2usize.pow(index as u32) % 2,
                None => usize::from(sources[1][8 * index]) / 2 % 2,
            };
            pick(
                &sources[0],
                8,
                (0..2 * lanes).map(|index| index / 2 * 2 + per(index)),
            )
        }
        // Whole moves.
        "vmovaps" | "vmovapd" | "vmovups" | "vmovupd" | "vmovdqa" | "vmovdqu" | "vlddqu"
        | "vmovntdqa" | "vmovntps" | "vmovntpd" | "vmovntdq" => sources[0].clone(),
        other => panic!("{other}"),
    };
    store(m, destination, &value);
}

// --- harness ---------------------------------------------------------------

fn load(machine: &Machine, code: &[u8], permissions: MemoryPermissions) -> (CpuState, GuestMemory) {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    let mut data = vec![0xEE; 0x100];
    data[32..64].copy_from_slice(&machine.operand);
    memory
        .map_region_with_data(DATA, 0x100, permissions, &data)
        .unwrap();
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr = machine.gpr;
    for (index, register) in machine.ymm.iter().enumerate() {
        cpu.xmm[index] = u128::from_le_bytes(register[..16].try_into().unwrap());
        cpu.ymm_high[index] = u128::from_le_bytes(register[16..].try_into().unwrap());
    }
    cpu.mxcsr = Mxcsr(machine.mxcsr);
    cpu.flags = FLAGS;
    (cpu, memory)
}

const READ_WRITE: MemoryPermissions = MemoryPermissions::READ_WRITE;

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

/// Exit reason, state with RIP from the end of the instruction, and memory.
fn outcome(
    native: bool,
    cpu: &CpuState,
    code: &[u8],
    operand: &[u8; 32],
) -> (ExitReason, CpuState, Vec<u8>) {
    let machine = Machine {
        gpr: cpu.gpr,
        ymm: [[0; 32]; 16],
        operand: *operand,
        mxcsr: 0,
    };
    let (_, mut memory) = load(&machine, code, READ_WRITE);
    let result = run(native, cpu, &mut memory, 1);
    let mut state = result.final_state;
    state.rip = state.rip.wrapping_sub(code.len() as u64);
    (result.exit_reason, state, data(&memory))
}

/// Operands that must be 32- or 16-byte aligned, by the instruction pages of SDM Vol. 2.
fn needs_alignment(name: &str) -> bool {
    [
        "vmovaps",
        "vmovapd",
        "vmovdqa",
        "vmovntps",
        "vmovntpd",
        "vmovntdq",
        "vmovntdqa",
    ]
    .contains(&name)
}

#[test]
fn every_form_matches_the_reference_in_both_engines() {
    let mut random = Random(0xA5A5_5A5A_1234_5678);
    for case in cases() {
        let text = case.text;
        let insn = at(&case.code).unwrap_or_else(|error| panic!("{text}: {error:?}"));
        assert!(!is_native(&insn), "{text}");
        assert_eq!(insn.name().split('_').next(), Some(&*insn.name()), "{text}");
        assert!(insn.name().starts_with('v'), "{text}: {}", insn.name());
        let mut engine = DbtEngine::new(64 * 1024).unwrap();
        for _ in 0..48 {
            let machine = random.machine();
            let mut expected = machine.clone();
            reference(&mut expected, text);
            let (mut wanted, _) = load(&expected, &case.code, READ_WRITE);
            wanted.rip = ENTRY + case.code.len() as u64;
            let (cpu, mut memory) = load(&machine, &case.code, READ_WRITE);
            let interpreted = Interpreter::new(cpu.clone(), &mut memory, 1).run();
            let (mut dbt_cpu, mut dbt_memory) = load(&machine, &case.code, READ_WRITE);
            let translated = engine.run(&mut dbt_cpu, &mut dbt_memory, 1);
            for (result, memory) in [(interpreted, memory), (translated, dbt_memory)] {
                assert_eq!(result.exit_reason, ExitReason::StepLimitExceeded, "{text}");
                assert_eq!(result.final_state, wanted, "{text} from {cpu:x?}");
                let mut bytes = vec![0xEE; 0x100];
                bytes[32..64].copy_from_slice(&expected.operand);
                assert_eq!(data(&memory), bytes, "{text}");
            }
        }
        assert!(engine.metrics.interpreter_fallbacks > 0, "{text}");
    }
}

/// SDM Vol. 1, 14.1: legacy SSE instructions leave bits 255:128 alone.
#[test]
fn legacy_instructions_keep_the_upper_halves() {
    let mut random = Random(0x001E_6AC7);
    let fixtures = [
        include_str!("x86/vector.txt"),
        include_str!("x86/float.txt"),
    ];
    let mut checked = 0;
    for case in fixtures.into_iter().flat_map(parse) {
        let mut machine = random.machine();
        machine.mxcsr |= 0x1F80;
        for native in [false, true] {
            let (cpu, mut memory) = load(&machine, &case.code, READ_WRITE);
            let result = run(native, &cpu, &mut memory, 1);
            assert_eq!(
                result.exit_reason,
                ExitReason::StepLimitExceeded,
                "{}",
                case.text
            );
            assert_eq!(result.final_state.ymm_high, cpu.ymm_high, "{}", case.text);
        }
        checked += 1;
    }
    assert!(checked > 1500, "{checked}");
}

#[test]
fn a_new_processor_has_zero_upper_halves() {
    assert_eq!(CpuState::new(ENTRY, 0).ymm_high, [0; 16]);
}

/// The VEX prefix of an encoding: the offsets of the bytes holding R and vvvv, and
/// whether it has three bytes.
fn layout(code: &[u8]) -> (usize, bool) {
    match code[0] {
        0xC5 => (1, false),
        0xC4 => (2, true),
        other => panic!("{other:x}"),
    }
}

/// The same instruction with the three-byte prefix and the given W.
fn three_byte(code: &[u8], wide: bool) -> Vec<u8> {
    let w = if wide { 0x80 } else { 0 };
    let mut out = if code[0] == 0xC5 {
        // R moves to the second byte; X and B are absent, so their inverses are one.
        let mut out = vec![0xC4, code[1] & 0x80 | 0x61, code[1] & 0x7F];
        out.extend(&code[2..]);
        out
    } else {
        code.to_vec()
    };
    out[2] = out[2] & 0x7F | w;
    out
}

/// Forms with a fixed W, from the SDM opcode columns (.W0).
fn fixed_w0(name: &str) -> bool {
    name.starts_with("vbroadcast")
        || name.starts_with("vpermil")
        || name.starts_with("vblendv")
        || ["vinsertf128", "vextractf128", "vperm2f128", "vpblendvb"].contains(&name)
}

/// Forms where W is the size of a general operand, or of the lengths of a string.
fn sized_by_w(case: &Case) -> bool {
    let name = case.name();
    let general = case.text.contains(" r") || case.text.contains(" e");
    name == "vmovd"
        || name == "vmovq" && general
        || name.starts_with("vcvtsi2")
        || name.ends_with("2si")
        || name.starts_with("vpcmpestr")
        || ["vpinsrd", "vpinsrq", "vpextrd", "vpextrq"].contains(&name)
}

/// Scalar floating point, whose forms ignore VEX.L.
fn is_scalar(name: &str) -> bool {
    let scalar = name.ends_with("ss") || name.ends_with("sd");
    scalar && !name.starts_with("vp") && name != "vbroadcastss" && name != "vbroadcastsd"
        || name.ends_with("2si")
        || name.starts_with("vcvtsi2")
}

#[test]
fn the_three_byte_prefix_and_an_ignored_w_change_nothing() {
    let mut random = Random(0x3B17);
    let mut ignored = 0;
    for case in all_cases() {
        let name = case.name();
        let machine = random.machine();
        let (cpu, _) = load(&machine, &case.code, READ_WRITE);
        for native in [false, true] {
            let plain = outcome(native, &cpu, &case.code, &machine.operand);
            assert_eq!(plain.0, ExitReason::StepLimitExceeded, "{}", case.text);
            // A W that is set selects the operand size.
            if case.code[0] == 0xC5 || case.code[2] & 0x80 == 0 {
                let long = three_byte(&case.code, false);
                assert_eq!(
                    outcome(native, &cpu, &long, &machine.operand),
                    plain,
                    "{}",
                    case.text
                );
            }
            if fixed_w0(name) {
                let wide = three_byte(&case.code, true);
                let (exit, state, _) = outcome(native, &cpu, &wide, &machine.operand);
                assert!(
                    matches!(exit, ExitReason::IllegalInstruction(_)),
                    "{}",
                    case.text
                );
                assert_eq!(state.xmm, cpu.xmm);
            } else if !sized_by_w(&case) {
                let wide = three_byte(&case.code, true);
                assert_eq!(
                    outcome(native, &cpu, &wide, &machine.operand),
                    plain,
                    "{}",
                    case.text
                );
                ignored += 1;
            }
        }
    }
    assert!(ignored > 1500, "{ignored}");
}

fn refused(code: &[u8], what: &str) {
    let padded = [code, &[0x90; 12][..]].concat();
    let machine = Random(0x0DD).machine();
    for native in [false, true] {
        let (cpu, mut memory) = load(&machine, &padded, READ_WRITE);
        let result = run(native, &cpu, &mut memory, 1);
        assert!(
            matches!(result.exit_reason, ExitReason::IllegalInstruction(_)),
            "{what} {code:02X?}: {:?}",
            result.exit_reason
        );
        assert_eq!(result.final_state, cpu, "{what}");
    }
}

/// SDM Vol. 2A, 2.3.2 to 2.3.4: LOCK, 66, F2, F3 or REX before VEX is an invalid
/// opcode, also with a segment override in between.
#[test]
fn prefixes_before_vex_are_invalid() {
    for case in all_cases() {
        for prefix in [0x66, 0xF2, 0xF3, 0xF0].into_iter().chain(0x40..=0x4F) {
            refused(&[&[prefix][..], &case.code].concat(), case.text);
        }
    }
    let code = [0xC5, 0xF8, 0x28, 0xC1]; // vmovaps xmm0, xmm1
    for prefixes in [
        [0x66, 0x65],
        [0x65, 0x66],
        [0xF3, 0x2E],
        [0x2E, 0xF0],
        [0x65, 0x48],
        [0x48, 0x65],
    ] {
        refused(&[&prefixes[..], &code].concat(), "two prefixes");
    }
}

/// VEX.vvvv must be 1111b unless the form has a source there (SDM Vol. 2A, 2.3.5.6).
#[test]
fn an_unused_vvvv_must_be_all_ones() {
    let (mut unused, mut used) = (0, 0);
    for case in all_cases() {
        let (position, _) = layout(&case.code);
        let registers = case
            .operands()
            .iter()
            .filter(|text| text.parse::<u32>().is_err())
            .count();
        // A shift by an immediate writes the register of VEX.vvvv.
        let shifts = case.name().starts_with("vpsll") || case.name().starts_with("vpsr");
        for vvvv in 0..15u8 {
            let mut code = case.code.clone();
            code[position] = code[position] & !0x78 | vvvv << 3;
            if registers < 3 && !shifts {
                refused(&code, case.text);
                unused += 1;
            } else {
                assert!(at(&code).is_ok(), "{}", case.text);
                used += 1;
            }
        }
    }
    assert!(unused > 2000 && used > 8000, "{unused} {used}");
}

/// Forms defined for one vector length only, from the SDM opcode columns. For AVX,
/// the packed integer forms are 128-bit.
#[test]
fn a_vector_length_without_a_form_is_invalid() {
    let only_128 = [
        "vmovd",
        "vmovq",
        "vmovlps",
        "vmovlpd",
        "vmovhps",
        "vmovhpd",
        "vmovhlps",
        "vmovlhps",
        "vmovntdqa",
        "vldmxcsr",
        "vstmxcsr",
        "vinsertps",
        "vextractps",
        "vmpsadbw",
        "vdppd",
    ];
    let only_256 = [
        "vbroadcastsd",
        "vbroadcastf128",
        "vinsertf128",
        "vextractf128",
        "vperm2f128",
    ];
    let cases = all_cases();
    let wide_integer_forms: BTreeSet<_> = cases
        .iter()
        .filter(|case| case.text.contains("ymm"))
        .map(Case::name)
        .collect();
    for case in &cases {
        let (position, _) = layout(&case.code);
        let mut code = case.code.clone();
        code[position] ^= 4;
        let name = case.name();
        let integer = name.starts_with("vp")
            && !name.starts_with("vperm")
            && !wide_integer_forms.contains(name);
        if integer || only_128.contains(&name) || only_256.contains(&name) {
            refused(&code, case.text);
        } else {
            assert!(at(&code).is_ok(), "{}", case.text);
        }
    }
}

/// Scalar forms ignore VEX.L.
#[test]
fn scalar_forms_ignore_the_length() {
    let mut random = Random(0x716);
    let mut checked = 0;
    for case in all_cases() {
        if !is_scalar(case.name()) {
            continue;
        }
        checked += 1;
        let machine = random.machine();
        let (cpu, _) = load(&machine, &case.code, READ_WRITE);
        let mut long = case.code.clone();
        long[layout(&case.code).0] |= 4;
        for native in [false, true] {
            assert_eq!(
                outcome(native, &cpu, &long, &machine.operand),
                outcome(native, &cpu, &case.code, &machine.operand),
                "{}",
                case.text
            );
        }
    }
    assert!(checked > 150, "{checked}");
}

#[test]
fn undeclared_vex_encodings_are_rejected() {
    let rejected: &[&[u8]] = &[
        &[0xC4, 0xE0, 0x78, 0x28, 0xC1],       // map 0
        &[0xC4, 0xE4, 0x78, 0x28, 0xC1],       // map 4
        &[0xC4, 0xE5, 0x78, 0x28, 0xC1],       // map 5, whose low bits are those of map 1
        &[0xC4, 0xF1, 0x78, 0x28, 0xC1],       // map 17
        &[0xC4, 0xFF, 0x78, 0x28, 0xC1],       // map 31
        &[0xC5, 0xF8, 0x53, 0xC1],             // vrcpps (not implemented)
        &[0xC5, 0xFB, 0x7C, 0xC1],             // vhaddps (not implemented)
        &[0xC4, 0xE2, 0x79, 0x0E, 0xC1],       // vtestps (not implemented)
        &[0xC4, 0xE2, 0x71, 0x2C, 0x01],       // vmaskmovps (not implemented)
        &[0xC5, 0xF9, 0xF7, 0xC1],             // vmaskmovdqu (not implemented)
        &[0xC4, 0xE2, 0x71, 0xDC, 0xC2],       // vaesenc
        &[0xC4, 0xE3, 0x71, 0x44, 0xC2, 0],    // vpclmulqdq
        &[0xC5, 0xFD, 0x70, 0xC1, 0x1B],       // vpshufd ymm (AVX2)
        &[0xC5, 0xF5, 0x71, 0xF1, 0x01],       // vpsllw ymm by an immediate (AVX2)
        &[0xC5, 0xFD, 0xD7, 0xC1],             // vpmovmskb ymm (AVX2)
        &[0xC4, 0xE3, 0x75, 0x4C, 0xC2, 0x30], // vpblendvb ymm (AVX2)
        &[0xC5, 0xF1, 0x71, 0x31, 0x01],       // vpsllw of memory
        &[0xC5, 0xF1, 0x71, 0xC9, 0x01],       // 0F 71 /1
        &[0xC5, 0xF9, 0xC5, 0x01, 0x01],       // vpextrw from memory
        &[0xC5, 0xF9, 0xD7, 0x01],             // vpmovmskb from memory
        &[0xC4, 0xE2, 0x79, 0x10, 0xC1],       // the legacy opcode of pblendvb
        &[0xC4, 0xE2, 0x79, 0x14, 0xC1],       // the legacy opcode of blendvps
        &[0xC5, 0xF0, 0x2A, 0xC1],             // cvtpi2ps
        &[0xC5, 0xF8, 0x2D, 0xC1],             // cvtps2pi
        &[0xC5, 0xF1, 0x73, 0xE1, 0x01],       // 0F 73 /4
        &[0xC5, 0xFA, 0x28, 0xC1],             // F3 0F 28
        &[0xC5, 0xF8, 0x2B, 0xC1],             // vmovntps with a register
        &[0xC5, 0xF9, 0xE7, 0xC1],             // vmovntdq with a register
        &[0xC5, 0xFB, 0xF0, 0xC1],             // vlddqu with a register
        &[0xC5, 0xF8, 0x13, 0xC1],             // vmovlps store with a register
        &[0xC5, 0xF9, 0x12, 0xC1],             // vmovlpd with a register
        &[0xC5, 0xF8, 0x50, 0x01],             // vmovmskps from memory
        &[0xC5, 0xF8, 0xAE, 0xD1],             // vldmxcsr with a register
        &[0xC5, 0xF8, 0xAE, 0x01],             // 0F AE /0
        &[0xC5, 0xFC, 0xAE, 0x11],             // vldmxcsr with VEX.L
        &[0xC5, 0xF9, 0x77],                   // 66 0F 77
        &[0xC4, 0xE2, 0x79, 0x18, 0xC1],       // vbroadcastss from a register (AVX2)
        &[0xC4, 0xE2, 0x7D, 0x19, 0xC1],       // vbroadcastsd from a register (AVX2)
        &[0xC4, 0xE2, 0x7D, 0x1A, 0xC1],       // vbroadcastf128 from a register
        &[0xC4, 0xE2, 0x7D, 0x2A, 0x01],       // vmovntdqa ymm (AVX2)
        &[0xC4, 0xE3, 0x7D, 0x39, 0xC1, 0x00], // vextracti128 (AVX2)
        &[0xC4, 0xE2, 0x71, 0x98, 0xC2],       // vfmadd132ps
        &[0xC4, 0xE2, 0x70, 0xF2, 0xC2],       // andn
        &[0x62, 0xF1, 0x7C, 0x48, 0x28, 0xC1], // EVEX
        &[0x8F, 0xE8, 0x78, 0xC2, 0xC1, 0x00], // XOP
    ];
    for code in rejected {
        refused(code, "undeclared");
    }
}

#[test]
fn decoding_fetches_exactly_the_instruction() {
    for case in all_cases() {
        let insn = at(&case.code).unwrap();
        assert_eq!(usize::from(insn.len), case.code.len(), "{}", case.text);
        for length in 1..case.code.len() {
            assert!(
                matches!(at(&case.code[..length]), Err(DecodeError::Fetch(()))),
                "{} cut to {length}",
                case.text
            );
        }
    }
    // Segment overrides count towards the limit of 15 bytes.
    let code = [0xC5, 0xF8, 0x28, 0x83, 1, 0, 0, 0]; // vmovaps xmm0, [rbx + 1]
    let long = [&[0x65; 7][..], &code].concat();
    assert_eq!(at(&long).unwrap().len, 15);
    assert!(matches!(
        at(&[&[0x65][..], &long].concat()),
        Err(DecodeError::TooLong)
    ));
}

/// Every extension bit of both prefixes names the register it should.
#[test]
fn extension_bits_name_every_register() {
    let mut random = Random(0x9E6);
    for destination in 0..16u8 {
        for source in 0..16u8 {
            // vorps ymm(destination), ymm(source), ymm(rm)
            let rm = (destination * 7 + source * 3 + 1) % 16;
            let r = if destination < 8 { 0x80 } else { 0 };
            let b = if rm < 8 { 0x20 } else { 0 };
            let code = [
                0xC4,
                r | 0x40 | b | 1,
                (15 - source) << 3 | 4,
                0x56,
                0xC0 | (destination & 7) << 3 | rm & 7,
            ];
            let machine = random.machine();
            let mut expected = machine.clone();
            let text = format!("vorps ymm{destination}, ymm{source}, ymm{rm}");
            reference(&mut expected, &text);
            let (wanted, _) = load(&expected, &code, READ_WRITE);
            for native in [false, true] {
                let (cpu, mut memory) = load(&machine, &code, READ_WRITE);
                let result = run(native, &cpu, &mut memory, 1);
                assert_eq!(result.final_state.xmm, wanted.xmm, "{text}");
                assert_eq!(result.final_state.ymm_high, wanted.ymm_high, "{text}");
            }
        }
    }
    // Base and index registers, with and without the extension bits.
    for (code, base, index, scale, disp) in [
        (
            &[0xC4u8, 0x81, 0x7C, 0x10, 0x4C, 0x6C, 0xF8][..],
            12,
            Some(13),
            2,
            -8i64,
        ),
        (
            &[0xC4, 0xA1, 0x7C, 0x10, 0x4C, 0x25, 0x00],
            5,
            Some(12),
            1,
            0,
        ),
        (&[0xC4, 0xC1, 0x7C, 0x10, 0x09], 9, None, 0, 0),
        (&[0xC5, 0xFC, 0x10, 0x4C, 0x24, 0x10], 4, None, 0, 16),
    ] {
        let mut machine = random.machine();
        machine.gpr.fill(0);
        let offset = index.map_or(0, |_| 0x10u64);
        machine.gpr[base] = OPERAND
            .wrapping_sub(offset * scale)
            .wrapping_sub(disp as u64);
        if let Some(index) = index {
            machine.gpr[index] = offset;
        }
        for native in [false, true] {
            let (cpu, mut memory) = load(&machine, code, READ_WRITE);
            let result = run(native, &cpu, &mut memory, 1);
            assert_eq!(
                result.exit_reason,
                ExitReason::StepLimitExceeded,
                "{code:02X?}"
            );
            let low = u128::from_le_bytes(machine.operand[..16].try_into().unwrap());
            let high = u128::from_le_bytes(machine.operand[16..].try_into().unwrap());
            assert_eq!(result.final_state.xmm[1], low, "{code:02X?}");
            assert_eq!(result.final_state.ymm_high[1], high, "{code:02X?}");
        }
    }
}

#[test]
fn a_rip_relative_operand_counts_the_immediate() {
    // vpermilps ymm1, [rip + displacement], 0x1B: ten bytes.
    let displacement = (OPERAND as i64 - (ENTRY as i64 + 10)) as i32;
    let mut code = vec![0xC4, 0xE3, 0x7D, 0x04, 0x0D];
    code.extend(displacement.to_le_bytes());
    code.push(0x1B);
    let machine = Random(0x717).machine();
    let mut expected = machine.clone();
    reference(
        &mut expected,
        "vpermilps ymm1, ymmword ptr [rsi + 4*rdi + 16], 27",
    );
    let (wanted, _) = load(&expected, &code, READ_WRITE);
    for native in [false, true] {
        let (cpu, mut memory) = load(&machine, &code, READ_WRITE);
        let result = run(native, &cpu, &mut memory, 1);
        assert_eq!(result.final_state.xmm, wanted.xmm);
        assert_eq!(result.final_state.ymm_high, wanted.ymm_high);
    }
}

#[test]
fn only_the_listed_operands_must_be_aligned() {
    let mut random = Random(0xA116);
    let mut seen = [0, 0];
    for case in all_cases().into_iter().filter(Case::has_memory) {
        let aligned = needs_alignment(case.name());
        for offset in [1, 2, 4, 8, 16, 24, 31] {
            let mut machine = random.machine();
            machine.gpr[6] = DATA + offset;
            let (cpu, mut memory) = load(&machine, &case.code, READ_WRITE);
            if case.name() == "vldmxcsr" {
                // Keep the reserved bits of the moved operand clear.
                memory
                    .write_bytes(OPERAND + offset, &[0x80, 0x1F, 0, 0])
                    .unwrap();
            }
            let start = data(&memory);
            let place = 32 + offset as usize;
            // The same bytes at an aligned address give the result.
            let mut plain = cpu.clone();
            plain.gpr[6] = DATA;
            let operand: [u8; 32] = start[place..place + 32].try_into().unwrap();
            let (exit, mut wanted, after) = outcome(false, &plain, &case.code, &operand);
            assert_eq!(exit, ExitReason::StepLimitExceeded, "{}", case.text);
            wanted.gpr[6] = machine.gpr[6];
            wanted.rip += case.code.len() as u64;
            let fails = aligned && !(OPERAND + offset).is_multiple_of(case.width());
            for native in [false, true] {
                memory.write_bytes(DATA, &start).unwrap();
                let result = run(native, &cpu, &mut memory, 1);
                if fails {
                    assert_eq!(
                        result.exit_reason,
                        ExitReason::GeneralProtection,
                        "{}",
                        case.text
                    );
                    assert_eq!(result.final_state, cpu, "{}", case.text);
                    assert_eq!(data(&memory), start, "{}", case.text);
                } else {
                    assert_eq!(
                        result.exit_reason,
                        ExitReason::StepLimitExceeded,
                        "{}",
                        case.text
                    );
                    assert_eq!(result.final_state, wanted, "{}", case.text);
                    let mut bytes = start.clone();
                    if case.is_store() {
                        bytes[place..place + 32].copy_from_slice(&after[32..64]);
                    }
                    assert_eq!(data(&memory), bytes, "{}", case.text);
                }
            }
            seen[usize::from(fails)] += 1;
        }
    }
    assert!(seen[0] > 300 && seen[1] > 100, "{seen:?}");
}

const EXECUTE_ONLY: MemoryPermissions = MemoryPermissions {
    read: false,
    write: false,
    execute: true,
};

#[test]
fn faults_name_the_access_and_the_first_bad_byte() {
    let end = DATA + 0x100;
    for case in all_cases().into_iter().filter(Case::has_memory) {
        let (text, store, width) = (case.text, case.is_store(), case.width());
        let mut tries = vec![
            (0x9000, READ_WRITE, Some(0x9000)),
            (DATA + 0x40, MemoryPermissions::READ_ONLY, None),
            (DATA + 0x40, EXECUTE_ONLY, None),
        ];
        if !needs_alignment(case.name()) {
            // All but the last byte, and only the first one, are mapped.
            tries.push((end - width + 1, READ_WRITE, Some(end)));
            if width > 1 {
                tries.push((end - 1, READ_WRITE, Some(end)));
            }
        }
        for (address, permissions, unmapped) in tries {
            let mut machine = Random(0x3C).machine();
            machine.gpr[6] = address - 32;
            for native in [false, true] {
                let (cpu, mut memory) = load(&machine, &case.code, permissions);
                let before = permissions.read.then(|| data(&memory));
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
                // Not even the mapped part of a store is written.
                let after = permissions.read.then(|| data(&memory));
                assert_eq!(after, before, "{text}");
            }
        }
        // The last operand in the region is inside it.
        let mut machine = Random(0x3C).machine();
        machine.gpr[6] = end - width - 32;
        for native in [false, true] {
            let (cpu, mut memory) = load(&machine, &case.code, READ_WRITE);
            memory.write_bytes(end - 8, &[0; 8]).unwrap();
            let result = run(native, &cpu, &mut memory, 1);
            assert_eq!(result.exit_reason, ExitReason::StepLimitExceeded, "{text}");
        }
    }
}

#[test]
fn non_canonical_operands_are_general_protection_faults() {
    const LIMIT: u64 = 0x0000_8000_0000_0000;
    for case in all_cases().into_iter().filter(Case::has_memory) {
        let width = case.width();
        for address in [
            LIMIT,
            0xFFFF_7FFF_FFFF_FFE0,
            1 << 63,
            LIMIT - width + 1,
            LIMIT - width.min(2) + 1,
        ] {
            let mut machine = Random(0x3D).machine();
            machine.gpr[6] = address.wrapping_sub(32);
            for native in [false, true] {
                let (cpu, mut memory) = load(&machine, &case.code, READ_WRITE);
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
    }
}

#[test]
fn segment_overrides_add_their_base() {
    let mut random = Random(0xF5);
    for case in all_cases().into_iter().filter(Case::has_memory) {
        let machine = random.machine();
        let (mut cpu, _) = load(&machine, &case.code, READ_WRITE);
        (cpu.fs_base, cpu.gs_base) = (0x1_0003, 0x2_0003);
        for native in [false, true] {
            let plain = outcome(native, &cpu, &case.code, &machine.operand);
            assert_eq!(plain.0, ExitReason::StepLimitExceeded, "{}", case.text);
            for (prefix, base) in [(0x64, 0x1_0003), (0x65, 0x2_0003), (0x2E, 0)] {
                let code = [&[prefix][..], &case.code].concat();
                let mut moved = cpu.clone();
                // Alignment is judged on the linear address.
                moved.gpr[6] = DATA.wrapping_sub(base);
                let mut result = outcome(native, &moved, &code, &machine.operand);
                result.1.gpr[6] = plain.1.gpr[6];
                assert_eq!(result, plain, "{prefix:02x} {}", case.text);
            }
        }
    }
}

#[test]
fn vldmxcsr_rejects_reserved_bits() {
    let case = cases()
        .into_iter()
        .find(|case| case.name() == "vldmxcsr")
        .unwrap();
    for (value, accepted) in [
        (0x1F80u32, true),
        (0xFFFF, true),
        (0x1_0000, false),
        (1 << 31, false),
    ] {
        let mut machine = Random(0x55).machine();
        machine.operand[..4].copy_from_slice(&value.to_le_bytes());
        for native in [false, true] {
            let (cpu, mut memory) = load(&machine, &case.code, READ_WRITE);
            let result = run(native, &cpu, &mut memory, 1);
            if accepted {
                assert_eq!(result.final_state.mxcsr, Mxcsr(value));
            } else {
                assert_eq!(result.exit_reason, ExitReason::GeneralProtection);
                assert_eq!(result.final_state, cpu);
            }
        }
    }
}

/// Encodings assemblers rarely emit: the store opcodes with a register operand, and
/// the W1 moves of a quadword in memory. `objdump` (LLVM) disassembles each as the
/// text given here.
#[test]
fn alternative_encodings_match_the_reference() {
    let cases: &[(&str, &[u8])] = &[
        ("vmovaps ymm3, ymm9", &[0xC5, 0x7C, 0x29, 0xCB]),
        ("vmovaps xmm3, xmm9", &[0xC5, 0x78, 0x29, 0xCB]),
        ("vmovapd ymm3, ymm9", &[0xC5, 0x7D, 0x29, 0xCB]),
        ("vmovups ymm3, ymm9", &[0xC5, 0x7C, 0x11, 0xCB]),
        ("vmovupd xmm3, xmm9", &[0xC5, 0x79, 0x11, 0xCB]),
        ("vmovdqa ymm3, ymm9", &[0xC5, 0x7D, 0x7F, 0xCB]),
        ("vmovdqu xmm3, xmm9", &[0xC5, 0x7A, 0x7F, 0xCB]),
        ("vmovq xmm3, xmm9", &[0xC5, 0x79, 0xD6, 0xCB]),
        (
            "vmovq xmm3, qword ptr [rsi + 4*rdi + 16]",
            &[0xC4, 0xE1, 0xF9, 0x6E, 0x5C, 0xBE, 0x10],
        ),
        (
            "vmovq qword ptr [rsi + 4*rdi + 16], xmm3",
            &[0xC4, 0xE1, 0xF9, 0x7E, 0x5C, 0xBE, 0x10],
        ),
        ("vmovss xmm3, xmm9, xmm10", &[0xC5, 0x32, 0x11, 0xD3]),
        ("vmovsd xmm3, xmm9, xmm10", &[0xC5, 0x33, 0x11, 0xD3]),
    ];
    let mut random = Random(0x1234);
    for (text, code) in cases {
        for _ in 0..16 {
            let machine = random.machine();
            let mut expected = machine.clone();
            reference(&mut expected, text);
            let (mut wanted, _) = load(&expected, code, READ_WRITE);
            wanted.rip += code.len() as u64;
            for native in [false, true] {
                let (cpu, mut memory) = load(&machine, code, READ_WRITE);
                let result = run(native, &cpu, &mut memory, 1);
                assert_eq!(result.final_state, wanted, "{text}");
                let mut bytes = vec![0xEE; 0x100];
                bytes[32..64].copy_from_slice(&expected.operand);
                assert_eq!(data(&memory), bytes, "{text}");
            }
        }
        let insn = at(code).unwrap();
        assert!(!text.contains("qword") || insn.rm_size == 8, "{text}");
    }
}

/// The status flags of an earlier instruction survive every form that does not
/// compare.
#[test]
fn flags_survive_every_form() {
    let added = RFlags {
        cf: true,
        zf: true,
        sf: false,
        of: false,
        pf: true,
        af: true,
    };
    for case in all_cases() {
        if ["vptest", "vcomi", "vucomi", "vpcmpestr", "vpcmpistr"]
            .iter()
            .any(|stem| case.name().starts_with(stem))
        {
            continue;
        }
        let code = [&[0x01, 0xC8][..], &case.code, &[0x89, 0xD2, 0xF4]].concat(); // add eax, ecx
        let mut machine = Random(0xADD).machine();
        (machine.gpr[0], machine.gpr[1]) = (0xFFFF_FFFF, 1);
        for budget in [2, 4] {
            for native in [false, true] {
                let (cpu, mut memory) = load(&machine, &code, READ_WRITE);
                let result = run(native, &cpu, &mut memory, budget);
                assert_eq!(result.final_state.flags, added, "{}", case.text);
            }
        }
    }
}

#[test]
fn mixed_vex_legacy_and_integer_sequences_agree() {
    let vex = all_cases();
    let legacy: Vec<Case> = parse(include_str!("x86/vector.txt"));
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
    for round in 0..2500 {
        let mut code = Vec::new();
        let mut text = Vec::new();
        for _ in 0..2 + random.next() % 30 {
            let pool = match random.next() % 4 {
                0 => &integers,
                1 => &legacy,
                _ => &vex,
            };
            let case = &pool[(random.next() % pool.len() as u64) as usize];
            code.extend_from_slice(&case.code);
            text.push(case.text);
        }
        code.push(0xF4);
        let mut machine = random.machine();
        if random.next().is_multiple_of(8) {
            machine.gpr[6] += 1 + random.next() % 31; // misaligned operand
        }
        let budget = 1 + (random.next() % 40) as usize;
        let (cpu, mut first) = load(&machine, &code, READ_WRITE);
        let interpreted = run(false, &cpu, &mut first, budget);
        let (_, mut second) = load(&machine, &code, READ_WRITE);
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
