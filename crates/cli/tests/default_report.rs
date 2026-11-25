// SPDX-License-Identifier: GPL-3.0-or-later

//! Default report folder of `betterxen probe` and `betterxen imports sync`.

use std::process::Command;

#[test]
fn probe_and_sync_use_the_same_default_folder() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/pe/delay-imports.exe"
    );
    let folder = std::env::temp_dir().join(format!("xenora-default-{}", std::process::id()));
    std::fs::create_dir(&folder).unwrap();
    for arguments in [&["probe", "--pe", fixture][..], &["imports", "sync"]] {
        let status = Command::new(env!("CARGO_BIN_EXE_betterxen"))
            .args(arguments)
            .current_dir(&folder)
            .status()
            .unwrap();
        assert!(status.success(), "{arguments:?}");
    }
    let written = ["imports.csv", "import_backlog.md"]
        .map(|name| folder.join("diagnostics/probe").join(name).is_file());
    std::fs::remove_dir_all(folder).unwrap();
    assert_eq!(written, [true, true]);
}
