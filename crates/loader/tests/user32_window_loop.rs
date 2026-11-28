// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::import_table::ImportName;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup_scheduled};

const PE: &[u8] = include_bytes!("../../../fixtures/pe/user32-window-loop/user32-window-loop.exe");

#[test]
fn guest_window_loop_uses_the_exact_rtcore_imports() {
    let report = probe_pe_bytes(PE, &ProbeConfig::default()).unwrap();
    assert_eq!(report.import_error, None);
    for (module, names) in [
        (
            "ext-ms-win-rtcore-ntuser-window-ansi-l1-1-0.dll",
            &["RegisterClassExA", "CreateWindowExA", "DefWindowProcA"][..],
        ),
        (
            "ext-ms-win-rtcore-ntuser-window-l1-1-0.dll",
            &["ShowWindow"],
        ),
        (
            "ext-ms-win-rtcore-ntuser-message-ansi-l1-1-0.dll",
            &["DispatchMessageA", "PeekMessageA"],
        ),
        (
            "ext-ms-win-rtcore-ntuser-message-l1-1-0.dll",
            &["TranslateMessage"],
        ),
    ] {
        for &symbol in names {
            let name = ImportName::Name(symbol.into());
            let matches: Vec<_> = report
                .imports
                .iter()
                .filter(|entry| entry.import_name == name)
                .collect();
            assert_eq!(matches.len(), 1, "{symbol}");
            assert_eq!(matches[0].dll, module, "{symbol}");
        }
    }

    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let layout = StartupLayout {
        image_base: 0x2_5000_0000,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    let result =
        run_synthetic_startup_scheduled(PE, layout, registry, state, 100_000, 64, |_| {}).unwrap();
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::ProcessExit(0),
        "a nonzero exit code identifies the fixture source line"
    );
}
