// SPDX-License-Identifier: GPL-3.0-or-later

//! API-Set Redirection and Alias Resolution.
//!
//! Windows uses virtual DLL names (e.g. `api-ms-win-core-processthreads-l1-1-0.dll`)
//! that are redirected to canonical underlying modules (e.g. `kernel32.dll`, `KernelBase.dll`).
//!
//! This module resolves these virtual API-sets to canonical modules and vice-versa,
//! preventing duplicate stub generation and allowing unified handler lookup.
//! Source: Public Windows SDK `apisetcounters.h` and documented Microsoft API-set schema.

/// Resolve an API-set virtual DLL name to its canonical Windows module name.
///
/// API sets are matched exactly against [`get_api_sets_for_canonical`]. A name
/// that merely starts like a known family is not assumed to forward anywhere.
pub fn resolve_api_set_canonical(module: &str) -> &'static str {
    let lower = module.to_ascii_lowercase();
    for canonical in ["kernel32.dll", "ucrtbase.dll"] {
        if get_api_sets_for_canonical(canonical).contains(&lower.as_str()) {
            return canonical;
        }
    }
    match lower.as_str() {
        "kernel32.dll" | "kernelbase.dll" => "kernel32.dll",
        "ucrtbase.dll" => "ucrtbase.dll",
        "ntdll.dll" => "ntdll.dll",
        "ws2_32.dll" => "ws2_32.dll",
        "advapi32.dll" => "advapi32.dll",
        "vcruntime140.dll" | "vcruntime140_1.dll" => "vcruntime140.dll",
        "msvcp140.dll" | "msvcp140_1.dll" | "msvcp140_2.dll" => "msvcp140.dll",
        _ => "unknown.dll",
    }
}

/// Known API sets that forward to the given canonical host module.
pub fn get_api_sets_for_canonical(canonical: &str) -> &'static [&'static str] {
    match canonical {
        "kernel32.dll" => &[
            "api-ms-win-core-processthreads-l1-1-0.dll",
            "api-ms-win-core-processthreads-l1-1-1.dll",
            "api-ms-win-core-processthreads-l1-1-2.dll",
            "api-ms-win-core-synch-l1-1-0.dll",
            "api-ms-win-core-synch-l1-2-0.dll",
            "api-ms-win-core-synch-l1-2-1.dll",
            "api-ms-win-core-sysinfo-l1-1-0.dll",
            "api-ms-win-core-sysinfo-l1-2-0.dll",
            "api-ms-win-core-profile-l1-1-0.dll",
            "api-ms-win-core-libraryloader-l1-2-0.dll",
            "api-ms-win-core-libraryloader-l1-2-1.dll",
            "api-ms-win-core-file-l1-1-0.dll",
            "api-ms-win-core-file-l2-1-0.dll",
            "api-ms-win-core-errorhandling-l1-1-0.dll",
            "api-ms-win-core-debug-l1-1-0.dll",
            "api-ms-win-core-handle-l1-1-0.dll",
            "api-ms-win-core-interlocked-l1-1-0.dll",
            "api-ms-win-core-fibers-l1-1-0.dll",
            "api-ms-win-core-datetime-l1-1-1.dll",
            "api-ms-win-core-timezone-l1-1-0.dll",
            "api-ms-win-core-realtime-l1-1-0.dll",
            "api-ms-win-core-memory-l1-1-0.dll",
        ],
        "ucrtbase.dll" => &[
            "api-ms-win-crt-math-l1-1-0.dll",
            "api-ms-win-crt-string-l1-1-0.dll",
            "api-ms-win-crt-stdio-l1-1-0.dll",
            "api-ms-win-crt-runtime-l1-1-0.dll",
            "api-ms-win-crt-heap-l1-1-0.dll",
            "api-ms-win-crt-time-l1-1-0.dll",
            "api-ms-win-crt-convert-l1-1-0.dll",
            "api-ms-win-crt-utility-l1-1-0.dll",
            "api-ms-win-crt-filesystem-l1-1-0.dll",
            "api-ms-win-crt-locale-l1-1-0.dll",
            "api-ms-win-crt-environment-l1-1-0.dll",
            // The API set a program built with the mingw-w64 UCRT runtime
            // imports `memcpy` and `__C_specific_handler` from. That it
            // forwards to ucrtbase.dll is inferred from its name and from
            // Microsoft's "Universal CRT deployment" page, which says the
            // Universal CRT ships as ucrtbase.dll plus `api-ms-win-*.dll`
            // forwarders. No document naming this set was found.
            "api-ms-win-crt-private-l1-1-0.dll",
        ],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_listed_api_sets_resolve() {
        for unknown in [
            "api-ms-win-core-file-l9-9-9.dll",
            "api-ms-win-core-processthreads",
            "api-ms-win-core-filesomething.dll",
            "api-ms-win-crt-unknown-l1-1-0.dll",
        ] {
            assert_eq!(
                resolve_api_set_canonical(unknown),
                "unknown.dll",
                "{unknown}"
            );
        }
        assert_eq!(
            resolve_api_set_canonical("API-MS-Win-Core-Memory-L1-1-0.DLL"),
            "kernel32.dll"
        );
    }

    #[test]
    fn test_api_set_resolution() {
        assert_eq!(
            resolve_api_set_canonical("api-ms-win-core-processthreads-l1-1-0.dll"),
            "kernel32.dll"
        );
        assert_eq!(
            resolve_api_set_canonical("api-ms-win-crt-math-l1-1-0.dll"),
            "ucrtbase.dll"
        );
        assert_eq!(
            resolve_api_set_canonical("api-ms-win-core-synch-l1-2-1.dll"),
            "kernel32.dll"
        );
    }
}
