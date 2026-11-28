// SPDX-License-Identifier: GPL-3.0-or-later

//! Check the import boundary for compiler-emitted Microsoft x64 RTTI calls.
//! Run `cargo test --locked --target aarch64-apple-darwin -p xenora-loader
//! --test guest_msvc_rtti_boundary` on Apple Silicon. The builder can also
//! be run as `sh fixtures/pe/msvc-rtti-boundary/build.sh artifacts/msvc-rtti`.
//!
//! At O0 and O2 the RTTI-free control runs virtual calls through two base
//! subobjects. The other three variants preserve compiler-emitted metadata
//! and each imports one helper for a dynamic type query, pointer downcast
//! and side cast, or cast to the complete object's address. They stop before
//! guest entry. Their inert `type_info` vftable is link-only data.
//!
//! References are [Microsoft's runtime cast contract] and [Clang's lowering].
//! [Microsoft's runtime cast contract]: https://learn.microsoft.com/en-us/cpp/c-runtime-library/rtdynamiccast
//! [Clang's lowering]: https://clang.llvm.org/doxygen/CodeGen_2MicrosoftCXXABI_8cpp_source.html

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use xenora_loader::imports::ImportError;
use xenora_loader::probe::{ProbeConfig, probe_pe_bytes};
use xenora_loader::startup::{StartupEvent, StartupLayout, run_synthetic_startup_with_observer};
use xenora_loader::{
    ExitReason, ImportRegistry, MockClock, ServicesState, register_standard_services,
};

#[test]
fn compiler_emitted_rtti_imports_stop_before_guest_entry() {
    let output = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("msvc-rtti-boundary-{}", std::process::id()));
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pe/msvc-rtti-boundary/build.sh");
    let build = Command::new("sh")
        .arg(script)
        .arg(&output)
        .output()
        .unwrap();
    assert!(build.status.success(), "{build:?}");

    for optimization in [0, 2] {
        for (case, helper) in [
            None,
            Some("__RTtypeid"),
            Some("__RTDynamicCast"),
            Some("__RTCastToVoid"),
        ]
        .into_iter()
        .enumerate()
        {
            let image = format!("msvc-rtti-O{optimization}-case{case}");
            let bytes = std::fs::read(output.join(format!("{image}.exe"))).unwrap();
            let probe = probe_pe_bytes(&bytes, &ProbeConfig::default()).unwrap();
            assert_eq!(probe.import_error, None, "{image}");
            let mut imports: Vec<_> = probe
                .imports
                .iter()
                .map(|import| (import.dll.as_str(), import.symbol.as_str()))
                .collect();
            imports.sort_unstable();
            let mut expected = vec![("kernel32.dll", "ExitProcess")];
            expected.extend(helper.map(|symbol| ("vcruntime140.dll", symbol)));
            assert_eq!(imports, expected, "{image}");

            let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
            let mut registry = ImportRegistry::new();
            register_standard_services(&mut registry, Arc::clone(&state));
            let mut events = Vec::new();
            let result = run_synthetic_startup_with_observer(
                &bytes,
                StartupLayout {
                    image_base: 0x1_4000_0000,
                    stack_top: 0x2_0001_0000,
                    tls_base: 0x4_0000_0000,
                },
                registry,
                100_000,
                |event| events.push(event),
            );
            if let Some(symbol) = helper {
                let Err(error) = result else {
                    panic!("{image}: startup accepted an unresolved RTTI helper");
                };
                assert_eq!(
                    error.downcast_ref::<ImportError>(),
                    Some(&ImportError::UnresolvedImport {
                        module: "vcruntime140.dll".into(),
                        symbol: symbol.into(),
                    }),
                    "{image}: {error}",
                );
                assert!(
                    matches!(
                        events.as_slice(),
                        [
                            StartupEvent::Parsed { .. },
                            StartupEvent::Mapped { .. },
                            StartupEvent::Relocated { .. }
                        ]
                    ),
                    "{image}: startup must stop before binding completes"
                );
                assert_eq!(state.exit_status(), None, "{image}");
            } else {
                let result = result.expect("execute the ordinary polymorphic control");
                assert_eq!(
                    result.execution.exit_reason,
                    ExitReason::ProcessExit(0),
                    "{image}"
                );
                assert_eq!(state.exit_status(), Some(0), "{image}");
                assert!(result.dbt_metrics.native_bytes_emitted > 0, "{image}");
                assert!(
                    matches!(events.last(), Some(StartupEvent::Entering { .. })),
                    "{image}: control must reach guest entry"
                );
            }
        }
    }
}
