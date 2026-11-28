// SPDX-License-Identifier: GPL-3.0-or-later

//! The 67H address-size override truncates a computed effective address to 32
//! bits and zero-extends it, before any segment base is added (Intel SDM
//! Vol. 1, section 3.3.7.1; Vol. 2A, 2.2.1.6). These original byte-level
//! tests pin the truncation, the segment ordering and the honest refusal of
//! the forms whose width or implicit registers the prefix would change.
//! Expectations are derived from the manual text, not from either engine.

use xenora_loader::cpu::{CpuState, ExitReason, Interpreter, Register};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};

const ENTRY: u64 = 0x1000;
const LOW: u64 = 0x4000;
const REGION_LEN: usize = 0x100;

fn machine(code: &[u8]) -> GuestMemory {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .expect("code");
    memory
        .map_region_with_data(
            LOW,
            REGION_LEN,
            MemoryPermissions::READ_WRITE,
            &[0xC3; REGION_LEN],
        )
        .expect("low data");
    memory
}

fn write_data(memory: &mut GuestMemory, address: u64, value: u64) {
    memory
        .write_bytes(address, &value.to_le_bytes())
        .expect("write data");
}

fn run_both(cpu: &CpuState, memory: &GuestMemory) -> [xenora_loader::cpu::ExecutionResult; 2] {
    let mut im = memory.clone();
    let interpreted = Interpreter::new(cpu.clone(), &mut im, 8).run();
    let mut tm = memory.clone();
    let mut tc = cpu.clone();
    let translated = DbtEngine::new(1024 * 1024)
        .expect("JIT")
        .run(&mut tc, &mut tm, 8);
    assert_eq!(
        interpreted.exit_reason, translated.exit_reason,
        "engines disagree on the exit"
    );
    [interpreted, translated]
}

fn cpu_with(rbx: u64) -> CpuState {
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.set(Register::Rbx, rbx);
    cpu
}

/// `67 48 8B 03` — mov rax, [rbx] with the address-size override — reads at
/// the truncated low 32 bits of RBX, where the untruncated address is unmapped.
#[test]
fn override_truncates_the_effective_address_to_32_bits() {
    // RBX carries a bit no 32-bit address could: the access must ignore it.
    let rbx = 0x1_0000_0000 | LOW;
    let code = [0x67, 0x48, 0x8B, 0x03, 0xF4];
    let mut memory = machine(&code);
    write_data(&mut memory, LOW, 0x1122_3344_5566_7788);
    let [interpreted, translated] = run_both(&cpu_with(rbx), &memory);
    for result in [interpreted, translated] {
        assert_eq!(result.exit_reason, ExitReason::Halt);
        assert_eq!(
            result.final_state.gpr[0], 0x1122_3344_5566_7788,
            "the read must come from the truncated address"
        );
    }
}

/// The same instruction without the override faults on the untruncated
/// address: the truncation above is what makes the access land low.
#[test]
fn without_the_override_the_untruncated_address_governs() {
    let rbx = 0x1_0000_0000 | LOW;
    let code = [0x48, 0x8B, 0x03, 0xF4];
    let memory = machine(&code);
    let [interpreted, translated] = run_both(&cpu_with(rbx), &memory);
    for result in [interpreted, translated] {
        assert!(
            matches!(result.exit_reason, ExitReason::MemoryFault(_)),
            "without 67H the high bits must reach the memory system: {:?}",
            result.exit_reason
        );
    }
}

/// `65 67 48 8B 03` — mov rax, gs:[rbx], the mingw CRT startup form. The
/// truncation applies to the effective address only; the segment base is
/// added afterwards at full width, so a GS base above 4 GiB survives.
#[test]
fn segment_base_is_added_after_the_truncation() {
    // GS base beyond the first 4 GiB, plus a low halfword offset in RBX.
    let gs_base = 0x1_0000_2000u64;
    let rbx = 0x1_0000_0000 | 0x40;
    let code = [0x65, 0x67, 0x48, 0x8B, 0x03, 0xF4];
    let mut memory = machine(&code);
    memory
        .map_region_with_data(
            gs_base,
            REGION_LEN,
            MemoryPermissions::READ_WRITE,
            &[0xC3; REGION_LEN],
        )
        .expect("gs data");
    write_data(&mut memory, gs_base + 0x40, 0xAABB_CCDD_EEFF_0011);
    let mut cpu = cpu_with(rbx);
    cpu.gs_base = gs_base;
    let [interpreted, translated] = run_both(&cpu, &memory);
    for result in [interpreted, translated] {
        assert_eq!(result.exit_reason, ExitReason::Halt);
        assert_eq!(
            result.final_state.gpr[0], 0xAABB_CCDD_EEFF_0011,
            "the segment base must be added at full width after truncation"
        );
    }
}

/// An index register participates before the truncation: base and index are
/// summed at full width and only the total is truncated.
#[test]
fn the_total_of_base_index_and_displacement_is_truncated() {
    // base + index*scale + disp wraps past 2^32 by exactly LOW.
    let base = 0xFFFF_F000u64;
    let code = [
        0x67, 0x48, 0x8B, 0x84, 0x98, 0x00, 0x10, 0x00, 0x00, // mov rax, [rax+rbx*4+disp]
        0xF4,
    ];
    // rax = base, rbx = 0x1000: total = 0xFFFF_F000 + 0x4000 + 0x1000 wraps.
    let mut memory = machine(&code);
    // Compute the exact landing address independently: the sum modulo 2^32.
    let total = (base).wrapping_add(0x1000u64 << 2).wrapping_add(0x1000) & 0xFFFF_FFFF;
    assert!(
        total < LOW + REGION_LEN as u64 && total + 8 <= LOW + REGION_LEN as u64,
        "the test's landing address must be inside the low region: {total:#x}"
    );
    write_data(&mut memory, total, 0xDEAD_BEEF_CAFE_F00D);
    let mut cpu = cpu_with(0x1000);
    cpu.set(Register::Rax, base);
    let [interpreted, translated] = run_both(&cpu, &memory);
    for result in [interpreted, translated] {
        assert_eq!(result.exit_reason, ExitReason::Halt);
        assert_eq!(result.final_state.gpr[0], 0xDEAD_BEEF_CAFE_F00D);
    }
}

/// `67 48 8D 03` — lea rax, [rbx] — truncates the computed address exactly
/// as a memory access would, with no memory access involved.
#[test]
fn lea_truncates_the_computed_address() {
    let rbx = 0x1_0000_0000 | 0x1234;
    let code = [0x67, 0x48, 0x8D, 0x03, 0xF4];
    let memory = machine(&code);
    let [interpreted, translated] = run_both(&cpu_with(rbx), &memory);
    for result in [interpreted, translated] {
        assert_eq!(result.exit_reason, ExitReason::Halt);
        assert_eq!(result.final_state.gpr[0], 0x1234);
    }
}

/// The override on a register-only instruction is meaningless and ignored.
#[test]
fn register_only_instructions_ignore_the_override() {
    // mov rax, rbx — no memory operand at all.
    let code = [0x67, 0x48, 0x89, 0xD8, 0xF4];
    let memory = machine(&code);
    let [interpreted, translated] = run_both(&cpu_with(0x5555_AAAA), &memory);
    for result in [interpreted, translated] {
        assert_eq!(result.exit_reason, ExitReason::Halt);
        assert_eq!(result.final_state.gpr[0], 0x5555_AAAA);
    }
}

/// A repeated override is idempotent, exactly as the other legacy prefixes.
#[test]
fn repeated_overrides_decode_and_execute() {
    let rbx = 0x1_0000_0000 | LOW;
    let code = [0x67, 0x67, 0x48, 0x8B, 0x03, 0xF4];
    let mut memory = machine(&code);
    write_data(&mut memory, LOW, 42);
    let [interpreted, translated] = run_both(&cpu_with(rbx), &memory);
    for result in [interpreted, translated] {
        assert_eq!(result.exit_reason, ExitReason::Halt);
        assert_eq!(result.final_state.gpr[0], 42);
    }
}

/// A string instruction under the override would address through ESI, EDI
/// and ECX, which is not yet implemented: it is refused at decode, not
/// answered with the wide registers.
#[test]
fn string_instructions_refuse_the_override() {
    let code = [0x67, 0xA4, 0xF4]; // movs byte
    let memory = machine(&code);
    let [interpreted, translated] = run_both(&CpuState::new(ENTRY, 0), &memory);
    for result in [interpreted, translated] {
        assert!(
            matches!(result.exit_reason, ExitReason::IllegalInstruction(0x67)),
            "the override must refuse honestly: {:?}",
            result.exit_reason
        );
    }
}

/// E0 through E3 count or test RCX by default; 67H selects ECX. JCXZ under
/// the override tests only the low 32 bits: bit 32 set is invisible to it,
/// exactly as Intel SDM Vol. 2A states the count-register rule.
#[test]
fn jecxz_tests_only_the_low_32_bits() {
    // xor eax, eax; jecxz over; inc eax; over: hlt — a taken branch leaves
    // eax zero. RCX has bit 32 set and a zero low half: the branch is taken
    // through ECX where the full RCX would fall through.
    let code = [0x31, 0xC0, 0x67, 0xE3, 0x02, 0xFF, 0xC0, 0xF4];
    let memory = machine(&code);
    let mut cpu = CpuState::new(ENTRY, 0);
    cpu.gpr[1] = 0x1_0000_0000; // RCX: high bit set, ECX zero
    let [interpreted, translated] = run_both(&cpu, &memory);
    for result in [interpreted, translated] {
        assert_eq!(result.exit_reason, ExitReason::Halt);
        assert_eq!(
            result.final_state.gpr[0] & 0xFFFF_FFFF,
            0,
            "jecxz must test ECX and branch"
        );
    }
    // Without the override the same bytes test RCX, which is nonzero here:
    // the branch falls through and the increment runs.
    let code = [0x31, 0xC0, 0xE3, 0x02, 0xFF, 0xC0, 0xF4];
    let memory = machine(&code);
    let [interpreted, translated] = run_both(&cpu, &memory);
    for result in [interpreted, translated] {
        assert_eq!(result.exit_reason, ExitReason::Halt);
        assert_eq!(
            result.final_state.gpr[0] & 0xFFFF_FFFF,
            1,
            "jrcxz must test the full RCX and fall through"
        );
    }
}

/// An accumulator offset form under the override would read a four-byte
/// offset where the decoder expects eight: refused at decode rather than
/// mis-read.
#[test]
fn offset_forms_refuse_the_override() {
    let code = [0x67, 0xA1, 0, 0, 0, 0, 0, 0, 0, 0, 0xF4]; // mov eax, [offset]
    let memory = machine(&code);
    let [interpreted, translated] = run_both(&CpuState::new(ENTRY, 0), &memory);
    for result in [interpreted, translated] {
        assert!(
            matches!(result.exit_reason, ExitReason::IllegalInstruction(0x67)),
            "the offset form must refuse honestly: {:?}",
            result.exit_reason
        );
    }
}

/// A VEX prefix after the override is invalid, as after the other legacy
/// prefixes the decoder rejects there.
#[test]
fn vex_after_the_override_is_rejected() {
    let code = [0x67, 0xC5, 0xFA, 0x10, 0x03, 0xF4]; // vmovss xmm0, [rbx]
    let memory = machine(&code);
    let [interpreted, translated] = run_both(&cpu_with(0), &memory);
    for result in [interpreted, translated] {
        assert!(
            matches!(result.exit_reason, ExitReason::IllegalInstruction(_)),
            "the VEX combination must be rejected: {:?}",
            result.exit_reason
        );
    }
}
