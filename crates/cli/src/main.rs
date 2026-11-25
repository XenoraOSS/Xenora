// SPDX-License-Identifier: GPL-3.0-or-later

mod coverage;
mod demo;
mod runner;
mod shaders;

use std::process::ExitCode;

/// Where `probe` writes and `imports sync` reads without `--report`.
const DEFAULT_REPORT_DIR: &str = "diagnostics/probe";

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("info") => {
            println!(
                "BetterXen {} ({})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::ARCH
            );
            ExitCode::SUCCESS
        }
        Some("inspect") => {
            let Some(file_path) = std::env::args().nth(2) else {
                eprintln!("usage: betterxen inspect <path-to-pe-binary>");
                return ExitCode::from(2);
            };
            match xenora_loader::triage_file(&file_path) {
                Ok(report) => {
                    print!("{report}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("inspect failed: {err}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("demo") => match demo::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("demo: {error}");
                ExitCode::FAILURE
            }
        },
        Some("run") => runner::command(std::env::args_os().skip(2)),
        Some("imports") => imports_command(std::env::args().skip(2).collect()),
        Some("shaders") => shaders::command(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("probe") => {
            let args: Vec<String> = std::env::args().collect();
            let mut pe_path: Option<String> = None;
            let mut mode = "static".to_string();
            let mut report_dir = DEFAULT_REPORT_DIR.to_string();

            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--pe" => {
                        if i + 1 < args.len() {
                            pe_path = Some(args[i + 1].clone());
                            i += 2;
                        } else {
                            eprintln!("missing argument for --pe");
                            return ExitCode::from(2);
                        }
                    }
                    "--mode" => {
                        if i + 1 < args.len() {
                            mode = args[i + 1].clone();
                            i += 2;
                        } else {
                            eprintln!("missing argument for --mode");
                            return ExitCode::from(2);
                        }
                    }
                    "--report" => {
                        if i + 1 < args.len() {
                            report_dir = args[i + 1].clone();
                            i += 2;
                        } else {
                            eprintln!("missing argument for --report");
                            return ExitCode::from(2);
                        }
                    }
                    other => {
                        eprintln!("unknown probe argument: {other}");
                        return ExitCode::from(2);
                    }
                }
            }

            let Some(pe) = pe_path else {
                eprintln!(
                    "usage: betterxen probe --pe <path> [--mode <static>] [--report <output-dir>]"
                );
                return ExitCode::from(2);
            };

            if mode != "static" {
                eprintln!("error: currently only --mode static is supported");
                return ExitCode::from(2);
            }

            let bytes = match std::fs::read(&pe) {
                Ok(b) => b,
                Err(err) => {
                    eprintln!("error reading PE binary '{pe}': {err}");
                    return ExitCode::FAILURE;
                }
            };

            let config = xenora_loader::ProbeConfig::default();
            match xenora_loader::run_probe_and_export(&bytes, &report_dir, &config) {
                Ok(report) => {
                    println!("BetterXen static PE probe complete.");
                    println!("Artifacts exported to: {report_dir}");
                    println!("Sections: {}", report.parsed.sections.len());
                    println!("Relocations: {}", report.relocations.total_entries);
                    println!("TLS Callbacks: {}", report.tls.callback_targets.len());
                    println!("Imported Thunks: {}", report.imports.len());
                    println!(
                        "Unresolved Standard Import Thunks: {}",
                        report
                            .imports
                            .iter()
                            .filter(|import| !import.is_supported)
                            .count()
                    );
                    if let Some(insn) = report.first_unsupported_dbt_insn {
                        println!("First DBT Blocker: RVA 0x{:08x} ({})", insn.rva, insn.notes);
                    }
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("probe failed: {err}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("usage: betterxen <info|demo|inspect|probe|run|imports|shaders>");
            ExitCode::from(2)
        }
    }
}

/// `betterxen imports sync [--report <dir>] [--check]`
/// `betterxen imports generate --pe <path> [--out <dir>]`
/// `betterxen imports metadata sync [--out <dir>]`
/// `betterxen imports verify`
/// `betterxen imports coverage [--out <file>]`
fn imports_command(args: Vec<String>) -> ExitCode {
    if args.is_empty() {
        eprintln!("usage: betterxen imports <sync|generate|metadata|verify|coverage> [options]");
        return ExitCode::from(2);
    }

    match args[0].as_str() {
        "generate" => run_imports_generate_cli(&args[1..]),
        "sync" => run_imports_sync_cli(&args[1..]),
        "metadata" => run_imports_metadata_cli(&args[1..]),
        "verify" => run_imports_verify_cli(&args[1..]),
        "coverage" => coverage::command(&args[1..]),
        other => {
            eprintln!("unknown imports subcommand: {other}");
            eprintln!(
                "usage: betterxen imports <sync|generate|metadata|verify|coverage> [options]"
            );
            ExitCode::from(2)
        }
    }
}

fn run_imports_metadata_cli(args: &[String]) -> ExitCode {
    let mut out_dir = "artifacts/metadata".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "sync" => {
                i += 1;
            }
            "--out" => {
                if i + 1 < args.len() {
                    out_dir = args[i + 1].clone();
                    i += 2;
                } else {
                    eprintln!("missing argument for --out");
                    return ExitCode::from(2);
                }
            }
            other => {
                eprintln!("unknown metadata argument: {other}");
                return ExitCode::from(2);
            }
        }
    }

    println!("Synchronizing offline API metadata catalog from public specifications...");
    let catalog = xenora_import_gen::MetadataCatalog::builtin();
    println!(
        "Loaded version {} with {} entries.",
        catalog.version,
        catalog.entries.len()
    );

    let _ = std::fs::create_dir_all(&out_dir);
    let out_file = format!("{out_dir}/api_catalog.json");
    let mut json = String::from("{\n  \"version\": 1,\n  \"entries\": [\n");
    let mut first = true;
    for ((module, sym), sig) in &catalog.entries {
        if !first {
            json.push_str(",\n");
        }
        first = false;
        json.push_str(&format!(
            "    {{\"module\": \"{}\", \"symbol\": \"{}\", \"header\": \"{}\", \"doc\": \"{}\"}}",
            module, sym, sig.header_source, sig.doc
        ));
    }
    json.push_str("\n  ]\n}\n");

    if let Err(e) = std::fs::write(&out_file, json) {
        eprintln!("failed to write metadata catalog: {e}");
        return ExitCode::FAILURE;
    }
    println!("API metadata catalog synchronized to: {out_file}");
    ExitCode::SUCCESS
}

fn run_imports_verify_cli(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        eprintln!("warning: verify takes no additional arguments");
    }
    println!("Running automated behavioral verification for generated import bindings...");
    match xenora_import_gen::run_verification() {
        Ok(res) => {
            for detail in &res.details {
                println!("  {detail}");
            }
            println!(
                "Verification summary: {}/{} tests passed.",
                res.passed, res.total
            );
            if res.failed > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(err) => {
            eprintln!("verification failed: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_imports_generate_cli(args: &[String]) -> ExitCode {
    let mut pe_path: Option<String> = None;
    let mut csv_path: Option<String> = None;
    let mut out_dir = "crates/loader/src/generated".to_string();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pe" => {
                if i + 1 < args.len() {
                    pe_path = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("missing argument for --pe");
                    return ExitCode::from(2);
                }
            }
            "--csv" => {
                if i + 1 < args.len() {
                    csv_path = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("missing argument for --csv");
                    return ExitCode::from(2);
                }
            }
            "--out" => {
                if i + 1 < args.len() {
                    out_dir = args[i + 1].clone();
                    i += 2;
                } else {
                    eprintln!("missing argument for --out");
                    return ExitCode::from(2);
                }
            }
            other => {
                eprintln!("unknown generate argument: {other}");
                return ExitCode::from(2);
            }
        }
    }

    let mut records = if let Some(ref pe) = pe_path {
        println!("Extracting imports from PE executable: {pe}");
        let bytes = match std::fs::read(pe) {
            Ok(b) => b,
            Err(err) => {
                eprintln!("error reading PE binary '{pe}': {err}");
                return ExitCode::FAILURE;
            }
        };
        let probe_cfg = xenora_loader::ProbeConfig::default();
        let report = match xenora_loader::probe::probe_pe_bytes(&bytes, &probe_cfg) {
            Ok(rep) => rep,
            Err(err) => {
                eprintln!("error probing PE binary: {err}");
                return ExitCode::FAILURE;
            }
        };
        report
            .imports
            .into_iter()
            .map(|imp| xenora_import_gen::ImportRecord {
                module: imp.dll,
                symbol: imp.symbol,
                ordinal: match imp.import_name {
                    xenora_loader::import_table::ImportName::Ordinal(ordinal) => Some(ordinal),
                    xenora_loader::import_table::ImportName::Name(_) => None,
                },
                iat_rva: imp.iat_rva as u64,
            })
            .collect::<Vec<_>>()
    } else if let Some(ref csv) = csv_path {
        match xenora_import_gen::parse_csv(csv) {
            Ok(recs) => recs,
            Err(err) => {
                eprintln!("error reading CSV '{csv}': {err}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        eprintln!(
            "usage: betterxen imports generate <--pe <executable> | --csv <missing_imports.csv>> [--out <dir>]"
        );
        return ExitCode::from(2);
    };

    records.sort();
    records.dedup();
    println!("Total imports extracted: {}", records.len());
    let db = xenora_import_gen::SymbolDb::builtin();
    let manifest = xenora_import_gen::Manifest::build(&records, &db);
    let summary = manifest.summary();

    println!("Catalog classification by import slot:");
    for (tier, count) in &summary {
        let pct = (*count as f64 / records.len() as f64 * 100.0) as u32;
        println!("  - {tier}: {count} ({pct}%)");
    }

    println!("Emitted disposition by distinct import:");
    for (disposition, count) in manifest.emission_summary() {
        println!("  - {disposition}: {count}");
    }

    let files = xenora_import_gen::generate(&manifest, &db);
    if let Err(err) = files.write_all(&out_dir) {
        eprintln!("failed to write generated files: {err}");
        return ExitCode::FAILURE;
    }
    let manifest_csv = format!("{out_dir}/generated_manifest.csv");
    if let Err(err) = manifest.write_csv(&manifest_csv) {
        eprintln!("failed to write manifest CSV: {err}");
        return ExitCode::FAILURE;
    }

    println!("Successfully generated bindings in '{out_dir}':");
    println!("  - {out_dir}/generated_crt.rs");
    println!("  - {out_dir}/generated_stubs.rs");
    println!("  - {out_dir}/generated_tests.rs");
    println!("  - {out_dir}/generated_manifest.md");
    println!("  - {out_dir}/generated_manifest.csv");

    ExitCode::SUCCESS
}

fn run_imports_sync_cli(args: &[String]) -> ExitCode {
    let mut report_dir = DEFAULT_REPORT_DIR.to_string();
    let mut check_mode = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--report" => {
                if i + 1 < args.len() {
                    report_dir = args[i + 1].clone();
                    i += 2;
                } else {
                    eprintln!("missing argument for --report");
                    return ExitCode::from(2);
                }
            }
            "--check" => {
                check_mode = true;
                i += 1;
            }
            other => {
                eprintln!("unknown sync argument: {other}");
                return ExitCode::from(2);
            }
        }
    }

    match run_imports_sync(&report_dir, check_mode) {
        Ok(changed) => {
            if check_mode {
                if changed {
                    eprintln!("imports sync --check: import_backlog.md is out of date");
                    ExitCode::FAILURE
                } else {
                    println!("imports sync --check: import_backlog.md is up to date");
                    ExitCode::SUCCESS
                }
            } else {
                println!("imports sync complete. Report written to: {report_dir}");
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("imports sync failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_imports_sync(
    report_dir: &str,
    check_mode: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use xenora_loader::ImportRegistry;
    use xenora_loader::import_table::ImportName;
    use xenora_loader::imports::ServiceStatus;
    use xenora_loader::services::{MockClock, ServicesState, register_standard_services};

    let imports_csv_path = std::path::Path::new(report_dir).join("imports.csv");
    if !imports_csv_path.exists() {
        return Err(format!(
            "imports.csv not found in '{report_dir}'. Run `betterxen probe` first."
        )
        .into());
    }

    // Build the standard registry to get current implementation state.
    let mut registry = ImportRegistry::new();
    register_standard_services(
        &mut registry,
        Arc::new(ServicesState::new(Arc::new(MockClock::new(0)))),
    );

    // Parse imports.csv (skip header line).
    let csv_text = std::fs::read_to_string(&imports_csv_path)?;
    let mut by_module: BTreeMap<String, Vec<(String, bool, Option<ServiceStatus>)>> =
        BTreeMap::new();
    let mut total = 0usize;
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();

    let mut lines = csv_text.lines();
    let header = lines.next().unwrap_or_default();
    let has_import_kind = header.ends_with(",import_kind");
    // Binding does not resolve delay-loaded imports, so the backlog omits them.
    let table_column = header
        .split(',')
        .position(|column| column == "import_table");
    for line in lines {
        if table_column.and_then(|index| line.split(',').nth(index)) == Some("delayed") {
            continue;
        }
        // Format: "module","symbol",0xRVA,category,registered,status,is_alias,"canonical",priority,cluster
        // We re-query the live registry rather than trusting cached status.
        let fields: Vec<&str> = line.splitn(10, ',').collect();
        if fields.len() < 2 {
            continue;
        }
        let module = fields[0].trim_matches('"').to_string();
        let symbol = fields[1].trim_matches('"').to_string();
        if module.is_empty() || symbol.is_empty() {
            continue;
        }

        let name = if has_import_kind && line.ends_with(",ordinal") {
            let Some(ordinal) = symbol.strip_prefix("Ordinal#").and_then(|n| n.parse().ok()) else {
                continue;
            };
            ImportName::Ordinal(ordinal)
        } else {
            ImportName::Name(symbol.clone())
        };
        let binding = registry.resolve(&module, &name);
        let is_registered = binding.is_some();
        let status = binding.map(|b| b.status);

        let label: &'static str = match status {
            Some(ServiceStatus::Implemented) => "implemented",
            Some(ServiceStatus::Partial) => "partial",
            Some(ServiceStatus::Stub) => "stub",
            None => {
                // Check explicit known-unsupported list
                match (module.to_ascii_lowercase().as_str(), symbol.as_str()) {
                    (
                        "api-ms-win-core-processthreads-l1-1-0.dll",
                        "CreateThread" | "ResumeThread" | "SuspendThread" | "TerminateProcess",
                    )
                    | (
                        "api-ms-win-core-synch-l1-1-0.dll",
                        "WaitForSingleObject" | "WaitForSingleObjectEx",
                    )
                    | ("api-ms-win-core-synch-l1-2-1.dll", "WaitForMultipleObjects")
                    | ("api-ms-win-core-rtlsupport-l1-1-0.dll", "RtlVirtualUnwind") => {
                        "unsupported"
                    }
                    _ => "missing",
                }
            }
        };

        *counts.entry(label).or_default() += 1;
        total += 1;
        by_module
            .entry(module.to_ascii_lowercase())
            .or_default()
            .push((symbol, is_registered, status));
    }

    // Build the refreshed import_backlog.md.
    let mut md = String::new();
    let _ = writeln!(md, "# BetterXen import compatibility backlog");
    let _ = writeln!(md);
    let _ = writeln!(
        md,
        "Generated by `betterxen imports sync`. Do not edit manually."
    );
    let _ = writeln!(
        md,
        "Re-run after adding service handlers to refresh this file."
    );
    let _ = writeln!(md);
    let _ = writeln!(md, "## Summary");
    let _ = writeln!(md);
    let _ = writeln!(md, "| Status | Count |");
    let _ = writeln!(md, "| --- | ---: |");
    let _ = writeln!(md, "| **Total** | {total} |");
    for (label, count) in &counts {
        let _ = writeln!(md, "| {label} | {count} |");
    }
    let _ = writeln!(md);

    let _ = writeln!(md, "## Missing handlers (highest value first)");
    let _ = writeln!(md);

    // Priority ordering: processthreads, synch, rtlsupport, memory, crt, file, graphics, other
    fn priority_key(module: &str) -> u8 {
        if module.contains("processthreads") || module.contains("core-handle") {
            1
        } else if module.contains("fibers") || module.contains("tls") {
            2
        } else if module.contains("core-synch") {
            3
        } else if module.contains("core-profile") || module.contains("core-sysinfo") {
            4
        } else if module.contains("core-memory")
            || module.contains("core-heap")
            || module == "xmem.dll"
        {
            5
        } else if module.starts_with("api-ms-win-crt-")
            || module.contains("vcruntime")
            || module.contains("libraryloader")
        {
            6
        } else if module.contains("rtlsupport") || module.contains("errorhandling") {
            7
        } else if module.contains("core-file") || module == "xcrdapi.dll" {
            8
        } else {
            9
        }
    }

    // Collect missing/unsupported into a prioritized list.
    let mut missing_entries: Vec<(u8, String, String)> = Vec::new();
    for (module, symbols) in &by_module {
        for (symbol, is_registered, _status) in symbols {
            if !is_registered {
                missing_entries.push((priority_key(module), module.clone(), symbol.clone()));
            }
        }
    }
    missing_entries.sort();

    let mut prev_priority = 255u8;
    for (priority, module, symbol) in &missing_entries {
        if *priority != prev_priority {
            let _ = writeln!(md, "### Priority {priority}");
            let _ = writeln!(md);
            prev_priority = *priority;
        }
        let _ = writeln!(md, "- `{module}!{symbol}`");
    }

    let _ = writeln!(md);
    let _ = writeln!(md, "## Registered handlers");
    let _ = writeln!(md);
    let _ = writeln!(md, "| Module | Symbol | Status |");
    let _ = writeln!(md, "| --- | --- | --- |");
    // Enumerate canonical registrations sorted by module, symbol
    let mut registered: BTreeSet<(String, String, &'static str)> = BTreeSet::new();
    for (module, symbol, status) in registry.iter_registered() {
        registered.insert((
            module.to_ascii_lowercase(),
            symbol.to_string(),
            status.as_str(),
        ));
    }
    for (module, symbol, status) in &registered {
        let _ = writeln!(md, "| `{module}` | `{symbol}` | {status} |");
    }

    let out_path = std::path::Path::new(report_dir).join("import_backlog.md");
    let existing = std::fs::read_to_string(&out_path).unwrap_or_default();
    let changed = existing != md;

    if !check_mode {
        std::fs::write(&out_path, &md)?;
        println!(
            "  Total imports: {total} | implemented: {} | partial: {} | stub: {} | missing: {}",
            counts.get("implemented").copied().unwrap_or(0),
            counts.get("partial").copied().unwrap_or(0),
            counts.get("stub").copied().unwrap_or(0),
            counts.get("missing").copied().unwrap_or(0),
        );
    }

    Ok(changed)
}

use std::fmt::Write as _;
