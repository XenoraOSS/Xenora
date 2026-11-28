// SPDX-License-Identifier: GPL-3.0-or-later

//! The original guest checks independent COM initialization in a live STA and
//! MTA. In the opt-in synthetic server, unload eligibility follows the last
//! outstanding object reference. Each object stays on its creating thread.
//! The fixture exercises eligibility through the export; it does not unload a DLL.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use xenora_loader::imports::ImportError;
use xenora_loader::services::{MockClock, ServicesState, com_fixture, register_standard_services};
use xenora_loader::startup::{StartupEvent, StartupLayout, run_synthetic_startup_scheduled};
use xenora_loader::{ExitReason, ImportRegistry};

#[test]
fn com_initialization_and_object_lifetime_are_balanced_across_guest_threads() {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("com-apartments-{}", std::process::id()));
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pe/com-apartments/build.sh");
    let build = Command::new("sh")
        .arg(&script)
        .arg(&directory)
        .output()
        .expect("run the COM apartment fixture builder");
    assert!(
        build.status.success(),
        "{} failed:\n{}{}",
        script.display(),
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let executable = fs::read(directory.join("com-apartments.exe")).expect("read the guest PE");

    for (enabled, quantum) in [(true, 1024), (false, 1024), (true, 8)] {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, Arc::clone(&state));
        if enabled {
            com_fixture::register(&mut registry, &state);
        }
        let mut entering = false;
        let result = run_synthetic_startup_scheduled(
            &executable,
            StartupLayout {
                image_base: 0x2_5000_0000,
                stack_top: 0x2_0001_0000,
                tls_base: 0x3_0000_0000,
            },
            registry,
            Arc::clone(&state),
            2_000_000,
            quantum,
            |event| entering |= matches!(event, StartupEvent::Entering { .. }),
        );
        assert_eq!(state.open_handle_count(), 0, "scheduler quantum {quantum}");
        if enabled {
            let run = result.expect("start the opt-in COM guest");
            assert!(entering);
            assert!(run.dbt_metrics.native_bytes_emitted > 0);
            assert_eq!(
                run.execution.exit_reason,
                ExitReason::ProcessExit(0),
                "guest check failed at scheduler quantum {quantum}"
            );
            assert_eq!(state.exit_status(), Some(0));
        } else {
            let Err(error) = result else {
                panic!("guest started without opting in to the COM fixture");
            };
            assert_eq!(
                error.downcast_ref::<ImportError>(),
                Some(&ImportError::UnresolvedImport {
                    module: "xenora_com_fixture.dll".to_owned(),
                    symbol: "DllCanUnloadNow".to_owned(),
                })
            );
            assert!(!entering);
            assert_eq!(state.exit_status(), None);
        }
    }
}
