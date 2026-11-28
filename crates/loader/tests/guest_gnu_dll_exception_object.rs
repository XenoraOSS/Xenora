// SPDX-License-Identifier: GPL-3.0-or-later

//! On Apple Silicon, run `cargo test --locked --target aarch64-apple-darwin
//! -p xenora-loader --test guest_gnu_dll_exception_object`. The test builds
//! original C++ sources with an installed native MinGW-w64 GCC cross-toolchain.
//! To build separately, run `sh fixtures/pe/gnu-dll-exception-object/build.sh
//! artifacts/gnu-dll-exception-object -O1` (or use `-O2`).
//!
//! GCC's [link options] document shared libgcc for exceptions crossing modules.
//! The builder records the installed libgcc path in the output directory.
//! Both guests link libstdc++ statically and use Xenora's synthetic
//! pthread services. This exercises one typed catch and the producer DLL's
//! exception-object destructor using the shared SEH unwinder.
//!
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

const ROOT: &str = "gnu-dll-exception-object.exe";
const THROWER: &str = "thrower.dll";
const UNWINDER: &str = "libgcc_s_seh-1.dll";

#[test]
fn gnu_dll_exception_object_is_destroyed_after_the_exe_catch() {
    let out = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("gnu-dll-exception-object-{}", std::process::id()));
    let builder = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pe/gnu-dll-exception-object/build.sh");

    for optimization in ["-O1", "-O2"] {
        let out = out.join(optimization);
        let build = Command::new("sh")
            .arg(&builder)
            .arg(&out)
            .arg(optimization)
            .output()
            .expect("build original GNU DLL exception-object package");
        assert!(
            build.status.success(),
            "{optimization}: {} failed: {}{}",
            builder.display(),
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );

        let names = [ROOT, THROWER, UNWINDER];
        let unwinder_path = std::fs::read_to_string(out.join("unwinder-path.txt"))
            .expect("read installed libgcc path");
        let bytes = [
            out.join(ROOT),
            out.join(THROWER),
            Path::new(unwinder_path.trim_end_matches(['\r', '\n'])).to_owned(),
        ]
        .map(|path| {
            std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        });
        let package = std::array::from_fn::<_, 3, _>(|index| PackageModule {
            file_name: names[index],
            bytes: &bytes[index],
        });
        let resume = ImportName::Name("_Unwind_Resume".to_owned());

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
                    panic!("{optimization}: absent unwinder must fail during binding: {result:?}");
                };
                let unresolved = diagnostics.unresolved_imports().collect::<Vec<_>>();
                assert!(unresolved.iter().all(|binding| {
                    binding.requested_module == UNWINDER
                        && matches!(&binding.disposition,
                            ImportDisposition::Unresolved(ImportFailure::MissingExport { module, .. })
                            if module == UNWINDER)
                }), "{optimization}: unrelated refusal: {unresolved:?}");
                for caller in [ROOT, THROWER] {
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
            assert!(
                result
                    .exceptions
                    .iter()
                    .any(|report| { report.image_index == 1 && report.function_entries > 0 })
            );
            assert!(result.diagnostics.imports.iter().any(|binding| {
                binding.file_name == ROOT
                    && binding.requested_module == THROWER
                    && binding.symbol == ImportName::Name("ThrowFromDll".to_owned())
                    && matches!(
                        binding.disposition,
                        ImportDisposition::Package {
                            provider_index: 1,
                            ..
                        }
                    )
            }));
            for caller in [ROOT, THROWER] {
                assert!(
                    result.diagnostics.imports.iter().any(|binding| {
                        binding.file_name == caller
                            && binding.requested_module == UNWINDER
                            && binding.symbol == resume
                            && matches!(
                                binding.disposition,
                                ImportDisposition::Package {
                                    provider_index: 2,
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
