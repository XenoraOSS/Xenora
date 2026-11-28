// SPDX-License-Identifier: GPL-3.0-or-later

//! On Apple Silicon, run `cargo test --locked --target aarch64-apple-darwin
//! -p xenora-loader --test guest_gnu_nested_exceptions`. The test builds
//! original C++ sources with an installed native MinGW-w64 GCC cross-toolchain.
//! To build separately, run `sh fixtures/pe/gnu-nested-exceptions/build.sh
//! artifacts/gnu-nested-exceptions -O1` (or use `-O2`).
//!
//! GCC's [link options] document shared libgcc for exceptions crossing modules.
//! The builder records the installed libgcc path in the output directory.
//! The EXE and both DLLs link libstdc++ statically and use Xenora's synthetic
//! pthread services. The bridge catches an inner exception while handling an
//! outer exception and then rethrows the same outer object to the EXE. A
//! bare throw reactivates the existing exception object ([C++ throw expression]).
//! The guest checks both object addresses and exact cleanup order.
//!
//! [C++ throw expression]: https://eel.is/c++draft/expr.throw
//! [link options]: https://gcc.gnu.org/onlinedocs/gcc/Link-Options.html

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use xenora_loader::import_table::ImportName;
use xenora_loader::module_graph::PackageModule;
use xenora_loader::module_layout::ModuleLayoutArena;
use xenora_loader::package::{
    ImportDisposition, ImportFailure, PackageEntryEvent, PackageStartupError, PackageStartupLayout,
    run_package_startup_with_entry_observer,
};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::{ExitReason, ImportRegistry};

const ROOT: &str = "gnu-nested-exceptions.exe";
const BRIDGE: &str = "nested_bridge.dll";
const PRODUCER: &str = "nested_producer.dll";
const UNWINDER: &str = "libgcc_s_seh-1.dll";

#[test]
fn gnu_nested_catch_rethrows_the_same_outer_object() {
    let out = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("gnu-nested-exceptions-{}", std::process::id()));
    let builder = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pe/gnu-nested-exceptions/build.sh");

    for optimization in ["-O1", "-O2"] {
        let out = out.join(optimization);
        let build = Command::new("sh")
            .arg(&builder)
            .arg(&out)
            .arg(optimization)
            .output()
            .expect("build original GNU nested exception package");
        assert!(
            build.status.success(),
            "{optimization}: {} failed: {}{}",
            builder.display(),
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );

        let names = [ROOT, BRIDGE, PRODUCER, UNWINDER];
        let unwinder_path = std::fs::read_to_string(out.join("unwinder-path.txt"))
            .expect("read installed libgcc path");
        let bytes = [
            out.join(ROOT),
            out.join(BRIDGE),
            out.join(PRODUCER),
            Path::new(unwinder_path.trim_end_matches(['\r', '\n'])).to_owned(),
        ]
        .map(|path| {
            std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        });
        let package = std::array::from_fn::<_, 4, _>(|index| PackageModule {
            file_name: names[index],
            bytes: &bytes[index],
        });
        let resume = ImportName::Name("_Unwind_Resume".to_owned());

        for image_count in [4, 3, 4] {
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

            if image_count == 3 {
                let Err(PackageStartupError::UnresolvedImports(diagnostics)) = result else {
                    panic!("{optimization}: absent unwinder must fail during binding: {result:?}");
                };
                let unresolved = diagnostics.unresolved_imports().collect::<Vec<_>>();
                assert!(unresolved.iter().all(|binding| {
                    binding.requested_module == UNWINDER
                        && matches!(&binding.disposition,
                            ImportDisposition::Unresolved(ImportFailure::MissingExport { module, .. })
                            if module == UNWINDER)
                }), "{optimization}: unrelated refusal: {unresolved:?}");
                for caller in [ROOT, BRIDGE, PRODUCER] {
                    assert!(
                        unresolved
                            .iter()
                            .any(|binding| binding.file_name == caller && binding.symbol == resume),
                        "{optimization}: expected unresolved {UNWINDER}!_Unwind_Resume in {caller}: {unresolved:?}"
                    );
                }
                assert!(diagnostics.initialization.is_empty());
                assert!(events.is_empty());
                assert_eq!(state.exit_status(), None);
                continue;
            }

            let result = result.unwrap_or_else(|error| panic!("{optimization}: {error:?}"));
            assert_eq!(
                result.execution.exit_reason,
                ExitReason::ProcessExit(0),
                "{optimization}"
            );
            assert_eq!(state.exit_status(), Some(0));
            assert!(result.dbt_metrics.native_bytes_emitted > 0);
            assert!(matches!(
                events.as_slice(),
                [
                    PackageEntryEvent::Entering { image_index: 0, .. },
                    PackageEntryEvent::Leaving
                ]
            ));
            for image_index in 1..=2 {
                assert!(result.exceptions.iter().any(|report| {
                    report.image_index == image_index && report.function_entries > 0
                }));
            }
            for (consumer, provider, provider_index, symbol) in [
                (ROOT, BRIDGE, 1, "Bridge"),
                (BRIDGE, PRODUCER, 2, "ProduceOuter"),
                (BRIDGE, PRODUCER, 2, "ProduceInner"),
            ] {
                assert!(
                    result.diagnostics.imports.iter().any(|binding| {
                        binding.file_name == consumer
                            && binding.requested_module == provider
                            && binding.symbol == ImportName::Name(symbol.to_owned())
                            && matches!(binding.disposition, ImportDisposition::Package {
                            provider_index: index, ..
                        } if index == provider_index)
                    }),
                    "{optimization}: expected {consumer} to import {provider}!{symbol}"
                );
            }
            for caller in [ROOT, BRIDGE, PRODUCER] {
                assert!(
                    result.diagnostics.imports.iter().any(|binding| {
                        binding.file_name == caller
                            && binding.requested_module == UNWINDER
                            && binding.symbol == resume
                            && matches!(
                                binding.disposition,
                                ImportDisposition::Package {
                                    provider_index: 3,
                                    ..
                                }
                            )
                    }),
                    "{optimization}: expected {caller} to import the packaged unwinder"
                );
            }
        }
    }
}
