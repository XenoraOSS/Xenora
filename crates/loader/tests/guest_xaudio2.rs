// SPDX-License-Identifier: GPL-3.0-or-later

//! The guest-visible XAudio2 objects, exercised by the acceptance program.
//!
//! The program is the one in `fixtures/pe/xaudio2-voices.cpp`, built from
//! source by `fixtures/pe/build-xaudio2-voices.sh`. Its exit code is the
//! number of the first check that failed, so a nonzero code names the check
//! that broke.

use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};

#[test]
fn guest_exercises_the_xaudio2_objects() {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let layout = StartupLayout {
        image_base: 0x2_5000_0000,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let result = run_synthetic_startup(
        include_bytes!("../../../fixtures/pe/xaudio2-voices.exe"),
        layout,
        registry,
        20_000_000,
    )
    .unwrap();
    let ExitReason::ProcessExit(code) = result.execution.exit_reason else {
        panic!("the acceptance program did not exit");
    };
    assert_eq!(code, 0, "the acceptance program failed check {code}");
    // Nothing the guest created is left behind.
    let diagnostics = state.xaudio2_diagnostics();
    assert_eq!(diagnostics.engines, 0);
    assert_eq!(diagnostics.mastering_voices, 0);
    assert_eq!(diagnostics.source_voices, 0);
    assert_eq!(diagnostics.queued_buffers, 0);
    assert_eq!(diagnostics.queued_bytes, 0);
}
