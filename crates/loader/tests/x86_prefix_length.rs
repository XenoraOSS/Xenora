// SPDX-License-Identifier: GPL-3.0-or-later

//! Regression tests for bounded x86-64 prefix decoding and the architectural
//! instruction-length limit, enforced across the static PE probe, the software
//! interpreter and the native DBT (issue #69).
//!
//! The architectural limit is 15 bytes. The production constant is
//! `xenora_loader::x86_decode::MAX_INSTRUCTION_LEN`; the value is mirrored here
//! so the tests fail against the pre-fix implementation rather than only
//! compiling.

use xenora_loader::cpu::{CpuState, ExitReason, Interpreter};
use xenora_loader::dbt::DbtEngine;
use xenora_loader::memory::{GuestMemory, MemoryPermissions};
use xenora_loader::probe::{InstructionRecord, ProbeConfig, probe_pe_bytes};

const MAX_INSTRUCTION_LEN: usize = 15;
const ENTRY: u64 = 0x1000;

/// Minimal synthetic PE32+ image with one executable section whose code
/// starts at RVA `ENTRY`. Modelled on the fixtures in `probe_static.rs`.
fn synthetic_pe_with_code(code: &[u8]) -> Vec<u8> {
    let mut pe = vec![0u8; 1024];
    pe[0] = b'M';
    pe[1] = b'Z';
    pe[0x3c] = 0x80;
    pe[0x80..0x84].copy_from_slice(b"PE\0\0");
    let coff = 0x84;
    pe[coff..coff + 2].copy_from_slice(&0x8664u16.to_le_bytes());
    pe[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes());
    pe[coff + 16..coff + 18].copy_from_slice(&240u16.to_le_bytes());
    let opt = coff + 20; // 0x98
    pe[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
    pe[opt + 16..opt + 20].copy_from_slice(&(ENTRY as u32).to_le_bytes()); // entry point RVA
    pe[opt + 24..opt + 32].copy_from_slice(&0x140000000u64.to_le_bytes()); // image base
    pe[opt + 32..opt + 36].copy_from_slice(&0x1000u32.to_le_bytes()); // section alignment
    pe[opt + 36..opt + 40].copy_from_slice(&0x200u32.to_le_bytes()); // file alignment
    pe[opt + 56..opt + 60].copy_from_slice(&0x2000u32.to_le_bytes()); // size of image
    pe[opt + 60..opt + 64].copy_from_slice(&0x400u32.to_le_bytes()); // size of headers
    pe[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes()); // number of rva and sizes

    let sec = opt + 240;
    pe[sec..sec + 5].copy_from_slice(b".text");
    pe[sec + 8..sec + 12].copy_from_slice(&0x200u32.to_le_bytes()); // virtual size
    pe[sec + 12..sec + 16].copy_from_slice(&(ENTRY as u32).to_le_bytes()); // virtual address
    pe[sec + 16..sec + 20].copy_from_slice(&0x200u32.to_le_bytes()); // raw data size
    pe[sec + 20..sec + 24].copy_from_slice(&0x200u32.to_le_bytes()); // raw data pointer
    let chars: u32 = 0x6000_0020; // CODE | EXECUTE | READ
    pe[sec + 36..sec + 40].copy_from_slice(&chars.to_le_bytes());

    pe[0x200..0x200 + code.len()].copy_from_slice(code);
    pe
}

fn probe_records(code: &[u8]) -> Vec<InstructionRecord> {
    let pe = synthetic_pe_with_code(code);
    probe_pe_bytes(&pe, &ProbeConfig::default())
        .expect("probe should succeed")
        .instruction_records
}

#[test]
fn probe_reports_overlong_segment_prefix_run() {
    let records = probe_records(&[0x65; 16]);
    assert!(
        records.iter().any(|r| r.mnemonic == "overlong prefix run"),
        "expected an overlong-prefix record, got {records:?}"
    );
    assert!(
        records
            .iter()
            .all(|r| (1..=MAX_INSTRUCTION_LEN).contains(&r.bytes.len())),
        "every record stays within the architectural limit: {records:?}"
    );
}

#[test]
fn probe_instruction_lengths_stay_within_the_architectural_limit() {
    // With 0..=13 prefixes the decoded instruction (prefixes + `xor eax,eax`)
    // stays within the architectural 15-byte limit.
    for prefixes in 0..=13usize {
        let mut code = vec![0x65; prefixes];
        code.extend_from_slice(&[0x31, 0xc0, 0xc3]); // xor eax, eax; ret
        let records = probe_records(&code);
        assert!(
            records
                .iter()
                .all(|r| (1..=MAX_INSTRUCTION_LEN).contains(&r.bytes.len())),
            "prefixes={prefixes}: {records:?}"
        );
    }
}

/// Replicate the `cpu_decode` fuzz target's `build_image` for the minimized
/// artifact and assert the fuzz target's length invariant on every record.
#[test]
fn probe_fuzz_harness_minimized_artifact() {
    const PE_OFFSET: usize = 0x40;
    const COFF: usize = PE_OFFSET + 4;
    const OPTIONAL: usize = COFF + 20;
    const OPTIONAL_LEN: u16 = 0xf0;
    const TEXT: usize = OPTIONAL + OPTIONAL_LEN as usize;
    const HEADER_LEN: usize = 0x200;
    const TEXT_RVA: u32 = 0x1000;
    const SECTION_ALIGNMENT: u32 = 0x1000;

    // Minimized `cpu_decode` input recorded in the campaign ledger, kept inline
    // because `fuzz/artifacts/` is not tracked.
    let mut artifact = vec![0x65u8; 12];
    artifact.push(0x73);
    artifact.extend([0x65; 4]);

    let mut header = [0u8; HEADER_LEN];
    let mut write_field = |offset: usize, bytes: &[u8]| {
        header[offset..offset + bytes.len()].copy_from_slice(bytes);
    };
    write_field(0, b"MZ");
    write_field(0x3c, &(PE_OFFSET as u32).to_le_bytes());
    write_field(PE_OFFSET, b"PE\0\0");
    write_field(COFF, &0x8664u16.to_le_bytes()); // Machine
    write_field(COFF + 2, &1u16.to_le_bytes()); // NumberOfSections
    write_field(COFF + 16, &OPTIONAL_LEN.to_le_bytes());
    write_field(OPTIONAL, &0x20bu16.to_le_bytes()); // Magic
    write_field(OPTIONAL + 16, &TEXT_RVA.to_le_bytes()); // AddressOfEntryPoint
    write_field(OPTIONAL + 24, &0x1_4000_0000u64.to_le_bytes()); // ImageBase
    write_field(OPTIONAL + 32, &SECTION_ALIGNMENT.to_le_bytes());
    write_field(OPTIONAL + 36, &0x200u32.to_le_bytes()); // FileAlignment
    write_field(OPTIONAL + 108, &16u32.to_le_bytes()); // NumberOfRvaAndSizes
    write_field(TEXT, b".text");
    write_field(TEXT + 12, &TEXT_RVA.to_le_bytes()); // VirtualAddress
    write_field(TEXT + 20, &(HEADER_LEN as u32).to_le_bytes()); // PointerToRawData
    write_field(TEXT + 36, &0x6000_0020u32.to_le_bytes()); // CODE | EXECUTE | READ
    let code_size = artifact.len() as u32;
    write_field(TEXT + 8, &code_size.to_le_bytes()); // VirtualSize
    write_field(TEXT + 16, &code_size.to_le_bytes()); // SizeOfRawData

    let mut image = Vec::with_capacity(HEADER_LEN + artifact.len());
    image.extend_from_slice(&header);
    image.extend_from_slice(&artifact);

    let report = probe_pe_bytes(&image, &ProbeConfig::default()).expect("probe should succeed");
    assert!(
        !report.instruction_records.is_empty(),
        "entry is decoded at least once"
    );
    for rec in &report.instruction_records {
        assert!(
            (1..=MAX_INSTRUCTION_LEN).contains(&rec.bytes.len()),
            "fuzz-harness record: {rec:?}"
        );
    }
}

#[test]
fn probe_minimized_overlong_prefix_run_input() {
    // Minimized crash artifact found by the `cpu_decode` fuzz target:
    // 12x GS + 0x73 (JAE rel8) + 4x GS = 17 bytes, which the fuzz invariant
    // flags as a decoded instruction longer than 15 bytes.
    let mut code = vec![0x65; 12];
    code.push(0x73);
    code.extend([0x65; 4]);
    let records = probe_records(&code);
    assert!(
        records
            .iter()
            .all(|r| (1..=MAX_INSTRUCTION_LEN).contains(&r.bytes.len())),
        "all records within the limit: {records:?}"
    );
}

#[test]
fn interpreter_rejects_overlong_segment_prefix_run() {
    let mut code = vec![0x65; 16];
    code.push(0xC3);
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, &code)
        .unwrap();
    let cpu = CpuState::new(ENTRY, 0);
    let result = Interpreter::new(cpu, &mut memory, 8).run();
    assert_eq!(result.exit_reason, ExitReason::GeneralProtection);
    assert_eq!(result.instructions_executed, 0);
    assert_eq!(result.final_state.rip, ENTRY);
}

#[test]
fn dbt_rejects_overlong_segment_prefix_run() {
    let mut code = vec![0x65; 16];
    code.push(0xC3);
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(ENTRY, code.len(), MemoryPermissions::READ_EXECUTE, &code)
        .unwrap();
    let mut cpu = CpuState::new(ENTRY, 0);
    let result = DbtEngine::new(1024 * 1024)
        .expect("JIT memory allocation")
        .run(&mut cpu, &mut memory, 8);
    assert_eq!(result.exit_reason, ExitReason::GeneralProtection);
    assert_eq!(result.instructions_executed, 0);
    assert_eq!(cpu.rip, ENTRY);
}

/// Runs `prefixes` bytes of 0x66 that end exactly at the end of a mapped page,
/// with nothing mapped after it, on both engines.
fn run_prefixes_at_page_end(prefixes: usize) -> [(ExitReason, u64, u64); 2] {
    const PAGE: u64 = 0x2000;
    const PAGE_LEN: usize = 0x1000;
    let mut data = vec![0x90; PAGE_LEN];
    data[PAGE_LEN - prefixes..].fill(0x66);
    let entry = PAGE + (PAGE_LEN - prefixes) as u64;
    let mut memory = GuestMemory::new();
    memory
        .map_region_with_data(PAGE, PAGE_LEN, MemoryPermissions::READ_EXECUTE, &data)
        .unwrap();

    let interpreted = Interpreter::new(CpuState::new(entry, 0), &mut memory, 8).run();
    let mut cpu = CpuState::new(entry, 0);
    let translated = DbtEngine::new(1024 * 1024)
        .expect("JIT memory allocation")
        .run(&mut cpu, &mut memory, 8);
    [
        (
            interpreted.exit_reason,
            interpreted.final_state.rip,
            interpreted.instructions_executed,
        ),
        (
            translated.exit_reason,
            cpu.rip,
            translated.instructions_executed,
        ),
    ]
    .map(|(exit, rip, count)| (exit, rip.wrapping_sub(entry), count as u64))
}

/// Fifteen prefixes are already too long whatever follows, so the 16th byte is
/// never fetched: #GP, not a fetch fault on the unmapped page after them
/// (Intel SDM Vol. 3A, 7.15).
#[test]
fn overlong_instruction_is_gp_without_fetching_the_sixteenth_byte() {
    for (exit, rip_offset, count) in run_prefixes_at_page_end(MAX_INSTRUCTION_LEN) {
        assert_eq!(exit, ExitReason::GeneralProtection);
        assert_eq!((rip_offset, count), (0, 0));
    }
}

/// Control for the test above: fourteen prefixes leave room for an opcode, so
/// the fetch of the 15th byte is what fails.
#[test]
fn fourteen_prefixes_at_a_page_end_fault_on_the_fetch() {
    for (exit, rip_offset, count) in run_prefixes_at_page_end(MAX_INSTRUCTION_LEN - 1) {
        assert!(matches!(exit, ExitReason::MemoryFault(_)), "{exit:?}");
        assert_eq!((rip_offset, count), (0, 0));
    }
}
