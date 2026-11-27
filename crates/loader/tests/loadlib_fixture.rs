// SPDX-License-Identifier: GPL-3.0-or-later

//! Static verification of the original dynamic-load fixture (#923).
//!
//! These checks prove only that the committed PE32+ images have the intended
//! shape. They do not claim that LoadLibrary, initializer execution or
//! GetProcAddress already works; that end-to-end evidence comes later.

use std::collections::BTreeSet;
use std::sync::Arc;

use xenora_loader::module_graph::{ModuleGraph, PackageModule};
use xenora_loader::module_layout::{AddressInterval, ModuleLayoutArena};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::{
    GuestMemory, ImportRegistry, ParsedPe, ProvisionedModuleStore, prepare_runtime_load,
};

const APP: &[u8] = include_bytes!("../../../fixtures/pe/loadlib/loadlib.exe");
const LIB: &[u8] = include_bytes!("../../../fixtures/pe/loadlib/xenoralib.dll");
const NO_TRIPLE: &[u8] = include_bytes!("../../../fixtures/pe/loadlib/xenoralib_no_triple.dll");
const FAIL_INIT: &[u8] = include_bytes!("../../../fixtures/pe/loadlib/xenoralib_fail_init.dll");

const ARENA_BASE: u64 = 0x1_0000_0000;
const ARENA_SIZE: u64 = 0x1000_0000;
const TLS_BASE: u64 = 0x3_0000_0000;
const TLS_SIZE: u64 = 0x1000;

#[test]
fn committed_images_are_x86_64_pe32_plus_outputs() {
    let app = ParsedPe::parse(APP).expect("the EXE parses");
    let lib = ParsedPe::parse(LIB).expect("the DLL parses");

    assert_eq!(app.machine, xenora_loader::pe::IMAGE_FILE_MACHINE_AMD64);
    assert_eq!(lib.machine, xenora_loader::pe::IMAGE_FILE_MACHINE_AMD64);
    assert_eq!(app.characteristics & xenora_loader::pe::IMAGE_FILE_DLL, 0);
    assert_ne!(
        lib.characteristics & xenora_loader::pe::IMAGE_FILE_DLL,
        0,
        "the provider must be a DLL"
    );
}

#[test]
fn fixture_declares_the_expected_imports_and_exports() {
    let packages = [
        PackageModule {
            file_name: "loadlib.exe",
            bytes: APP,
        },
        PackageModule {
            file_name: "xenoralib.dll",
            bytes: LIB,
        },
    ];
    let graph = ModuleGraph::build(&packages).expect("the fixture is a valid two-image package");
    let app = graph
        .modules()
        .iter()
        .find(|node| node.file_name == "loadlib.exe")
        .expect("the EXE node");
    let lib = graph
        .modules()
        .iter()
        .find(|node| node.file_name == "xenoralib.dll")
        .expect("the DLL node");

    assert_eq!(app.import_error, None, "the EXE import table is complete");
    let imports: BTreeSet<String> = app
        .imports
        .iter()
        .filter(|entry| entry.module.eq_ignore_ascii_case("kernel32.dll"))
        .map(|entry| entry.name.to_string())
        .collect();
    for expected in [
        "LoadLibraryA",
        "GetProcAddress",
        "FreeLibrary",
        "ExitProcess",
    ] {
        assert!(
            imports.contains(expected),
            "missing kernel32 import {expected}: {imports:?}"
        );
    }

    assert_eq!(lib.export_error, None, "the DLL export table is complete");
    let exports: BTreeSet<String> = lib
        .exports
        .as_ref()
        .expect("the DLL has an export table")
        .entries
        .iter()
        .flat_map(|entry| entry.names.iter().cloned())
        .collect();
    for expected in [
        "XenoraTriple",
        "XenoraLibInit",
        "XenoraLibTls",
        "XenoraLibDetach",
    ] {
        assert!(
            exports.contains(expected),
            "missing export {expected}: {exports:?}"
        );
    }
}

#[test]
fn provider_tls_directory_is_prepared_without_running_callbacks() {
    let services = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut imports = ImportRegistry::new();
    register_standard_services(&mut imports, services);

    let mut store = ProvisionedModuleStore::new();
    store
        .insert("xenoralib.dll", LIB, &imports)
        .expect("the DLL is admitted");
    let mut memory = GuestMemory::new();
    let prepared = prepare_runtime_load(
        &store,
        &["xenoralib.dll"],
        ModuleLayoutArena {
            base: ARENA_BASE,
            size: ARENA_SIZE,
        },
        &[AddressInterval::new(TLS_BASE, TLS_BASE + TLS_SIZE)],
        &mut imports,
        TLS_BASE,
        &mut memory,
    )
    .expect("the DLL prepares without guest execution");

    assert!(prepared.tls.iter().any(|report| {
        report
            .prepared
            .as_ref()
            .is_some_and(|prepared| !prepared.callbacks.is_empty())
    }));
}

#[test]
fn negative_controls_have_the_expected_export_shapes() {
    let packages = [
        PackageModule {
            file_name: "xenoralib_no_triple.dll",
            bytes: NO_TRIPLE,
        },
        PackageModule {
            file_name: "xenoralib_fail_init.dll",
            bytes: FAIL_INIT,
        },
    ];
    let graph = ModuleGraph::build(&packages).expect("both negative controls are valid PE32+ DLLs");
    let exports_of = |file_name: &str| -> BTreeSet<String> {
        graph
            .modules()
            .iter()
            .find(|node| node.file_name == file_name)
            .and_then(|node| node.exports.as_ref())
            .expect("the control has an export table")
            .entries
            .iter()
            .flat_map(|entry| entry.names.iter().cloned())
            .collect()
    };

    let no_triple = exports_of("xenoralib_no_triple.dll");
    assert!(
        !no_triple.contains("XenoraTriple"),
        "the missing-export control must not expose the function"
    );
    assert!(
        no_triple.contains("XenoraLibInit"),
        "the control still needs a resolvable data export"
    );

    let fail_init = exports_of("xenoralib_fail_init.dll");
    assert!(
        fail_init.contains("XenoraTriple"),
        "the failed-initializer control keeps the full export surface"
    );
}
