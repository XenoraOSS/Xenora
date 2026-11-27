// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded provisioned module store and transactional runtime load preparation.
//!
//! The positive cases use the original three-image multi-DLL fixture. Negative
//! cases reuse those images under bounded logical names or malformed bytes; no
//! restricted module, host search or guest execution takes part.

use std::sync::Arc;

use xenora_loader::module_layout::{AddressInterval, ModuleLayoutArena};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::{
    GuestMemory, ImportDisposition, ImportRegistry, MemoryPermissions, ModuleStoreError,
    ProvisionedModuleStore, RuntimeLoadPreparationError, prepare_runtime_load,
};

const CONSUMER: &[u8] = include_bytes!("../../../fixtures/pe/multidll/multidll-consumer.exe");
const CHAIN: &[u8] = include_bytes!("../../../fixtures/pe/multidll/xenora_chain.dll");
const MATH: &[u8] = include_bytes!("../../../fixtures/pe/multidll/xenora_math.dll");

const ARENA_BASE: u64 = 0x1_0000_0000;
const ARENA_SIZE: u64 = 0x1000_0000;
const TLS_BASE: u64 = 0x3_0000_0000;
const TLS_SIZE: u64 = 0x1000;

fn registry() -> ImportRegistry {
    let services = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut imports = ImportRegistry::new();
    register_standard_services(&mut imports, services);
    imports
}

fn arena() -> ModuleLayoutArena {
    ModuleLayoutArena {
        base: ARENA_BASE,
        size: ARENA_SIZE,
    }
}

fn occupied() -> Vec<AddressInterval> {
    vec![AddressInterval::new(TLS_BASE, TLS_BASE + TLS_SIZE)]
}

fn store_of(modules: &[(&str, &[u8])]) -> ProvisionedModuleStore {
    let services = registry();
    let mut store = ProvisionedModuleStore::new();
    for (name, bytes) in modules {
        store
            .insert(*name, *bytes, &services)
            .unwrap_or_else(|error| panic!("admit {name}: {error}"));
    }
    store
}

#[test]
fn single_dll_prepares_without_executing_guest_code() {
    let store = store_of(&[("xenora_math.dll", MATH)]);
    let mut services = registry();
    let mut memory = GuestMemory::new();
    let prepared = prepare_runtime_load(
        &store,
        &["xenora_math.dll"],
        arena(),
        &occupied(),
        &mut services,
        TLS_BASE,
        &mut memory,
    )
    .expect("the single DLL prepares");

    assert_eq!(prepared.roots, ["xenora_math.dll"]);
    assert_eq!(prepared.reservations.len(), 1);
    assert_eq!(prepared.graph.modules().len(), 1);
    assert_eq!(prepared.service_bindings, 0);
    assert_eq!(prepared.diagnostics.unresolved_imports().count(), 0);
    assert!(prepared.diagnostics.modules[0].tls_index.is_some());
    assert!(prepared.tls.iter().any(|report| {
        report
            .prepared
            .as_ref()
            .is_some_and(|prepared| !prepared.callbacks.is_empty())
    }));
}

#[test]
fn dependency_chain_binds_package_and_service_slots_for_readback() {
    let store = store_of(&[
        ("xenora_math.dll", MATH),
        ("multidll-consumer.exe", CONSUMER),
        ("xenora_chain.dll", CHAIN),
    ]);
    let mut services = registry();
    let mut memory = GuestMemory::new();
    let prepared = prepare_runtime_load(
        &store,
        &["multidll-consumer.exe"],
        arena(),
        &occupied(),
        &mut services,
        TLS_BASE,
        &mut memory,
    )
    .expect("the transitive chain prepares");

    assert_eq!(prepared.reservations.len(), 3);
    assert_eq!(prepared.diagnostics.unresolved_imports().count(), 0);
    assert_eq!(prepared.service_bindings, 1);

    let package_import = prepared
        .diagnostics
        .imports
        .iter()
        .find(|import| {
            import.requested_module == "xenora_math.dll" && import.symbol.to_string() == "XenoraAdd"
        })
        .expect("the data and function imports from the leaf");
    let ImportDisposition::Package { target_address, .. } = package_import.disposition else {
        panic!(
            "the leaf export must bind from the package: {:?}",
            package_import
        );
    };
    let base = prepared.reservations[package_import.image_index].base;
    assert_eq!(
        memory
            .read_u64(base + u64::from(package_import.iat_rva))
            .expect("the package IAT slot is mapped"),
        target_address
    );

    let service_import = prepared
        .diagnostics
        .imports
        .iter()
        .find(|import| import.requested_module == "kernel32.dll")
        .expect("the ExitProcess service import");
    let ImportDisposition::Service { address, .. } = service_import.disposition else {
        panic!("the service must win for its own key: {:?}", service_import);
    };
    let base = prepared.reservations[service_import.image_index].base;
    assert_eq!(
        memory
            .read_u64(base + u64::from(service_import.iat_rva))
            .expect("the service IAT slot is mapped"),
        address
    );
}

#[test]
fn circular_dependency_is_refused_before_mapping() {
    // The chain image imports `xenora_math.dll`; storing those bytes under
    // that identity makes the import self-referential without new binaries.
    let store = store_of(&[("xenora_math.dll", CHAIN)]);
    let mut services = registry();
    let mut memory = GuestMemory::new();
    let error = prepare_runtime_load(
        &store,
        &["xenora_math.dll"],
        arena(),
        &occupied(),
        &mut services,
        TLS_BASE,
        &mut memory,
    )
    .expect_err("a self-dependency is not a loadable package");
    assert!(
        matches!(
            error,
            RuntimeLoadPreparationError::CircularDependency { ref modules }
                if modules == &["xenora_math.dll"]
        ),
        "unexpected circular diagnostic: {error:?}"
    );
}

#[test]
fn service_module_name_collision_is_refused_at_admission() {
    let services = registry();
    let mut store = ProvisionedModuleStore::new();
    let error = store
        .insert("kernel32.dll", MATH, &services)
        .expect_err("a provisioned module may not own a service module name");
    assert!(
        matches!(error, ModuleStoreError::ServiceNameCollision { .. }),
        "unexpected collision diagnostic: {error:?}"
    );
    assert!(store.is_empty());
}

#[test]
fn unresolved_import_leaves_caller_memory_unchanged() {
    let store = store_of(&[
        ("multidll-consumer.exe", CONSUMER),
        ("xenora_math.dll", MATH),
    ]);
    let mut services = registry();
    let mut memory = GuestMemory::new();
    let sentinel = 0x4_0000_0000;
    memory
        .map_region_with_data(
            sentinel,
            8,
            MemoryPermissions::READ_WRITE,
            &0xA5A5_A5A5_A5A5_A5A5u64.to_le_bytes(),
        )
        .expect("the sentinel is mapped");

    let error = prepare_runtime_load(
        &store,
        &["multidll-consumer.exe"],
        arena(),
        &occupied(),
        &mut services,
        TLS_BASE,
        &mut memory,
    )
    .expect_err("the missing middle provider must refuse the load");
    assert!(
        matches!(
            error,
            RuntimeLoadPreparationError::UnresolvedImports(ref diagnostics)
                if diagnostics
                    .unresolved_imports()
                    .any(|import| import.requested_module == "xenora_chain.dll")
        ),
        "unexpected unresolved diagnostic: {error:?}"
    );
    assert_eq!(
        memory
            .read_u64(sentinel)
            .expect("the sentinel remains readable"),
        0xA5A5_A5A5_A5A5_A5A5
    );
}

#[test]
fn placement_avoids_existing_regions_and_reservations() {
    let store = store_of(&[("xenora_math.dll", MATH), ("xenora_chain.dll", CHAIN)]);
    let mut services = registry();
    let mut memory = GuestMemory::new();
    let blocker = AddressInterval::new(ARENA_BASE, ARENA_BASE + 0x1000);
    memory
        .map_region(blocker.start, 0x1000, MemoryPermissions::READ_WRITE)
        .expect("the blocker is mapped");
    let mut occupied = occupied();
    occupied.push(blocker);

    let prepared = prepare_runtime_load(
        &store,
        &["xenora_chain.dll"],
        arena(),
        &occupied,
        &mut services,
        TLS_BASE,
        &mut memory,
    )
    .expect("placement avoids the blocker");

    for reservation in &prepared.reservations {
        assert!(
            reservation.base >= blocker.end || reservation.end <= blocker.start,
            "reservation {:?} overlaps the blocker {:?}",
            reservation,
            blocker
        );
    }
}

#[test]
fn malformed_duplicate_and_budget_inputs_are_refused() {
    let services = registry();
    let mut store = ProvisionedModuleStore::new();
    assert!(
        matches!(
            store.insert("bad.dll", &b"not a PE image"[..], &services),
            Err(ModuleStoreError::InvalidImage { .. })
        ),
        "malformed bytes must not enter the store"
    );

    store.insert("xenora_math.dll", MATH, &services).unwrap();
    let before = store.total_bytes();
    assert_eq!(
        store.remaining_budget(),
        xenora_loader::MAX_PROVISIONED_TOTAL_BYTES - before
    );
    assert!(
        matches!(
            store.insert("XENORA_MATH.DLL", MATH, &services),
            Err(ModuleStoreError::DuplicateModule { .. })
        ),
        "canonical identities must be unique"
    );

    for index in 0..xenora_loader::MAX_PROVISIONED_MODULES - 1 {
        let name = format!("module-{index:03}.dll");
        store.insert(name, MATH, &services).unwrap();
    }
    assert!(
        matches!(
            store.insert("one-too-many.dll", MATH, &services),
            Err(ModuleStoreError::TooManyModules { .. })
        ),
        "the store count is bounded"
    );
}
