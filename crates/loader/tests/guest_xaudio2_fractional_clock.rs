// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};

#[test]
fn guest_fractional_audio_clocks_keep_independent_progress() {
    let guest = include_bytes!(
        "../../../fixtures/pe/xaudio2-fractional-clock/xaudio2-fractional-clock.exe"
    );
    for _ in 0..2 {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, Arc::clone(&state));
        let layout = StartupLayout {
            image_base: 0x2_5000_0000,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        };
        let result = run_synthetic_startup(guest, layout, registry, 100_000).unwrap();
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(0),
            "a nonzero guest exit identifies the fixture source line"
        );
        let diagnostics = state.xaudio2_diagnostics();
        assert_eq!(diagnostics.engines, 0);
        assert_eq!(diagnostics.source_voices, 0);
        assert_eq!(diagnostics.queued_buffers, 0);
    }
}
