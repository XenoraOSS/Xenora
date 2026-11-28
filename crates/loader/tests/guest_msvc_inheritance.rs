// SPDX-License-Identifier: GPL-3.0-or-later

//! Run `cargo test --locked -p xenora-loader --test guest_msvc_inheritance`
//! on Apple Silicon. The test source-builds an EXE and two independently
//! compiled DLLs with the Microsoft x64 ABI. The consumer sees only the
//! three base classes. It converts both views to the shared virtual base,
//! dispatches through them, and deletes through the virtual-base pointer.
//!
//! A normal or retried handoff prints `SLRPabccprlsF`, with construction
//! in the first four events, virtual calls in the next four, and destruction
//! and free in the last five. The rejected view leaves the object live for
//! a retry. The normal run exits with 70 and the successful retry with 24.
//!
//! The expected trace follows [multiple and virtual inheritance],
//! [base initialization order], and [destruction order]. HeapSize observes
//! the synthetic guest heap. The fixture disables RTTI and exceptions.
//!
//! [multiple and virtual inheritance]: https://eel.is/c++draft/class.mi
//! [base initialization order]: https://eel.is/c++draft/class.base.init
//! [destruction order]: https://eel.is/c++draft/class.dtor

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use xenora_loader::import_table::ImportName;
use xenora_loader::module_graph::PackageModule;
use xenora_loader::module_layout::ModuleLayoutArena;
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::{
    ExitReason, ImportDisposition, ImportRegistry, PackageStartupLayout, run_package_startup,
};

const PRODUCER: &str = "xenora_inheritance_producer.dll";
const CONSUMER: &str = "xenora_inheritance_consumer.dll";
const NORMAL: &str = "inheritance.exe";
const REJECT: &str = "inheritance-reject.exe";
const EXPECTED_TRACE: &[u8] = b"SLRPabccprlsF";

#[test]
fn microsoft_x64_base_views_share_a_virtual_base_and_release_the_complete_object() {
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("msvc-inheritance-{}", std::process::id()));
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pe/msvc-inheritance/build.sh");
    let build = Command::new("sh")
        .arg(&script)
        .arg(&directory)
        .output()
        .expect("run the native inheritance fixture builder");
    assert!(
        build.status.success(),
        "{} failed:\n{}{}",
        script.display(),
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let read = |name| fs::read(directory.join(name)).expect("read the compiled image");
    let normal = read(NORMAL);
    let rejected = read(REJECT);
    let producer = read(PRODUCER);
    let consumer = read(CONSUMER);

    // In the middle run, a null right view is rejected before dispatch or
    // release. The root checks the same allocation is live and retries it.
    for (name, application, expected_status) in [
        (NORMAL, &normal, 70),
        (REJECT, &rejected, 24),
        (NORMAL, &normal, 70),
    ] {
        let package = [
            PackageModule {
                file_name: name,
                bytes: application,
            },
            PackageModule {
                file_name: CONSUMER,
                bytes: &consumer,
            },
            PackageModule {
                file_name: PRODUCER,
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
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut imports = ImportRegistry::new();
        register_standard_services(&mut imports, Arc::clone(&state));
        let result = run_package_startup(&package, layout, 0, imports, 200_000, None)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            result.execution.exit_reason,
            ExitReason::ProcessExit(expected_status),
            "{name}: trace {:?}",
            String::from_utf8_lossy(&state.standard_output())
        );
        assert_eq!(state.exit_status(), Some(expected_status), "{name}");
        assert_eq!(state.standard_output(), EXPECTED_TRACE, "{name}");
        assert!(state.standard_error().is_empty(), "{name}");
        assert_eq!(state.open_handle_count(), 0, "{name}");
        assert!(result.dbt_metrics.native_bytes_emitted > 0, "{name}");
        assert_eq!(result.diagnostics.unresolved_imports().count(), 0, "{name}");

        for (provider, symbol) in [
            (PRODUCER, "ProduceViews"),
            (PRODUCER, "EventCount"),
            (CONSUMER, "ConsumeViews"),
        ] {
            assert!(
                result.diagnostics.imports.iter().any(|entry| {
                    entry.file_name == name
                        && entry.requested_module == provider
                        && entry.symbol == ImportName::Name(symbol.to_owned())
                        && matches!(
                            &entry.disposition,
                            ImportDisposition::Package { provider: bound, .. } if bound == provider
                        )
                }),
                "{name}: missing {provider}!{symbol}"
            );
        }
        assert!(
            result
                .diagnostics
                .imports
                .iter()
                .all(|entry| entry.file_name != CONSUMER),
            "the consumer dispatches through the supplied views"
        );
    }
}
