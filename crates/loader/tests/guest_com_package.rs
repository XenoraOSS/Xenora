// SPDX-License-Identifier: GPL-3.0-or-later

//! Original producer and consumer DLLs share an opt-in synthetic IUnknown
//! across the explicit package boundary.
//!
//! Run `cargo test --locked -p xenora-loader --test guest_com_package` on
//! Apple Silicon. The test invokes `fixtures/pe/com-package/build.sh` to build
//! the three images with native Clang and LLD. With the fixture enabled, the
//! root expects exit status 0. Without it, startup must report exactly the
//! unresolved unload-query import before entering the guest.
//!
//! The expectations follow the public [QueryInterface], [reference counting],
//! and [DllCanUnloadNow] contracts. Exact reference counts are specific to the
//! synthetic fixture. This test uses one guest thread and checks unload
//! eligibility within the explicit package.
//!
//! [QueryInterface]: https://learn.microsoft.com/en-us/windows/win32/api/unknwn/nf-unknwn-iunknown-queryinterface(refiid_void)
//! [reference counting]: https://learn.microsoft.com/en-us/windows/win32/com/implementing-reference-counting
//! [DllCanUnloadNow]: https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-dllcanunloadnow

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use xenora_loader::import_table::ImportName;
use xenora_loader::module_graph::PackageModule;
use xenora_loader::module_layout::ModuleLayoutArena;
use xenora_loader::package::run_package_startup_with_entry_observer;
use xenora_loader::services::{MockClock, ServicesState, com_fixture, register_standard_services};
use xenora_loader::{
    ExitReason, ImportDisposition, ImportFailure, ImportRegistry, PackageStartupError,
    PackageStartupLayout,
};

const ROOT: &str = "com-package.exe";
const PRODUCER: &str = "xenora_com_producer.dll";
const CONSUMER: &str = "xenora_com_consumer.dll";
const FIXTURE: &str = "xenora_com_fixture.dll";

#[test]
fn guest_com_ownership_crosses_package_modules_and_requires_opt_in() {
    let directory =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("com-package-{}", std::process::id()));
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pe/com-package/build.sh");
    let build = Command::new("sh")
        .arg(&script)
        .arg(&directory)
        .output()
        .expect("run the COM package builder");
    assert!(
        build.status.success(),
        "{} failed:\n{}{}",
        script.display(),
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let names = [ROOT, PRODUCER, CONSUMER];
    let images = names.map(|name| {
        fs::read(directory.join(name)).unwrap_or_else(|error| panic!("read {name}: {error}"))
    });
    let package: [_; 3] = std::array::from_fn(|index| PackageModule {
        file_name: names[index],
        bytes: &images[index],
    });

    // The middle run refuses the missing fixture import. The final run gets
    // a fresh registry and state, including a fresh COM ownership table.
    for enabled in [true, false, true] {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, Arc::clone(&state));
        if enabled {
            com_fixture::register(&mut registry, &state);
        }
        let mut entry_events = Vec::new();
        let run = run_package_startup_with_entry_observer(
            &package,
            PackageStartupLayout {
                arena: ModuleLayoutArena {
                    base: 0x1_0000_0000,
                    size: 0x1000_0000,
                },
                stack_top: 0x2_0001_0000,
                thread_tls_base: 0x3_0000_0000,
            },
            0,
            registry,
            2_000_000,
            None,
            |event| entry_events.push(event),
        );
        assert_eq!(state.open_handle_count(), 0);
        if enabled {
            let result = run.expect("start the opt-in package");
            assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
            assert_eq!(state.exit_status(), Some(0));
            assert_eq!(entry_events.len(), 2);
            assert!(result.dbt_metrics.native_bytes_emitted > 0);
            assert_eq!(result.diagnostics.unresolved_imports().count(), 0);

            for (provider, name) in [
                (PRODUCER, "ProduceObject"),
                (CONSUMER, "InspectObject"),
                (CONSUMER, "ReleaseObject"),
            ] {
                assert!(result.diagnostics.imports.iter().any(|import| {
                    import.file_name == ROOT
                        && import.requested_module == provider
                        && import.symbol == ImportName::Name(name.to_owned())
                        && matches!(
                            &import.disposition,
                            ImportDisposition::Package { provider: binding, .. } if binding == provider
                        )
                }), "missing root binding {provider}!{name}");
            }
            for (caller, module, name) in [
                (PRODUCER, "ole32.dll", "CoCreateInstance"),
                (ROOT, FIXTURE, "DllCanUnloadNow"),
            ] {
                assert!(
                    result.diagnostics.imports.iter().any(|import| {
                        import.file_name == caller
                            && import.requested_module == module
                            && import.symbol == ImportName::Name(name.to_owned())
                            && matches!(&import.disposition, ImportDisposition::Service { .. })
                    }),
                    "missing service binding {caller} → {module}!{name}"
                );
            }
        } else {
            let PackageStartupError::UnresolvedImports(diagnostics) =
                run.expect_err("the package requires explicit COM fixture registration")
            else {
                panic!("expected an unresolved fixture import");
            };
            let unresolved: Vec<_> = diagnostics.unresolved_imports().collect();
            assert_eq!(unresolved.len(), 1);
            let import = unresolved[0];
            assert_eq!(import.file_name, ROOT);
            assert_eq!(import.requested_module, FIXTURE);
            assert_eq!(import.symbol, ImportName::Name("DllCanUnloadNow".into()));
            assert_eq!(
                import.disposition,
                ImportDisposition::Unresolved(ImportFailure::MissingModule {
                    module: FIXTURE.into(),
                })
            );
            assert!(entry_events.is_empty());
            assert!(diagnostics.initialization.is_empty());
            assert_eq!(state.exit_status(), None);
        }
    }
}
