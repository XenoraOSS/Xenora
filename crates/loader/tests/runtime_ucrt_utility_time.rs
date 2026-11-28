// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::import_table::ImportName;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{
    StartupLayout, run_synthetic_startup, run_synthetic_startup_scheduled,
};

const RNG: &[u8] = include_bytes!("../../../fixtures/pe/ucrt-utility-time/ucrt-utility-rng.exe");
const TIME: &[u8] = include_bytes!("../../../fixtures/pe/ucrt-utility-time/ucrt-utility-time.exe");
const UNIX_EPOCH: u64 = 116_444_736_000_000_000;
const SECOND: u64 = 10_000_000;
const SDK_LAST_SECOND: u64 = 32_536_799_999;
const LAYOUT: StartupLayout = StartupLayout {
    image_base: 0x2_5000_0000,
    stack_top: 0x2_0001_0000,
    tls_base: 0x3_0000_0000,
};

fn assert_imports(pe: &[u8], symbol: &str, mut expected: Vec<&str>) {
    let report = probe_pe_bytes(pe, &ProbeConfig::default()).unwrap();
    assert_eq!(report.import_error, None);
    let name = ImportName::Name(symbol.into());
    let mut modules: Vec<_> = report
        .imports
        .iter()
        .filter(|entry| entry.import_name == name)
        .map(|entry| entry.dll.as_str())
        .collect();
    modules.sort_unstable();
    expected.sort_unstable();
    assert_eq!(modules, expected, "{symbol}");
}

fn runtime(filetime: u64) -> (Arc<ServicesState>, ImportRegistry) {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(filetime))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    (state, registry)
}

#[test]
fn original_guest_checks_ucrt_rng_state_per_thread_and_crt() {
    for symbol in ["rand", "srand"] {
        assert_imports(
            RNG,
            symbol,
            vec![
                "api-ms-win-crt-utility-l1-1-0.dll",
                "msvcrt.dll",
                "ucrtbase.dll",
            ],
        );
    }
    for quantum in [1, 64] {
        let (state, registry) = runtime(UNIX_EPOCH);
        let result =
            run_synthetic_startup_scheduled(RNG, LAYOUT, registry, state, 100_000, quantum, |_| {})
                .unwrap();
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(0),
            "quantum {quantum}, a nonzero guest exit code identifies the fixture source line"
        );
    }
}

#[test]
fn original_guest_checks_ucrt_time64_width_optional_output_and_limits() {
    assert_imports(
        TIME,
        "_time64",
        vec!["api-ms-win-crt-time-l1-1-0.dll", "ucrtbase.dll"],
    );
    for (label, filetime, expected) in [
        (
            "negative elapsed seconds",
            UNIX_EPOCH - 3 * SECOND + 1,
            -2i64,
        ),
        ("just before the epoch", UNIX_EPOCH - 1, 0),
        ("at the epoch", UNIX_EPOCH, 0),
        (
            "more than 32 bits",
            UNIX_EPOCH + ((1 << 32) + 123) * SECOND + 8,
            4_294_967_419,
        ),
        (
            "documented calendar endpoint",
            UNIX_EPOCH + 32_535_215_999 * SECOND,
            32_535_215_999,
        ),
        (
            "last second accepted by the reference SDK",
            UNIX_EPOCH + SDK_LAST_SECOND * SECOND + SECOND - 1,
            32_536_799_999,
        ),
        (
            "first second rejected by the reference SDK",
            UNIX_EPOCH + (SDK_LAST_SECOND + 1) * SECOND,
            -1,
        ),
    ] {
        let (state, registry) = runtime(filetime);
        let result = run_synthetic_startup(TIME, LAYOUT, registry, 100_000).unwrap();
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(0),
            "{label}, a nonzero guest exit code identifies the fixture source line"
        );
        assert_eq!(state.standard_output(), expected.to_le_bytes(), "{label}");
    }
}
