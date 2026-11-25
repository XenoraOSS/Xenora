// SPDX-License-Identifier: GPL-3.0-or-later

//! Rust source code generator.
//!
//! Produces three files:
//! - `generated_crt.rs`   — `register_generated_crt_services(registry)`
//! - `generated_stubs.rs` — `register_generated_stubs(registry)`
//! - `generated_tests.rs` — `#[cfg(test)] mod generated_tests { ... }`
//!
//! All generated code uses only the existing `ImportRegistry` / `ServiceHandler` types.
//! No unsafe code is emitted in the registration layer; `extern "C"` libc calls are
//! wrapped in `unsafe` blocks with explicit guest-memory bounds checks.

use crate::classify::Classification;
use crate::classify::SymbolDb;
use crate::manifest::Manifest;

/// The three output files plus the Markdown manifest summary.
pub struct GeneratedFiles {
    pub crt_rs: String,
    pub stubs_rs: String,
    pub tests_rs: String,
    pub manifest_md: String,
}

impl GeneratedFiles {
    /// Write all files into `out_dir`, creating it if needed.
    pub fn write_all(&self, out_dir: &str) -> Result<(), String> {
        std::fs::create_dir_all(out_dir).map_err(|e| format!("create dir {out_dir}: {e}"))?;
        write_file(out_dir, "generated_crt.rs", &self.crt_rs)?;
        write_file(out_dir, "generated_stubs.rs", &self.stubs_rs)?;
        write_file(out_dir, "generated_tests.rs", &self.tests_rs)?;
        write_file(out_dir, "generated_manifest.md", &self.manifest_md)?;
        Ok(())
    }
}

fn write_file(dir: &str, name: &str, content: &str) -> Result<(), String> {
    let path = format!("{dir}/{name}");
    std::fs::write(&path, content).map_err(|e| format!("write {path}: {e}"))
}

fn unique_entries(manifest: &Manifest) -> impl Iterator<Item = &crate::manifest::ManifestEntry> {
    let mut seen = std::collections::BTreeSet::new();
    manifest
        .entries
        .iter()
        .filter(move |entry| seen.insert(entry.key()))
}

/// Generate all files from the manifest.
pub fn generate(manifest: &Manifest, _db: &SymbolDb) -> GeneratedFiles {
    let crt_rs = gen_crt(manifest);
    let stubs_rs = gen_stubs(manifest);
    let tests_rs = gen_tests(manifest);
    let manifest_md = manifest.render_markdown();
    GeneratedFiles {
        crt_rs,
        stubs_rs,
        tests_rs,
        manifest_md,
    }
}

// ---------------------------------------------------------------------------
// CRT native bindings
// ---------------------------------------------------------------------------

fn gen_crt(manifest: &Manifest) -> String {
    let mut out = file_header("generated_crt.rs");
    out.push_str(
        r#"
//! Auto-generated CRT service registrations.
//! Maps Windows CRT API-set names to native macOS libc equivalents.
//! All mappings are derived from public ISO C / POSIX documentation.
//! Do not edit manually — regenerate with `xenora imports generate`.

use crate::cpu::{CpuState, ExitReason, Register};
use crate::imports::{ImportRegistry, ServiceStatus, Win64Args};
use crate::memory::GuestMemory;

/// Register all automatically generated CRT bindings into `registry`.
///
/// These implementations are STUB-VERIFIED, not independently confirmed against
/// a live Xbox runtime.  They must pass the conformance tests in
/// `generated_tests.rs` before being marked `ServiceStatus::Implemented`.
#[allow(unused_variables)]
pub fn register_generated_crt_services(registry: &mut ImportRegistry) {
"#,
    );

    for entry in unique_entries(manifest) {
        match &entry.classification {
            Classification::CrtNative { libc_sym, note } => {
                out.push_str(&gen_crt_native_registration(
                    &entry.module,
                    &entry.symbol,
                    libc_sym,
                    note,
                ));
            }
            Classification::CrtWrapper {
                libc_sym,
                wrapper_note,
            } => {
                out.push_str(&gen_crt_wrapper_registration(
                    &entry.module,
                    &entry.symbol,
                    libc_sym,
                    wrapper_note,
                ));
            }
            Classification::RuntimeHelper { note } => {
                out.push_str(&gen_runtime_helper_registration(
                    &entry.module,
                    &entry.symbol,
                    note,
                ));
            }
            _ => {} // Win32Stub and Manual go to stubs_rs
        }
    }

    out.push_str("}\n");
    out
}

/// Generate a registration block for a CrtNative symbol.
/// These use a simple `Win64Args` → libc dispatch pattern.
fn gen_crt_native_registration(module: &str, symbol: &str, _libc_sym: &str, note: &str) -> String {
    // Determine the calling pattern based on symbol name
    let (status, dispatch) = match crt_dispatch_expr(symbol) {
        Some(dispatch) => ("Partial", dispatch),
        None => (
            "Stub",
            "Err(ExitReason::IllegalInstruction(0xFE))".to_owned(),
        ),
    };

    format!(
        r#"
    // {note}
    // Source: ISO C / POSIX public specification.
    registry.register_with_status(
        "{module}", "{symbol}",
        ServiceStatus::{status},
        |args: &Win64Args, _cpu: &mut CpuState, mem: &mut GuestMemory| -> Result<u64, ExitReason> {{
            {dispatch}
        }},
    );
"#,
        module = module,
        symbol = symbol,
        note = note,
        dispatch = dispatch,
    )
}

/// Generate a stub-wrapper registration for a CrtWrapper symbol (e.g. strcat_s → strcat).
fn gen_crt_wrapper_registration(module: &str, symbol: &str, _libc_sym: &str, note: &str) -> String {
    format!(
        r#"
    // {note}
    registry.register_with_status(
        "{module}", "{symbol}",
        ServiceStatus::Stub,
        |args: &Win64Args, _cpu: &mut CpuState, mem: &mut GuestMemory| -> Result<u64, ExitReason> {{
            Err(ExitReason::IllegalInstruction(0xFE))
        }},
    );
"#,
    )
}

/// Generate a registration for a runtime helper (EH frame handlers, etc.).
fn gen_runtime_helper_registration(module: &str, symbol: &str, note: &str) -> String {
    // Not implemented: fails explicitly instead of reporting success.
    let body = "Err(ExitReason::IllegalInstruction(0xFE)) // 0xFE = Unsupported Host Service Call";
    format!(
        r#"
    // {note}
    registry.register_with_status(
        "{module}", "{symbol}",
        ServiceStatus::Stub,
        |args: &Win64Args, _cpu: &mut CpuState, mem: &mut GuestMemory| -> Result<u64, ExitReason> {{
            {body}
        }},
    );
"#,
    )
}

// ---------------------------------------------------------------------------
// Win32 stubs
// ---------------------------------------------------------------------------

fn gen_stubs(manifest: &Manifest) -> String {
    let mut out = file_header("generated_stubs.rs");
    out.push_str(
        r#"
//! Auto-generated Win32 service stubs.
//! Each stub stops the guest with an explicit unsupported-service exit.
//! Do not edit manually — regenerate with `xenora imports generate`.

use crate::cpu::{CpuState, ExitReason};
use crate::imports::{ImportRegistry, ServiceStatus, Win64Args};
use crate::memory::GuestMemory;

/// Register all automatically generated Win32 stubs into `registry`.
#[allow(unused_variables)]
pub fn register_generated_stubs(registry: &mut ImportRegistry) {
"#,
    );

    for entry in unique_entries(manifest) {
        if let Classification::Win32Stub { note, .. } = &entry.classification {
            out.push_str(&format!(
                r#"
    // {note}
    // STUB: Fails explicitly with unsupported status instead of fabricating success.
    registry.register_with_status(
        "{module}", "{symbol}",
        ServiceStatus::Stub,
        |_args: &Win64Args, _cpu: &mut CpuState, _mem: &mut GuestMemory| -> Result<u64, ExitReason> {{
            Err(ExitReason::IllegalInstruction(0xFE)) // 0xFE = Unsupported Host Service Call
        }},
    );
"#,
                module = entry.module,
                symbol = entry.symbol,
                note = note,
            ));
        }
    }

    out.push_str("}\n");
    out
}

// ---------------------------------------------------------------------------
// Conformance tests
// ---------------------------------------------------------------------------

fn gen_tests(manifest: &Manifest) -> String {
    let mut out = file_header("generated_tests.rs");
    out.push_str(
        r#"
//! Conformance tests for emitted service registrations.
//! Place this file and the two generated registration files together as
//! an integration test in a Cargo package that depends on xenora-loader.

pub use xenora_loader::{cpu, imports, memory};

#[path = "generated_crt.rs"]
mod generated_crt;
#[path = "generated_stubs.rs"]
mod generated_stubs;

use cpu::{CpuState, ExitReason};
use imports::{ImportRegistry, ServiceStatus, Win64Args};
use memory::GuestMemory;

fn check(module: &str, symbol: &str, input: u64, status: ServiceStatus, expected: Result<u64, ExitReason>) {
    let mut registry = ImportRegistry::new();
    generated_crt::register_generated_crt_services(&mut registry);
    generated_stubs::register_generated_stubs(&mut registry);
    let service = registry.binding(module, symbol).expect("generated binding");
    assert_eq!(service.status, status);
    let args = Win64Args { rcx: input, rdx: 0, r8: 0, r9: 0, rsp: 0x8000 };
    let mut cpu = CpuState::new(0x1000, 0x8000);
    let mut memory = GuestMemory::new();
    assert_eq!((service.handler)(&args, &mut cpu, &mut memory), expected);
}
"#,
    );

    for (index, entry) in unique_entries(manifest).enumerate() {
        let (input, status, expected) = match &entry.classification {
            Classification::CrtNative { .. } if crt_dispatch_expr(&entry.symbol).is_some() => {
                let Some((input, expected)) = canonical_integer_case(&entry.symbol) else {
                    continue;
                };
                (input, "Partial", format!("Ok({expected})"))
            }
            Classification::CrtNative { .. }
            | Classification::CrtWrapper { .. }
            | Classification::RuntimeHelper { .. }
            | Classification::Win32Stub { .. } => (
                0,
                "Stub",
                "Err(ExitReason::IllegalInstruction(0xFE))".to_owned(),
            ),
            _ => continue,
        };
        out.push_str(&format!(
            r#"
#[test]
fn generated_test_{name}_{index}() {{
    check({module:?}, {symbol:?}, {input}, ServiceStatus::{status}, {expected});
}}
"#,
            name = symbol_to_ident_lower(&entry.symbol),
            module = entry.module,
            symbol = entry.symbol,
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn file_header(filename: &str) -> String {
    format!(
        "// SPDX-License-Identifier: GPL-3.0-or-later\n\
         // AUTO-GENERATED by xenora-import-gen — DO NOT EDIT MANUALLY.\n\
         // Source: public Win32/CRT documentation and ISO C standard.\n\
         // Regenerate: `xenora imports generate --pe <executable>`\n\
         // File: {filename}\n\n"
    )
}

/// Convert a C symbol name to a valid Rust identifier (snake_case, no special chars).
fn symbol_to_ident(sym: &str) -> String {
    sym.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn symbol_to_ident_lower(sym: &str) -> String {
    symbol_to_ident(sym).to_ascii_lowercase()
}

/// Returns a dispatch body for CRT calls with a compatible guest ABI adapter.
pub(crate) fn crt_dispatch_expr(symbol: &str) -> Option<String> {
    Some(match symbol {
        // abs(int) -> int
        // Windows long is 32 bits. The minimum wraps, as x86 neg does.
        "abs" | "labs" => r#"let x = args.rcx as i32;
            Ok(u64::from(x.wrapping_abs() as u32))"#
            .to_string(),
        "llabs" => r#"let x = args.rcx as i64;
            Ok(x.unsigned_abs())"#
            .to_string(),
        // ISO C classification functions: char in RCX
        "isalnum" | "isalpha" | "isdigit" | "islower" | "isupper" | "isspace" | "isprint"
        | "iscntrl" | "ispunct" | "isxdigit" => {
            let test = match symbol {
                "isalnum" => "(c.is_ascii_alphanumeric())",
                "isalpha" => "(c.is_ascii_alphabetic())",
                "isdigit" => "(c.is_ascii_digit())",
                "islower" => "(c.is_ascii_lowercase())",
                "isupper" => "(c.is_ascii_uppercase())",
                "isspace" => "(c.is_ascii_whitespace())",
                "isprint" => "(!c.is_ascii_control())",
                "iscntrl" => "(c.is_ascii_control())",
                "ispunct" => "(c.is_ascii_punctuation())",
                "isxdigit" => "(c.is_ascii_hexdigit())",
                _ => "(false)",
            };
            format!(
                r#"let c = (args.rcx & 0xFF) as u8 as char;
            Ok(if {test} {{ 1 }} else {{ 0 }})"#,
                test = test,
            )
        }
        "tolower" => r#"let c = (args.rcx & 0xFF) as u8;
            Ok(c.to_ascii_lowercase() as u64)"#
            .to_string(),
        "toupper" => r#"let c = (args.rcx & 0xFF) as u8;
            Ok(c.to_ascii_uppercase() as u64)"#
            .to_string(),
        // Floating-point CRT calls also land here until XMM state is available.
        _ => return None,
    })
}

/// Independent canonical inputs and expected integer return bits.
fn canonical_integer_case(symbol: &str) -> Option<(u64, u64)> {
    Some(match symbol {
        "abs" | "labs" | "llabs" => ((-42i64) as u64, 42),
        "tolower" => (65, 97),
        "toupper" => (122, 90),
        "isalnum" | "isalpha" | "isupper" | "isxdigit" => (65, 1),
        "isdigit" => (53, 1),
        "islower" => (122, 1),
        "isspace" => (32, 1),
        "isprint" => (65, 1),
        "iscntrl" => (127, 1),
        "ispunct" => (33, 1),
        _ => return None,
    })
}
