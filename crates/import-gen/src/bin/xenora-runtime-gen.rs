// SPDX-License-Identifier: GPL-3.0-or-later

//! Generate the typed runtime adapters and report. Run from the repository root.
//! `--check` regenerates in memory and fails if a committed file differs.

use std::{path::Path, process::ExitCode};
use xenora_import_gen::typed::{generate_report, generate_rust, parse_with_test_suites};

const REGISTRY: &str = "crates/loader/runtime/services.txt";
const TESTS: &str = "crates/loader/tests/runtime_services.rs";
const RUST: &str = "crates/loader/src/runtime_table.rs";
const REPORT: &str = "docs/runtime/services.md";

fn run(mode: &str, root: &Path) -> Result<String, String> {
    let read =
        |path: &str| std::fs::read_to_string(root.join(path)).map_err(|e| format!("{path}: {e}"));
    let services = parse_with_test_suites(&read(REGISTRY)?, &read(TESTS)?, |suite| {
        read(&format!("crates/loader/tests/{suite}.rs"))
            .map_err(|error| format!("could not read test suite {suite}: {error}"))
    })
    .map_err(|e| format!("{REGISTRY}: {e}"))?;
    let outputs = [
        (RUST, generate_rust(&services)),
        (REPORT, generate_report(&services)),
    ];
    for (path, text) in &outputs {
        match mode {
            "generate" => {
                std::fs::write(root.join(path), text).map_err(|e| format!("{path}: {e}"))?
            }
            "--check" if read(path)? != *text => {
                return Err(format!(
                    "{path} is out of date; run xenora-runtime-gen generate"
                ));
            }
            "--check" | "validate" => {}
            _ => return Err("usage: xenora-runtime-gen validate|generate|--check".into()),
        }
    }
    Ok(format!("{} services", services.len()))
}

fn main() -> ExitCode {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match run(&mode, Path::new(".")) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_outputs_match_the_authoritative_catalog() {
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        run("--check", &crate_dir.join("../..")).unwrap();
    }
}
