// SPDX-License-Identifier: GPL-3.0-or-later

//! Build the original fixtures on Apple Silicon with Clang, LLD and an
//! installed native MinGW-w64 GCC cross-toolchain.
//! Run `cargo test --locked -p xenora-loader --test guest_gnu_cross_dll_unwind`
//! or build the images separately with
//! `sh fixtures/pe/gnu-cross-dll-unwind/build.sh artifacts/gnu-cross-dll-unwind`.
//!
//! The C relay DLLs carry Win64 unwind records. The EXE owns the GNU exception
//! runtime, throws integer 71, records the two destructors, and catches the
//! exception. The fixture checks an ordinary relay return, exact refusal when
//! the inner import provider is missing, and a successful fresh run.
//!
//! [GNU C++ exception ABI]: https://itanium-cxx-abi.github.io/cxx-abi/abi-eh.html
//! [Windows x64 unwind metadata]: https://learn.microsoft.com/en-us/cpp/build/exception-handling-x64

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

const ROOT: &str = "gnu-cross-dll-unwind.exe";
const OUTER: &str = "unwind_outer.dll";
const INNER: &str = "unwind_inner.dll";

#[test]
fn gnu_exception_unwinds_through_both_relay_dlls() {
    let out = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("gnu-cross-dll-unwind-{}", std::process::id()));
    let builder = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pe/gnu-cross-dll-unwind/build.sh");
    let build = Command::new("sh")
        .arg(&builder)
        .arg(&out)
        .output()
        .expect("build original GNU unwind package");
    assert!(
        build.status.success(),
        "{} failed:\n{}\n{}",
        builder.display(),
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let package_bytes = [ROOT, OUTER, INNER].map(|name| {
        std::fs::read(out.join(name)).unwrap_or_else(|error| panic!("read {name}: {error}"))
    });
    let package = std::array::from_fn::<_, 3, _>(|index| PackageModule {
        file_name: [ROOT, OUTER, INNER][index],
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
            20_000_000,
            None,
            |event| events.push(event),
        );
        if image_count == 2 {
            let Err(PackageStartupError::UnresolvedImports(diagnostics)) = result else {
                panic!("missing inner DLL must fail during binding: {result:?}");
            };
            let unresolved = diagnostics.unresolved_imports().collect::<Vec<_>>();
            let [missing] = unresolved.as_slice() else {
                panic!("expected only the inner relay to be unresolved: {unresolved:?}");
            };
            assert_eq!(missing.file_name, OUTER);
            assert_eq!(missing.requested_module, INNER);
            assert_eq!(missing.symbol, ImportName::Name("RelayInner".to_owned()));
            assert_eq!(
                missing.disposition,
                ImportDisposition::Unresolved(ImportFailure::MissingModule {
                    module: INNER.to_owned()
                })
            );
            assert!(diagnostics.initialization.is_empty());
            assert!(events.is_empty());
            assert_eq!(state.exit_status(), None);
            continue;
        }

        let result = result.expect("run complete GNU unwind package");
        assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
        assert_eq!(state.exit_status(), Some(0));
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
            (1, ROOT, OUTER, "RelayOuter"),
            (2, OUTER, INNER, "RelayInner"),
        ] {
            assert!(
                result
                    .exceptions
                    .iter()
                    .any(|report| { report.image_index == index && report.function_entries > 0 }),
                "{provider}: expected registered unwind information"
            );
            assert!(result.diagnostics.imports.iter().any(|binding| {
                binding.file_name == consumer
                    && binding.requested_module == provider
                    && binding.symbol == ImportName::Name(symbol.to_owned())
                    && matches!(&binding.disposition, ImportDisposition::Package { provider_index, .. } if *provider_index == index)
            }), "{consumer}: expected package import {provider}!{symbol}");
        }
    }
}
