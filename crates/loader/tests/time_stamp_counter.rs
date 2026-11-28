// SPDX-License-Identifier: GPL-3.0-or-later

//! The guest time-stamp counter and RDTSC (issue #973).
//!
//! RDTSC loads the counter into EDX:EAX: EDX takes the high-order 32 bits and
//! EAX the low-order 32, and the high-order 32 bits of RAX and RDX are cleared
//! (Intel SDM Vol. 2B, "RDTSC"). The counter itself is Xenora's own, documented
//! in `docs/cpu/instruction-registry.md`: one per guest address space, 0 when
//! that is created, one count per RDTSC, never a host timer. The expected
//! values below are worked out by hand from those two statements. Encodings
//! were assembled with `clang -target x86_64-apple-macos` and read with
//! `objdump`.

use xenora_loader::cpu::{CpuState, ExitReason, Interpreter, RFlags};
use xenora_loader::dbt::{DbtEngine, is_native};
use xenora_loader::memory::{GuestMemory, MemoryPermissions};
use xenora_loader::x86_decode::decode;

const ENTRY: u64 = 0x1000;
const RAX: usize = 0;
const RDX: usize = 2;
const R8: usize = 8;
const R9: usize = 9;
const R10: usize = 10;
const R11: usize = 11;

const RDTSC: [u8; 2] = [0x0F, 0x31];

//       rdtsc
//       mov r8d, eax
//       mov r9d, edx
//       rdtsc
//       mov r10d, eax
//       mov r11d, edx
//       rdtsc
//       hlt
const THREE_READS: [u8; 19] = [
    0x0F, 0x31, 0x41, 0x89, 0xC0, 0x41, 0x89, 0xD1, 0x0F, 0x31, 0x41, 0x89, 0xC2, 0x41, 0x89, 0xD3,
    0x0F, 0x31, 0xF4,
];

const SET: RFlags = RFlags {
    cf: true,
    zf: true,
    sf: true,
    of: true,
    pf: true,
    af: true,
};

fn image(code: &[u8], counter: u64) -> GuestMemory {
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, code)
        .unwrap();
    memory.advance_time_stamp_counter_to(counter);
    memory
}

/// Every general register holds a value with both halves set, and every flag is set.
fn noisy_cpu() -> CpuState {
    let mut cpu = CpuState::new(ENTRY, 0);
    for (index, register) in cpu.gpr.iter_mut().enumerate() {
        *register = 0xA5A5_5A5A_0000_0000 | (index as u64) << 8 | 0xEE;
    }
    cpu.flags = SET;
    cpu
}

/// Final state, exit reason and counter after the interpreter, then the DBT,
/// each from the same counter in a fresh address space.
fn both(
    cpu: &CpuState,
    code: &[u8],
    counter: u64,
    budget: usize,
) -> [(&'static str, CpuState, ExitReason, u64); 2] {
    let mut memory = image(code, counter);
    let interpreted = Interpreter::new(cpu.clone(), &mut memory, budget).run();
    let interpreter_counter = memory.time_stamp_counter();
    let mut memory = image(code, counter);
    let mut state = cpu.clone();
    let translated = DbtEngine::new(64 * 1024)
        .unwrap()
        .run(&mut state, &mut memory, budget);
    [
        (
            "interpreter",
            interpreted.final_state,
            interpreted.exit_reason,
            interpreter_counter,
        ),
        (
            "DBT",
            translated.final_state,
            translated.exit_reason,
            memory.time_stamp_counter(),
        ),
    ]
}

/// What the three reads of [`THREE_READS`] leave from a noisy CPU when the
/// counter starts at `0x7_FFFF_FFFE`.
fn three_reads_across_the_low_word() -> CpuState {
    let mut expected = noisy_cpu();
    // 0x7_FFFF_FFFE: the first read, saved in R8D and R9D.
    expected.gpr[R8] = 0xFFFF_FFFE;
    expected.gpr[R9] = 7;
    // 0x7_FFFF_FFFF: the last value with this high half.
    expected.gpr[R10] = 0xFFFF_FFFF;
    expected.gpr[R11] = 7;
    // 0x8_0000_0000: the low half rolled over into the high half.
    expected.gpr[RAX] = 0;
    expected.gpr[RDX] = 8;
    expected.rip = ENTRY + THREE_READS.len() as u64;
    expected
}

#[test]
fn rdtsc_crosses_the_low_word_rollover_in_both_engines() {
    let cpu = noisy_cpu();
    let expected = three_reads_across_the_low_word();
    for (engine, state, exit, counter) in both(&cpu, &THREE_READS, 0x7_FFFF_FFFE, 16) {
        assert_eq!(exit, ExitReason::Halt, "{engine}");
        assert_eq!(state, expected, "{engine}");
        // Three reads counted, so the next one would see 0x8_0000_0001.
        assert_eq!(counter, 0x8_0000_0001, "{engine}");
    }
    // The DBT does not lower RDTSC: both engines run the same shared code.
    let insn = decode(ENTRY, |at| {
        RDTSC.get((at - ENTRY) as usize).copied().ok_or(())
    })
    .unwrap();
    assert!(!is_native(&insn));
}

#[test]
fn rdtsc_crosses_the_rollover_one_dispatch_at_a_time() {
    // Each instruction in a run of its own, so the DBT translates and enters a
    // block per instruction and the counter must carry between them.
    let expected = three_reads_across_the_low_word();
    let mut memory = image(&THREE_READS, 0x7_FFFF_FFFE);
    let mut cpu = noisy_cpu();
    let mut exit = ExitReason::StepLimitExceeded;
    while exit == ExitReason::StepLimitExceeded {
        let result = Interpreter::new(cpu, &mut memory, 1).run();
        cpu = result.final_state;
        exit = result.exit_reason;
    }
    assert_eq!(exit, ExitReason::Halt, "interpreter");
    assert_eq!(cpu, expected, "interpreter");
    assert_eq!(memory.time_stamp_counter(), 0x8_0000_0001, "interpreter");

    let mut memory = image(&THREE_READS, 0x7_FFFF_FFFE);
    let mut cpu = noisy_cpu();
    let mut engine = DbtEngine::new(64 * 1024).unwrap();
    let mut exit = ExitReason::StepLimitExceeded;
    while exit == ExitReason::StepLimitExceeded {
        exit = engine.run(&mut cpu, &mut memory, 1).exit_reason;
    }
    assert_eq!(exit, ExitReason::Halt, "DBT");
    assert_eq!(cpu, expected, "DBT");
    assert_eq!(memory.time_stamp_counter(), 0x8_0000_0001, "DBT");
}

#[test]
fn a_new_address_space_reads_zero_first() {
    assert_eq!(GuestMemory::new().time_stamp_counter(), 0);
    let cpu = noisy_cpu();
    let mut expected = cpu.clone();
    expected.gpr[RAX] = 0;
    expected.gpr[RDX] = 0;
    expected.rip = ENTRY + 2;
    for (engine, state, exit, counter) in both(&cpu, &RDTSC, 0, 1) {
        assert_eq!(exit, ExitReason::StepLimitExceeded, "{engine}");
        // Only RAX, RDX and RIP change; no flag does.
        assert_eq!(state, expected, "{engine}");
        assert_eq!(counter, 1, "{engine}");
    }
}

#[test]
fn the_counter_belongs_to_the_address_space_not_to_a_register_state() {
    // Exception dispatch puts back register states saved earlier, and every
    // guest thread has a state of its own. Neither repeats or rewinds a value:
    // the counter is shared by every state that runs in the address space.
    let first = noisy_cpu();
    let mut other = noisy_cpu();
    other.gpr[R8] = 0x1234;

    let mut memory = image(&RDTSC, 41);
    let mut read = Vec::new();
    for cpu in [&first, &first, &other] {
        let state = Interpreter::new(cpu.clone(), &mut memory, 1)
            .run()
            .final_state;
        read.push(state.gpr[RDX] << 32 | state.gpr[RAX]);
    }
    assert_eq!(read, [41, 42, 43], "interpreter");

    let mut memory = image(&RDTSC, 41);
    let mut engine = DbtEngine::new(64 * 1024).unwrap();
    let mut read = Vec::new();
    for cpu in [&first, &first, &other] {
        let mut state = cpu.clone();
        engine.run(&mut state, &mut memory, 1);
        read.push(state.gpr[RDX] << 32 | state.gpr[RAX]);
    }
    assert_eq!(read, [41, 42, 43], "DBT");
}

#[test]
fn the_counter_wraps_to_zero_after_its_last_value() {
    // A local rule: the counter adds one modulo 2^64. At one read per
    // nanosecond, reaching the wrap from 0 would take about 584 years.
    let cpu = noisy_cpu();
    let mut expected = cpu.clone();
    expected.gpr[RAX] = 0xFFFF_FFFF;
    expected.gpr[RDX] = 0xFFFF_FFFF;
    expected.rip = ENTRY + 2;
    for (engine, state, _, counter) in both(&cpu, &RDTSC, u64::MAX, 1) {
        assert_eq!(state, expected, "{engine}");
        assert_eq!(counter, 0, "{engine}");
    }
}

#[test]
fn advancing_never_moves_the_counter_backward() {
    let mut memory = GuestMemory::new();
    memory.advance_time_stamp_counter_to(10);
    assert_eq!(memory.time_stamp_counter(), 10);
    memory.advance_time_stamp_counter_to(3);
    assert_eq!(memory.time_stamp_counter(), 10);
    memory.advance_time_stamp_counter_to(10);
    assert_eq!(memory.time_stamp_counter(), 10);
    memory.advance_time_stamp_counter_to(0x1_0000_0000);
    assert_eq!(memory.time_stamp_counter(), 0x1_0000_0000);
}

#[test]
fn a_guest_reads_the_counter_after_testing_the_tsc_bit() {
    //       mov eax, 1
    //       cpuid
    //       bt edx, 4         ; TSC
    //       jnc done
    //       rdtsc
    // done: hlt
    let code = [
        0xB8, 0x01, 0x00, 0x00, 0x00, 0x0F, 0xA2, 0x0F, 0xBA, 0xE2, 0x04, 0x73, 0x02, 0x0F, 0x31,
        0xF4,
    ];
    let cpu = CpuState::new(ENTRY, 0);
    for (engine, state, exit, counter) in both(&cpu, &code, 0x1234_5678_9ABC_DEF0, 16) {
        assert_eq!(exit, ExitReason::Halt, "{engine}");
        // A skipped RDTSC would leave CPUID's leaf 1 EAX (0) and EDX.
        assert_eq!(state.gpr[RAX], 0x9ABC_DEF0, "{engine}");
        assert_eq!(state.gpr[RDX], 0x1234_5678, "{engine}");
        assert_eq!(counter, 0x1234_5678_9ABC_DEF1, "{engine}");
        assert_eq!(state.rip, ENTRY + code.len() as u64, "{engine}");
    }
}
