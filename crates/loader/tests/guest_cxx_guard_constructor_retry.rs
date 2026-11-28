// SPDX-License-Identifier: GPL-3.0-or-later

//! A compiled Itanium C++ guard is aborted after a constructor throws, and
//! its next initialization succeeds on a different guest thread.
//! Run `cargo test --locked -p xenora-loader --test guest_cxx_guard_constructor_retry`
//! with a native ARM64 MinGW-w64 toolchain targeting UCRT. The test builds
//! both guest variants from source before execution.
//!
//! The expected retry follows [Itanium C++ ABI §3.3.3] and [stmt.dcl].
//!
//! [Itanium C++ ABI §3.3.3]: https://itanium-cxx-abi.github.io/cxx-abi/abi.html#once-ctor
//! [stmt.dcl]: https://eel.is/c++draft/stmt.dcl#3

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::{ImportError, ImportRegistry};
use xenora_loader::pe::ParsedPe;
use xenora_loader::scheduler::DEFAULT_QUANTUM;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupEvent, StartupLayout, run_synthetic_startup_scheduled};

const GUARD_LIBRARY: &str = "libstdc++-6.dll";
const GUARD_SYMBOLS: [&str; 3] = [
    "__cxa_guard_acquire",
    "__cxa_guard_release",
    "__cxa_guard_abort",
];
const EXPECTED_OUTPUT: &[u8] = b"caught 73 after 1 attempt\r\n\
    worker returned 17 after 2 attempts\r\n\
    main reused 41 after 2 attempts and 1 completion\r\n";

fn build_fixture() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "cxx-guard-constructor-retry-{}",
        std::process::id()
    ));
    let build = Command::new("sh")
        .arg(root.join("tests/compat_fixtures/build-cxx-guard-constructor-retry.sh"))
        .arg(&output)
        .output()
        .expect("run the C++ guard constructor retry builder");
    assert!(
        build.status.success(),
        "C++ guard constructor retry build failed: {}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    output
}

/// Corrupt one import name in the MinGW import section, leaving its size
/// and PE offsets unchanged. The loader must stop before entering the guest.
fn with_missing_guard(bytes: &[u8], symbol: &str) -> Vec<u8> {
    let pe = ParsedPe::parse(bytes).expect("parse the fixture PE");
    let section = pe
        .sections
        .iter()
        .find(|section| section.name == ".idata")
        .expect("MinGW import section");
    let start = section.raw_data_ptr as usize;
    let end = start + section.raw_data_size as usize;
    let name = format!("{symbol}\0");
    let mut locations = bytes[start..end]
        .windows(name.len())
        .enumerate()
        .filter_map(|(offset, candidate)| (candidate == name.as_bytes()).then_some(offset));
    let offset = locations.next().expect("guard import name");
    assert!(
        locations.next().is_none(),
        "duplicate import name: {symbol}"
    );
    let mut modified = bytes.to_vec();
    modified[start + offset] = b'x';
    modified
}

#[test]
fn compiled_constructor_retries_on_a_worker_after_throwing() {
    let output = build_fixture();
    for level in ["O0", "O2"] {
        let bytes = fs::read(output.join(format!("guest-cxx-guard-constructor-retry-{level}.exe")))
            .expect("read the compiled fixture");
        let layout = || StartupLayout {
            image_base: 0x2_5000_0000,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        };
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, Arc::clone(&state));
        let result = run_synthetic_startup_scheduled(
            &bytes,
            layout(),
            registry,
            Arc::clone(&state),
            20_000_000,
            DEFAULT_QUANTUM,
            |_| {},
        )
        .unwrap_or_else(|error| panic!("{level}: {error}"));
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(0),
            "{level}: stdout {:?}, stderr {:?}",
            String::from_utf8_lossy(&state.standard_output()),
            String::from_utf8_lossy(&state.standard_error())
        );
        assert_eq!(state.standard_output(), EXPECTED_OUTPUT, "{level}");
        assert!(state.standard_error().is_empty(), "{level}");
        assert_eq!(state.exit_status(), Some(0), "{level}");
        assert_eq!(state.open_handle_count(), 0, "{level}");
        assert!(result.dbt_metrics.native_bytes_emitted > 0, "{level}");

        // Refuse each import in isolation. This catches a build that stops
        // importing a guard service and silently uses a statically linked one.
        for symbol in GUARD_SYMBOLS {
            let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
            let mut registry = ImportRegistry::new();
            register_standard_services(&mut registry, Arc::clone(&state));
            let modified = with_missing_guard(&bytes, symbol);
            let missing_name = format!("x{}", &symbol[1..]);
            let error = match run_synthetic_startup_scheduled(
                &modified,
                layout(),
                registry,
                Arc::clone(&state),
                20_000_000,
                DEFAULT_QUANTUM,
                |event| {
                    assert!(
                        !matches!(event, StartupEvent::Entering { .. }),
                        "{level}: {symbol}"
                    )
                },
            ) {
                Err(error) => error,
                Ok(_) => panic!("{level}: startup accepted missing {symbol}"),
            };
            assert_eq!(
                error.downcast_ref::<ImportError>(),
                Some(&ImportError::UnresolvedImport {
                    module: GUARD_LIBRARY.to_owned(),
                    symbol: missing_name,
                }),
                "{level}: {error}"
            );
            assert_eq!(state.exit_status(), None, "{level}: {symbol}");
            assert!(state.standard_output().is_empty(), "{level}: {symbol}");
            assert_eq!(state.open_handle_count(), 0, "{level}: {symbol}");
        }
    }
}
