// SPDX-License-Identifier: GPL-3.0-or-later

//! Build with `sh fixtures/pe/cxx-worker-fls/build.sh artifacts/cxx-worker-fls`
//! and run `cargo test --locked --target aarch64-apple-darwin -p xenora-loader
//! --test guest_cxx_worker_fls` on Apple Silicon.
//!
//! Both workers hold separate C++ records before the release event is set.
//! The consumer DLL receives each [FLS exit callback] and asks the producer
//! DLL to destroy and free its record. The thread's signaled handle and exit
//! code are checked against the [thread termination contract].
//!
//! [FLS exit callback]: https://learn.microsoft.com/en-us/windows/win32/api/winnt/nc-winnt-pfls_callback_function
//! [thread termination contract]: https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createthread

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use xenora_loader::import_table::ImportName;
use xenora_loader::module_graph::PackageModule;
use xenora_loader::module_layout::ModuleLayoutArena;
use xenora_loader::package::{
    ImportDisposition, ImportFailure, InitializationOutcome, InitializationPhase,
    PackageEntryEvent, PackageStartupError, PackageStartupLayout,
    run_package_startup_with_entry_observer,
};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::{ExitReason, ImportRegistry};

const ROOT: &str = "cxx-worker-fls.exe";
const PRODUCER: &str = "fls_producer.dll";
const CONSUMER: &str = "fls_consumer.dll";

#[test]
fn workers_destroy_cpp_records_through_fls_callbacks_in_two_dlls() {
    let out = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("cxx-worker-fls-{}", std::process::id()));
    let builder =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pe/cxx-worker-fls/build.sh");
    let build = Command::new("sh")
        .arg(&builder)
        .arg(&out)
        .output()
        .expect("build original C++ worker package");
    assert!(
        build.status.success(),
        "{} failed:\n{}\n{}",
        builder.display(),
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let package_bytes = [ROOT, PRODUCER, CONSUMER].map(|name| {
        std::fs::read(out.join(name)).unwrap_or_else(|error| panic!("read {name}: {error}"))
    });
    let package = std::array::from_fn::<_, 3, _>(|index| PackageModule {
        file_name: [ROOT, PRODUCER, CONSUMER][index],
        bytes: &package_bytes[index],
    });

    for image_count in [3, 2, 3] {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        state.set_program_arguments(&[ROOT]).unwrap();
        let mut imports = ImportRegistry::new();
        register_standard_services(&mut imports, Arc::clone(&state));
        let mut events = Vec::new();
        let result = run_package_startup_with_entry_observer(
            &package[..image_count],
            PackageStartupLayout {
                arena: ModuleLayoutArena {
                    base: 0x1_0000_0000,
                    size: 0x1000_0000,
                },
                stack_top: 0x2_0001_0000,
                thread_tls_base: 0x3_0000_0000,
            },
            0,
            imports,
            2_000_000,
            Some((Arc::clone(&state), 64)),
            |event| events.push(event),
        );
        if image_count == 2 {
            let Err(PackageStartupError::UnresolvedImports(diagnostics)) = result else {
                panic!("missing consumer DLL must fail during binding: {result:?}");
            };
            let unresolved = diagnostics.unresolved_imports().collect::<Vec<_>>();
            let [missing] = unresolved.as_slice() else {
                panic!("expected only the FLS callback to be unresolved: {unresolved:?}");
            };
            assert_eq!(missing.file_name, ROOT);
            assert_eq!(missing.requested_module, CONSUMER);
            assert_eq!(missing.symbol, ImportName::Name("WorkerCleanup".to_owned()));
            assert_eq!(
                missing.disposition,
                ImportDisposition::Unresolved(ImportFailure::MissingModule {
                    module: CONSUMER.to_owned()
                })
            );
            assert!(diagnostics.initialization.is_empty());
            assert!(events.is_empty());
            assert_eq!(state.exit_status(), None);
            assert_eq!(state.open_handle_count(), 0);
            continue;
        }

        let result = result.expect("run complete C++ worker package");
        assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
        assert_eq!(state.exit_status(), Some(0));
        assert_eq!(state.open_handle_count(), 0);
        assert!(result.dbt_metrics.native_bytes_emitted > 0);
        assert_eq!(result.diagnostics.unresolved_imports().count(), 0);
        assert!(matches!(
            events.as_slice(),
            [
                PackageEntryEvent::Entering { image_index: 0, .. },
                PackageEntryEvent::Leaving
            ]
        ));
        let entry = result.diagnostics.initialization.last().unwrap();
        assert_eq!(entry.file_name, ROOT);
        assert_eq!(entry.phase, InitializationPhase::ProcessEntry);
        assert_eq!(entry.outcome, InitializationOutcome::ProcessExit(0));

        for (index, consumer, provider, symbol) in [
            (1, ROOT, PRODUCER, "MakeRecord"),
            (2, ROOT, CONSUMER, "WorkerCleanup"),
            (1, CONSUMER, PRODUCER, "FinishRecord"),
        ] {
            assert!(result.diagnostics.imports.iter().any(|binding| {
                binding.file_name == consumer
                    && binding.requested_module == provider
                    && binding.symbol == ImportName::Name(symbol.to_owned())
                    && matches!(&binding.disposition, ImportDisposition::Package { provider_index, .. } if *provider_index == index)
            }), "{consumer}: expected package import {provider}!{symbol}");
        }
        for (caller, symbol) in [
            (ROOT, "CreateThread"),
            (ROOT, "ExitThread"),
            (ROOT, "FlsAlloc"),
            (ROOT, "FlsSetValue"),
            (ROOT, "WaitForSingleObject"),
            (CONSUMER, "GetCurrentThreadId"),
            (PRODUCER, "HeapAlloc"),
            (PRODUCER, "HeapFree"),
        ] {
            assert!(
                result.diagnostics.imports.iter().any(|binding| {
                    binding.file_name == caller
                        && binding.requested_module == "kernel32.dll"
                        && binding.symbol == ImportName::Name(symbol.to_owned())
                        && matches!(&binding.disposition, ImportDisposition::Service { .. })
                }),
                "{caller}: expected kernel32.dll!{symbol}"
            );
        }
    }
}
