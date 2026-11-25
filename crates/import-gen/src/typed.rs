// SPDX-License-Identifier: GPL-3.0-or-later

//! Typed runtime services: parse and validate the declarative registry, and
//! generate Win64 adapters, static metadata and a coverage report.

use crate::apiset::{get_api_sets_for_canonical, get_compat_modules_for_canonical};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// Most arguments one service may declare.
const MAX_PARAMETERS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parameter {
    pub name: String,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub library: String,
    pub symbol: String,
    pub ordinal: Option<(u16, String)>,
    pub parameters: Vec<Parameter>,
    pub result: String,
    pub handler: String,
    pub status: String,
    pub aliases: Vec<String>,
    pub failure: String,
    pub reference: String,
    pub tests: Vec<String>,
}

fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && !text.as_bytes()[0].is_ascii_digit()
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Adapter expression reading parameter `index`, or `None` for an unknown type.
fn reader(kind: &str, index: usize) -> Option<String> {
    Some(match kind {
        "u32" | "i32" | "u64" | "bool32" | "f32" | "f64" => format!("call.{kind}({index})?"),
        "handle" | "ptr" => format!("call.u64({index})?"),
        _ => {
            let size: u32 = kind.strip_prefix("out<")?.strip_suffix('>')?.parse().ok()?;
            if !(1..=4096).contains(&size) {
                return None;
            }
            format!("call.out::<{size}>({index})?")
        }
    })
}

/// Rust type of a result other than void.
fn result_type(kind: &str) -> &'static str {
    match kind {
        "u32" => "u32",
        "i32" => "i32",
        "bool32" => "bool",
        "f32" => "crate::runtime::Float32",
        "f64" => "crate::runtime::Float64",
        _ => "u64",
    }
}

const RESULTS: [&str; 9] = [
    "void", "u32", "i32", "u64", "bool32", "handle", "ptr", "f32", "f64",
];

/// Parse and validate the registry. `test_source` is the text of the test file
/// that must define every named test.
pub fn parse(text: &str, test_source: &str) -> Result<Vec<Service>, String> {
    let mut services: Vec<Service> = Vec::new();
    let mut seen_fields = BTreeSet::new();
    for (number, raw) in text.lines().enumerate() {
        let at = |message: &str| format!("line {}: {message}", number + 1);
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (keyword, rest) = line.split_once(' ').ok_or_else(|| at("missing value"))?;
        let rest = rest.trim();
        if keyword == "service" {
            services.push(parse_signature(rest).map_err(|message| at(&message))?);
            seen_fields.clear();
            continue;
        }
        let service = services
            .last_mut()
            .ok_or_else(|| at("field outside a service"))?;
        let repeatable = matches!(keyword, "alias" | "test");
        if !repeatable && !seen_fields.insert(keyword.to_owned()) {
            return Err(at(&format!("repeated field {keyword}")));
        }
        match keyword {
            "handler" => service.handler = rest.to_owned(),
            "status" => service.status = rest.to_owned(),
            "alias" => service.aliases.push(rest.to_owned()),
            "ordinal" => {
                let (value, source) = rest
                    .split_once(' ')
                    .ok_or_else(|| at("ordinal needs a number and a source"))?;
                let value = value
                    .parse::<u16>()
                    .ok()
                    .filter(|value| *value != 0)
                    .ok_or_else(|| at("ordinal must be in 1..=65535"))?;
                service.ordinal = Some((value, source.trim().to_owned()));
            }
            "fails" => service.failure = rest.to_owned(),
            "ref" => service.reference = rest.to_owned(),
            "test" => service.tests.push(rest.to_owned()),
            other => return Err(at(&format!("unknown field {other}"))),
        }
    }
    validate(&services, test_source)?;
    Ok(services)
}

fn parse_signature(text: &str) -> Result<Service, String> {
    let (library, rest) = text.split_once(' ').ok_or("missing symbol")?;
    let (symbol, rest) = rest.split_once('(').ok_or("missing parameter list")?;
    let (list, result) = rest.rsplit_once(") -> ").ok_or("missing result type")?;
    let mut parameters = Vec::new();
    if !list.trim().is_empty() {
        for parameter in list.split(',') {
            let (name, kind) = parameter.split_once(':').ok_or("parameter needs a type")?;
            parameters.push(Parameter {
                name: name.trim().to_owned(),
                kind: kind.trim().to_owned(),
            });
        }
    }
    Ok(Service {
        library: library.to_owned(),
        symbol: symbol.trim().to_owned(),
        ordinal: None,
        parameters,
        result: result.trim().to_owned(),
        handler: String::new(),
        status: String::new(),
        aliases: Vec::new(),
        failure: String::new(),
        reference: String::new(),
        tests: Vec::new(),
    })
}

fn validate(services: &[Service], test_source: &str) -> Result<(), String> {
    // Every key a guest could import, with the service it resolves to.
    let mut keys: BTreeMap<(String, &str), usize> = BTreeMap::new();
    let mut ordinals = BTreeSet::new();
    for (index, service) in services.iter().enumerate() {
        let name = format!("{}!{}", service.library, service.symbol);
        let fail = |message: String| Err(format!("{name}: {message}"));
        if service.library != service.library.to_ascii_lowercase()
            || !service.library.ends_with(".dll")
            || service.library.starts_with("api-ms-")
        {
            return fail("library must be a lower-case canonical module".into());
        }
        if !is_identifier(&service.symbol) {
            return fail("symbol is not an identifier".into());
        }
        if let Some((ordinal, source)) = &service.ordinal {
            if source.is_empty() {
                return fail("ordinal needs a source".into());
            }
            if !ordinals.insert((&service.library, ordinal)) {
                return fail(format!("ordinal {ordinal} already resolves in this module"));
            }
        }
        if !is_identifier(&service.handler) {
            return fail("missing or invalid handler".into());
        }
        if service.parameters.len() > MAX_PARAMETERS {
            return fail("too many parameters".into());
        }
        let mut names = BTreeSet::new();
        for (position, parameter) in service.parameters.iter().enumerate() {
            if !is_identifier(&parameter.name) || !names.insert(&parameter.name) {
                return fail(format!("invalid parameter name {:?}", parameter.name));
            }
            if reader(&parameter.kind, position).is_none() {
                return fail(format!("unsupported parameter type {:?}", parameter.kind));
            }
        }
        if !RESULTS.contains(&service.result.as_str()) {
            return fail(format!("unsupported result type {:?}", service.result));
        }
        if !matches!(service.status.as_str(), "implemented" | "partial" | "stub") {
            return fail("status must be implemented, partial or stub".into());
        }
        if service.failure.is_empty() || service.reference.is_empty() {
            return fail("needs a failure contract and a reference".into());
        }
        if service.tests.is_empty() && service.status != "stub" {
            return fail("needs a test before it may claim behaviour".into());
        }
        for test in &service.tests {
            if !is_identifier(test) || !test_source.contains(&format!("fn {test}()")) {
                return fail(format!("test {test} does not exist"));
            }
        }
        let documented = get_api_sets_for_canonical(&service.library);
        let compat = get_compat_modules_for_canonical(&service.library);
        let modules = std::iter::once(&service.library).chain(&service.aliases);
        for module in modules {
            if module != &service.library
                && !documented.contains(&module.as_str())
                && !compat.contains(&module.as_str())
            {
                return fail(format!(
                    "{module} is not a listed API set or compatibility module of its library"
                ));
            }
            if keys
                .insert((module.clone(), &service.symbol), index)
                .is_some()
            {
                return fail(format!("{module} already resolves this symbol"));
            }
        }
    }
    Ok(())
}

fn status_variant(status: &str) -> &'static str {
    match status {
        "implemented" => "Implemented",
        "partial" => "Partial",
        _ => "Stub",
    }
}

fn string_list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|item| format!("{item:?}")).collect();
    format!("&[{}]", quoted.join(", "))
}

/// Rust source of the adapters and metadata.
pub fn generate_rust(services: &[Service]) -> String {
    let mut out = String::from(
        "// SPDX-License-Identifier: GPL-3.0-or-later\n\n\
         // Generated by xenora-runtime-gen from crates/loader/runtime/services.txt.\n\
         // Do not edit.\n\n\
         use crate::imports::{ImportRegistry, ServiceStatus};\n\
         use crate::runtime::{Call, Parameter, Return, ServiceContext, ServiceMetadata};\n\
         use crate::services::{ServicesState, handlers};\n\
         use std::sync::Arc;\n\n",
    );
    let _ = writeln!(
        out,
        "pub static SERVICES: [ServiceMetadata; {}] = [",
        services.len()
    );
    for service in services {
        let parameters: Vec<String> = service
            .parameters
            .iter()
            .map(|p| format!("Parameter {{ name: {:?}, kind: {:?} }}", p.name, p.kind))
            .collect();
        let _ = writeln!(
            out,
            "    ServiceMetadata {{\n        library: {:?},\n        symbol: {:?},\n        \
             parameters: &[{}],\n        result: {:?},\n        status: ServiceStatus::{},\n        \
             aliases: {},\n        failure: {:?},\n        reference: {:?},\n        tests: {},\n    }},",
            service.library,
            service.symbol,
            parameters.join(", "),
            service.result,
            status_variant(&service.status),
            string_list(&service.aliases),
            service.failure,
            service.reference,
            string_list(&service.tests),
        );
    }
    out.push_str(
        "];\n\n/// Register every generated service and its API-set aliases.\n\
         pub fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {\n",
    );
    for service in services {
        let count = service.parameters.len();
        let names: String = (0..count).map(|index| format!(", a{index}")).collect();
        let _ = writeln!(
            out,
            "    let s = state.clone();\n    registry.register_with_status({:?}, {:?}, ServiceStatus::{}, move |args, cpu, memory| {{",
            service.library,
            service.symbol,
            status_variant(&service.status),
        );
        if count == 0 {
            out.push_str("        let _ = (args, &Call::new);\n");
        } else {
            out.push_str("        let call = Call::new(args, cpu, memory);\n");
        }
        for (index, parameter) in service.parameters.iter().enumerate() {
            let read = reader(&parameter.kind, index).unwrap_or_default();
            let _ = writeln!(out, "        let a{index} = {read};");
        }
        // The declared type is stated so that a handler returning another one
        // does not compile.
        let binding = match service.result.as_str() {
            "void" => "let ()".to_owned(),
            result => format!("let result: {}", result_type(result)),
        };
        let place = if service.result == "void" {
            "Ok(0)"
        } else {
            "Ok(result.place(cpu))"
        };
        let _ = writeln!(
            out,
            "        let mut context = ServiceContext {{ state: &s, memory, teb: cpu.gs_base }};\n        {binding} = handlers::{}(&mut context{names})?;\n        {place}\n    }});",
            service.handler
        );
        if let Some((ordinal, _)) = &service.ordinal {
            let _ = writeln!(
                out,
                "    registry.try_ordinal_alias({:?}, {:?}, {ordinal}).expect(\"validated export ordinal\");",
                service.library, service.symbol
            );
        }
    }
    out.push_str("    for service in &SERVICES {\n        for alias in service.aliases {\n            if let Err(error) = registry.try_alias(alias, service.library, service.symbol) {\n                panic!(\"{error}\");\n            }\n        }\n    }\n}\n");
    out.replace(
        "        let _ = (args, &Call::new);\n",
        "        let _ = args;\n",
    )
}

/// Coverage report. Each count is distinct; aliases are never services.
pub fn generate_report(services: &[Service]) -> String {
    let count = |status: &str| services.iter().filter(|s| s.status == status).count();
    let handlers: BTreeSet<&str> = services.iter().map(|s| s.handler.as_str()).collect();
    let aliases: usize = services.iter().map(|s| s.aliases.len()).sum();
    let tested = services.iter().filter(|s| !s.tests.is_empty()).count();
    let mut out = String::from(
        "# Typed runtime services\n\nGenerated by `xenora-runtime-gen` from `crates/loader/runtime/services.txt`. Do not edit.\n\n\
         A declared service is not a claim of compatibility. `implemented` and `partial` describe behaviour against the documented contract for the synthetic guest. No service has been independently accepted.\n\n\
         | Measure | Count |\n| --- | ---: |\n",
    );
    for (label, value) in [
        ("Services with a declared ABI", services.len()),
        ("Generated adapters", services.len()),
        ("Unique handler implementations", handlers.len()),
        ("Implemented", count("implemented")),
        ("Partial", count("partial")),
        ("Stub", count("stub")),
        ("Conformance-tested through guest dispatch", tested),
        ("API-set aliases (not services)", aliases),
        ("Independently accepted", 0),
    ] {
        let _ = writeln!(out, "| {label} | {value} |");
    }
    out.push_str("\n| Service | Signature | Status | Failure |\n| --- | --- | --- | --- |\n");
    for service in services {
        let parameters: Vec<String> = service
            .parameters
            .iter()
            .map(|p| format!("{}: {}", p.name, p.kind))
            .collect();
        let _ = writeln!(
            out,
            "| `{}!{}` | `({}) -> {}` | {} | {} |",
            service.library,
            service.symbol,
            parameters.join(", "),
            service.result,
            service.status,
            service.failure
        );
    }
    for service in services {
        if let Some((ordinal, source)) = &service.ordinal {
            let _ = writeln!(
                out,
                "\nOrdinal {ordinal} for `{}!{}` is documented by {source}.",
                service.library, service.symbol
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TESTS: &str = "fn works() {}";

    fn entry(signature: &str, extra: &str) -> String {
        format!(
            "service {signature}\nhandler h\nstatus partial\nfails never\nref x.h\ntest works\n{extra}\n"
        )
    }

    fn rejects(text: &str, message: &str) {
        let error = parse(text, TESTS).unwrap_err();
        assert!(error.contains(message), "{error}");
    }

    #[test]
    fn valid_entries_parse_and_generate_deterministically() {
        let text = entry(
            "kernel32.dll F(a: u32, b: f64, c: out<8>) -> handle",
            "alias api-ms-win-core-memory-l1-1-0.dll",
        );
        let services = parse(&text, TESTS).unwrap();
        assert_eq!(services[0].parameters[2].kind, "out<8>");
        let rust = generate_rust(&services);
        assert_eq!(rust, generate_rust(&services));
        for expected in ["call.u32(0)?", "call.f64(1)?", "call.out::<8>(2)?"] {
            assert!(rust.contains(expected), "{expected}");
        }
        assert!(generate_report(&services).contains("| Unique handler implementations | 1 |"));
    }

    #[test]
    fn ordinal_requires_a_source_and_unique_module_key() {
        let first = entry("kernel32.dll F() -> void", "ordinal 42 test catalog");
        let services = parse(&first, TESTS).unwrap();
        assert!(
            generate_rust(&services).contains("try_ordinal_alias(\"kernel32.dll\", \"F\", 42)")
        );
        assert!(generate_report(&services).contains("documented by test catalog"));
        let second = entry("kernel32.dll G() -> void", "ordinal 42 test catalog");
        rejects(&(first + &second), "ordinal 42 already resolves");
        rejects(
            &entry("kernel32.dll F() -> void", "ordinal 42"),
            "needs a number and a source",
        );
        rejects(
            &entry("kernel32.dll F() -> void", "ordinal 65536 test catalog"),
            "1..=65535",
        );
    }

    #[test]
    fn conflicting_keys_are_rejected() {
        let one = entry("kernel32.dll F() -> void", "");
        rejects(&(one.clone() + &one), "already resolves");
        let aliased = entry(
            "kernel32.dll F() -> void",
            "alias api-ms-win-core-memory-l1-1-0.dll",
        );
        let twice = entry(
            "kernel32.dll G() -> void",
            "alias api-ms-win-core-memory-l1-1-0.dll\nalias api-ms-win-core-memory-l1-1-0.dll",
        );
        assert!(parse(&aliased, TESTS).is_ok());
        rejects(&twice, "already resolves");
    }

    #[test]
    fn unsupported_declarations_are_rejected() {
        rejects(
            &entry("kernel32.dll F(a: struct) -> void", ""),
            "parameter type",
        );
        rejects(
            &entry("kernel32.dll F(a: out<0>) -> void", ""),
            "parameter type",
        );
        rejects(
            &entry("kernel32.dll F(a: void) -> void", ""),
            "parameter type",
        );
        rejects(&entry("kernel32.dll F() -> out<4>", ""), "result type");
        rejects(
            &entry("kernel32.dll F(a: u32, a: u32) -> void", ""),
            "parameter name",
        );
        rejects(&entry("Kernel32.dll F() -> void", ""), "canonical module");
        rejects(
            &entry("api-ms-win-core-memory-l1-1-0.dll F() -> void", ""),
            "canonical module",
        );
        rejects(
            &entry(
                "kernel32.dll F() -> void",
                "alias api-ms-win-core-made-up-l1-1-0.dll",
            ),
            "not a listed API set",
        );
        rejects(
            &entry("kernel32.dll F() -> void", "colour blue"),
            "unknown field",
        );
        rejects(
            &entry("kernel32.dll F() -> void", "status stub"),
            "repeated field",
        );
    }

    #[test]
    fn behaviour_needs_evidence() {
        let untested =
            "service kernel32.dll F() -> void\nhandler h\nstatus partial\nfails never\nref x.h\n";
        rejects(untested, "needs a test");
        rejects(&(untested.to_owned() + "test missing\n"), "does not exist");
        let stub = untested.replace("partial", "stub");
        assert!(parse(&stub, TESTS).is_ok());
        rejects(
            &untested.replace("ref x.h\n", "test works\n"),
            "needs a failure contract",
        );
        rejects("handler h\n", "outside a service");
    }
}
