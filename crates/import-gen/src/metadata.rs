// SPDX-License-Identifier: GPL-3.0-or-later

//! Versioned, offline API metadata definitions.
//!
//! Stores typed function signatures, parameter descriptions, calling conventions,
//! and header sources derived from public Win32 and MinGW-w64 metadata.
//!
//! Strictly clean-room: derived from public documentation, MIT/BSD-licensed headers,
//! or ISO C/POSIX standards.

use std::collections::BTreeMap;

/// Calling convention used by the foreign function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallingConvention {
    /// Standard Microsoft x64 (RCX, RDX, R8, R9, Stack)
    Win64,
    /// Standard CDECL / System V AMD64
    Cdecl,
}

/// Primitive argument and return types supported by the ABI translator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbiType {
    Void,
    Bool,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    /// Opaque Win64 object handle.
    Handle,
    /// Raw guest pointer (virtual address).
    Pointer {
        /// Whether the immediate pointee is const-qualified.
        is_const: bool,
        pointee: String,
    },
    /// Null-terminated UTF-8 / ANSI string in guest memory.
    StringAnsi,
    /// Null-terminated UTF-16 wide string in guest memory.
    StringWide,
    /// Complex composite struct passed by pointer.
    Struct(String),
}

/// Complete parameter specification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamDesc {
    pub name: String,
    pub abi_type: AbiType,
}

/// Complete function signature in the metadata database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionSignature {
    pub name: String,
    pub return_type: AbiType,
    pub params: Vec<ParamDesc>,
    pub convention: CallingConvention,
    pub header_source: String,
    pub doc: String,
}

/// Versioned metadata catalog for a subsystem or DLL.
pub struct MetadataCatalog {
    pub version: u32,
    pub entries: BTreeMap<(String, String), FunctionSignature>, // (canonical_module, symbol) -> sig
}

impl Default for MetadataCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataCatalog {
    pub fn new() -> Self {
        Self {
            version: 1,
            entries: BTreeMap::new(),
        }
    }

    /// Load the built-in clean-room metadata catalog and ingest system headers.
    pub fn builtin() -> Self {
        let mut cat = Self::new();
        cat.populate_crt();
        cat.populate_win32_core();
        // Genuine automated ingestion of available public system C/POSIX headers
        crate::ingest::ingest_standard_system_headers(&mut cat);
        cat
    }

    pub fn lookup(&self, canonical_module: &str, symbol: &str) -> Option<&FunctionSignature> {
        let key = (canonical_module.to_ascii_lowercase(), symbol.to_string());
        self.entries.get(&key)
    }

    fn add(
        &mut self,
        module: &str,
        name: &str,
        ret: AbiType,
        params: Vec<(&str, AbiType)>,
        header: &str,
        doc: &str,
    ) {
        let sig = FunctionSignature {
            name: name.to_string(),
            return_type: ret,
            params: params
                .into_iter()
                .map(|(pname, ptype)| ParamDesc {
                    name: pname.to_string(),
                    abi_type: ptype,
                })
                .collect(),
            convention: CallingConvention::Win64,
            header_source: header.to_string(),
            doc: doc.to_string(),
        };
        self.entries
            .insert((module.to_ascii_lowercase(), name.to_string()), sig);
    }

    fn populate_crt(&mut self) {
        // math.h
        for name in &[
            "cos", "sin", "tan", "acos", "asin", "atan", "exp", "log", "sqrt", "fabs",
        ] {
            self.add(
                "ucrtbase.dll",
                name,
                AbiType::F64,
                vec![("x", AbiType::F64)],
                "<math.h>",
                "ISO C standard math single-argument double",
            );
        }
        for name in &[
            "cosf", "sinf", "tanf", "acosf", "asinf", "atanf", "expf", "logf", "sqrtf", "fabsf",
        ] {
            self.add(
                "ucrtbase.dll",
                name,
                AbiType::F32,
                vec![("x", AbiType::F32)],
                "<math.h>",
                "ISO C standard math single-argument float",
            );
        }
        self.add(
            "ucrtbase.dll",
            "atan2",
            AbiType::F64,
            vec![("y", AbiType::F64), ("x", AbiType::F64)],
            "<math.h>",
            "ISO C arc tangent of two variables",
        );
        self.add(
            "ucrtbase.dll",
            "atan2f",
            AbiType::F32,
            vec![("y", AbiType::F32), ("x", AbiType::F32)],
            "<math.h>",
            "ISO C arc tangent of two variables (float)",
        );
        self.add(
            "ucrtbase.dll",
            "pow",
            AbiType::F64,
            vec![("base", AbiType::F64), ("exp", AbiType::F64)],
            "<math.h>",
            "ISO C power function",
        );
        self.add(
            "ucrtbase.dll",
            "powf",
            AbiType::F32,
            vec![("base", AbiType::F32), ("exp", AbiType::F32)],
            "<math.h>",
            "ISO C power function (float)",
        );

        // string.h
        self.add(
            "ucrtbase.dll",
            "strlen",
            AbiType::U64,
            vec![("str", AbiType::StringAnsi)],
            "<string.h>",
            "Calculates the length of a string in guest memory",
        );
        self.add(
            "ucrtbase.dll",
            "strnlen",
            AbiType::U64,
            vec![("str", AbiType::StringAnsi), ("count", AbiType::U64)],
            "<string.h>",
            "Calculates a byte string length up to the supplied bound",
        );
        self.add(
            "ucrtbase.dll",
            "strcmp",
            AbiType::I32,
            vec![("str1", AbiType::StringAnsi), ("str2", AbiType::StringAnsi)],
            "<string.h>",
            "Compares two strings lexicographically",
        );
        self.add(
            "ucrtbase.dll",
            "strncmp",
            AbiType::I32,
            vec![
                ("str1", AbiType::StringAnsi),
                ("str2", AbiType::StringAnsi),
                ("count", AbiType::U64),
            ],
            "<string.h>",
            "Compares up to count characters of two strings",
        );
        self.add(
            "ucrtbase.dll",
            "wcslen",
            AbiType::U64,
            vec![("str", AbiType::StringWide)],
            "<wchar.h>",
            "Counts 16-bit Windows code units before the terminator",
        );
        self.add(
            "ucrtbase.dll",
            "wcsnlen",
            AbiType::U64,
            vec![("str", AbiType::StringWide), ("count", AbiType::U64)],
            "<wchar.h>",
            "Counts 16-bit Windows code units up to the supplied bound",
        );
        self.add(
            "ucrtbase.dll",
            "wcscmp",
            AbiType::I32,
            vec![("str1", AbiType::StringWide), ("str2", AbiType::StringWide)],
            "<wchar.h>",
            "Compares two strings of 16-bit Windows code units",
        );
        self.add(
            "ucrtbase.dll",
            "wcsncmp",
            AbiType::I32,
            vec![
                ("str1", AbiType::StringWide),
                ("str2", AbiType::StringWide),
                ("count", AbiType::U64),
            ],
            "<wchar.h>",
            "Compares up to count 16-bit Windows code units of two strings",
        );
        self.add(
            "ucrtbase.dll",
            "memset",
            AbiType::Pointer {
                is_const: false,
                pointee: "void".into(),
            },
            vec![
                (
                    "dest",
                    AbiType::Pointer {
                        is_const: false,
                        pointee: "void".into(),
                    },
                ),
                ("c", AbiType::I32),
                ("count", AbiType::U64),
            ],
            "<string.h>",
            "Fills memory with a constant byte",
        );
        self.add(
            "ucrtbase.dll",
            "memcpy",
            AbiType::Pointer {
                is_const: false,
                pointee: "void".into(),
            },
            vec![
                (
                    "dest",
                    AbiType::Pointer {
                        is_const: false,
                        pointee: "void".into(),
                    },
                ),
                (
                    "src",
                    AbiType::Pointer {
                        is_const: true,
                        pointee: "void".into(),
                    },
                ),
                ("count", AbiType::U64),
            ],
            "<string.h>",
            "Copies memory between non-overlapping guest regions",
        );
        self.add(
            "ucrtbase.dll",
            "memmove",
            AbiType::Pointer {
                is_const: false,
                pointee: "void".into(),
            },
            vec![
                (
                    "dest",
                    AbiType::Pointer {
                        is_const: false,
                        pointee: "void".into(),
                    },
                ),
                (
                    "src",
                    AbiType::Pointer {
                        is_const: true,
                        pointee: "void".into(),
                    },
                ),
                ("count", AbiType::U64),
            ],
            "<string.h>",
            "Copies memory between guest ranges that may overlap",
        );
        self.add(
            "ucrtbase.dll",
            "memcmp",
            AbiType::I32,
            vec![
                (
                    "buf1",
                    AbiType::Pointer {
                        is_const: true,
                        pointee: "void".into(),
                    },
                ),
                (
                    "buf2",
                    AbiType::Pointer {
                        is_const: true,
                        pointee: "void".into(),
                    },
                ),
                ("count", AbiType::U64),
            ],
            "<string.h>",
            "Compares bytes in two memory buffers",
        );
    }

    fn populate_win32_core(&mut self) {
        // kernel32 processthreads / synch
        self.add(
            "kernel32.dll",
            "GetCurrentProcessId",
            AbiType::U32,
            vec![],
            "<processthreadsapi.h>",
            "Retrieves the process identifier of the calling process",
        );
        self.add(
            "kernel32.dll",
            "GetCurrentThreadId",
            AbiType::U32,
            vec![],
            "<processthreadsapi.h>",
            "Retrieves the thread identifier of the calling thread",
        );
        self.add(
            "kernel32.dll",
            "GetLastError",
            AbiType::U32,
            vec![],
            "<errhandlingapi.h>",
            "Retrieves the calling thread's last-error code value",
        );
        self.add(
            "kernel32.dll",
            "SetLastError",
            AbiType::Void,
            vec![("dwErrCode", AbiType::U32)],
            "<errhandlingapi.h>",
            "Sets the last-error code for the calling thread",
        );
        self.add(
            "kernel32.dll",
            "GetThreadId",
            AbiType::U32,
            vec![("Thread", AbiType::Handle)],
            "<processthreadsapi.h>",
            "Retrieves the identifier of the specified thread",
        );
        self.add(
            "kernel32.dll",
            "DuplicateHandle",
            AbiType::Bool,
            vec![
                ("hSourceProcessHandle", AbiType::Handle),
                ("hSourceHandle", AbiType::Handle),
                ("hTargetProcessHandle", AbiType::Handle),
                (
                    "lpTargetHandle",
                    AbiType::Pointer {
                        is_const: false,
                        pointee: "HANDLE".into(),
                    },
                ),
                ("dwDesiredAccess", AbiType::U32),
                ("bInheritHandle", AbiType::Bool),
                ("dwOptions", AbiType::U32),
            ],
            "<handleapi.h>",
            "Creates another handle to the same object",
        );
        for (name, return_type) in [
            ("GetTickCount", AbiType::U32),
            ("GetTickCount64", AbiType::U64),
        ] {
            self.add(
                "kernel32.dll",
                name,
                return_type,
                vec![],
                "<sysinfoapi.h>",
                "Returns elapsed system uptime in milliseconds",
            );
        }
        self.add(
            "kernel32.dll",
            "QueryPerformanceCounter",
            AbiType::Bool,
            vec![(
                "lpPerformanceCount",
                AbiType::Pointer {
                    is_const: false,
                    pointee: "LARGE_INTEGER".into(),
                },
            )],
            "<profileapi.h>",
            "Retrieves the current value of the performance counter",
        );
        self.add(
            "kernel32.dll",
            "QueryPerformanceFrequency",
            AbiType::Bool,
            vec![(
                "lpFrequency",
                AbiType::Pointer {
                    is_const: false,
                    pointee: "LARGE_INTEGER".into(),
                },
            )],
            "<profileapi.h>",
            "Retrieves the frequency of the performance counter",
        );
    }
}
