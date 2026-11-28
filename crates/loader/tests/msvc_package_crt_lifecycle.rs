// SPDX-License-Identifier: GPL-3.0-or-later

//! Execute Clang-emitted Microsoft x64 C++ initializers and destructors through
//! explicit UCRT tables in two original dependent DLLs.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use xenora_loader::import_table::ImportName;
use xenora_loader::module_graph::PackageModule;
use xenora_loader::module_layout::ModuleLayoutArena;
use xenora_loader::package::ImportDisposition;
use xenora_loader::scheduler::DEFAULT_QUANTUM;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::{
    ExitReason, ImportRegistry, InitializationOutcome, InitializationPhase, PackageStartupLayout,
    run_package_startup,
};

const FILE_NAMES: [&str; 3] = [
    "lifecycle.exe",
    "lifecycle_consumer.dll",
    "lifecycle_provider.dll",
];
const CRT_CALLBACK_IMPORTS: [&str; 4] = [
    "_initterm",
    "_initialize_onexit_table",
    "_register_onexit_function",
    "_execute_onexit_table",
];

#[test]
fn cpp_global_initializers_and_module_exit_tables_run_in_order() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pe/msvc-package-crt-lifecycle/build.sh");
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("msvc-package-crt-lifecycle-{}", std::process::id()));
    let build = Command::new("sh")
        .arg(&script)
        .arg(&directory)
        .output()
        .expect("run the native MSVC fixture builder");
    assert!(
        build.status.success(),
        "{} failed:\n{}{}",
        script.display(),
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let contents = FILE_NAMES.map(|name| fs::read(directory.join(name)).unwrap());
    fs::remove_dir_all(directory).expect("remove generated PE images");

    // The caller lists the root first. Dependencies still initialize first.
    let package: Vec<_> = FILE_NAMES
        .iter()
        .zip(&contents)
        .map(|(file_name, bytes)| PackageModule { file_name, bytes })
        .collect();
    let layout = PackageStartupLayout {
        arena: ModuleLayoutArena {
            base: 0x1_5000_0000,
            size: 0x0100_0000,
        },
        stack_top: 0x2_0001_0000,
        thread_tls_base: 0x3_0000_0000,
    };
    for scheduled in [false, true] {
        let services = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, Arc::clone(&services));
        let scheduler = scheduled.then(|| (Arc::clone(&services), DEFAULT_QUANTUM));
        let result = run_package_startup(&package, layout, 0, registry, 100_000, scheduler)
            .unwrap_or_else(|error| panic!("package failed, scheduler={scheduled}: {error}"));

        assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
        assert_eq!(services.exit_status(), Some(0));
        assert_eq!(services.open_handle_count(), 0);
        assert_eq!(result.diagnostics.modules[0].dependencies, [1]);
        assert_eq!(result.diagnostics.modules[1].dependencies, [2]);
        for file_name in &FILE_NAMES[1..] {
            for symbol in CRT_CALLBACK_IMPORTS {
                let bound = result.diagnostics.imports.iter().any(|record| {
                    record.file_name == *file_name
                        && record.requested_module == "ucrtbase.dll"
                        && matches!(&record.symbol, ImportName::Name(name) if name == symbol)
                        && matches!(&record.disposition, ImportDisposition::Service { .. })
                });
                assert!(bound, "{file_name} did not bind ucrtbase.dll!{symbol}");
            }
        }
        let detached: Vec<_> = result
            .diagnostics
            .shutdown
            .iter()
            .map(|record| {
                assert_eq!(record.phase, InitializationPhase::DllDetach);
                assert_eq!(record.outcome, InitializationOutcome::Returned(1));
                record.file_name.as_str()
            })
            .collect();
        assert_eq!(detached, &FILE_NAMES[1..]);

        // Provider A/B, consumer C/D, root M, then consumer R/S and provider T/U.
        assert_eq!(
            services
                .file_contents("C:\\msvc-crt-lifecycle.bin")
                .unwrap(),
            b"ABCDMRSTU",
            "guest journal with scheduler={scheduled}"
        );
    }
}
