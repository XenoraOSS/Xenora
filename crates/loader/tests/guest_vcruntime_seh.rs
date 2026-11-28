// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};

const PE: &[u8] = include_bytes!("../../../fixtures/pe/vcruntime-seh/vcruntime-seh.exe");
const BASE: u64 = 0x2_5000_0000;

#[test]
fn compiled_seh_searches_then_unwinds_through_the_vcruntime_import() {
    let probe = probe_pe_bytes(PE, &ProbeConfig::default()).unwrap();
    assert_eq!(probe.import_error, None);
    let mut handlers = probe
        .imports
        .iter()
        .filter(|entry| entry.symbol == "__C_specific_handler");
    assert_eq!(handlers.next().unwrap().dll, "vcruntime140.dll");
    assert!(handlers.next().is_none());

    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    let result = run_synthetic_startup(
        PE,
        StartupLayout {
            image_base: BASE,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        },
        registry,
        100_000,
    )
    .unwrap();
    assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
    let section = result
        .image
        .pe
        .sections
        .iter()
        .find(|section| section.name == ".gate")
        .unwrap();
    let gate = BASE + u64::from(section.virtual_address);
    let words: [u32; 10] = std::array::from_fn(|index| {
        result
            .image
            .memory
            .read_u32(gate + 4 * index as u64)
            .unwrap()
    });
    // Entry, leaf, rejecting filter, accepting filter, abnormal finally,
    // catch handler, and the continuation after the outer guarded statement.
    assert_eq!(words, [0x5345_4838, 7, 1, 2, 3, 4, 5, 6, 7, 0]);
}
