// SPDX-License-Identifier: GPL-3.0-or-later

//! Execute the original hosted C record codec at each optimizer level.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{ExitReason, ImportRegistry};

const LAYOUT: StartupLayout = StartupLayout {
    image_base: 0x2_5000_0000,
    stack_top: 0x2_0001_0000,
    tls_base: 0x3_0000_0000,
};
const BUDGET: usize = 1_000_000;
const FILE_PATH: &str = r"C:\records.rle";

// Count, then for each record the little-endian ID and value, tag size and tag.
const RECORDS: &[u8] = b"\x03\x34\x12\x04\x03\x02\x01\x05AAAAB\xff\x00\x00\x00\x00\x00\x05ccccc\xef\xbe\xff\xff\xff\xff\x03XYZ";

// Hand-derived packet boundaries. Literal headers are length minus one.
// Run headers are 0x80 plus length minus three.
const PACKED: &[u8] = &[
    0x07, 0x03, 0x34, 0x12, 0x04, 0x03, 0x02, 0x01, 0x05, // Initial literal
    0x81, b'A', // Four A bytes
    0x01, b'B', 0xff, // End of the first tag and the second ID's low byte
    0x82, 0x00, // Five zero bytes, including the second ID's high byte
    0x00, 0x05, // Second tag length
    0x82, b'c', // Five c bytes
    0x01, 0xef, 0xbe, // Third ID
    0x81, 0xff, // Four 0xff bytes
    0x03, 0x03, b'X', b'Y', b'Z', // Last tag length and tag
];

fn built() -> &'static Path {
    static ARTIFACTS: OnceLock<PathBuf> = OnceLock::new();
    ARTIFACTS.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let out = root
            .join("artifacts/hosted-record-codec")
            .join(std::process::id().to_string());
        let build = Command::new("sh")
            .arg(root.join("tests/compat_fixtures/build-hosted-record-codec.sh"))
            .arg(&out)
            .output()
            .expect("invoke the hosted record codec build");
        assert!(
            build.status.success(),
            "hosted record codec build failed:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        out
    })
}

fn expected_stdout() -> String {
    let mut report = String::new();
    for (name, bytes) in [("records", RECORDS), ("packed", PACKED)] {
        write!(report, "{name}=").unwrap();
        for byte in bytes {
            write!(report, "{byte:02x}").unwrap();
        }
        report.push_str("\r\n");
    }
    report.push_str("controls=truncated,overflow,length,record\r\n");
    report
}

fn check_codec(profile: &str) {
    let name = format!("hosted-record-codec-{profile}.exe");
    let bytes = std::fs::read(built().join(&name)).expect("read the original hosted guest PE");
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    state
        .set_program_arguments(&[&name])
        .expect("set hosted argv");

    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));
    let result = run_synthetic_startup(&bytes, LAYOUT, registry, BUDGET)
        .unwrap_or_else(|error| panic!("{profile} startup: {error}"));
    assert_eq!(
        result.execution.exit_reason,
        ExitReason::ProcessExit(0),
        "{profile} stopped at RIP {:#x} after {} guest instructions, stdout {:?}, stderr {:?}",
        result.execution.final_state.rip,
        result.execution.instructions_executed,
        String::from_utf8_lossy(&state.standard_output()),
        String::from_utf8_lossy(&state.standard_error()),
    );
    assert_eq!(state.exit_status(), Some(0));
    assert!(result.dbt_metrics.native_bytes_emitted > 0);
    assert!(state.standard_error().is_empty());
    assert_eq!(
        String::from_utf8(state.standard_output()).expect("guest report must be ASCII"),
        expected_stdout(),
        "{profile} codec and malformed-input report"
    );
    assert_eq!(
        state.file_contents(FILE_PATH).as_deref(),
        Some(PACKED),
        "{profile} compressed file"
    );
}

#[test]
fn hosted_record_codec_o0() {
    check_codec("O0");
}

#[test]
fn hosted_record_codec_o1() {
    check_codec("O1");
}

#[test]
fn hosted_record_codec_o2() {
    check_codec("O2");
}

#[test]
fn hosted_record_codec_o3() {
    check_codec("O3");
}
