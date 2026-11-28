// SPDX-License-Identifier: GPL-3.0-or-later

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use xenora_loader::module_graph::PackageModule;
use xenora_loader::module_layout::ModuleLayoutArena;
use xenora_loader::package::{PackageEntryEvent, run_package_startup_with_entry_observer};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::{
    ExitReason, ImportRegistry, InitializationPhase, PackageStartupError, PackageStartupLayout,
};

const ENTRY_BUDGET: usize = 20_000;
const APP_BASE: u64 = 0x1_5000_0000;
const TLS_BASE: u64 = 0x3_0000_0000;
const MAIN_TEB: u64 = TLS_BASE + 0x1_0000;

struct Fixtures {
    app: Vec<u8>,
    provider: Vec<u8>,
    rejected: Vec<u8>,
}

fn fixtures() -> &'static Fixtures {
    static FIXTURES: OnceLock<Fixtures> = OnceLock::new();
    FIXTURES.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let out = root
            .join("target")
            .join(format!("package-entry-observer-{}", std::process::id()));
        let build = Command::new("sh")
            .arg(root.join("fixtures/pe/package-entry-observer/build.sh"))
            .arg(&out)
            .output()
            .expect("run native fixture builder");
        assert!(
            build.status.success(),
            "observer fixture build failed:\n{}\n{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let fixtures = Fixtures {
            app: fs::read(out.join("observer-app.exe")).unwrap(),
            provider: fs::read(out.join("observer_provider.dll")).unwrap(),
            rejected: fs::read(out.join("reject/observer_provider.dll")).unwrap(),
        };
        fs::remove_dir_all(out).unwrap();
        fixtures
    })
}

fn layout() -> PackageStartupLayout {
    PackageStartupLayout {
        arena: ModuleLayoutArena {
            base: APP_BASE,
            size: 0x20_0000,
        },
        stack_top: 0x2_0001_0000,
        thread_tls_base: TLS_BASE,
    }
}

fn package(provider: &[u8]) -> [PackageModule<'_>; 2] {
    // The root is first, so running the provider before entry proves that
    // startup follows the import dependency rather than caller order.
    [
        PackageModule {
            file_name: "observer-app.exe",
            bytes: &fixtures().app,
        },
        PackageModule {
            file_name: "observer_provider.dll",
            bytes: provider,
        },
    ]
}

#[derive(Debug, PartialEq, Eq)]
enum Observation {
    Guest {
        marker: u32,
        teb: u64,
    },
    Enter {
        image_index: usize,
        address: u64,
        main_teb: u64,
    },
    Leave,
}

fn registry(state: &Arc<ServicesState>, seen: &Arc<Mutex<Vec<Observation>>>) -> ImportRegistry {
    let mut imports = ImportRegistry::new();
    register_standard_services(&mut imports, Arc::clone(state));
    let guest = Arc::clone(seen);
    imports.register("phase_probe.dll", "PhaseProbe", move |args, cpu, _| {
        guest.lock().unwrap().push(Observation::Guest {
            marker: args.rcx as u32,
            teb: cpu.gs_base,
        });
        Ok(0)
    });
    imports
}

fn record(seen: &Mutex<Vec<Observation>>, event: PackageEntryEvent) {
    let observation = match event {
        PackageEntryEvent::Entering {
            image_index,
            address,
            main_teb,
        } => Observation::Enter {
            image_index,
            address,
            main_teb,
        },
        PackageEntryEvent::Leaving => Observation::Leave,
    };
    seen.lock().unwrap().push(observation);
}

#[test]
fn the_entry_observer_brackets_real_guest_entry_without_including_dll_callbacks() {
    for scheduled in [false, true] {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let result = run_package_startup_with_entry_observer(
            &package(&fixtures().provider),
            layout(),
            0,
            registry(&state, &seen),
            ENTRY_BUDGET,
            scheduled.then(|| (Arc::clone(&state), 64)),
            |event| record(&seen, event),
        )
        .expect("run the original observer package");

        assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
        let entry = result
            .diagnostics
            .initialization
            .iter()
            .find(|record| record.phase == InitializationPhase::ProcessEntry)
            .expect("process entry diagnostics");
        let pe_offset = u32::from_le_bytes(fixtures().app[0x3c..0x40].try_into().unwrap()) as usize;
        let entry_rva = u32::from_le_bytes(
            fixtures().app[pe_offset + 40..pe_offset + 44]
                .try_into()
                .unwrap(),
        );
        assert_eq!(entry.address, APP_BASE + u64::from(entry_rva));
        assert_eq!(
            *seen.lock().unwrap(),
            [
                Observation::Guest {
                    marker: 4,
                    teb: MAIN_TEB
                },
                Observation::Guest {
                    marker: 1,
                    teb: MAIN_TEB
                },
                Observation::Enter {
                    image_index: 0,
                    address: entry.address,
                    main_teb: MAIN_TEB
                },
                Observation::Guest {
                    marker: 2,
                    teb: MAIN_TEB
                },
                Observation::Leave,
                Observation::Guest {
                    marker: 3,
                    teb: MAIN_TEB
                },
            ],
            "scheduled={scheduled}"
        );
    }
}

#[test]
fn a_rejected_dll_attach_never_enters_the_process_observer() {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let result = run_package_startup_with_entry_observer(
        &package(&fixtures().rejected),
        layout(),
        0,
        registry(&state, &seen),
        ENTRY_BUDGET,
        Some((state, 64)),
        |event| record(&seen, event),
    );
    let Err(PackageStartupError::Initialization { detail, .. }) = result else {
        panic!("DLL rejected its attach but package did not report initialization failure");
    };
    assert!(
        detail.contains("observer_provider.dll returned FALSE"),
        "{detail}"
    );
    assert_eq!(
        *seen.lock().unwrap(),
        [
            Observation::Guest {
                marker: 4,
                teb: MAIN_TEB
            },
            Observation::Guest {
                marker: 1,
                teb: MAIN_TEB
            },
        ]
    );
}

#[test]
fn a_refused_scheduler_attachment_leaves_the_entry_observer() {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    const LIMIT: Duration = Duration::from_secs(5);
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let (entered, ready) = mpsc::sync_channel(1);
    let (resume, release) = mpsc::channel();
    let outer_state = Arc::clone(&state);
    let provider = &fixtures().provider;
    let outer = thread::spawn(move || {
        let mut imports = ImportRegistry::new();
        register_standard_services(&mut imports, Arc::clone(&outer_state));
        let release = Mutex::new(release);
        imports.register("phase_probe.dll", "PhaseProbe", move |args, _, _| {
            if args.rcx as u32 == 2 {
                // Hold the outer scheduler attached while the test tries to
                // attach another scheduler to the same ServicesState.
                entered.send(()).unwrap();
                release.lock().unwrap().recv_timeout(LIMIT).unwrap();
            }
            Ok(0)
        });
        xenora_loader::run_package_startup(
            &package(provider),
            layout(),
            0,
            imports,
            ENTRY_BUDGET,
            Some((outer_state, 64)),
        )
        .unwrap()
        .execution
        .exit_reason
    });
    ready.recv_timeout(LIMIT).unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let result = run_package_startup_with_entry_observer(
        &package(provider),
        layout(),
        0,
        registry(&state, &seen),
        ENTRY_BUDGET,
        Some((state, 64)),
        |event| record(&seen, event),
    );
    resume.send(()).unwrap();
    assert_eq!(outer.join().unwrap(), ExitReason::ProcessExit(0));
    assert!(
        matches!(result, Err(PackageStartupError::Entry(detail)) if detail.contains("ServiceAbiViolation"))
    );
    assert!(matches!(
        seen.lock().unwrap().as_slice(),
        [
            Observation::Guest {
                marker: 4,
                teb: MAIN_TEB
            },
            Observation::Guest {
                marker: 1,
                teb: MAIN_TEB
            },
            Observation::Enter {
                image_index: 0,
                main_teb: MAIN_TEB,
                ..
            },
            Observation::Leave,
        ]
    ));
}
