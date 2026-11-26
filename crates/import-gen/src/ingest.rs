// SPDX-License-Identifier: GPL-3.0-or-later

//! Automated C / POSIX header parser for metadata ingestion.
//!
//! Ingests standard system headers (e.g. `/usr/include/math.h`, `_string.h`)
//! and extracts function signatures into typed `FunctionSignature` definitions.
//!
//! Preserves clean-room provenance and licensing attribution.

use crate::metadata::{AbiType, CallingConvention, FunctionSignature, MetadataCatalog, ParamDesc};
use std::path::Path;

/// Ingest C header files from disk into `catalog`.
pub fn ingest_header_file(
    catalog: &mut MetadataCatalog,
    module: &str,
    header_path: &Path,
) -> Result<usize, String> {
    let content = std::fs::read_to_string(header_path)
        .map_err(|e| format!("cannot read {}: {e}", header_path.display()))?;

    let filename = header_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("header.h");

    let count = parse_c_declarations(catalog, module, filename, &content);
    Ok(count)
}

/// Ingest standard macOS SDK math and string headers if available.
pub fn ingest_standard_system_headers(catalog: &mut MetadataCatalog) -> usize {
    let candidates = [
        (
            "ucrtbase.dll",
            "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include/math.h",
        ),
        (
            "ucrtbase.dll",
            "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include/_string.h",
        ),
    ];

    let mut total = 0;
    for (mod_name, path_str) in candidates {
        let p = Path::new(path_str);
        if p.exists()
            && let Ok(n) = ingest_header_file(catalog, mod_name, p)
        {
            total += n;
        }
    }
    total
}

/// Parse C function prototypes from `content`.
fn parse_c_declarations(
    catalog: &mut MetadataCatalog,
    module: &str,
    header_name: &str,
    content: &str,
) -> usize {
    let mut added = 0;

    for line in content.lines() {
        let line = line.trim();
        // Look for lines containing function prototypes, e.g. "extern double acos(double);" or "int strcmp(...);"
        if !line.contains('(')
            || !line.contains(')')
            || line.starts_with('#')
            || line.starts_with("/*")
            || line.starts_with('*')
        {
            continue;
        }

        // Clean up __attribute__, extern, etc.
        let cleaned = line
            .replace("extern ", "")
            .replace("__DARWIN_ALIAS(", "/* alias */ ")
            .replace("__DARWIN_ALIAS_C(", "/* alias */ ");
        let cleaned = strip_pointer_annotations(&cleaned);

        let Some(open_paren) = cleaned.find('(') else {
            continue;
        };
        let Some(close_paren) = cleaned[open_paren..].find(')') else {
            continue;
        };
        let close_paren = open_paren + close_paren;

        let decl_left = cleaned[..open_paren].trim();
        let args_str = cleaned[open_paren + 1..close_paren].trim();

        let Some((ret_str, name_str)) = split_named_declarator(decl_left) else {
            continue;
        };
        let Some(ret_type) = parse_c_type(ret_str) else {
            continue;
        };

        let mut params = Vec::new();
        if !args_str.is_empty() && args_str != "void" {
            for (idx, arg) in args_str.split(',').enumerate() {
                let arg = arg.trim();
                let (name, type_str) = match split_named_declarator(arg) {
                    Some((type_str, name)) => (name.to_string(), type_str),
                    None if arg.is_empty() => (format!("arg{idx}"), "void"),
                    None => (format!("arg{idx}"), arg),
                };

                let abi_type = parse_c_type(type_str).unwrap_or(AbiType::U64);
                params.push(ParamDesc { name, abi_type });
            }
        }

        let sig = FunctionSignature {
            name: name_str.to_string(),
            return_type: ret_type,
            params,
            convention: CallingConvention::Win64,
            header_source: format!("<{header_name}>"),
            doc: format!("Ingested from {header_name}"),
        };

        catalog
            .entries
            .insert((module.to_ascii_lowercase(), name_str.to_string()), sig);
        added += 1;
    }

    added
}

fn is_c_identifier_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn strip_pointer_annotations(line: &str) -> String {
    line.split_inclusive(|ch| !is_c_identifier_continue(ch))
        .map(|segment| {
            let word = segment.trim_end_matches(|ch| !is_c_identifier_continue(ch));
            match word {
                "_LIBC_CSTR" | "_LIBC_UNSAFE_INDEXABLE" => &segment[word.len()..],
                _ => segment,
            }
        })
        .collect()
}

/// Split the trailing identifier from a simple named declarator and keep any
/// pointer stars with the type, including when a star touches the name.
fn split_named_declarator(declaration: &str) -> Option<(&str, &str)> {
    let type_end = declaration.trim_end_matches(is_c_identifier_continue).len();
    let (type_str, name) = declaration.split_at(type_end);
    if !type_str.ends_with(|ch: char| ch.is_whitespace() || ch == '*')
        || !name.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_')
        || is_c_qualifier(name)
        || matches!(
            name,
            "void"
                | "char"
                | "short"
                | "int"
                | "long"
                | "signed"
                | "unsigned"
                | "float"
                | "double"
                | "_Bool"
        )
    {
        return None;
    }
    Some((type_str.trim_end(), name))
}

fn is_c_qualifier(word: &str) -> bool {
    matches!(
        word,
        "const"
            | "volatile"
            | "restrict"
            | "__restrict"
            | "__restrict__"
            | "_Nullable"
            | "_Nonnull"
            | "_Null_unspecified"
            | "__nullable"
            | "__nonnull"
            | "__null_unspecified"
    )
}

fn parse_c_type(s: &str) -> Option<AbiType> {
    if let Some((pointee, _)) = s.rsplit_once('*') {
        // The catalog models a single pointer to plain char as an ANSI string.
        // Additional indirection has to keep the generic pointer category.
        if pointee
            .split_whitespace()
            .filter(|word| !is_c_qualifier(word))
            .eq(["char"])
        {
            return Some(AbiType::StringAnsi);
        }

        // In `int *const *`, the immediate pointee is the const-qualified
        // inner pointer. In `const int **`, that inner pointer is mutable.
        let immediate = pointee.rsplit('*').next().unwrap_or(pointee);
        return Some(AbiType::Pointer {
            is_const: immediate.split_whitespace().any(|word| word == "const"),
            pointee: "void".into(),
        });
    }

    let normalized = s
        .split_whitespace()
        .filter(|word| !is_c_qualifier(word))
        .collect::<Vec<_>>()
        .join(" ");
    match normalized.as_str() {
        "void" => Some(AbiType::Void),
        "int" | "int32_t" => Some(AbiType::I32),
        "unsigned int" | "uint32_t" | "unsigned" => Some(AbiType::U32),
        "long long" | "int64_t" => Some(AbiType::I64),
        "size_t" | "unsigned long" | "unsigned long long" | "uint64_t" => Some(AbiType::U64),
        "float" => Some(AbiType::F32),
        "double" => Some(AbiType::F64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_pointer_declarators_preserve_stars_across_spacing() {
        let mut catalog = MetadataCatalog::new();
        let declarations = "
            int *attached(int *out);
            int * separated(int * out);
            int*compact(int*out);
            int **indirect(int * *out);
        ";
        assert_eq!(
            parse_c_declarations(&mut catalog, "example.dll", "test.h", declarations),
            4
        );
        let pointer = AbiType::Pointer {
            is_const: false,
            pointee: "void".into(),
        };
        for symbol in ["attached", "separated", "compact", "indirect"] {
            let signature = catalog.lookup("example.dll", symbol).unwrap();
            assert_eq!(signature.return_type, pointer, "{symbol}");
            assert_eq!(signature.params.len(), 1, "{symbol}");
            assert_eq!(signature.params[0].name, "out", "{symbol}");
            assert_eq!(signature.params[0].abi_type, pointer, "{symbol}");
        }
    }

    #[test]
    fn unnamed_supported_parameters_keep_their_complete_types() {
        let mut catalog = MetadataCatalog::new();
        let declarations = "void inspect(unsigned  int, long long, unsigned long long, double, int *, unsigned int named);";
        assert_eq!(
            parse_c_declarations(&mut catalog, "example.dll", "test.h", declarations),
            1
        );
        let signature = catalog.lookup("example.dll", "inspect").unwrap();
        let expected = vec![
            ("arg0", AbiType::U32),
            ("arg1", AbiType::I64),
            ("arg2", AbiType::U64),
            ("arg3", AbiType::F64),
            (
                "arg4",
                AbiType::Pointer {
                    is_const: false,
                    pointee: "void".into(),
                },
            ),
            ("named", AbiType::U32),
        ];
        let actual: Vec<_> = signature
            .params
            .iter()
            .map(|param| (param.name.as_str(), param.abi_type.clone()))
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn postfix_qualified_scalars_keep_their_abi_types() {
        let mut catalog = MetadataCatalog::new();
        let declarations = "void inspect(double const, float const, int const, unsigned int const, long long const, double const named);";
        assert_eq!(
            parse_c_declarations(&mut catalog, "example.dll", "test.h", declarations),
            1
        );
        let signature = catalog.lookup("example.dll", "inspect").unwrap();
        let expected = vec![
            ("arg0", AbiType::F64),
            ("arg1", AbiType::F32),
            ("arg2", AbiType::I32),
            ("arg3", AbiType::U32),
            ("arg4", AbiType::I64),
            ("named", AbiType::F64),
        ];
        let actual: Vec<_> = signature
            .params
            .iter()
            .map(|param| (param.name.as_str(), param.abi_type.clone()))
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn sdk_nullability_annotations_do_not_become_parameter_names() {
        let mut catalog = MetadataCatalog::new();
        let declarations = "void inspect(char * _Nullable, int * _Nonnull);";
        assert_eq!(
            parse_c_declarations(&mut catalog, "example.dll", "test.h", declarations),
            1
        );
        let signature = catalog.lookup("example.dll", "inspect").unwrap();
        let pointer = AbiType::Pointer {
            is_const: false,
            pointee: "void".into(),
        };
        assert_eq!(
            signature
                .params
                .iter()
                .map(|param| (param.name.as_str(), param.abi_type.clone()))
                .collect::<Vec<_>>(),
            vec![("arg0", AbiType::StringAnsi), ("arg1", pointer)]
        );
    }

    #[test]
    fn pointer_return_types_are_ingested() {
        let mut catalog = MetadataCatalog::new();
        let declarations = "void *opaque(void);\nchar **table(void);\nstruct record *record(void);";
        assert_eq!(
            parse_c_declarations(&mut catalog, "example.dll", "test.h", declarations),
            3
        );
        let pointer = AbiType::Pointer {
            is_const: false,
            pointee: "void".into(),
        };
        for symbol in ["opaque", "table", "record"] {
            assert_eq!(
                catalog.lookup("example.dll", symbol).unwrap().return_type,
                pointer,
                "{symbol}"
            );
        }
    }

    #[test]
    fn public_ingestion_preserves_character_and_indirect_pointer_types() {
        let declarations = "
            typedef int constant_t;
            struct char_record;
            char*text(const char*input, char const * suffix);
            const char **strings(char **mutable_table, const char **table);
            void pointers(const int *value, int *const top, int *const *slot, const int **table, int * const, int * __restrict);
            void opaque(constant_t *value, struct char_record *record);
        ";
        let header = std::env::temp_dir().join(format!(
            "xenora-import-gen-pointer-types-{}.h",
            std::process::id()
        ));
        std::fs::write(&header, declarations).unwrap();
        let mut catalog = MetadataCatalog::new();
        let ingested = ingest_header_file(&mut catalog, "example.dll", &header);
        std::fs::remove_file(header).unwrap();
        assert_eq!(ingested.unwrap(), 4);

        let generic_pointer = |is_const| AbiType::Pointer {
            is_const,
            pointee: "void".into(),
        };
        let return_types = [
            ("text", AbiType::StringAnsi),
            ("strings", generic_pointer(false)),
            ("pointers", AbiType::Void),
            ("opaque", AbiType::Void),
        ];
        let param_types = [
            vec![AbiType::StringAnsi, AbiType::StringAnsi],
            vec![generic_pointer(false), generic_pointer(false)],
            vec![
                generic_pointer(true),
                generic_pointer(false),
                generic_pointer(true),
                generic_pointer(false),
                generic_pointer(false),
                generic_pointer(false),
            ],
            vec![generic_pointer(false), generic_pointer(false)],
        ];
        for ((symbol, ret), params) in return_types.into_iter().zip(param_types) {
            let signature = catalog.lookup("example.dll", symbol).unwrap();
            assert_eq!(signature.return_type, ret, "{symbol}");
            assert_eq!(
                signature
                    .params
                    .iter()
                    .map(|param| param.abi_type.clone())
                    .collect::<Vec<_>>(),
                params,
                "{symbol}"
            );
        }
        let pointers = catalog.lookup("example.dll", "pointers").unwrap();
        assert_eq!(pointers.params[4].name, "arg4");
        assert_eq!(pointers.params[5].name, "arg5");
    }

    #[test]
    fn header_pointer_annotations_do_not_change_the_type_or_symbol() {
        let mut catalog = MetadataCatalog::new();
        let declarations = "
            char *_LIBC_CSTR find_LIBC_CSTR(char *_LIBC_CSTR input);
        ";
        assert_eq!(
            parse_c_declarations(&mut catalog, "example.dll", "test.h", declarations),
            1
        );
        let signature = catalog.lookup("example.dll", "find_LIBC_CSTR").unwrap();
        assert_eq!(signature.return_type, AbiType::StringAnsi);
        assert_eq!(signature.params[0].name, "input");
        assert_eq!(signature.params[0].abi_type, AbiType::StringAnsi);
    }

    #[test]
    fn test_parse_declarations_from_string() {
        let mut cat = MetadataCatalog::new();
        let c_code = r#"
            extern double cos(double x);
            extern double atan2(double y, double x);
            int strcmp(const char *s1, const char *s2);
        "#;
        let count = parse_c_declarations(&mut cat, "ucrtbase.dll", "test.h", c_code);
        assert_eq!(count, 3);
        assert!(cat.lookup("ucrtbase.dll", "cos").is_some());
        assert!(cat.lookup("ucrtbase.dll", "atan2").is_some());
        assert!(cat.lookup("ucrtbase.dll", "strcmp").is_some());
    }
}
