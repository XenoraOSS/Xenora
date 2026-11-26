// SPDX-License-Identifier: GPL-3.0-or-later

//! Opt-in synthetic single-library Gaming Runtime lifecycle.
use super::ServicesState;
use crate::imports::{ImportRegistry, ServiceStatus};
use std::sync::{Arc, Mutex};

const ACTIVE_OBJECTS: u64 = 0x8924_0105;

#[derive(Default)]
struct Lifecycle {
    initialized: bool,
    terminal_error: Option<u64>,
}

impl Lifecycle {
    fn initialize(&mut self) -> u64 {
        if let Some(error) = self.terminal_error {
            return error;
        }
        self.initialized = true;
        0
    }

    fn uninitialize(&mut self, active: usize) {
        if !self.initialized {
            return;
        }
        self.initialized = false;
        if active != 0 {
            self.terminal_error = Some(ACTIVE_OBJECTS);
        }
    }
}

/// Register the bounded synthetic lifecycle explicitly. Standard services do
/// not opt in. One registration represents one linked library; repeated init
/// calls are idempotent. No runtime DLL or platform capabilities are supplied.
/// Call once per registry, with its matching service state.
pub fn register_synthetic_lifecycle(registry: &mut ImportRegistry, state: Arc<ServicesState>) {
    let lifecycle = Arc::new(Mutex::new(Lifecycle::default()));
    let init = lifecycle.clone();
    registry.register_with_status(
        "xgameruntime.dll",
        "XGameRuntimeInitialize",
        ServiceStatus::Partial,
        move |_, _, _| Ok(init.lock().unwrap().initialize()),
    );
    registry.register_with_status(
        "xgameruntime.dll",
        "XGameRuntimeUninitialize",
        ServiceStatus::Partial,
        move |_, _, _| {
            let queues = state.gdk_queues.lock().unwrap();
            lifecycle.lock().unwrap().uninitialize(queues.open_count());
            Ok(0)
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_library_initialization_is_idempotent() {
        let mut lifecycle = Lifecycle::default();
        lifecycle.uninitialize(1);
        assert_eq!(lifecycle.initialize(), 0);
        assert_eq!(lifecycle.initialize(), 0);
        lifecycle.uninitialize(0);
        assert!(!lifecycle.initialized);
        assert_eq!(lifecycle.initialize(), 0);
    }

    #[test]
    fn active_objects_make_failure_terminal() {
        let mut lifecycle = Lifecycle::default();
        assert_eq!(lifecycle.initialize(), 0);
        lifecycle.uninitialize(1);
        assert!(!lifecycle.initialized);
        assert_eq!(lifecycle.initialize(), 0x8924_0105);
        lifecycle.uninitialize(0);
        assert_eq!(lifecycle.initialize(), 0x8924_0105);
    }
    #[test]
    fn scarlett_guest_runs_through_normal_startup() {
        use crate::ExitReason;
        use crate::startup::{StartupLayout, run_synthetic_startup};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let out = root.join("artifacts/lifecycle-767");
        assert!(
            std::process::Command::new("sh")
                .arg(root.join("tests/compat_fixtures/build-xbox-launch.sh"))
                .arg(&out)
                .status()
                .unwrap()
                .success()
        );
        let bytes = std::fs::read(out.join("scarlett-runtime.exe")).unwrap();
        for _ in 0..2 {
            let state = Arc::new(ServicesState::new(Arc::new(super::super::MockClock::new(
                0,
            ))));
            let mut registry = ImportRegistry::new();
            super::super::register_standard_services(&mut registry, state.clone());
            register_synthetic_lifecycle(&mut registry, state.clone());
            let result = run_synthetic_startup(
                &bytes,
                StartupLayout {
                    image_base: 0x1_4000_0000,
                    stack_top: 0x2_0001_0000,
                    tls_base: 0x3_0000_0000,
                },
                registry,
                1000,
            )
            .unwrap();
            assert_eq!(result.bound_imports, 2);
            assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
            assert!(state.standard_output().is_empty());
            assert!(state.standard_error().is_empty());
        }
    }
}
