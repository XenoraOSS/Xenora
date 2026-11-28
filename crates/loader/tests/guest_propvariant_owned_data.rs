// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::memory::MemoryError;
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, StartupResult, run_synthetic_startup};

const POSITIVE: &[u8] =
    include_bytes!("../../../fixtures/pe/propvariant-owned-data/propvariant-owned-data.exe");
const WRONG_OWNER: &[u8] = include_bytes!(
    "../../../fixtures/pe/propvariant-owned-data/propvariant-owned-data-wrong-owner.exe"
);
const READONLY: &[u8] = include_bytes!(
    "../../../fixtures/pe/propvariant-owned-data/propvariant-owned-data-readonly.exe"
);
const OVERLAP: &[u8] = include_bytes!(
    "../../../fixtures/pe/propvariant-owned-data/propvariant-owned-data-overlap.exe"
);
const IMAGE_BASE: u64 = 0x2_5000_0000;

fn run(pe: &[u8]) -> StartupResult {
    let probe = probe_pe_bytes(pe, &ProbeConfig::default()).unwrap();
    assert_eq!(probe.import_error, None);
    let mut clear = probe
        .imports
        .iter()
        .filter(|entry| entry.symbol == "PropVariantClear");
    assert_eq!(clear.next().unwrap().dll, "combase.dll");
    assert!(clear.next().is_none());

    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    run_synthetic_startup(
        pe,
        StartupLayout {
            image_base: IMAGE_BASE,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        },
        registry,
        100_000,
    )
    .unwrap()
}

fn gate(result: &StartupResult) -> [u64; 4] {
    let section = result
        .image
        .pe
        .sections
        .iter()
        .find(|section| section.name == ".gate")
        .unwrap();
    let address = IMAGE_BASE + u64::from(section.virtual_address);
    let memory = &result.image.memory;
    assert_eq!(memory.read_u64(address), Ok(0x5056_4f44));
    std::array::from_fn(|index| memory.read_u64(address + 8 * (index as u64 + 1)).unwrap())
}

fn read<const N: usize>(result: &StartupResult, address: u64) -> [u8; N] {
    let mut bytes = [0; N];
    result.image.memory.read_bytes(address, &mut bytes).unwrap();
    bytes
}

fn expected_clsid(pointer: u64) -> [u8; 24] {
    let mut variant = [0; 24];
    variant[..2].copy_from_slice(&72u16.to_le_bytes());
    variant[8..16].copy_from_slice(&pointer.to_le_bytes());
    variant
}

fn expected_blob(pointer: u64, count: u32) -> [u8; 24] {
    let mut variant = [0; 24];
    variant[..2].copy_from_slice(&65u16.to_le_bytes());
    variant[8..12].copy_from_slice(&count.to_le_bytes());
    variant[16..24].copy_from_slice(&pointer.to_le_bytes());
    variant
}

#[test]
fn original_guest_clears_and_releases_owned_guid_and_blob() {
    let result = run(POSITIVE);
    assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
    let [guid, blob, neighbor, descriptor] = gate(&result);
    let memory = &result.image.memory;
    assert_eq!(
        memory.read_u8(guid),
        Err(MemoryError::UnmappedAddress(guid))
    );
    assert_eq!(
        memory.read_u8(blob),
        Err(MemoryError::UnmappedAddress(blob))
    );
    assert_eq!(
        read::<9>(&result, neighbor),
        [0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98]
    );
    assert_eq!(read::<24>(&result, descriptor), [0; 24]);
}

#[test]
fn original_guest_keeps_storage_when_owner_or_descriptor_is_invalid() {
    let wrong_owner = run(WRONG_OWNER);
    let [payload, descriptor, ..] = gate(&wrong_owner);
    assert_eq!(
        wrong_owner.execution.exit_reason,
        ExitReason::MemoryFault(MemoryError::UnmappedAddress(payload))
    );
    assert_eq!(
        read::<24>(&wrong_owner, descriptor),
        expected_clsid(payload)
    );
    assert_eq!(wrong_owner.image.memory.read_u8(payload), Ok(0x7e));

    let read_only = run(READONLY);
    let [payload, descriptor, ..] = gate(&read_only);
    assert_eq!(
        read_only.execution.exit_reason,
        ExitReason::MemoryFault(MemoryError::PermissionViolation {
            address: descriptor,
            required: "write",
        })
    );
    assert_eq!(
        read::<24>(&read_only, descriptor),
        expected_blob(payload, 16)
    );
    assert_eq!(read_only.image.memory.read_u8(payload), Ok(0x7e));
}

#[test]
fn original_guest_does_not_free_a_payload_that_contains_the_descriptor() {
    let result = run(OVERLAP);
    assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
    let [payload, descriptor, ..] = gate(&result);
    assert_eq!(descriptor, payload + 8);
    assert_eq!(read::<24>(&result, descriptor), expected_blob(payload, 48));
    assert_eq!(result.image.memory.read_u8(payload), Ok(0x7e));
    assert_eq!(result.image.memory.read_u8(payload + 47), Ok(0xe7));
}
