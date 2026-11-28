// SPDX-License-Identifier: GPL-3.0-or-later

//! Rebuild the original freestanding PE fixtures from the repository root with
//! `fixtures/pe/runtime-ownership/build.sh artifacts/runtime-ownership`.
//! `cargo test --locked -p xenora-loader --test runtime_package_ownership`
//! rebuilds them and checks positive, corrupt-payload and fresh runs (70/23/70).
//! The corrupt run must still release the allocation. A fresh session checks
//! its initial imported marker and errno before allocating. The package uses
//! one guest thread and the synthetic UCRT and process heap.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use xenora_loader::module_graph::PackageModule;
use xenora_loader::module_layout::ModuleLayoutArena;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::{
    ExitReason, ImportDisposition, ImportRegistry, PackageStartupLayout, run_package_startup,
};

#[test]
fn independently_compiled_modules_share_ucrt_ownership_and_errno() {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pe/runtime-ownership/build.sh");
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("runtime-package-ownership-{}", std::process::id()));
    let build = Command::new("sh")
        .arg(&script)
        .arg(&directory)
        .output()
        .expect("run the native runtime ownership fixture builder");
    assert!(
        build.status.success(),
        "{} failed:\n{}{}",
        script.display(),
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let read = |name| fs::read(directory.join(name)).expect("read the compiled package image");
    let application = read("ownership.exe");
    let corrupted = read("ownership-corrupt.exe");
    let allocate = read("xenora_allocate.dll");
    let release = read("xenora_release.dll");
    fs::remove_dir_all(directory).expect("remove generated package images");

    // The successful run after the corrupt one must start with the original
    // imported data and zero errno, even though both were changed in the control.
    for (application, status) in [(&application, 70), (&corrupted, 23), (&application, 70)] {
        let package = [
            PackageModule {
                file_name: "ownership.exe",
                bytes: application,
            },
            PackageModule {
                file_name: "xenora_release.dll",
                bytes: &release,
            },
            PackageModule {
                file_name: "xenora_allocate.dll",
                bytes: &allocate,
            },
        ];
        let layout = PackageStartupLayout {
            arena: ModuleLayoutArena {
                base: 0x1_0000_0000,
                size: 0x1000_0000,
            },
            stack_top: 0x2_0001_0000,
            thread_tls_base: 0x3_0000_0000,
        };
        let services = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, Arc::clone(&services));
        let result = run_package_startup(&package, layout, 0, registry, 20_000, None)
            .expect("the explicit package starts");

        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(status)
        );
        assert_eq!(services.exit_status(), Some(status));
        assert_eq!(result.diagnostics.unresolved_imports().count(), 0);
        assert_eq!(
            result
                .diagnostics
                .imports
                .iter()
                .filter(|binding| matches!(
                    &binding.disposition,
                    ImportDisposition::Package { provider, .. } if provider == "xenora_allocate.dll"
                ))
                .count(),
            3
        );
    }
}
