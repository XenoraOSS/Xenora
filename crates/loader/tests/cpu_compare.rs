// SPDX-License-Identifier: GPL-3.0-or-later

//! CMP and TEST memory-operand verification (issue #119).
//!
//! Every instruction stream below is hand-assembled from Intel SDM Vol. 2
//! opcode tables: the byte comments name the exact encoding. Expected flags
//! are derived here from the architecture with 128-bit arithmetic, never by
//! reading back what either engine produced. Both engines run every stream;
//! the memory operands live in read-only pages so even one wrongly attempted
//! write stops the run with a fault instead of passing silently.
//!
//! Coverage:
//! - every memory form at every width: CMP r/m,reg (38/39), CMP reg,r/m
//!   (3A/3B), CMP r/m,imm (80/7, 81/7, 83/7), TEST r/m,reg (84/85) and
//!   TEST r/m,imm (F6/0, F7/0);
//! - taken and untaken JE branches driven by the flags those forms write;
//! - effective addresses: positive and negative disp8, disp32 with and
//!   without a base, RIP-relative, extended base/index registers and the FS
//!   and GS segment overrides;
//! - precise faults: unmapped, non-readable and crossing reads name the
//!   first inaccessible byte and leave the whole guest state untouched, and
//!   non-canonical addresses raise #GP before any memory lookup.

use xenora_loader::cpu::{CpuState, ExecutionResult, ExitReason, Interpreter, RFlags, Register};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryError, MemoryPermissions};

const ENTRY: u64 = 0x40_0000;
const DATA: u64 = 0x50_0000;
const FS_DATA: u64 = 0x60_0000;
const GS_DATA: u64 = 0x70_0000;
const REGION_LEN: usize = 0x1000;
const OPERAND_OFFSET: usize = 0x20;
const POISON: u64 = 0xA5A5_A5A5_A5A5_A5A5;

/// The memory forms of CMP and TEST that the supported subset encodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// 38/r or 39/r: CMP r/m, reg, memory is the first operand.
    CmpRmReg,
    /// 3A/r or 3B/r: CMP reg, r/m, memory is the second operand.
    CmpRegRm,
    /// 80 /7 ib: CMP r/m8, imm8.
    CmpRmImm80,
    /// 81 /7: CMP r/m, sign-extended imm16/imm32.
    CmpRmImm81,
    /// 83 /7 ib: CMP r/m, sign-extended imm8.
    CmpRmImm83,
    /// 84/r or 85/r: TEST r/m, reg.
    TestRmReg,
    /// F6 /0 ib or F7 /0: TEST r/m, imm.
    TestRmImm,
}

impl Form {
    fn is_cmp(self) -> bool {
        matches!(
            self,
            Self::CmpRmReg
                | Self::CmpRegRm
                | Self::CmpRmImm80
                | Self::CmpRmImm81
                | Self::CmpRmImm83
        )
    }

    fn is_immediate(self) -> bool {
        matches!(
            self,
            Self::CmpRmImm80 | Self::CmpRmImm81 | Self::CmpRmImm83 | Self::TestRmImm
        )
    }

    /// Bit width of the encoded immediate field (Intel SDM Vol. 2A, the
    /// operand-size and imm8/imm16/imm32 rules of each opcode).
    fn immediate_field_bits(self, bits: u32) -> u32 {
        match self {
            Self::CmpRmImm80 | Self::CmpRmImm83 => 8,
            Self::CmpRmImm81 | Self::TestRmImm => match bits {
                8 => 8,
                16 => 16,
                _ => 32,
            },
            _ => unreachable!("register form has no immediate field"),
        }
    }
}

fn mask(bits: u32) -> u64 {
    if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

fn sign_bit(bits: u32) -> u64 {
    1u64 << (bits - 1)
}

/// Sign-extend the low `field_bits` of `value` to 64 bits.
fn sign_extend(value: u64, field_bits: u32) -> u64 {
    let shift = 64 - field_bits;
    ((value << shift) as i64 >> shift) as u64
}

/// The operand an immediate field actually contributes: its bytes are
/// sign-extended to the operand width (or already match it, for bytes).
fn architectural_immediate(form: Form, bits: u32, raw: u64) -> u64 {
    sign_extend(raw, form.immediate_field_bits(bits)) & mask(bits)
}

/// A width-truncated value that depends on `index`, including zero.
fn pattern(bits: u32, index: usize) -> u64 {
    let m = mask(bits);
    match index % 6 {
        0 => 0,
        1 => 1,
        2 => sign_bit(bits),
        3 => sign_bit(bits) - 1,
        4 => m,
        _ => 0x5555_5555_5555_5555 & m,
    }
}

/// Same spread, never zero.
fn nonzero_pattern(bits: u32, index: usize) -> u64 {
    let m = mask(bits);
    match index % 5 {
        0 => 1,
        1 => sign_bit(bits),
        2 => sign_bit(bits) - 1,
        3 => m,
        _ => 0x5555_5555_5555_5555 & m,
    }
}

/// Memory operand, second operand and raw immediate bytes for one matrix
/// case. `zero` selects operands whose comparison or test sets ZF.
fn operand_values(form: Form, bits: u32, salt: usize, zero: bool) -> (u64, u64, u64) {
    let mut raw = if form.is_cmp() {
        pattern(bits, salt)
    } else {
        nonzero_pattern(bits, salt)
    };
    // TEST with a zero immediate cannot produce a nonzero result, so pick
    // raw bytes that survive sign extension as a nonzero value.
    if form.is_immediate() && !form.is_cmp() && architectural_immediate(form, bits, raw) == 0 {
        raw = 1;
    }
    let other = if form.is_immediate() {
        architectural_immediate(form, bits, raw)
    } else {
        raw & mask(bits)
    };
    let m = mask(bits);
    let memory = if form.is_cmp() {
        if zero {
            other
        } else {
            let candidate = pattern(bits, salt + 2);
            if candidate == other {
                other ^ 1
            } else {
                candidate
            }
        }
    } else if zero {
        // The bitwise complement never shares a set bit with `other`.
        !other & m
    } else {
        // `other` is never zero for TEST, so the result stays nonzero.
        other
    };
    (memory, other, raw)
}

/// The instruction bytes of one memory form against `[rsi + rdi*4 + 0x10]`.
///
/// ModRM 01 reg 100 pairs mod=01 with the SIB byte; SIB 0xBE is scale 2,
/// index RDI, base RSI; 0x10 is the disp8. Group bytes use reg field 7 for
/// CMP and 0 for TEST.
fn memory_instruction(form: Form, bits: u32, raw_immediate: u64) -> Vec<u8> {
    let mut code = Vec::new();
    if bits == 16 {
        code.push(0x66);
    }
    if bits == 64 {
        code.push(0x48); // REX.W
    }
    let byte = bits == 8;
    let (opcode, reg_field) = match form {
        Form::CmpRmReg => (if byte { 0x38 } else { 0x39 }, 2),
        Form::CmpRegRm => (if byte { 0x3A } else { 0x3B }, 2),
        Form::CmpRmImm80 => (0x80, 7),
        Form::CmpRmImm81 => (0x81, 7),
        Form::CmpRmImm83 => (0x83, 7),
        Form::TestRmReg => (if byte { 0x84 } else { 0x85 }, 2),
        Form::TestRmImm => (if byte { 0xF6 } else { 0xF7 }, 0),
    };
    code.push(opcode);
    code.push(0x40 | (reg_field << 3) | 0x04); // mod=01, rm=SIB
    code.push(0xBE); // scale=2, index=RDI, base=RSI
    code.push(0x10); // disp8
    if form.is_immediate() {
        let bytes = (form.immediate_field_bits(bits) / 8) as usize;
        code.extend_from_slice(&raw_immediate.to_le_bytes()[..bytes]);
    }
    code
}

/// `instruction`, then `je +6` over a mov that loads 0x31 into EDX, with a
/// second mov loading 0x73 on the taken path. Both paths end in HLT, so the
/// final RIP proves which way the branch went and EDX proves it independently.
///
/// ```text
/// +len   74 06              je +6
/// +len+2 BA 31 00 00 00     mov edx, 0x31   (untaken)
/// +len+7 F4                 hlt             -> next rip len+8
/// +len+8 BA 73 00 00 00     mov edx, 0x73   (taken)
/// +len+13 F4                hlt             -> next rip len+14
/// ```
fn branch_program(instruction: &[u8]) -> Vec<u8> {
    let mut code = instruction.to_vec();
    code.extend_from_slice(&[
        0x74, 0x06, // je +6
        0xBA, 0x31, 0x00, 0x00, 0x00, // mov edx, 0x31
        0xF4, // hlt
        0xBA, 0x73, 0x00, 0x00, 0x00, // mov edx, 0x73
        0xF4, // hlt
    ]);
    code
}

/// Architectural subtraction flags of `left - right` at `bits`, derived with
/// 128-bit arithmetic from Intel SDM Vol. 1 appendix A definitions.
fn cmp_flags(left: u64, right: u64, bits: u32) -> RFlags {
    let m = mask(bits);
    let l = left & m;
    let r = right & m;
    let result = l.wrapping_sub(r) & m;
    let modulus = 1i128 << bits;
    let signed = |value: u64| {
        let value = i128::from(value & m);
        if value >= modulus / 2 {
            value - modulus
        } else {
            value
        }
    };
    let difference = signed(l) - signed(r);
    let min = -(1i128 << (bits - 1));
    let max = (1i128 << (bits - 1)) - 1;
    let low_nibbles = i128::from(l & 0xF) - i128::from(r & 0xF);
    RFlags {
        cf: l < r,
        zf: result == 0,
        sf: result & sign_bit(bits) != 0,
        of: !(min..=max).contains(&difference),
        pf: (result as u8).count_ones().is_multiple_of(2),
        af: !(0..=0xF).contains(&low_nibbles),
    }
}

/// Defined architectural logical flags of `left & right` at `bits`. TEST
/// always clears CF and OF. AF is architecturally undefined, so it is never
/// part of the independent comparison; Xenora's documented preservation of
/// incoming AF is checked separately as implementation-specific behavior.
fn test_flags(left: u64, right: u64, bits: u32) -> RFlags {
    let result = left & right & mask(bits);
    RFlags {
        cf: false,
        zf: result == 0,
        sf: result & sign_bit(bits) != 0,
        of: false,
        pf: (result as u8).count_ones().is_multiple_of(2),
        af: false,
    }
}

/// Data image filled with a sentinel, with the operand bytes at
/// `OPERAND_OFFSET`. Bytes after the operand stay nonzero so a read wider
/// than the operand width changes the flags and fails the test.
fn data_image(value: u64, bits: u32) -> Vec<u8> {
    let mut image = vec![0xE7u8; REGION_LEN];
    let width = (bits / 8) as usize;
    image[OPERAND_OFFSET..OPERAND_OFFSET + width].copy_from_slice(&value.to_le_bytes()[..width]);
    image
}

fn memory_with(code: &[u8], data: &[u8]) -> GuestMemory {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .expect("code region");
    memory
        .map_region_with_data(DATA, REGION_LEN, MemoryPermissions::READ_ONLY, data)
        .expect("read-only data region");
    memory
}

/// CPU state for the matrix: every register poisoned, RSI/RDI choosing the
/// effective address, RDX carrying the second operand below poisoned high
/// bits that the narrower widths must ignore.
fn matrix_cpu(bits: u32, other: u64) -> CpuState {
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr = [POISON; 16];
    cpu.set(Register::Rsi, DATA);
    cpu.set(Register::Rdi, 4);
    let low = other & mask(bits);
    let rdx = if bits == 64 {
        low
    } else {
        (POISON & !mask(bits)) | low
    };
    cpu.set(Register::Rdx, rdx);
    cpu
}

struct Outcomes {
    interpreted: ExecutionResult,
    translated: ExecutionResult,
    interpreted_memory: GuestMemory,
    translated_memory: GuestMemory,
}

fn run_both(cpu: &CpuState, memory: &GuestMemory) -> Outcomes {
    let mut interpreted_memory = memory.clone();
    let mut translated_memory = memory.clone();
    let mut interpreter = Interpreter::new(cpu.clone(), &mut interpreted_memory, 16);
    let interpreted = interpreter.run();
    let mut engine = DbtEngine::new(1024 * 1024).expect("JIT memory");
    let mut translated_cpu = cpu.clone();
    let translated = engine.run(&mut translated_cpu, &mut translated_memory, 16);
    Outcomes {
        interpreted,
        translated,
        interpreted_memory,
        translated_memory,
    }
}

/// The data bytes of one region must be byte-for-byte what the setup wrote:
/// CMP and TEST never store.
fn assert_region_unchanged(memory: &GuestMemory, base: u64, image: &[u8], label: &str) {
    let mut actual = vec![0u8; image.len()];
    memory
        .read_bytes(base, &mut actual)
        .expect("region stays readable");
    assert_eq!(actual, image, "{label}: compared memory was written");
}

/// Run one memory form inside the branch harness in both engines and verify
/// the final state against independently derived flags. Returns the flags
/// the architecture predicts, so callers can account for branch coverage.
fn verify_form(
    form: Form,
    bits: u32,
    memory_operand: u64,
    other: u64,
    raw_immediate: u64,
    initial_af: bool,
    label: &str,
) -> RFlags {
    let instruction = memory_instruction(form, bits, raw_immediate);
    let insn_len = instruction.len() as u64;
    let code = branch_program(&instruction);
    let data = data_image(memory_operand, bits);
    let mut cpu = matrix_cpu(bits, other);
    // A deliberately mixed incoming flag pattern: CMP must overwrite every
    // bit, TEST must overwrite all but AF.
    cpu.flags = RFlags {
        cf: true,
        zf: true,
        sf: false,
        of: true,
        pf: true,
        af: initial_af,
    };

    let (left, right) = if form == Form::CmpRegRm {
        (other, memory_operand)
    } else {
        (memory_operand, other)
    };
    let flags = if form.is_cmp() {
        cmp_flags(left, right, bits)
    } else {
        test_flags(left, right, bits)
    };

    let mut expected = cpu.clone();
    expected.flags = flags;
    expected.set(Register::Rdx, if flags.zf { 0x73 } else { 0x31 });
    expected.rip = ENTRY + insn_len + if flags.zf { 14 } else { 8 };

    let memory = memory_with(&code, &data);
    let outcomes = run_both(&cpu, &memory);
    for (engine, result, final_memory) in [
        (
            "interpreter",
            &outcomes.interpreted,
            &outcomes.interpreted_memory,
        ),
        ("dbt", &outcomes.translated, &outcomes.translated_memory),
    ] {
        assert_eq!(
            result.exit_reason,
            ExitReason::Halt,
            "{label}: {engine} exit"
        );
        assert_eq!(
            result.instructions_executed, 4,
            "{label}: {engine} instruction count"
        );
        let mut actual_state = result.final_state.clone();
        let mut expected_state = expected.clone();
        if !form.is_cmp() {
            // TEST leaves AF architecturally undefined. Never rely on the
            // implementation's preservation choice; compare only defined state.
            actual_state.flags.af = false;
            expected_state.flags.af = false;
        }
        assert_eq!(
            &actual_state, &expected_state,
            "{label}: {engine} final state"
        );
        assert_region_unchanged(final_memory, DATA, &data, &format!("{label}: {engine}"));
    }
    flags
}

#[test]
fn every_memory_form_at_every_width_drives_taken_and_untaken_branches() {
    let widths = [8u32, 16, 32, 64];
    let mut cases = 0usize;
    let mut taken = 0usize;
    let mut untaken = 0usize;
    for bits in widths {
        let forms: &[Form] = if bits == 8 {
            &[
                Form::CmpRmReg,
                Form::CmpRegRm,
                Form::CmpRmImm80,
                Form::TestRmReg,
                Form::TestRmImm,
            ]
        } else {
            &[
                Form::CmpRmReg,
                Form::CmpRegRm,
                Form::CmpRmImm81,
                Form::CmpRmImm83,
                Form::TestRmReg,
                Form::TestRmImm,
            ]
        };
        for (index, form) in forms.iter().enumerate() {
            for zero in [true, false] {
                let salt = cases + index;
                let (memory_operand, other, raw) = operand_values(*form, bits, salt, zero);
                let label = format!("{form:?} {bits}-bit zero={zero}");
                let flags = verify_form(*form, bits, memory_operand, other, raw, zero, &label);
                if flags.zf {
                    taken += 1;
                } else {
                    untaken += 1;
                }
                cases += 1;
            }
        }
    }
    // 5 forms at 8 bits, 6 at each wider width: 23 forms, each run for both
    // branch directions.
    assert_eq!(cases, 46);
    assert_eq!(taken, 23, "the zero operand pairs must set ZF");
    assert_eq!(untaken, 23, "the nonzero operand pairs must clear ZF");
}

#[test]
fn cmp_memory_flags_follow_the_width_edges() {
    for bits in [8u32, 16, 32, 64] {
        let sign = sign_bit(bits);
        let m = mask(bits);
        // (sign, 1): overflow with auxiliary carry. (0, 1): borrow out of
        // the width with a negative result. (1, sign): carry and overflow
        // together. (mask, 1): a negative result with no borrow anywhere.
        // (sign, sign): equality.
        let pairs = [(sign, 1), (0, 1), (1, sign), (m, 1), (sign, sign)];
        for (index, (memory_operand, register)) in pairs.iter().enumerate() {
            verify_form(
                Form::CmpRmReg,
                bits,
                *memory_operand,
                *register,
                0,
                false,
                &format!("cmp edge {bits}-bit pair {index}"),
            );
        }
    }
}

#[test]
fn test_memory_flags_follow_the_width_edges() {
    for bits in [8u32, 16, 32, 64] {
        let sign = sign_bit(bits);
        let m = mask(bits);
        // (mask, 1): a single low bit set. (mask, sign): the sign bit alone.
        // (mask, sign|1): sign plus a low bit. (sign, 1): disjoint, ZF set.
        let pairs = [(m, 1), (m, sign), (m, sign | 1), (sign, 1)];
        for (index, (memory_operand, register)) in pairs.iter().enumerate() {
            verify_form(
                Form::TestRmReg,
                bits,
                *memory_operand,
                *register,
                0,
                true,
                &format!("test edge {bits}-bit pair {index}"),
            );
        }
    }
}

#[test]
fn memory_immediates_are_sign_extended_to_the_operand_width() {
    // 81 /7 id: imm32 0x80000000 sign-extends to 0xffffffff80000000. Equality
    // with that full-width value only holds when the extension happened.
    verify_form(
        Form::CmpRmImm81,
        64,
        0xFFFF_FFFF_8000_0000,
        0xFFFF_FFFF_8000_0000,
        0x8000_0000,
        false,
        "cmp qword [mem], imm32 negative",
    );
    // 83 /7 ib: imm8 0x80 sign-extends to -128 across the whole qword.
    verify_form(
        Form::CmpRmImm83,
        64,
        0xFFFF_FFFF_FFFF_FF80,
        0xFFFF_FFFF_FFFF_FF80,
        0x80,
        false,
        "cmp qword [mem], imm8 negative",
    );
    // F7 /0 id: the same sign-extended immediate ANDs to a value whose bit
    // 63 is set only when the extension happened, so SF proves it.
    verify_form(
        Form::TestRmImm,
        64,
        u64::MAX,
        0xFFFF_FFFF_8000_0000,
        0x8000_0000,
        true,
        "test qword [mem], imm32 negative",
    );
    // 83 /7 ib sign-extends at the narrower widths too: 0xFD becomes -3
    // across 16 and 32 bits. Equality with the extended value proves it.
    verify_form(
        Form::CmpRmImm83,
        16,
        0xFFFD,
        0xFFFD,
        0xFD,
        false,
        "cmp word [mem], imm8 negative",
    );
    verify_form(
        Form::CmpRmImm83,
        32,
        0xFFFF_FFFD,
        0xFFFF_FFFD,
        0xFD,
        false,
        "cmp dword [mem], imm8 negative",
    );
}

/// Run one byte-wide CMP or TEST against a single byte set to 1 at `target`
/// with RDX holding 1. Every surrounding byte is zero in every region, so an
/// address calculated even slightly wrong reads a zero and produces the
/// opposite flags.
fn verify_effective_address(
    test: bool,
    instruction: &[u8],
    target: u64,
    setup: impl Fn(&mut CpuState),
    label: &str,
) {
    let mut code = instruction.to_vec();
    code.push(0xF4); // hlt
    let mut images = [
        (DATA, vec![0u8; REGION_LEN]),
        (FS_DATA, vec![0u8; REGION_LEN]),
        (GS_DATA, vec![0u8; REGION_LEN]),
    ];
    let index = images
        .iter()
        .position(|(base, image)| (*base..*base + image.len() as u64).contains(&target))
        .unwrap_or_else(|| panic!("{label}: target outside every region"));
    let (base, image) = &mut images[index];
    image[(target - *base) as usize] = 1;

    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, &code)
        .expect("code region");
    for (base, image) in &images {
        memory
            .map_region_with_data(*base, REGION_LEN, MemoryPermissions::READ_ONLY, image)
            .expect("data region");
    }

    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr = [POISON; 16];
    cpu.set(Register::Rdx, 1);
    setup(&mut cpu);

    // Byte 1 compared with or tested against DL=1.
    let flags = if test {
        test_flags(1, 1, 8)
    } else {
        cmp_flags(1, 1, 8)
    };
    let mut expected = cpu.clone();
    expected.flags = flags;
    expected.rip = ENTRY + instruction.len() as u64 + 1;

    let outcomes = run_both(&cpu, &memory);
    for (engine, result, final_memory) in [
        (
            "interpreter",
            &outcomes.interpreted,
            &outcomes.interpreted_memory,
        ),
        ("dbt", &outcomes.translated, &outcomes.translated_memory),
    ] {
        assert_eq!(
            result.exit_reason,
            ExitReason::Halt,
            "{label}: {engine} exit"
        );
        let mut actual_state = result.final_state.clone();
        let mut expected_state = expected.clone();
        if test {
            actual_state.flags.af = false;
            expected_state.flags.af = false;
        }
        assert_eq!(
            &actual_state, &expected_state,
            "{label}: {engine} final state"
        );
        for (base, image) in &images {
            assert_region_unchanged(final_memory, *base, image, &format!("{label}: {engine}"));
        }
    }
}

fn disp32(value: i64) -> [u8; 4] {
    i32::try_from(value)
        .expect("displacement fits imm32")
        .to_le_bytes()
}

#[test]
fn effective_address_forms_select_the_intended_memory_operand() {
    // cmp byte ptr [rsi + rdi*4 + 0x10], dl: 38 54 BE 10.
    verify_effective_address(
        false,
        &[0x38, 0x54, 0xBE, 0x10],
        DATA + 0x20,
        |cpu| {
            cpu.set(Register::Rsi, DATA);
            cpu.set(Register::Rdi, 4);
        },
        "cmp [rsi+rdi*4+disp8]",
    );
    // test byte ptr [rbx + rcx*8 - 0x18], dl: 84 54 CB E8. SIB scale 3,
    // index RCX, base RBX; EA = DATA+0x40 + 8*8 - 0x18 = DATA+0x68.
    verify_effective_address(
        true,
        &[0x84, 0x54, 0xCB, 0xE8],
        DATA + 0x68,
        |cpu| {
            cpu.set(Register::Rbx, DATA + 0x40);
            cpu.set(Register::Rcx, 8);
        },
        "test [rbx+rcx*8-negative disp8]",
    );
    // cmp byte ptr [rbx - 8], dl: 38 53 F8.
    verify_effective_address(
        false,
        &[0x38, 0x53, 0xF8],
        DATA + 0x18,
        |cpu| cpu.set(Register::Rbx, DATA + 0x20),
        "cmp [rbx-negative disp8]",
    );
    // cmp byte ptr [rbx - 0x20], dl with a 32-bit displacement: mod 10 makes
    // the ModRM 0x93, so 38 93 E0 FF FF FF. EA = DATA+0x40 - 0x20.
    verify_effective_address(
        false,
        &[0x38, 0x93, 0xE0, 0xFF, 0xFF, 0xFF],
        DATA + 0x20,
        |cpu| cpu.set(Register::Rbx, DATA + 0x40),
        "cmp [rbx-negative disp32]",
    );
    // test byte ptr [rdi*8 + disp32], dl with no base: 84 14 FD <disp32>;
    // SIB scale 3, index RDI, base 101 means no base in mode 00.
    let mut no_base = vec![0x84, 0x14, 0xFD];
    no_base.extend_from_slice(&disp32((DATA + 0x8) as i64));
    verify_effective_address(
        true,
        &no_base,
        DATA + 0x20,
        |cpu| cpu.set(Register::Rdi, 3),
        "test [rdi*8+disp32] no base",
    );
    // cmp byte ptr [disp32], dl with neither base nor index: 38 14 25 <disp32>;
    // SIB index 100 means no index, base 101 no base.
    let mut absolute = vec![0x38, 0x14, 0x25];
    absolute.extend_from_slice(&(DATA as u32 + 0x30).to_le_bytes());
    verify_effective_address(
        false,
        &absolute,
        DATA + 0x30,
        |_| {},
        "cmp [disp32] absolute",
    );
    // test byte ptr [rip + disp32], dl: 84 15 <disp32>. The displacement is
    // relative to the end of the six-byte instruction.
    let target = DATA + 0x28;
    let mut rip_relative = vec![0x84, 0x15];
    rip_relative.extend_from_slice(&disp32(target as i64 - (ENTRY + 6) as i64));
    verify_effective_address(true, &rip_relative, target, |_| {}, "test [rip+disp32]");
    // cmp byte ptr fs:[rsi + 0x8], dl: 64 38 56 08.
    verify_effective_address(
        false,
        &[0x64, 0x38, 0x56, 0x08],
        FS_DATA + 0x8,
        |cpu| {
            cpu.set(Register::Rsi, 0);
            cpu.fs_base = FS_DATA;
        },
        "cmp fs:[rsi+disp8]",
    );
    // test byte ptr gs:[rsi - 0x8], dl: 65 84 56 F8.
    verify_effective_address(
        true,
        &[0x65, 0x84, 0x56, 0xF8],
        GS_DATA + 0x18,
        |cpu| {
            cpu.set(Register::Rsi, 0);
            cpu.gs_base = GS_DATA + 0x20;
        },
        "test gs:[rsi-negative disp8]",
    );
    // cmp byte ptr [r8 + r9*2], dl: 43 38 14 48. REX.X and REX.B extend the
    // SIB index and base to r9 and r8; scale 1, index 1, base 0.
    verify_effective_address(
        false,
        &[0x43, 0x38, 0x14, 0x48],
        DATA + 0x18,
        |cpu| {
            cpu.set(Register::R8, DATA + 0x8);
            cpu.set(Register::R9, 8);
        },
        "cmp [r8+r9*2] extended registers",
    );
}

/// The scalar `[rbx]` encoding of one compare or test at one width.
fn scalar_memory_instruction(test: bool, bits: u32) -> Vec<u8> {
    let mut code = Vec::new();
    if bits == 16 {
        code.push(0x66);
    }
    if bits == 64 {
        code.push(0x48);
    }
    let byte = bits == 8;
    code.push(match (test, byte) {
        (false, true) => 0x38,
        (false, false) => 0x39,
        (true, true) => 0x84,
        (true, false) => 0x85,
    });
    code.push(0x13); // mod=00, reg=2 (DL/EDX/RDX), rm=011 [rbx]
    code
}

fn faulting_cpu(target: u64) -> CpuState {
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr = [POISON; 16];
    cpu.set(Register::Rbx, target);
    cpu.flags = RFlags {
        cf: true,
        zf: true,
        sf: true,
        of: true,
        pf: true,
        af: true,
    };
    cpu
}

fn fault_memory(code: &[u8], populate: impl Fn(&mut GuestMemory)) -> GuestMemory {
    let mut image = code.to_vec();
    image.push(0xF4);
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, image.len(), MemoryPermissions::READ_EXECUTE, &image)
        .expect("code region");
    populate(&mut memory);
    memory
}

/// A faulting memory read must stop before anything happens: same registers,
/// same flags, RIP still on the faulting instruction, zero instructions
/// retired, in both engines.
fn verify_precise_fault(memory: &GuestMemory, target: u64, expected_exit: ExitReason, label: &str) {
    let cpu = faulting_cpu(target);
    let outcomes = run_both(&cpu, memory);
    for (engine, result) in [
        ("interpreter", &outcomes.interpreted),
        ("dbt", &outcomes.translated),
    ] {
        assert_eq!(result.exit_reason, expected_exit, "{label}: {engine} exit");
        assert_eq!(
            result.instructions_executed, 0,
            "{label}: {engine} must retire nothing"
        );
        assert_eq!(
            &result.final_state, &cpu,
            "{label}: {engine} state after fault"
        );
    }
}

#[test]
fn unmapped_reads_report_the_first_inaccessible_byte_and_change_nothing() {
    for bits in [8u32, 16, 32, 64] {
        for test in [false, true] {
            let instruction = scalar_memory_instruction(test, bits);
            let width = u64::from(bits / 8);
            let name = if test { "test" } else { "cmp" };

            // An operand at a wholly unmapped address.
            let target = DATA + 0x800;
            let memory = fault_memory(&instruction, |_| {});
            verify_precise_fault(
                &memory,
                target,
                ExitReason::MemoryFault(MemoryError::UnmappedAddress(target)),
                &format!("{name} {bits}-bit unmapped"),
            );

            // An operand whose first bytes are readable but that crosses out
            // of the last mapped page: the fault names the boundary byte.
            if width > 1 {
                let boundary = DATA + REGION_LEN as u64;
                let target = boundary + 1 - width;
                let memory = fault_memory(&instruction, |memory| {
                    memory
                        .map_region(DATA, REGION_LEN, MemoryPermissions::READ_ONLY)
                        .expect("data region");
                });
                verify_precise_fault(
                    &memory,
                    target,
                    ExitReason::MemoryFault(MemoryError::UnmappedAddress(boundary)),
                    &format!("{name} {bits}-bit crossing unmapped"),
                );
            }
        }
    }
}

#[test]
fn unreadable_reads_report_the_first_inaccessible_byte_and_change_nothing() {
    for bits in [8u32, 16, 32, 64] {
        for test in [false, true] {
            let instruction = scalar_memory_instruction(test, bits);
            let width = u64::from(bits / 8);
            let name = if test { "test" } else { "cmp" };

            // An operand wholly inside a no-access page.
            let target = DATA + 0x40;
            let memory = fault_memory(&instruction, |memory| {
                memory
                    .map_region(DATA, REGION_LEN, MemoryPermissions::NO_ACCESS)
                    .expect("no-access region");
            });
            verify_precise_fault(
                &memory,
                target,
                ExitReason::MemoryFault(MemoryError::PermissionViolation {
                    address: target,
                    required: "read",
                }),
                &format!("{name} {bits}-bit no-access"),
            );

            // An operand that crosses from readable memory into a no-access
            // page: the fault names the first protected byte.
            if width > 1 {
                let boundary = DATA + REGION_LEN as u64;
                let target = boundary + 1 - width;
                let memory = fault_memory(&instruction, |memory| {
                    memory
                        .map_region(DATA, REGION_LEN, MemoryPermissions::READ_ONLY)
                        .expect("data region");
                    memory
                        .map_region(boundary, REGION_LEN, MemoryPermissions::NO_ACCESS)
                        .expect("guard region");
                });
                verify_precise_fault(
                    &memory,
                    target,
                    ExitReason::MemoryFault(MemoryError::PermissionViolation {
                        address: boundary,
                        required: "read",
                    }),
                    &format!("{name} {bits}-bit crossing no-access"),
                );
            }
        }
    }
}

#[test]
fn noncanonical_addresses_raise_gp_before_any_memory_lookup() {
    // Bit 47 set with bits 63:48 clear is the first non-canonical low half
    // address (Intel SDM Vol. 1, 3.3.7.1). Nothing is mapped there; only a
    // #GP raised before the lookup proves the canonical check came first.
    const NONCANONICAL: u64 = 0x0000_8000_0000_0000;
    for bits in [8u32, 16, 32, 64] {
        for test in [false, true] {
            let instruction = scalar_memory_instruction(test, bits);
            let memory = fault_memory(&instruction, |_| {});
            verify_precise_fault(
                &memory,
                NONCANONICAL,
                ExitReason::GeneralProtection,
                &format!(
                    "{} {bits}-bit non-canonical",
                    if test { "test" } else { "cmp" }
                ),
            );
        }
    }
}

#[test]
fn a_qword_operand_spanning_two_readable_regions_compares_correctly() {
    // The operand starts four bytes before the end of one region and ends
    // four bytes into the next. Both halves are readable, so the split read
    // assembles the full qword and equality sets ZF.
    let boundary = DATA + REGION_LEN as u64;
    let target = boundary - 4;
    let value = 0xA5C3_5A3C_1234_5678u64;
    // cmp qword ptr [rbx], rdx: 48 39 13.
    let code = [0x48, 0x39, 0x13, 0xF4];
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, &code)
        .expect("code region");
    let mut low = vec![0u8; REGION_LEN];
    low[REGION_LEN - 4..].copy_from_slice(&value.to_le_bytes()[..4]);
    let mut high = vec![0u8; REGION_LEN];
    high[..4].copy_from_slice(&value.to_le_bytes()[4..]);
    memory
        .map_region_with_data(DATA, REGION_LEN, MemoryPermissions::READ_ONLY, &low)
        .expect("low region");
    memory
        .map_region_with_data(boundary, REGION_LEN, MemoryPermissions::READ_ONLY, &high)
        .expect("high region");

    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr = [POISON; 16];
    cpu.set(Register::Rbx, target);
    cpu.set(Register::Rdx, value);

    let mut expected = cpu.clone();
    expected.flags = cmp_flags(value, value, 64);
    expected.rip = ENTRY + 4;

    let outcomes = run_both(&cpu, &memory);
    for (engine, result) in [
        ("interpreter", &outcomes.interpreted),
        ("dbt", &outcomes.translated),
    ] {
        assert_eq!(result.exit_reason, ExitReason::Halt, "{engine} exit");
        assert_eq!(&result.final_state, &expected, "{engine} final state");
    }
}

#[test]
fn wide_memory_forms_run_as_native_arm64_not_interpreter_fallbacks() {
    // The translator lowers 32- and 64-bit CMP/TEST memory forms to ARM64.
    // If that lowering is ever lost the architectural tests above keep
    // passing silently through the shared-semantics fallback, so pin the
    // native coverage down directly: zero fallback instructions, and the
    // eight- and sixteen-bit forms execute through the documented fallback.
    for bits in [32u32, 64] {
        for form in [
            Form::CmpRmReg,
            Form::CmpRegRm,
            Form::CmpRmImm81,
            Form::CmpRmImm83,
            Form::TestRmReg,
            Form::TestRmImm,
        ] {
            let (memory_operand, other, raw) = operand_values(form, bits, 1, false);
            let instruction = memory_instruction(form, bits, raw);
            let code = branch_program(&instruction);
            let data = data_image(memory_operand, bits);
            let cpu = matrix_cpu(bits, other);
            let mut memory = memory_with(&code, &data);
            let mut engine = DbtEngine::new(1024 * 1024).expect("JIT memory");
            let mut translated_cpu = cpu.clone();
            let result = engine.run(&mut translated_cpu, &mut memory, 16);
            assert_eq!(result.exit_reason, ExitReason::Halt, "{form:?} {bits}-bit");
            assert_eq!(
                engine.metrics.interpreter_fallbacks, 0,
                "{form:?} {bits}-bit fell back to the interpreter"
            );
            assert_eq!(
                engine.metrics.instructions_executed, 4,
                "{form:?} {bits}-bit instruction count"
            );
        }
    }
}

#[test]
fn narrow_memory_forms_use_the_shared_semantics_fallback() {
    // 8- and 16-bit forms are not lowered yet; each run must account for
    // exactly one fallback instruction (the compare itself) rather than
    // silently dropping coverage or natively lowering them wrong.
    for bits in [8u32, 16] {
        let forms: &[Form] = if bits == 8 {
            &[
                Form::CmpRmReg,
                Form::CmpRegRm,
                Form::CmpRmImm80,
                Form::TestRmReg,
                Form::TestRmImm,
            ]
        } else {
            &[
                Form::CmpRmReg,
                Form::CmpRegRm,
                Form::CmpRmImm81,
                Form::CmpRmImm83,
                Form::TestRmReg,
                Form::TestRmImm,
            ]
        };
        for &form in forms {
            let (memory_operand, other, raw) = operand_values(form, bits, 1, false);
            let instruction = memory_instruction(form, bits, raw);
            let code = branch_program(&instruction);
            let data = data_image(memory_operand, bits);
            let cpu = matrix_cpu(bits, other);
            let mut memory = memory_with(&code, &data);
            let mut engine = DbtEngine::new(1024 * 1024).expect("JIT memory");
            let mut translated_cpu = cpu.clone();
            let result = engine.run(&mut translated_cpu, &mut memory, 16);
            assert_eq!(result.exit_reason, ExitReason::Halt, "{bits}-bit cmp");
            assert_eq!(
                engine.metrics.interpreter_fallbacks, 1,
                "{bits}-bit {form:?} memory form should use exactly one fallback instruction"
            );
        }
    }
}

#[test]
fn both_engines_agree_with_each_other_on_representative_matrix_cases() {
    // Belt and braces on top of the architectural assertions: the two engines
    // must not only be right but identical, byte for byte, in memory too.
    for bits in [8u32, 16, 32, 64] {
        let (memory_operand, other, raw) = operand_values(Form::CmpRmReg, bits, 3, false);
        let instruction = memory_instruction(Form::CmpRmReg, bits, raw);
        let code = branch_program(&instruction);
        let data = data_image(memory_operand, bits);
        let cpu = matrix_cpu(bits, other);
        let memory = memory_with(&code, &data);
        let outcomes = run_both(&cpu, &memory);
        assert_eq!(
            outcomes.interpreted.final_state, outcomes.translated.final_state,
            "{bits}-bit final states diverge"
        );
        let mut first = vec![0u8; REGION_LEN];
        let mut second = vec![0u8; REGION_LEN];
        outcomes
            .interpreted_memory
            .read_bytes(DATA, &mut first)
            .expect("interpreted memory");
        outcomes
            .translated_memory
            .read_bytes(DATA, &mut second)
            .expect("translated memory");
        assert_eq!(first, second, "{bits}-bit memory images diverge");
    }
}
