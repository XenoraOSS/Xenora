// SPDX-License-Identifier: GPL-3.0-or-later

//! Rebuild with `fixtures/pe/msvc-object-ownership/build.sh artifacts/msvc-object-ownership`.
//! Each DLL is compiled separately for Microsoft's x64 C++ ABI without RTTI,
//! exception handling, vendor headers, or vendor binaries. The consumer sees
//! only the base class and uses ordinary virtual dispatch and scalar delete.
//!
//! The executable checks trace 12345: construction, virtual method, derived
//! destructor, base destructor, and deallocation. That trace is local to the
//! producer. The control validates a rejected handoff without mutation or
//! release, then succeeds with the same object. HeapSize observes Xenora's
//! synthetic heap, which is not a claim about Windows allocation bookkeeping.
//! Destruction order and dispatch follow the public Microsoft C++ references:
//! <https://learn.microsoft.com/en-us/cpp/cpp/destructors-cpp>
//! and <https://learn.microsoft.com/en-us/cpp/cpp/virtual-functions>.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use xenora_loader::module_graph::PackageModule;
use xenora_loader::module_layout::ModuleLayoutArena;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::{ExitReason, ImportRegistry, PackageStartupLayout, run_package_startup};

#[test]
fn microsoft_x64_object_lifetime_crosses_dlls() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pe/msvc-object-ownership/build.sh");
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "msvc-package-object-ownership-{}",
        std::process::id()
    ));
    let build = Command::new("sh")
        .arg(&script)
        .arg(&directory)
        .output()
        .expect("run the native MSVC object ownership fixture builder");
    assert!(
        build.status.success(),
        "{} failed:\n{}{}",
        script.display(),
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let read = |name| fs::read(directory.join(name)).expect("read the compiled package image");
    let application = read("object-ownership.exe");
    let rejected = read("object-ownership-reject.exe");
    let producer = read("xenora_producer.dll");
    let consumer = read("xenora_consumer.dll");
    fs::remove_dir_all(directory).expect("remove generated package images");

    // The control rejects the first handoff and then releases the same live
    // object. The next package must start with an empty producer trace.
    for (application, status) in [(&application, 70), (&rejected, 24), (&application, 70)] {
        let package = [
            PackageModule {
                file_name: "object-ownership.exe",
                bytes: application,
            },
            PackageModule {
                file_name: "xenora_consumer.dll",
                bytes: &consumer,
            },
            PackageModule {
                file_name: "xenora_producer.dll",
                bytes: &producer,
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
    }
}
