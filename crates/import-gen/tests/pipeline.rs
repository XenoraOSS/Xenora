// SPDX-License-Identifier: GPL-3.0-or-later

use xenora_import_gen::{Classification, Manifest, SymbolDb, generate, parse_csv};

#[test]
fn test_builtin_db_classifies_crt_math() {
    let db = SymbolDb::builtin();
    let acos_cls = db.get("api-ms-win-crt-math-l1-1-0.dll", "acos");
    assert!(matches!(acos_cls, Some(Classification::CrtNative { .. })));

    let sqrtf_cls = db.get("api-ms-win-crt-math-l1-1-0.dll", "sqrtf");
    assert!(matches!(sqrtf_cls, Some(Classification::StandardService)));
}

#[test]
fn test_builtin_db_classifies_crt_string() {
    let db = SymbolDb::builtin();
    let strlen_cls = db.get("api-ms-win-crt-string-l1-1-0.dll", "strlen");
    assert!(matches!(strlen_cls, Some(Classification::StandardService)));

    assert!(matches!(
        db.get("api-ms-win-crt-string-l1-1-0.dll", "strtok"),
        Some(Classification::StandardService)
    ));

    // Bounds-checked copies must reach the runtime service, never an
    // unchecked libc wrapper that ignores the destination size.
    for symbol in ["strcat_s", "strcpy_s", "strncpy_s", "wcscpy_s"] {
        assert!(
            matches!(
                db.get("api-ms-win-crt-string-l1-1-0.dll", symbol),
                Some(Classification::StandardService)
            ),
            "{symbol}"
        );
    }
}

#[test]
fn test_builtin_db_case_insensitive() {
    let db = SymbolDb::builtin();
    let sym1 = db.get("API-MS-WIN-CRT-MATH-L1-1-0.DLL", "acos");
    let sym2 = db.get("api-ms-win-crt-math-l1-1-0.dll", "acos");
    assert_eq!(sym1, sym2);
}

#[test]
fn typed_csv_preserves_ordinals_and_identically_spelled_names() {
    let dir = std::env::temp_dir().join(format!("xenora-typed-imports-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("imports.csv");
    std::fs::write(
        &input,
        "module,symbol_or_ordinal,iat_rva,import_kind\n\
         \"demo.dll\",\"Ordinal#42\",0x1000,ordinal\n\
         \"DEMO.dll\",\"Ordinal#42\",0x1004,ordinal\n\
         \"demo.dll\",\"Ordinal#42\",0x1008,name\n",
    )
    .unwrap();
    let records = parse_csv(input.to_str().unwrap()).unwrap();
    let db = SymbolDb::builtin();
    let manifest = Manifest::build(&records, &db);
    assert_eq!(manifest.emission_summary().get("no-binding"), Some(&2));
    assert_eq!(manifest.generatable().count(), 0);
    let output = dir.join("manifest.csv");
    manifest.write_csv(output.to_str().unwrap()).unwrap();
    let csv = std::fs::read_to_string(output).unwrap();
    assert!(csv.contains("0x00001000,manual,no-binding,ordinal"));
    assert!(csv.contains("0x00001008,manual,no-binding,name"));
    let files = generate(&manifest, &db);
    assert!(!files.crt_rs.contains("Ordinal#42"));
    assert!(!files.stubs_rs.contains("Ordinal#42"));
    assert!(
        files
            .manifest_md
            .contains("requires a documented export mapping")
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn test_manifest_and_codegen_pipeline() {
    let csv_content = r#""module","symbol_or_ordinal","iat_rva"
"api-ms-win-crt-math-l1-1-0.dll","acos",0x1000
"custom-crt.dll","toupper",0x1008
"CUSTOM-CRT.DLL","toupper",0x1048
"api-ms-win-core-processthreads-l1-1-2.dll","CreateThread",0x1010
"custom.dll","UnknownCustomFunc",0x1018
"kernel32.dll","QueryPerformanceCounter",0x1020
"api-ms-win-core-profile-l1-1-0.dll","QueryPerformanceFrequency",0x1028
"api-ms-win-core-processthreads-l1-1-0.dll","GetThreadId",0x1030
"api-ms-win-core-handle-l1-1-0.dll","DuplicateHandle",0x1038
"api-ms-win-core-sysinfo-l1-1-0.dll","GetTickCount64",0x1040
"#;

    let tmp_dir = std::env::temp_dir().join("xenora_test_pipeline");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let csv_file = tmp_dir.join("test_imports.csv");
    std::fs::write(&csv_file, csv_content).unwrap();

    let records = parse_csv(csv_file.to_str().unwrap()).unwrap();
    assert_eq!(records.len(), 10);

    // Every built-in symbol with a generated adapter is now a runtime
    // service, so the adapter path is exercised through a project-specific
    // module classified as native.
    let mut db = SymbolDb::builtin();
    db.insert(
        "custom-crt.dll",
        "toupper",
        Classification::CrtNative {
            libc_sym: "toupper",
            note: "test-only native mapping",
        },
    );
    let manifest = Manifest::build(&records, &db);
    let summary = manifest.summary();

    assert_eq!(*summary.get("crt-native").unwrap_or(&0), 3);
    assert_eq!(*summary.get("win32-stub").unwrap_or(&0), 1);
    assert_eq!(*summary.get("manual").unwrap_or(&0), 1);

    assert_eq!(summary.get("standard-service"), Some(&5));
    assert_eq!(manifest.generatable().count(), 4);

    let emissions = manifest.emission_summary();
    assert_eq!(emissions.get("generated-adapter"), Some(&1));
    assert_eq!(emissions.get("generated-stub"), Some(&2));
    assert_eq!(emissions.get("runtime-service"), Some(&5));
    assert_eq!(emissions.get("no-binding"), Some(&1));
    let csv = tmp_dir.join("manifest.csv");
    manifest.write_csv(csv.to_str().unwrap()).unwrap();
    let csv = std::fs::read_to_string(csv).unwrap();
    assert!(csv.contains("\"acos\",0x00001000,crt-native,generated-stub"));

    let files = generate(&manifest, &db);
    assert!(files.crt_rs.contains("register_generated_crt_services"));
    assert_eq!(files.crt_rs.matches("\"toupper\"").count(), 1);
    let acos = files.crt_rs.split_once("\"acos\"").unwrap().1;
    let acos = acos.split("registry.register_with_status").next().unwrap();
    assert!(acos.contains("ServiceStatus::Stub"));
    assert!(acos.contains("Err(ExitReason::IllegalInstruction(0xFE))"));
    assert!(files.tests_rs.contains("generated_test_acos"));
    assert!(
        files
            .tests_rs
            .contains("generated_crt::register_generated_crt_services")
    );
    assert!(files.tests_rs.contains("(service.handler)"));
    assert!(files.tests_rs.contains("generated_test_createthread"));
    assert!(files.stubs_rs.contains("CreateThread"));
    assert!(!files.stubs_rs.contains("QueryPerformance"));
    assert!(!files.stubs_rs.contains("GetThreadId"));
    assert!(!files.stubs_rs.contains("DuplicateHandle"));
    assert!(!files.stubs_rs.contains("GetTickCount64"));
    assert!(files.tests_rs.contains("generated_test_toupper"));

    let out_dir = tmp_dir.join("out");
    files.write_all(out_dir.to_str().unwrap()).unwrap();
    assert!(out_dir.join("generated_crt.rs").exists());
    assert!(out_dir.join("generated_stubs.rs").exists());
    assert!(out_dir.join("generated_tests.rs").exists());
    assert!(out_dir.join("generated_manifest.md").exists());
}

#[test]
fn test_catalog_describes_windows_wide_string_signatures() {
    use xenora_import_gen::{AbiType, MetadataCatalog};

    let catalog = MetadataCatalog::builtin();
    let wide = AbiType::StringWide;
    let wide_pointer = AbiType::Pointer {
        is_const: false,
        pointee: "wchar_t".into(),
    };
    for (symbol, return_type, parameters) in [
        ("wcslen", AbiType::U64, vec![wide.clone()]),
        ("wcsnlen", AbiType::U64, vec![wide.clone(), AbiType::U64]),
        ("wcscmp", AbiType::I32, vec![wide.clone(), wide.clone()]),
        (
            "wcsncmp",
            AbiType::I32,
            vec![wide.clone(), wide.clone(), AbiType::U64],
        ),
        (
            "wcsstr",
            wide_pointer.clone(),
            vec![wide.clone(), wide.clone()],
        ),
        (
            "wcspbrk",
            wide_pointer.clone(),
            vec![wide.clone(), wide.clone()],
        ),
        ("wcsspn", AbiType::U64, vec![wide.clone(), wide.clone()]),
        ("wcscspn", AbiType::U64, vec![wide.clone(), wide.clone()]),
        (
            "wcstok",
            wide_pointer.clone(),
            vec![
                wide_pointer,
                wide,
                AbiType::Pointer {
                    is_const: false,
                    pointee: "wchar_t *".into(),
                },
            ],
        ),
    ] {
        let signature = catalog.lookup("ucrtbase.dll", symbol).unwrap();
        assert_eq!(signature.return_type, return_type);
        assert_eq!(
            signature
                .params
                .iter()
                .map(|p| p.abi_type.clone())
                .collect::<Vec<_>>(),
            parameters
        );
    }
    let copy = catalog.lookup("ucrtbase.dll", "memcpy").unwrap();
    let move_ = catalog.lookup("ucrtbase.dll", "memmove").unwrap();
    assert_eq!(move_.params, copy.params);
    assert_eq!(move_.return_type, copy.return_type);
}
