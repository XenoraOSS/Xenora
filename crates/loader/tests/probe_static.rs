// SPDX-License-Identifier: GPL-3.0-or-later

//! Tests for Xenora X14.5 non-executing PE compatibility probe.

use xenora_loader::pe::{IMAGE_SCN_CNT_CODE, IMAGE_SCN_MEM_EXECUTE, IMAGE_SCN_MEM_READ};
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes, run_probe_and_export};

fn create_synthetic_probe_pe() -> Vec<u8> {
    let mut pe = vec![0u8; 1024];
    // MZ header
    pe[0] = b'M';
    pe[1] = b'Z';
    // e_lfanew = 0x80
    pe[0x3c] = 0x80;

    let pe_sig = 0x80;
    pe[pe_sig..pe_sig + 4].copy_from_slice(b"PE\0\0");

    let coff = pe_sig + 4;
    pe[coff..coff + 2].copy_from_slice(&0x8664u16.to_le_bytes()); // AMD64
    pe[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes()); // 1 section
    pe[coff + 16..coff + 18].copy_from_slice(&240u16.to_le_bytes()); // opt header size

    let opt = coff + 20; // 0x80 + 24 = 0x98
    pe[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes()); // PE32+
    pe[opt + 16..opt + 20].copy_from_slice(&0x1000u32.to_le_bytes()); // entry point RVA = 0x1000
    pe[opt + 24..opt + 32].copy_from_slice(&0x140000000u64.to_le_bytes()); // image base
    pe[opt + 32..opt + 36].copy_from_slice(&0x1000u32.to_le_bytes()); // section alignment
    pe[opt + 36..opt + 40].copy_from_slice(&0x200u32.to_le_bytes()); // file alignment
    pe[opt + 56..opt + 60].copy_from_slice(&0x2000u32.to_le_bytes()); // size of image
    pe[opt + 60..opt + 64].copy_from_slice(&0x400u32.to_le_bytes()); // size of headers
    pe[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes()); // number of rva and sizes

    // Section header at opt + 240 = 0x98 + 240 = 0x188
    let sec = opt + 240;
    pe[sec..sec + 5].copy_from_slice(b".text");
    pe[sec + 8..sec + 12].copy_from_slice(&0x200u32.to_le_bytes()); // virtual size
    pe[sec + 12..sec + 16].copy_from_slice(&0x1000u32.to_le_bytes()); // virtual address
    pe[sec + 16..sec + 20].copy_from_slice(&0x200u32.to_le_bytes()); // raw data size
    pe[sec + 20..sec + 24].copy_from_slice(&0x200u32.to_le_bytes()); // raw data pointer (offset 0x200)
    let chars = IMAGE_SCN_CNT_CODE | IMAGE_SCN_MEM_EXECUTE | IMAGE_SCN_MEM_READ;
    pe[sec + 36..sec + 40].copy_from_slice(&chars.to_le_bytes());

    // Place instructions in .text at raw offset 0x200:
    // 48 83 ec 28 : sub rsp, 0x28
    // 48 89 5c 24 20 : mov [rsp + 0x20], rbx (SIB store, now DBT-supported)
    // c3 : ret
    pe[0x200..0x204].copy_from_slice(&[0x48, 0x83, 0xec, 0x28]);
    pe[0x204..0x209].copy_from_slice(&[0x48, 0x89, 0x5c, 0x24, 0x20]);
    pe[0x209] = 0xc3;

    pe
}

fn create_synthetic_probe_pe_with_unsupported_op() -> Vec<u8> {
    let mut pe = create_synthetic_probe_pe();
    // Replace the SIB store slot with INT3: the native DBT returns
    // ExitReason::IllegalInstruction for it, so it is genuinely unsupported
    // and must keep being reported as a startup blocker.
    pe[0x204..0x209].copy_from_slice(&[0xCC, 0x90, 0x90, 0x90, 0x90]);
    pe
}

#[test]
fn test_probe_static_analysis_does_not_execute_code() {
    let pe_bytes = create_synthetic_probe_pe();
    let config = ProbeConfig::default();

    let report = probe_pe_bytes(&pe_bytes, &config).expect("probe should succeed");

    assert_eq!(report.parsed.sections.len(), 1);
    assert_eq!(report.parsed.entry_point_rva, 0x1000);
    assert_eq!(report.parsed.image_base, 0x140000000);

    // The SIB store `mov [rsp + 0x20], rbx` at RVA 0x1004 is supported by the
    // native DBT (verified by the DBT execution test
    // `dbt::tests::sib_store_executes_through_the_native_dbt`), so the static
    // probe reports no blocker for this program.
    assert!(report.first_unsupported_dbt_insn.is_none());

    let sib_store = report
        .instruction_records
        .iter()
        .find(|record| record.rva == 0x1004)
        .expect("SIB store decoded at RVA 0x1004");
    assert_eq!(sib_store.bytes, vec![0x48, 0x89, 0x5c, 0x24, 0x20]);
    assert!(sib_store.is_supported_dbt);
    assert!(sib_store.is_supported_interp);
}

#[test]
fn test_probe_static_analysis_detects_genuinely_unsupported_instruction() {
    let pe_bytes = create_synthetic_probe_pe_with_unsupported_op();
    let report = probe_pe_bytes(&pe_bytes, &ProbeConfig::default()).expect("probe should succeed");

    let blocker = report
        .first_unsupported_dbt_insn
        .expect("blocker must be detected");
    assert_eq!(blocker.rva, 0x1004);
    assert_eq!(blocker.bytes, vec![0xCC]);
    assert_eq!(blocker.mnemonic, "int3");
}

#[test]
fn test_probe_artifact_export_creates_all_files() {
    let pe_bytes = create_synthetic_probe_pe();
    let temp_dir = std::env::temp_dir().join("xenora_probe_test_export");
    let _ = std::fs::remove_dir_all(&temp_dir);

    let config = ProbeConfig::default();
    let report =
        run_probe_and_export(&pe_bytes, &temp_dir, &config).expect("export should succeed");

    assert!(temp_dir.join("candidate_static_report.md").exists());
    assert!(temp_dir.join("candidate_static_report.json").exists());
    assert!(temp_dir.join("imports.csv").exists());
    assert!(temp_dir.join("missing_imports.csv").exists());
    assert!(temp_dir.join("cpu_coverage.csv").exists());
    assert!(temp_dir.join("runtime_gap_matrix.md").exists());
    assert!(temp_dir.join("x15_priority_backlog.md").exists());
    assert!(temp_dir.join("test_commands.txt").exists());

    assert_eq!(report.parsed.sections.len(), 1);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn probe_hashes_the_entire_input() {
    let mut bytes = create_synthetic_probe_pe();
    let config = ProbeConfig::default();
    let first = probe_pe_bytes(&bytes, &config).unwrap();
    bytes.push(42);
    let second = probe_pe_bytes(&bytes, &config).unwrap();
    assert_eq!(
        first.sha256,
        "72f97ed9d24af84928a313d4c47bfedae0c46e38fab265b3bb8d922566d816f9"
    );
    assert_eq!(
        second.sha256,
        "7da76a9dfe391814ed41c01417c00b85ab2e9d122d1b0e56e45d39ad4e81df30"
    );
}

#[test]
fn probe_distinguishes_aliases_from_unavailable_services() {
    use xenora_loader::imports::ServiceStatus;

    let mut pe = create_synthetic_probe_pe();
    // Import directory in the fixture's file-backed section at RVA 0x1040.
    pe[0x110..0x114].copy_from_slice(&0x1040u32.to_le_bytes());
    pe[0x114..0x118].copy_from_slice(&40u32.to_le_bytes());
    pe[0x240..0x244].copy_from_slice(&0x1120u32.to_le_bytes());
    pe[0x24c..0x250].copy_from_slice(&0x1090u32.to_le_bytes());
    pe[0x250..0x254].copy_from_slice(&0x1160u32.to_le_bytes());
    let dll = b"API-MS-WIN-CORE-PROCESSTHREADS-L1-1-0.DLL";
    pe[0x290..0x290 + dll.len()].copy_from_slice(dll);

    for (index, (rva, name)) in [
        (0x1180u64, "GetCurrentProcess"),
        (0x11a0u64, "TerminateProcess"),
        (0x11c0u64, "getcurrentprocess"),
    ]
    .into_iter()
    .enumerate()
    {
        let offset = 0x320 + index * 8;
        pe[offset..offset + 8].copy_from_slice(&rva.to_le_bytes());
        let name_offset = 0x200 + (rva as usize - 0x1000) + 2;
        pe[name_offset..name_offset + name.len()].copy_from_slice(name.as_bytes());
    }

    let report = probe_pe_bytes(&pe, &ProbeConfig::default()).unwrap();
    assert_eq!(report.imports.len(), 3);
    let [alias, terminate, unknown] = report.imports.as_slice() else {
        unreachable!()
    };
    assert_eq!(alias.iat_rva, 0x1160);
    assert_eq!(alias.canonical_module.as_deref(), Some("kernel32.dll"));
    assert_eq!(alias.service_status, Some(ServiceStatus::Implemented));
    // TerminateProcess is a partial service since #475.
    assert!(terminate.is_supported);
    assert_eq!(terminate.service_status, Some(ServiceStatus::Partial));
    assert!(!unknown.is_supported);
    assert_eq!(terminate.iat_rva, 0x1168);
    assert_eq!(unknown.iat_rva, 0x1170);

    let path = std::env::temp_dir().join(format!("xenora-probe-alias-{}", std::process::id()));
    run_probe_and_export(&pe, &path, &ProbeConfig::default()).unwrap();
    let csv = std::fs::read_to_string(path.join("imports.csv")).unwrap();
    assert!(csv.contains("true,implemented,true,\"kernel32.dll\",1"));
    assert!(csv.contains("true,partial,true,\"kernel32.dll\",1"));
    assert!(csv.contains("false,missing,false,\"\",1"));
    let missing = std::fs::read_to_string(path.join("missing_imports.csv")).unwrap();
    assert_eq!(missing.lines().count(), 2);
    assert!(!missing.contains("GetCurrentProcess"));
    let json = std::fs::read_to_string(path.join("candidate_static_report.json")).unwrap();
    assert!(json.contains("\"distinct_import_keys\": 3"));
    assert!(json.contains("\"canonical_registered_services\": 2"));
    assert!(json.contains("\"unresolved_imports\": 1"));
    assert!(json.contains("\"recognized_unsupported_slots\": 0"));
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn probe_distinguishes_ordinal_from_matching_symbol_text() {
    let mut pe = include_bytes!("../../../fixtures/pe/runtime-services.exe").to_vec();
    pe[0x720..0x728].copy_from_slice(&((1_u64 << 63) | 42).to_le_bytes());
    pe[0x762..0x76e].copy_from_slice(b"Ordinal#42\0\0");
    let path = std::env::temp_dir().join(format!("xenora-probe-ordinal-{}", std::process::id()));
    run_probe_and_export(&pe, &path, &ProbeConfig::default()).unwrap();
    let csv = std::fs::read_to_string(path.join("imports.csv")).unwrap();
    let json = std::fs::read_to_string(path.join("candidate_static_report.json")).unwrap();
    std::fs::remove_dir_all(path).unwrap();
    let rows: Vec<_> = csv
        .lines()
        .filter(|row| row.contains("Ordinal#42"))
        .collect();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].ends_with(",ordinal"));
    assert!(rows[1].ends_with(",name"));
    assert!(json.contains("\"distinct_import_keys\": 3"));
}

#[test]
fn trusted_runner_callback_and_report_match_the_fixture() {
    let pe = include_bytes!("../../../fixtures/pe/trusted-runner.exe");
    let path = std::env::temp_dir().join(format!("xenora-probe-trusted-{}", std::process::id()));
    let report = run_probe_and_export(pe, &path, &ProbeConfig::default()).unwrap();
    let read = |name| std::fs::read_to_string(path.join(name)).unwrap();
    let md = read("candidate_static_report.md");
    let json = read("candidate_static_report.json");
    let imports = read("imports.csv");
    std::fs::remove_dir_all(path).unwrap();

    let callback: Vec<_> = report
        .instruction_records
        .iter()
        .filter(|i| i.rva >= 0x1040)
        .collect();
    assert_eq!(
        callback.iter().map(|i| i.rva).collect::<Vec<_>>(),
        [0x1040, 0x1047, 0x104e]
    );
    assert_eq!(callback[1].bytes, [0x48, 0xc7, 0, 17, 0, 0, 0]);
    assert!(callback[1].is_supported_dbt);
    assert!(callback[1].is_supported_interp);
    assert!(report.first_unsupported_dbt_insn.is_none());
    assert!(md.contains("Sections Census (4)"));
    assert!(json.contains("\"sections_count\": 4"));
    assert!(!report.imports[0].is_supported);
    assert!(
        imports
            .lines()
            .any(|line| line.contains("fixture_draw") && line.ends_with(",true,name"))
    );
    assert!(json.contains("\"trusted_runner_binding_slots\": 1"));
    assert!(json.contains("\"unresolved_imports\": 1"));
}

#[test]
fn probe_consumes_complete_c7_operands_and_reports_their_support() {
    for encoding in [
        &[0x48, 0xc7, 0x05, 0, 0, 0, 0, 17, 0, 0, 0][..],
        &[0xc7, 0xc0, 17, 0, 0, 0],
        &[0x66, 0xc7, 0xc0, 17, 0],
        &[0xc7, 0x00, 17, 0, 0, 0],
        &[0x48, 0xc7, 0x40, 4, 17, 0, 0, 0],
        &[0x48, 0xc7, 0x04, 0x25, 0, 0, 0, 0, 17, 0, 0, 0],
    ] {
        let mut pe = create_synthetic_probe_pe();
        let end = 0x200 + encoding.len();
        pe[0x200..end].copy_from_slice(encoding);
        pe[end] = 0xcc;
        let report = probe_pe_bytes(&pe, &ProbeConfig::default()).unwrap();
        let records = report.instruction_records;
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].bytes, encoding);
        assert!(records[0].is_supported_dbt && records[0].is_supported_interp);
        assert_eq!(records[1].rva, 0x1000 + encoding.len() as u32);
        assert!(!records[1].is_supported_dbt);
    }
}

#[test]
fn exported_reports_agree_across_fixtures_and_blocker_locations() {
    let dir =
        std::env::temp_dir().join(format!("xenora-report-consistency-{}", std::process::id()));
    // (file offset, RVA) for a reachable instruction immediately after a MOV.
    for (fixture, file_offset, rva, sections, imports) in [
        (create_synthetic_probe_pe(), 0x209, 0x1009_u32, 1, 0),
        (
            include_bytes!("../../../fixtures/pe/trusted-runner.exe").to_vec(),
            0x44e,
            0x104e,
            4,
            1,
        ),
    ] {
        assert_eq!(fixture[file_offset], 0xc3);
        for padding in [None, Some(0), Some(1), Some(7)] {
            let mut pe = fixture.clone();
            let blocker = padding.map(|n| {
                pe[file_offset..file_offset + n].fill(0x90);
                pe[file_offset + n] = 0xcc;
                rva + n as u32
            });
            run_probe_and_export(&pe, &dir, &ProbeConfig::default()).unwrap();
            let read = |name| std::fs::read_to_string(dir.join(name)).unwrap();
            let md = read("candidate_static_report.md");
            let json = read("candidate_static_report.json");
            let cpu = read("cpu_coverage.csv");
            let import_csv = read("imports.csv");
            std::fs::remove_dir_all(&dir).unwrap();

            let json_field = |name: &str| {
                let prefix = format!("\"{name}\": ");
                json.lines()
                    .find_map(|line| line.trim().strip_prefix(&prefix))
                    .unwrap()
                    .trim_end_matches(',')
                    .trim_matches('"')
            };
            assert_eq!(json_field("sections_count"), sections.to_string());
            assert!(
                md.lines()
                    .any(|line| line == format!("## Sections Census ({sections})"))
            );
            assert_eq!(
                md.lines()
                    .filter(|line| line.contains(" VirtualSize: "))
                    .count(),
                sections
            );

            assert_eq!(json_field("total_imports"), imports.to_string());
            assert!(
                md.lines()
                    .any(|line| line == format!("- **Total Imported Thunks**: `{imports}`"))
            );
            assert_eq!(import_csv.lines().skip(1).count(), imports);

            let expected = blocker.map(|addr| format!("0x{addr:08x}"));
            assert_eq!(
                json_field("first_unsupported_dbt_rva"),
                expected.as_deref().unwrap_or("none")
            );
            let md_rvas: Vec<_> = md
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("- **RVA**: `")?
                        .split_once('`')
                        .map(|(addr, _)| addr)
                })
                .collect();
            assert_eq!(md_rvas, expected.iter().collect::<Vec<_>>());

            // The first columns precede quoted fields. Read the support flags from
            // the closing quote of the mnemonic, which can itself contain commas.
            let mut unsupported = Vec::new();
            let mut tail = Vec::new();
            for row in cpu.lines().skip(1) {
                let (addr, rest) = row.split_once(',').unwrap();
                let (_, fields) = rest.split_once(",\"").unwrap();
                let (bytes, mnemonic) = fields.split_once("\",\"").unwrap();
                let (_, flags) = mnemonic.split_once("\",").unwrap();
                if flags.split(',').next() == Some("false") {
                    unsupported.push(addr);
                }
                let address = u32::from_str_radix(addr.strip_prefix("0x").unwrap(), 16).unwrap();
                if address >= rva {
                    tail.push((address, bytes));
                }
            }
            assert_eq!(unsupported, expected.iter().collect::<Vec<_>>());
            let mut expected_tail = Vec::new();
            if let Some(end) = blocker {
                expected_tail.extend((rva..end).map(|addr| (addr, "90")));
                expected_tail.push((end, "cc"));
            } else {
                expected_tail.push((rva, "c3"));
            }
            assert_eq!(tail, expected_tail);
        }
    }
}
