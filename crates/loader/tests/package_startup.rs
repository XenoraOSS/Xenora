// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end startup of an original multi-DLL package.
//!
//! The three images are built from `fixtures/pe/multidll/*.c` by the native
//! ARM64 LLVM toolchain. The consumer imports a function and a data variable
//! from two package DLLs and `ExitProcess` from kernel32. Its exit status is a
//! checksum of the function results, the data import and both provider
//! `DllMain` markers, so a missing binding or an inverted initialization order
//! changes the observed status instead of passing silently.

use std::sync::Arc;

use xenora_loader::imports::ServiceStatus;
use xenora_loader::module_graph::PackageModule;
use xenora_loader::module_layout::ModuleLayoutArena;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::{
    ExitReason, ImportDisposition, ImportFailure, ImportRegistry, PackageStartupError,
    PackageStartupLayout, run_package_startup,
};

const CONSUMER: &[u8] = include_bytes!("../../../fixtures/pe/multidll/multidll-consumer.exe");
const CHAIN: &[u8] = include_bytes!("../../../fixtures/pe/multidll/xenora_chain.dll");
const CHAIN_NO_QUAD: &[u8] =
    include_bytes!("../../../fixtures/pe/multidll/xenora_chain_no_quad.dll");
const MATH: &[u8] = include_bytes!("../../../fixtures/pe/multidll/xenora_math.dll");

/// 42 from `XenoraAdd`, 8 from `XenoraQuad` through the chain, 10 from the
/// `XenoraScale` data import, 1 from the leaf `DllMain`, 2 from the middle
/// `DllMain`, 7 from the leaf TLS callback.
const EXPECTED_EXIT: u32 = 70;

fn layout() -> PackageStartupLayout {
    PackageStartupLayout {
        arena: ModuleLayoutArena {
            base: 0x1_0000_0000,
            size: 0x1000_0000,
        },
        stack_top: 0x2_0001_0000,
        thread_tls_base: 0x3_0000_0000,
    }
}

fn services() -> Arc<ServicesState> {
    Arc::new(ServicesState::new(Arc::new(MockClock::new(0))))
}

fn registry() -> ImportRegistry {
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, services());
    registry
}

#[test]
fn original_multi_dll_package_runs_its_entry_point() {
    // The consumer is first on purpose: dependency order must come from the
    // import graph, not from the caller's list order.
    let package = [
        PackageModule {
            file_name: "multidll-consumer.exe",
            bytes: CONSUMER,
        },
        PackageModule {
            file_name: "xenora_chain.dll",
            bytes: CHAIN,
        },
        PackageModule {
            file_name: "xenora_math.dll",
            bytes: MATH,
        },
    ];
    let result = run_package_startup(&package, layout(), 0, registry(), 2_000_000, None)
        .expect("the explicit package starts");

    assert_eq!(result.diagnostics.unresolved_imports().count(), 0);
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::ProcessExit(EXPECTED_EXIT)
    );
    assert_eq!(
        result
            .diagnostics
            .failure_counts()
            .into_iter()
            .filter(|(_, count)| *count != 0)
            .count(),
        0
    );

    // The leaf TLS callback ran before the leaf DllMain, and both provider
    // DllMains ran leaf-before-middle.
    let attach_phases: Vec<(&str, xenora_loader::InitializationPhase)> = result
        .diagnostics
        .initialization
        .iter()
        .map(|record| (record.file_name.as_str(), record.phase))
        .collect();
    let tls_position = attach_phases
        .iter()
        .position(|(file, phase)| {
            *file == "xenora_math.dll"
                && matches!(
                    phase,
                    xenora_loader::InitializationPhase::TlsCallback { .. }
                )
        })
        .expect("leaf TLS callback record");
    let math_position = attach_phases
        .iter()
        .position(|(file, phase)| {
            *file == "xenora_math.dll" && *phase == xenora_loader::InitializationPhase::DllMain
        })
        .expect("leaf DllMain record");
    let chain_position = attach_phases
        .iter()
        .position(|(file, phase)| {
            *file == "xenora_chain.dll" && *phase == xenora_loader::InitializationPhase::DllMain
        })
        .expect("middle DllMain record");
    assert!(tls_position < math_position);
    assert!(math_position < chain_position);

    // Orderly teardown detaches the middle provider before the leaf.
    let detach: Vec<&str> = result
        .diagnostics
        .shutdown
        .iter()
        .filter(|record| record.phase == xenora_loader::InitializationPhase::DllDetach)
        .map(|record| record.file_name.as_str())
        .collect();
    assert_eq!(detach, ["xenora_chain.dll", "xenora_math.dll"]);
    assert!(result.diagnostics.shutdown.iter().all(|record| !matches!(
        record.outcome,
        xenora_loader::InitializationOutcome::Fault(_)
    )));

    let math = result
        .diagnostics
        .modules
        .iter()
        .find(|module| module.file_name == "xenora_math.dll")
        .expect("math module report");
    assert_eq!(math.imports, 0);
    let chain = result
        .diagnostics
        .modules
        .iter()
        .find(|module| module.file_name == "xenora_chain.dll")
        .expect("chain module report");
    assert_eq!(chain.imports, 2);
    assert_eq!(chain.resolved_imports, 2);
    let consumer = result
        .diagnostics
        .modules
        .iter()
        .find(|module| module.file_name == "multidll-consumer.exe")
        .expect("consumer module report");
    assert_eq!(consumer.imports, 7);
    assert_eq!(consumer.resolved_imports, 7);
    assert_eq!(consumer.dependencies.len(), 2);

    // Package exports are bound to provider addresses, not to host services.
    let package_imports = result
        .diagnostics
        .imports
        .iter()
        .filter(|import| {
            matches!(
                import.disposition,
                xenora_loader::ImportDisposition::Package { .. }
            )
        })
        .count();
    // Eight package imports across both consumers; the ninth import is the
    // kernel32 service.
    assert_eq!(package_imports, 8);
}

#[test]
fn missing_provider_fails_closed_with_structured_diagnostics() {
    let package = [
        PackageModule {
            file_name: "multidll-consumer.exe",
            bytes: CONSUMER,
        },
        PackageModule {
            file_name: "xenora_math.dll",
            bytes: MATH,
        },
    ];
    let error = run_package_startup(&package, layout(), 0, registry(), 2_000_000, None)
        .expect_err("a package without its middle provider must not start");
    let diagnostics = error
        .diagnostics()
        .unwrap_or_else(|| panic!("unresolved imports keep structured evidence: {error:?}"));
    let unresolved: Vec<_> = diagnostics.unresolved_imports().collect();
    assert!(!unresolved.is_empty());
    assert!(
        unresolved
            .iter()
            .any(|import| import.requested_module == "xenora_chain.dll")
    );
    let counts = diagnostics.failure_counts();
    assert!(
        counts
            .iter()
            .any(|(name, count)| *name == "missing-module" && *count != 0)
    );
    assert!(matches!(error, PackageStartupError::UnresolvedImports(_)));
}

#[test]
fn package_exports_take_precedence_over_same_named_host_services() {
    // A host registration with the exact identity of a package export must
    // not win. Both handlers would return a value that changes the consumer's
    // checksum, so the expected exit status also proves they never ran.
    let mut imports = registry();
    imports.register("xenora_math.dll", "XenoraAdd", |_, _, _| Ok(0xDEAD_BEEF));
    imports.register("xenora_chain.dll", "XenoraQuad", |_, _, _| Ok(0xDEAD_BEEF));
    let package = [
        PackageModule {
            file_name: "multidll-consumer.exe",
            bytes: CONSUMER,
        },
        PackageModule {
            file_name: "xenora_chain.dll",
            bytes: CHAIN,
        },
        PackageModule {
            file_name: "xenora_math.dll",
            bytes: MATH,
        },
    ];
    let result = run_package_startup(&package, layout(), 0, imports, 2_000_000, None)
        .expect("the explicit package still starts");

    assert_eq!(
        result.execution.exit_reason,
        ExitReason::ProcessExit(EXPECTED_EXIT)
    );
    for (module, symbol) in [
        ("xenora_math.dll", "XenoraAdd"),
        ("xenora_chain.dll", "XenoraQuad"),
    ] {
        let import = result
            .diagnostics
            .imports
            .iter()
            .find(|import| import.requested_module == module && import.symbol.to_string() == symbol)
            .unwrap_or_else(|| panic!("import {module}!{symbol}"));
        assert!(
            matches!(import.disposition, ImportDisposition::Package { .. }),
            "{module}!{symbol} bound to a host service: {:?}",
            import.disposition
        );
    }
}

#[test]
fn a_present_provider_without_the_exported_symbol_fails_closed() {
    // The control DLL is the same middle provider built without XenoraQuad.
    // Its presence must not be treated as a successful binding.
    let package = [
        PackageModule {
            file_name: "multidll-consumer.exe",
            bytes: CONSUMER,
        },
        PackageModule {
            file_name: "xenora_chain.dll",
            bytes: CHAIN_NO_QUAD,
        },
        PackageModule {
            file_name: "xenora_math.dll",
            bytes: MATH,
        },
    ];
    let error = run_package_startup(&package, layout(), 0, registry(), 2_000_000, None)
        .expect_err("a provider missing an imported export must not start");
    let diagnostics = error
        .diagnostics()
        .unwrap_or_else(|| panic!("unresolved imports keep structured evidence: {error:?}"));
    assert!(
        diagnostics.unresolved_imports().any(|import| {
            import.requested_module == "xenora_chain.dll"
                && matches!(
                    import.disposition,
                    ImportDisposition::Unresolved(ImportFailure::MissingExport { .. })
                )
        }),
        "{:?}",
        diagnostics.unresolved_imports().collect::<Vec<_>>()
    );
    assert!(
        diagnostics
            .failure_counts()
            .iter()
            .any(|(name, count)| *name == "missing-export" && *count != 0)
    );
    assert!(matches!(error, PackageStartupError::UnresolvedImports(_)));
}

#[test]
fn an_explicit_stub_service_fails_closed() {
    let mut imports = registry();
    imports.set_status("kernel32.dll", "ExitProcess", ServiceStatus::Stub);
    let package = [
        PackageModule {
            file_name: "multidll-consumer.exe",
            bytes: CONSUMER,
        },
        PackageModule {
            file_name: "xenora_chain.dll",
            bytes: CHAIN,
        },
        PackageModule {
            file_name: "xenora_math.dll",
            bytes: MATH,
        },
    ];
    let error = run_package_startup(&package, layout(), 0, imports, 2_000_000, None)
        .expect_err("a stub service must not be reported as a resolved import");
    let diagnostics = error
        .diagnostics()
        .unwrap_or_else(|| panic!("unresolved imports keep structured evidence: {error:?}"));
    assert!(diagnostics.unresolved_imports().any(|import| {
        matches!(
            import.disposition,
            ImportDisposition::Unresolved(ImportFailure::StubService { .. })
        )
    }));
    assert!(
        diagnostics
            .failure_counts()
            .iter()
            .any(|(name, count)| *name == "stub" && *count != 0)
    );
    assert!(matches!(error, PackageStartupError::UnresolvedImports(_)));
}
