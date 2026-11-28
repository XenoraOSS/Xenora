// SPDX-License-Identifier: GPL-3.0-or-later

//! An original Microsoft x64 ABI package checks shared identity and isolation
//! of the four synthetic codecvt ID cells. The pinned [Microsoft export list]
//! defines the names. The fixture does not exercise locale ID assignment,
//! facet construction, or conversion.
//!
//! [Microsoft export list]: https://github.com/microsoft/STL/blob/0cabaf3c7e0a73fe3040aabce40cd8833e80392b/stl/msbuild/stl_codecvt_ids/amd64.exports
//!
//! Run `cargo test --locked -p xenora-loader --test guest_msvc_codecvt_package`
//! on Apple Silicon to build and execute all three package images.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use xenora_loader::import_table::ImportName;
use xenora_loader::module_graph::PackageModule;
use xenora_loader::module_layout::ModuleLayoutArena;
use xenora_loader::package::run_package_startup_with_entry_observer;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::{
    ExitReason, ImportDisposition, ImportFailure, ImportRegistry, PackageStartupError,
    PackageStartupLayout,
};

const NORMAL: &str = "codecvt-package.exe";
const CONTROL: &str = "codecvt-package-control.exe";
const PRODUCER: &str = "xenora_codecvt_producer.dll";
const CONSUMER: &str = "xenora_codecvt_consumer.dll";
const SATELLITE: &str = "msvcp140_codecvt_ids.dll";
const IDS: [&str; 4] = [
    "?id@?$codecvt@_SDU_Mbstatet@@@std@@2V0locale@2@A",
    "?id@?$codecvt@_UDU_Mbstatet@@@std@@2V0locale@2@A",
    "?id@?$codecvt@_S_QU_Mbstatet@@@std@@2V0locale@2@A",
    "?id@?$codecvt@_U_QU_Mbstatet@@@std@@2V0locale@2@A",
];
const UNSUPPORTED: &str = "?id@?$codecvt@_WDU_Mbstatet@@@std@@2V0locale@2@A";

#[test]
fn codecvt_data_cells_have_the_same_identity_in_each_package_image() {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("codecvt-package-{}", std::process::id()));
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/pe/msvc-codecvt-package/build.sh");
    let build = Command::new("sh")
        .arg(&script)
        .arg(&directory)
        .output()
        .expect("run the native codecvt package builder");
    assert!(
        build.status.success(),
        "{} failed:\n{}{}",
        script.display(),
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let read = |name| fs::read(directory.join(name)).expect("read the source-built guest image");
    let normal = read(NORMAL);
    let control = read(CONTROL);
    let producer = read(PRODUCER);
    let consumer = read(CONSUMER);

    for (name, executable) in [(NORMAL, &normal), (CONTROL, &control), (NORMAL, &normal)] {
        let package = [
            PackageModule {
                file_name: name,
                bytes: executable,
            },
            PackageModule {
                file_name: PRODUCER,
                bytes: &producer,
            },
            PackageModule {
                file_name: CONSUMER,
                bytes: &consumer,
            },
        ];
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register_standard_services(&mut registry, Arc::clone(&state));
        let mut entry_events = Vec::new();
        let run = run_package_startup_with_entry_observer(
            &package,
            PackageStartupLayout {
                arena: ModuleLayoutArena {
                    base: 0x1_0000_0000,
                    size: 0x1000_0000,
                },
                stack_top: 0x2_0001_0000,
                // Data imports allocate from the process heap before the
                // package maps its thread-local storage and TEB.
                thread_tls_base: 0x4_0000_0000,
            },
            0,
            registry,
            200_000,
            None,
            |event| entry_events.push(event),
        );

        if name == CONTROL {
            let Err(PackageStartupError::UnresolvedImports(diagnostics)) = run else {
                panic!("the unsupported codecvt specialization must refuse startup");
            };
            let unresolved: Vec<_> = diagnostics.unresolved_imports().collect();
            assert_eq!(unresolved.len(), 1);
            assert_eq!(unresolved[0].file_name, CONTROL);
            assert_eq!(unresolved[0].requested_module, SATELLITE);
            assert_eq!(unresolved[0].symbol, ImportName::Name(UNSUPPORTED.into()));
            assert_eq!(
                unresolved[0].disposition,
                ImportDisposition::Unresolved(ImportFailure::MissingExport {
                    module: SATELLITE.into(),
                    symbol: UNSUPPORTED.into(),
                })
            );
            assert!(entry_events.is_empty());
            assert!(diagnostics.initialization.is_empty());
            assert_eq!(state.exit_status(), None);
            continue;
        }

        let result = run.expect("start the source-built codecvt package");
        assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
        assert_eq!(state.exit_status(), Some(0));
        assert_eq!(entry_events.len(), 2);
        assert!(result.dbt_metrics.native_bytes_emitted > 0);
        assert_eq!(result.diagnostics.unresolved_imports().count(), 0);

        let satellite_imports: Vec<_> = result
            .diagnostics
            .imports
            .iter()
            .filter(|import| import.requested_module == SATELLITE)
            .collect();
        assert_eq!(satellite_imports.len(), 12);
        let mut unique_cells = BTreeSet::new();
        for symbol in IDS {
            let bindings: Vec<_> = satellite_imports
                .iter()
                .filter(|import| import.symbol == ImportName::Name(symbol.into()))
                .map(|import| {
                    let ImportDisposition::Service {
                        module,
                        address,
                        data: true,
                        ..
                    } = &import.disposition
                    else {
                        panic!("{symbol} must bind to a service's data cell");
                    };
                    assert_eq!(module, SATELLITE);
                    assert_ne!(*address, 0);
                    (import.file_name.as_str(), *address)
                })
                .collect();
            assert_eq!(bindings.len(), 3);
            let address = bindings[0].1;
            for image in [NORMAL, PRODUCER, CONSUMER] {
                assert!(bindings.contains(&(image, address)), "{image}!{symbol}");
            }
            assert!(
                unique_cells.insert(address),
                "{symbol} must have its own cell"
            );
        }

        for (provider, symbols) in [
            (PRODUCER, ["ProducerCell", "ProducerRead", "ProducerWrite"]),
            (CONSUMER, ["ConsumerCell", "ConsumerRead", "ConsumerWrite"]),
        ] {
            for symbol in symbols {
                assert!(result.diagnostics.imports.iter().any(|import| {
                    import.file_name == NORMAL
                        && import.requested_module == provider
                        && import.symbol == ImportName::Name(symbol.into())
                        && matches!(
                            &import.disposition,
                            ImportDisposition::Package { provider: bound, .. } if bound == provider
                        )
                }), "{provider}!{symbol} must resolve within the package");
            }
        }
    }
}
