// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use xenora_loader::cpu::{ExitReason, Register};
use xenora_loader::import_table::ImportName;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, StartupResult, run_synthetic_startup};

const POSITIVE: &[u8] = include_bytes!("../../../fixtures/pe/vcruntime-jump/vcruntime-jump.exe");
const NONNULL_FRAME: &[u8] =
    include_bytes!("../../../fixtures/pe/vcruntime-jump/vcruntime-jump-nonnull-frame.exe");
const NONZERO_SAVED_FRAME: &[u8] =
    include_bytes!("../../../fixtures/pe/vcruntime-jump/vcruntime-jump-nonzero-saved-frame.exe");
const LAYOUT: StartupLayout = StartupLayout {
    image_base: 0x2_5000_0000,
    stack_top: 0x2_0001_0000,
    tls_base: 0x3_0000_0000,
};

fn run(pe: &[u8], symbols: &[&str]) -> StartupResult {
    let report = probe_pe_bytes(pe, &ProbeConfig::default()).unwrap();
    assert_eq!(report.import_error, None);
    for &symbol in symbols {
        let name = ImportName::Name(symbol.into());
        let mut imports = report
            .imports
            .iter()
            .filter(|entry| entry.import_name == name);
        assert_eq!(imports.next().unwrap().dll, "vcruntime140.dll");
        assert!(imports.next().is_none(), "{symbol}");
    }

    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    run_synthetic_startup(pe, LAYOUT, registry, 100_000).unwrap()
}

fn jump_buffer(result: &StartupResult) -> u64 {
    let section = result
        .image
        .pe
        .sections
        .iter()
        .find(|section| section.name == ".jbuf")
        .unwrap();
    LAYOUT.image_base + u64::from(section.virtual_address)
}

#[test]
fn vcruntime_guest_restores_amd64_state_across_two_call_frames() {
    let result = run(POSITIVE, &["__intrinsic_setjmp", "longjmp"]);
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::ProcessExit(0),
        "a nonzero exit code identifies the failing assembly check"
    );
    assert_eq!(result.execution.final_state.x87.control, 0x077f);
}

#[test]
fn vcruntime_guest_refuses_frame_based_unwind() {
    let save = run(NONNULL_FRAME, &["__intrinsic_setjmp"]);
    assert_eq!(
        save.execution.exit_reason,
        ExitReason::InvalidUnwind,
        "reject the supplied frame before saving any state"
    );
    let mut bytes = [0; 256];
    save.image
        .memory
        .read_bytes(jump_buffer(&save), &mut bytes)
        .unwrap();
    assert_eq!(bytes, [0xa5; 256]);

    let restore = run(NONZERO_SAVED_FRAME, &["__intrinsic_setjmp", "longjmp"]);
    assert_eq!(
        restore.execution.exit_reason,
        ExitReason::InvalidUnwind,
        "reject a saved frame before resuming at the jump target"
    );
    let buffer = jump_buffer(&restore);
    let saved_rsp = restore.image.memory.read_u64(buffer + 0x10).unwrap();
    let saved_rip = restore.image.memory.read_u64(buffer + 0x50).unwrap();
    assert_eq!(
        restore.execution.final_state.get(Register::Rsp),
        saved_rsp - 8
    );
    assert_ne!(restore.execution.final_state.rip, saved_rip);
}
