// SPDX-License-Identifier: GPL-3.0-or-later

//! Initial-thread static TLS for an original Microsoft x64 EXE and two DLLs.
//! Clang emits each data access. The freestanding directory describes the
//! templates to the loader according to the [Microsoft PE format].
//!
//! Run `cargo test --locked -p xenora-loader --test guest_msvc_package_tls`
//! on Apple Silicon to build each image from source and run the packages.
//!
//! [Microsoft PE format]: https://learn.microsoft.com/en-us/windows/win32/debug/pe-format#the-tls-section

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use xenora_loader::import_table::ImportName;
use xenora_loader::module_graph::PackageModule;
use xenora_loader::module_layout::ModuleLayoutArena;
use xenora_loader::package::run_package_startup;
use xenora_loader::{
    ExitReason, ImportDisposition, ImportRegistry, MockClock, PackageStartupLayout, ServicesState,
    register_standard_services,
};

const ROOT: &str = "msvc-package-tls.exe";
const PRODUCER: &str = "xenora_tls_producer.dll";
const CONSUMER: &str = "xenora_tls_consumer.dll";

#[test]
fn compiled_package_tls_keeps_each_module_template_independent() {
    let out = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("msvc-package-tls-{}", std::process::id()));
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pe/msvc-package-tls/build.sh");
    let build = Command::new("sh")
        .arg(&script)
        .arg(&out)
        .output()
        .expect("run the native TLS package builder");
    assert!(
        build.status.success(),
        "{} failed:\n{}{}",
        script.display(),
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let root = fs::read(out.join(ROOT)).expect("read EXE");
    let producer = fs::read(out.join(PRODUCER)).expect("read producer DLL");
    let consumer = fs::read(out.join(CONSUMER)).expect("read consumer DLL");
    let altered = fs::read(out.join("reject").join(PRODUCER)).expect("read altered producer DLL");

    for (provider, expected_exit) in [(&producer, 0), (&altered, 11), (&producer, 0)] {
        let package = [
            PackageModule {
                file_name: ROOT,
                bytes: &root,
            },
            PackageModule {
                file_name: PRODUCER,
                bytes: provider,
            },
            PackageModule {
                file_name: CONSUMER,
                bytes: &consumer,
            },
        ];
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut imports = ImportRegistry::new();
        register_standard_services(&mut imports, Arc::clone(&state));
        let result = run_package_startup(
            &package,
            PackageStartupLayout {
                arena: ModuleLayoutArena {
                    base: 0x1_0000_0000,
                    size: 0x1000_0000,
                },
                stack_top: 0x2_0001_0000,
                thread_tls_base: 0x4_0000_0000,
            },
            0,
            imports,
            200_000,
            None,
        )
        .expect("map and start the explicit package");
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(expected_exit)
        );
        assert_eq!(state.exit_status(), Some(expected_exit));
        assert_eq!(state.open_handle_count(), 0);
        assert!(result.dbt_metrics.native_bytes_emitted > 0);
        assert_eq!(result.diagnostics.unresolved_imports().count(), 0);
        assert_eq!(result.tls.len(), 3);
        let mut indexes = BTreeSet::new();
        let mut bases = BTreeSet::new();
        for report in &result.tls {
            let prepared = report
                .prepared
                .as_ref()
                .expect("TLS directory in each image");
            let storage = prepared.storage.expect("copied TLS data");
            assert!(indexes.insert(prepared.tls_index), "shared TLS index");
            assert!(bases.insert(storage.base), "shared TLS block");
            assert!(storage.template_bytes >= 8);
            assert_eq!(
                result.diagnostics.modules[report.image_index].tls_index,
                Some(prepared.tls_index)
            );
        }
        assert!(
            result.diagnostics.imports.iter().any(|import| {
                import.file_name == CONSUMER
                    && import.symbol == ImportName::Name("ProducerSeed".to_owned())
                    && matches!(
                        &import.disposition,
                        ImportDisposition::Package { provider, .. } if provider == PRODUCER
                    )
            }),
            "consumer must call the producer through its own import"
        );
    }
}
