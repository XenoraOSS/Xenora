// SPDX-License-Identifier: GPL-3.0-or-later

//! Symbol classification database.
//!
//! All data is derived from public documentation:
//! - Microsoft Learn Win32 API reference (docs.microsoft.com/en-us/windows/win32/api)
//! - ISO C11 / POSIX 2017 standard library
//! - MinGW-w64 public header files (MIT licence, permissive attribution)
//! - api-ms-win-* API set documentation
//!
//! No leaked SDK data, no confidential Xbox documentation.

use std::collections::HashMap;

/// How a symbol can be provided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Classification {
    /// Maps directly to a libc symbol of the same name and signature.
    CrtNative {
        libc_sym: &'static str,
        /// Brief semantic note for the generated doc comment.
        note: &'static str,
    },
    /// Maps to libc with simple argument/return marshalling.
    CrtWrapper {
        libc_sym: &'static str,
        /// Rust expression describing the wrapper body (uses `args`, `mem`).
        wrapper_note: &'static str,
    },
    /// Win32 function; generate a stub that logs the call and returns a safe default.
    Win32Stub {
        return_zero: bool,
        /// Brief description for the stub comment.
        note: &'static str,
    },
    /// Service supplied by the loader standard registry.
    StandardService,
    /// Compiler intrinsic or SEH helper; stub that traps gracefully.
    RuntimeHelper { note: &'static str },
    /// Proprietary, platform-only, or requires significant manual engineering.
    Manual { reason: &'static str },
    /// Completely unsupported platform API; no code generated.
    Unsupported { reason: &'static str },
}

impl Classification {
    pub fn tier_name(&self) -> &'static str {
        match self {
            Self::CrtNative { .. } => "crt-native",
            Self::CrtWrapper { .. } => "crt-wrapper",
            Self::Win32Stub { .. } => "win32-stub",
            Self::StandardService => "standard-service",
            Self::RuntimeHelper { .. } => "runtime-helper",
            Self::Manual { .. } => "manual",
            Self::Unsupported { .. } => "unsupported",
        }
    }

    pub fn is_generatable(&self) -> bool {
        !matches!(
            self,
            Self::StandardService | Self::Manual { .. } | Self::Unsupported { .. }
        )
    }
}

/// Built-in symbol knowledge database.
pub struct SymbolDb {
    /// Keyed by lowercase `(module, symbol)`.
    entries: HashMap<(String, String), Classification>,
}

impl SymbolDb {
    /// Look up a symbol.  `module` and `symbol` are compared case-insensitively.
    pub fn get(&self, module: &str, symbol: &str) -> Option<&Classification> {
        let key = (module.to_ascii_lowercase(), symbol.to_string());
        self.entries.get(&key)
    }

    /// Every classified `(module, symbol)` pair, sorted, with modules in lowercase.
    pub fn entries(&self) -> Vec<(&str, &str, &Classification)> {
        let mut entries: Vec<_> = self
            .entries
            .iter()
            .map(|((module, symbol), class)| (module.as_str(), symbol.as_str(), class))
            .collect();
        entries.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        entries
    }

    /// Add or replace one classification, for a project-specific module the
    /// built-in database does not describe. `module` is matched
    /// case-insensitively, like [`SymbolDb::get`].
    pub fn insert(&mut self, module: &str, symbol: &str, classification: Classification) {
        self.ins(module, symbol, classification);
    }

    /// Build the built-in database.  Every entry here is derived exclusively from
    /// public Microsoft documentation and ISO/POSIX standards.
    pub fn builtin() -> Self {
        let mut db = Self {
            entries: HashMap::new(),
        };
        db.populate_crt_math();
        db.populate_crt_string();
        db.populate_crt_stdio();
        db.populate_crt_runtime();
        db.populate_crt_heap();
        db.populate_crt_time();
        db.populate_crt_convert();
        db.populate_crt_utility();
        db.populate_crt_filesystem();
        db.populate_crt_locale();
        db.populate_vcruntime();
        db.populate_win32_kernel();
        db.populate_win32_synch();
        db.populate_win32_loader();
        db.populate_win32_profile();
        db.populate_win32_sysinfo();
        db.populate_win32_file();
        db.populate_registered_services();
        db.populate_unsupported();
        db
    }

    fn ins(&mut self, module: &str, symbol: &str, c: Classification) {
        self.entries
            .insert((module.to_ascii_lowercase(), symbol.to_string()), c);
    }

    fn crt_native(&mut self, module: &str, sym: &str, libc_sym: &'static str, note: &'static str) {
        self.ins(module, sym, Classification::CrtNative { libc_sym, note });
    }

    fn win32_stub(&mut self, module: &str, sym: &str, return_zero: bool, note: &'static str) {
        self.ins(module, sym, Classification::Win32Stub { return_zero, note });
    }

    fn runtime_helper(&mut self, module: &str, sym: &str, note: &'static str) {
        self.ins(module, sym, Classification::RuntimeHelper { note });
    }

    fn unsupported(&mut self, module: &str, sym: &str, reason: &'static str) {
        self.ins(module, sym, Classification::Unsupported { reason });
    }

    // ---- api-ms-win-crt-math -----------------------------------------------

    fn populate_crt_math(&mut self) {
        let m = "api-ms-win-crt-math-l1-1-0.dll";
        for module in [m, "ucrtbase.dll"] {
            for symbol in ["sqrt", "sqrtf", "fabs", "fabsf"] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        for (sym, libc) in &[
            ("acos", "acos"),
            ("acosf", "acosf"),
            ("asin", "asin"),
            ("asinf", "asinf"),
            ("atan", "atan"),
            ("atanf", "atanf"),
            ("atan2", "atan2"),
            ("atan2f", "atan2f"),
            ("cbrtf", "cbrtf"),
            ("ceilf", "ceilf"),
            ("cos", "cos"),
            ("cosf", "cosf"),
            ("erff", "erff"),
            ("exp", "exp"),
            ("exp2", "exp2"),
            ("exp2f", "exp2f"),
            ("expf", "expf"),
            ("floor", "floor"),
            ("floorf", "floorf"),
            ("fma", "fma"),
            ("fmaf", "fmaf"),
            ("fmax", "fmax"),
            ("fmaxf", "fmaxf"),
            ("fmin", "fmin"),
            ("fminf", "fminf"),
            ("fmod", "fmod"),
            ("fmodf", "fmodf"),
            ("hypotf", "hypotf"),
            ("log", "log"),
            ("log10", "log10"),
            ("log10f", "log10f"),
            ("log2", "log2"),
            ("log2f", "log2f"),
            ("logf", "logf"),
            ("pow", "pow"),
            ("powf", "powf"),
            ("round", "round"),
            ("roundf", "roundf"),
            ("sin", "sin"),
            ("sinf", "sinf"),
            ("tan", "tan"),
            ("tanf", "tanf"),
            ("trunc", "trunc"),
            ("truncf", "truncf"),
        ] {
            self.crt_native(m, sym, libc, "ISO C99 math — identical signature on macOS");
        }
        // Windows-specific math helpers — stub
        self.win32_stub(
            m,
            "__setusermatherr",
            false,
            "Windows math error handler; no-op on macOS",
        );
        self.win32_stub(m, "_dtest", false, "Windows double classification helper");
        self.win32_stub(m, "_fdsign", false, "Windows float sign helper");
        self.win32_stub(m, "_fdtest", false, "Windows float classification helper");
        self.win32_stub(m, "_dsign", false, "Windows double sign helper");
        self.win32_stub(m, "_dpcomp", false, "Windows double comparison helper");
        self.win32_stub(m, "_fdpcomp", false, "Windows float comparison helper");
    }

    // ---- api-ms-win-crt-string ---------------------------------------------

    fn populate_crt_string(&mut self) {
        let m = "api-ms-win-crt-string-l1-1-0.dll";
        for module in [m, "ucrtbase.dll"] {
            for symbol in [
                "strlen", "strnlen", "strcmp", "strncmp", "wcslen", "wcsnlen", "wcscmp", "wcsncmp",
                "memset", "memcpy", "memmove", "memcmp", "memchr", "strcpy", "strncpy", "strcat",
                "strncat", "strstr", "wcsstr", "strspn", "strcspn", "strtok", "strtok_s",
                "strpbrk", "wcsspn", "wcscspn", "wcspbrk", "wcstok", "wcstok_s", "wcscpy",
                "wcsncpy", "wcscat", "wcschr", "wcsrchr", "isalnum", "isalpha", "isdigit",
                "islower", "isspace", "isxdigit", "isupper", "isprint", "iscntrl", "ispunct",
                "tolower", "toupper",
            ] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        self.crt_native(
            m,
            "wcsicmp",
            "wcscasecmp",
            "Legacy wide comparison candidate",
        );
        // Bounds-checked copies are runtime services: mapping them to the
        // unchecked libc functions would drop the size check they exist for.
        for module in [m, "ucrtbase.dll"] {
            for symbol in [
                "strcat_s",
                "strcpy_s",
                "strncpy_s",
                "strncat_s",
                "wcscpy_s",
                "wcscat_s",
                "wcsncpy_s",
                "wcsncat_s",
            ] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        self.win32_stub(
            m,
            "_stricmp",
            false,
            "case-insensitive strcmp; maps to strcasecmp",
        );
        self.win32_stub(
            m,
            "_strnicmp",
            false,
            "case-insensitive strncmp; maps to strncasecmp",
        );
        self.win32_stub(
            m,
            "_wcsicoll",
            false,
            "case-insensitive wide string collate",
        );
        self.win32_stub(m, "_wcsnicmp", false, "case-insensitive wide strncmp");
        self.win32_stub(m, "_mbstowcs_s_l", false, "Windows locale mbstowcs_s");
    }

    // ---- api-ms-win-crt-stdio ----------------------------------------------

    fn populate_crt_stdio(&mut self) {
        let m = "api-ms-win-crt-stdio-l1-1-0.dll";
        for module in [m, "ucrtbase.dll"] {
            for symbol in [
                "sprintf",
                "snprintf",
                "fopen",
                "fclose",
                "fread",
                "fwrite",
                "fseek",
                "ftell",
                "feof",
                "ferror",
                "rewind",
                "__acrt_iob_func",
                "fprintf",
                "printf",
                "fputc",
                "putc",
                "puts",
                "fgetc",
                "getc",
                "ungetc",
            ] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        for (sym, libc) in &[
            ("fflush", "fflush"),
            ("fgetpos", "fgetpos"),
            ("fputs", "fputs"),
            ("fsetpos", "fsetpos"),
            ("remove", "remove"),
            ("rename", "rename"),
            ("setvbuf", "setvbuf"),
            ("vsprintf", "vsprintf"),
            ("vsnprintf", "vsnprintf"),
            ("vfprintf", "vfprintf"),
            ("sscanf", "sscanf"),
            ("fscanf", "fscanf"),
            ("scanf", "scanf"),
        ] {
            self.crt_native(m, sym, libc, "ISO C stdio — identical ABI on macOS");
        }
        // Windows stdio_common helpers — stub (they're UCRT internal; we don't need them
        // when the guest calls the public functions)
        for sym in &[
            "__stdio_common_vfprintf",
            "__stdio_common_vfwprintf",
            "__stdio_common_vsprintf",
            "__stdio_common_vsprintf_s",
            "__stdio_common_vsscanf",
            "__stdio_common_vswprintf",
            "__stdio_common_vswprintf_s",
            "__stdio_common_vswscanf",
            "__stdio_common_vsnwprintf_s",
        ] {
            self.win32_stub(
                m,
                sym,
                false,
                "UCRT internal stdio helper — stub returning 0",
            );
        }
        self.win32_stub(
            m,
            "_fseeki64",
            false,
            "64-bit fseek; maps to fseeko on macOS",
        );
        self.win32_stub(
            m,
            "_lock_file",
            false,
            "Windows file lock — no-op in single-threaded context",
        );
        self.win32_stub(m, "_unlock_file", false, "Windows file unlock — no-op");
        self.win32_stub(
            m,
            "_get_stream_buffer_pointers",
            false,
            "UCRT internal stream buffer query",
        );
        self.win32_stub(m, "_wfopen", false, "Wide-char fopen — stub returning null");
    }

    // ---- api-ms-win-crt-runtime --------------------------------------------

    fn populate_crt_runtime(&mut self) {
        let m = "api-ms-win-crt-runtime-l1-1-0.dll";
        for (sym, libc) in &[
            ("abort", "abort"),
            ("exit", "exit"),
            ("_exit", "_exit"),
            ("rand", "rand"),
            ("srand", "srand"),
            ("atoi", "atoi"),
            ("atof", "atof"),
            ("atoll", "atoll"),
        ] {
            self.crt_native(m, sym, libc, "ISO C runtime — identical ABI on macOS");
        }
        for module in [m, "ucrtbase.dll"] {
            for symbol in ["_errno", "_get_errno", "_set_errno", "getenv"] {
                self.ins(module, symbol, Classification::StandardService);
            }
            // Abnormal termination ends the guest process, never the host.
            for symbol in ["terminate", "_purecall"] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        self.win32_stub(m, "_set_app_type", false, "CRT app-type hint — no-op");
        self.win32_stub(m, "_set_fmode", false, "CRT file mode hint — no-op");
        self.win32_stub(m, "_set_new_mode", false, "CRT new-mode hint — no-op");
        self.win32_stub(
            m,
            "_set_purecall_handler",
            false,
            "pure virtual call handler — stub",
        );
        self.win32_stub(
            m,
            "_set_invalid_parameter_handler",
            false,
            "CRT invalid param handler — stub",
        );
        self.win32_stub(
            m,
            "_invalid_parameter_noinfo",
            false,
            "CRT invalid param nop",
        );
        self.win32_stub(
            m,
            "_invalid_parameter_noinfo_noreturn",
            false,
            "CRT invalid param abort",
        );
        // Initializer and exit functions are guest code.
        for module in [m, "ucrtbase.dll"] {
            for symbol in [
                "_initterm",
                "_initterm_e",
                "_initialize_onexit_table",
                "_register_onexit_function",
                "_execute_onexit_table",
                "atexit",
                "_crt_atexit",
                "_cexit",
            ] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        self.win32_stub(
            m,
            "_register_thread_local_exe_atexit_callback",
            false,
            "TLS atexit — no-op",
        );
        self.win32_stub(m, "_configure_narrow_argv", false, "argv setup — no-op");
        self.win32_stub(
            m,
            "_initialize_narrow_environment",
            false,
            "env setup — no-op",
        );
        self.win32_stub(
            m,
            "_get_narrow_winmain_command_line",
            false,
            "command line — returns empty",
        );
        self.win32_stub(m, "_configthreadlocale", false, "thread locale — no-op");
        self.win32_stub(m, "_c_exit", false, "CRT _c_exit — no-op");
        self.win32_stub(m, "_cexit", false, "CRT _cexit — no-op");
        self.win32_stub(m, "_seh_filter_exe", false, "SEH filter — stub returning 0");
        self.win32_stub(
            m,
            "__p___argc",
            false,
            "argc pointer — returns pointer to 0",
        );
        self.win32_stub(m, "__p___argv", false, "argv pointer — returns null");
        self.win32_stub(m, "__p__commode", false, "_commode pointer — stub");
        self.win32_stub(
            m,
            "__std_terminate",
            false,
            "MSVC std::terminate — calls abort",
        );
        self.win32_stub(
            m,
            "_get_initial_narrow_environment",
            false,
            "env accessor — stub",
        );
        self.win32_stub(
            m,
            "longjmp",
            false,
            "longjmp — stub (setjmp not supported in DBT)",
        );
        self.win32_stub(m, "__intrinsic_setjmp", false, "setjmp intrinsic — stub");
    }

    // ---- api-ms-win-crt-heap -----------------------------------------------

    fn populate_crt_heap(&mut self) {
        let m = "api-ms-win-crt-heap-l1-1-0.dll";
        for module in [m, "ucrtbase.dll"] {
            for symbol in [
                "malloc",
                "free",
                "calloc",
                "realloc",
                "_msize",
                "_recalloc",
                "_aligned_malloc",
                "_aligned_realloc",
                "_aligned_msize",
                "_aligned_free",
            ] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        self.win32_stub(m, "_set_new_handler", false, "new handler — stub");
    }

    // ---- api-ms-win-crt-time -----------------------------------------------

    fn populate_crt_time(&mut self) {
        let m = "api-ms-win-crt-time-l1-1-0.dll";
        self.win32_stub(m, "_time64", false, "64-bit time() — returns host time_t");
        self.win32_stub(m, "_gmtime64", false, "64-bit gmtime — returns UTC tm");
        self.win32_stub(m, "_gmtime64_s", false, "64-bit gmtime_s — safe variant");
        self.win32_stub(m, "_localtime64_s", false, "64-bit localtime_s");
        self.win32_stub(m, "_mktime64", false, "64-bit mktime");
        self.win32_stub(m, "_mkgmtime64", false, "64-bit mkgmtime");
        self.win32_stub(m, "_difftime64", false, "64-bit difftime");
        self.win32_stub(m, "_ftime64", false, "64-bit ftime — fills FILETIME struct");
        self.crt_native(m, "strftime", "strftime", "ISO C strftime — identical ABI");
    }

    // ---- api-ms-win-crt-convert --------------------------------------------

    fn populate_crt_convert(&mut self) {
        let m = "api-ms-win-crt-convert-l1-1-0.dll";
        for module in [m, "ucrtbase.dll"] {
            for symbol in [
                "strtol",
                "strtoul",
                "wcstol",
                "wcstoul",
                "strtoll",
                "strtoull",
                "wcstoll",
                "wcstoull",
                "_strtoi64",
                "_strtoui64",
                "_wcstoi64",
                "_wcstoui64",
            ] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        for (sym, libc) in &[
            ("mbstowcs", "mbstowcs"),
            ("wcstombs", "wcstombs"),
            ("wctob", "wctob"),
            ("btowc", "btowc"),
        ] {
            self.crt_native(m, sym, libc, "ISO C conversion — identical ABI on macOS");
        }
        self.win32_stub(m, "_itoa", false, "Windows itoa — integer to ASCII");
        self.win32_stub(m, "_itoa_s", false, "Windows itoa_s — safe variant");
        self.win32_stub(m, "_wtoi", false, "Wide char to int");
        self.win32_stub(m, "_wtol", false, "Wide char to long");
        // Floating conversions return in XMM0 and parse in the "C" locale.
        for module in [m, "ucrtbase.dll"] {
            for symbol in ["strtod", "strtof", "wcstod", "wcstof", "atof", "_wtof"] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
    }

    // ---- api-ms-win-crt-utility --------------------------------------------

    fn populate_crt_utility(&mut self) {
        let m = "api-ms-win-crt-utility-l1-1-0.dll";
        // Windows long is 32 bits and lldiv_t returns through a hidden
        // pointer, so these cannot map to the LP64 host libc.
        for module in [m, "ucrtbase.dll"] {
            for symbol in ["abs", "labs", "llabs", "div", "ldiv", "lldiv"] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        // The comparators are guest code, so these cannot use host libc.
        for module in [m, "ucrtbase.dll"] {
            for symbol in ["qsort", "qsort_s", "bsearch", "bsearch_s"] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        self.crt_native(m, "rand", "rand", "ISO C rand");
        self.crt_native(m, "srand", "srand", "ISO C srand");
    }

    // ---- api-ms-win-crt-filesystem -----------------------------------------

    fn populate_crt_filesystem(&mut self) {
        let m = "api-ms-win-crt-filesystem-l1-1-0.dll";
        self.win32_stub(m, "_wstat64", false, "Wide stat64 — stub returning -1");
        self.win32_stub(m, "_stat64", false, "stat64 variant — stub");
        self.win32_stub(
            m,
            "_access",
            false,
            "Windows access check — stub returning 0",
        );
        self.win32_stub(m, "_mkdir", false, "Windows mkdir — stub");
        self.win32_stub(m, "_getcwd", false, "Windows getcwd — stub returning empty");
    }

    // ---- api-ms-win-crt-locale ---------------------------------------------

    fn populate_crt_locale(&mut self) {
        let m = "api-ms-win-crt-locale-l1-1-0.dll";
        for module in [m, "ucrtbase.dll"] {
            for symbol in ["setlocale", "localeconv"] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        self.win32_stub(
            m,
            "_create_locale",
            false,
            "Windows locale creation — stub returning null",
        );
        self.win32_stub(m, "_free_locale", false, "Windows locale free — no-op");
        self.win32_stub(
            m,
            "_configthreadlocale",
            false,
            "Thread locale config — no-op",
        );
    }

    // ---- Services the loader registry provides ------------------------------

    /// Standard services registered by `crates/loader/src/services` that no
    /// other table classifies. Synthetic fixture modules are left out.
    fn populate_registered_services(&mut self) {
        for (module, symbols) in [
            (
                "api-ms-win-core-processthreads-l1-1-0.dll",
                &["IsProcessorFeaturePresent"][..],
            ),
            (
                "api-ms-win-core-timezone-l1-1-0.dll",
                &["GetTimeZoneInformation"][..],
            ),
            (
                "api-ms-win-core-sysinfo-l1-1-0.dll",
                &["GetLogicalProcessorInformation"][..],
            ),
            (
                "kernel32.dll",
                &[
                    "IsProcessorFeaturePresent",
                    "GetTimeZoneInformation",
                    "GetLogicalProcessorInformation",
                    "CloseHandle",
                    "FreeLibrary",
                    "GetCurrentDirectoryA",
                    "GetCurrentDirectoryW",
                    "GetLastError",
                    "GetModuleFileNameA",
                    "GetModuleFileNameW",
                    "GetProcessId",
                    "GetProcessPriorityBoost",
                    "LoadLibraryA",
                    "LoadLibraryW",
                    "OpenThread",
                    "SetCurrentDirectoryA",
                    "SetCurrentDirectoryW",
                    "SetEnvironmentVariableA",
                    "SetEnvironmentVariableW",
                    "SetLastError",
                    "SetProcessPriorityBoost",
                    "VirtualAlloc",
                    "VirtualFree",
                ][..],
            ),
            (
                "ucrtbase.dll",
                &[
                    "_atoi64",
                    "_gmtime64_s",
                    "_localtime64_s",
                    "_putenv",
                    "_stricmp",
                    "_strnicmp",
                    "_time64",
                    "_wcsicmp",
                    "_wcsnicmp",
                    "_wtoi",
                    "_wtoi64",
                    "_wtol",
                    "atoi",
                    "atol",
                    "atoll",
                    "gmtime_s",
                    "localtime_s",
                    "rand",
                    "rand_s",
                    "srand",
                    "time",
                ][..],
            ),
            (
                "ws2_32.dll",
                &[
                    "WSACleanup",
                    "WSAGetLastError",
                    "WSAPoll",
                    "WSASetLastError",
                    "WSAStartup",
                    "accept",
                    "bind",
                    "closesocket",
                    "connect",
                    "getpeername",
                    "getsockname",
                    "getsockopt",
                    "htonl",
                    "htons",
                    "inet_ntop",
                    "inet_pton",
                    "ioctlsocket",
                    "listen",
                    "ntohl",
                    "ntohs",
                    "recv",
                    "recvfrom",
                    "send",
                    "sendto",
                    "shutdown",
                    "socket",
                ][..],
            ),
            (
                "xgameruntime.dll",
                &[
                    "XAsyncCancel",
                    "XAsyncGetResultSize",
                    "XAsyncGetStatus",
                    "XAsyncRun",
                    "XTaskQueueCloseHandle",
                    "XTaskQueueCreate",
                    "XTaskQueueCreateComposite",
                    "XTaskQueueDispatch",
                    "XTaskQueueDuplicateHandle",
                    "XTaskQueueGetPort",
                    "XTaskQueueSubmitCallback",
                    "XTaskQueueSubmitDelayedCallback",
                    "XTaskQueueTerminate",
                ][..],
            ),
            (
                "xinput1_4.dll",
                &[
                    "XInputEnable",
                    "XInputGetCapabilities",
                    "XInputGetState",
                    "XInputSetState",
                ][..],
            ),
        ] {
            for symbol in symbols {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
    }

    // ---- VCRUNTIME140 / VCRUNTIME140_1 -------------------------------------

    fn populate_vcruntime(&mut self) {
        for m in &["vcruntime140.dll", "vcruntime140_1.dll"] {
            self.runtime_helper(m, "__CxxFrameHandler3", "MSVC C++ EH frame handler — stub");
            self.runtime_helper(
                m,
                "__CxxFrameHandler4",
                "MSVC C++ EH frame handler v4 — stub",
            );
            self.runtime_helper(
                m,
                "__CxxThrowException",
                "MSVC C++ throw — terminates guest",
            );
            self.runtime_helper(m, "__C_specific_handler", "SEH __C_specific_handler — stub");
            self.runtime_helper(
                m,
                "__RTDynamicCast",
                "MSVC dynamic_cast helper — stub returning 0",
            );
            self.runtime_helper(
                m,
                "__current_exception",
                "current exception accessor — stub",
            );
            self.runtime_helper(
                m,
                "__current_exception_context",
                "current exception context — stub",
            );
            self.runtime_helper(m, "__std_terminate", "std::terminate — calls abort");
            if *m == "vcruntime140.dll" {
                for symbol in [
                    "__std_type_info_compare",
                    "__std_type_info_hash",
                    "__std_type_info_name",
                    "__std_type_info_destroy_list",
                    "__std_exception_copy",
                    "__std_exception_destroy",
                    "__std_terminate",
                    "_purecall",
                ] {
                    self.ins(m, symbol, Classification::StandardService);
                }
            } else {
                self.runtime_helper(
                    m,
                    "__std_type_info_compare",
                    "type_info comparison — stub returning -1",
                );
                self.runtime_helper(m, "__std_exception_copy", "std::exception copy — stub");
                self.runtime_helper(
                    m,
                    "__std_exception_destroy",
                    "std::exception destroy — stub",
                );
            }
            self.runtime_helper(m, "memcpy", "CRT memcpy delegate — same as libc");
            self.runtime_helper(m, "memset", "CRT memset delegate — same as libc");
            self.runtime_helper(m, "memmove", "CRT memmove delegate — same as libc");
            self.runtime_helper(m, "memcmp", "CRT memcmp delegate — same as libc");
        }
    }

    // ---- Win32 kernel / process --------------------------------------------

    fn populate_win32_kernel(&mut self) {
        for module in ["kernel32.dll", "api-ms-win-core-heap-l1-1-0.dll"] {
            for symbol in [
                "GetProcessHeap",
                "HeapAlloc",
                "HeapReAlloc",
                "HeapSize",
                "HeapFree",
                "HeapCreate",
                "HeapDestroy",
            ] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        let p1 = "api-ms-win-core-processthreads-l1-1-0.dll";
        let p2 = "api-ms-win-core-processthreads-l1-1-2.dll";
        for module in [p1, "kernel32.dll"] {
            self.ins(module, "GetThreadId", Classification::StandardService);
        }
        for module in ["kernel32.dll", "api-ms-win-core-handle-l1-1-0.dll"] {
            self.ins(module, "DuplicateHandle", Classification::StandardService);
        }
        for m in &[p1, p2, "kernel32.dll"] {
            self.win32_stub(
                m,
                "CreateThread",
                true,
                "Thread creation — stub returning fake handle 1",
            );
            self.win32_stub(m, "ResumeThread", false, "ResumeThread — stub returning 0");
            self.win32_stub(
                m,
                "SuspendThread",
                false,
                "SuspendThread — stub returning 0",
            );
            self.win32_stub(
                m,
                "TerminateProcess",
                false,
                "TerminateProcess — calls exit(1)",
            );
            self.win32_stub(m, "GetStartupInfoW", false, "GetStartupInfoW — fills zeros");
            // Typed services in crates/loader/runtime/services.txt, which
            // does not list the l1-1-2 API set for them.
            for (symbol, note) in [
                ("GetCurrentThreadId", "returns synthetic thread ID"),
                ("GetCurrentProcessId", "returns synthetic process ID"),
                ("GetCurrentThread", "returns pseudo-handle"),
                ("GetCurrentProcess", "returns pseudo-handle -1"),
                ("SetThreadPriority", "no-op in single-thread context"),
                ("GetThreadPriority", "returns THREAD_PRIORITY_NORMAL"),
            ] {
                if *m == p2 {
                    self.win32_stub(m, symbol, false, note);
                } else {
                    self.ins(m, symbol, Classification::StandardService);
                }
            }
            self.win32_stub(m, "Sleep", false, "Sleep — no-op (single-threaded)");
            self.win32_stub(
                m,
                "ExitProcess",
                false,
                "ExitProcess — not registered on this API set",
            );
            self.win32_stub(m, "ExitThread", false, "ExitThread — calls exit(0)");
        }
        // The guest thread scheduler provides these on kernel32 and the
        // l1-1-0 API set only; l1-1-2 keeps its stub classification.
        for module in [p1, "kernel32.dll"] {
            for symbol in [
                "CreateThread",
                "ExitThread",
                "TerminateThread",
                "GetExitCodeThread",
                "SuspendThread",
                "ResumeThread",
                "SwitchToThread",
                // Process exit (#331) ends the guest, never the host.
                "ExitProcess",
            ] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        let err = "api-ms-win-core-errorhandling-l1-1-0.dll";
        self.win32_stub(err, "GetLastError", false, "returns last_error from TEB");
        self.win32_stub(err, "SetLastError", false, "sets last_error in TEB");
        self.win32_stub(
            err,
            "SetUnhandledExceptionFilter",
            false,
            "exception filter — stub returning null",
        );
        self.win32_stub(
            err,
            "UnhandledExceptionFilter",
            false,
            "unhandled exception — calls abort",
        );
        self.win32_stub(
            err,
            "RaiseException",
            false,
            "exception raise — calls abort",
        );
    }

    // ---- Win32 synchronisation ---------------------------------------------

    fn populate_win32_synch(&mut self) {
        let m = "api-ms-win-core-synch-l1-1-0.dll";
        for module in [m, "kernel32.dll"] {
            for symbol in [
                "CreateEventA",
                "CreateEventW",
                "SetEvent",
                "ResetEvent",
                "CreateSemaphoreA",
                "CreateSemaphoreW",
                "ReleaseSemaphore",
                "WaitForSingleObject",
                "WaitForSingleObjectEx",
                "WaitForMultipleObjects",
                "WaitForMultipleObjectsEx",
                "CreateMutexA",
                "CreateMutexW",
                "CreateMutexExA",
                "CreateMutexExW",
                "ReleaseMutex",
                "InitializeCriticalSection",
                "InitializeCriticalSectionAndSpinCount",
                "InitializeCriticalSectionEx",
                "EnterCriticalSection",
                "TryEnterCriticalSection",
                "LeaveCriticalSection",
                "DeleteCriticalSection",
                "SetCriticalSectionSpinCount",
                "InitializeSRWLock",
                "AcquireSRWLockExclusive",
                "AcquireSRWLockShared",
                "TryAcquireSRWLockExclusive",
                "TryAcquireSRWLockShared",
                "ReleaseSRWLockExclusive",
                "ReleaseSRWLockShared",
            ] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        for module in ["api-ms-win-core-synch-l1-2-0.dll", "kernel32.dll"] {
            for symbol in [
                "InitializeConditionVariable",
                "WakeConditionVariable",
                "WakeAllConditionVariable",
                "SleepConditionVariableCS",
                "SleepConditionVariableSRW",
                "InitOnceInitialize",
                "InitOnceBeginInitialize",
                "InitOnceComplete",
                "InitOnceExecuteOnce",
            ] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        for module in ["api-ms-win-core-interlocked-l1-1-0.dll", "kernel32.dll"] {
            for symbol in [
                "InitializeSListHead",
                "InterlockedPushEntrySList",
                "InterlockedPopEntrySList",
                "InterlockedFlushSList",
                "QueryDepthSList",
            ] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        for module in ["api-ms-win-core-processthreads-l1-1-0.dll", "kernel32.dll"] {
            for symbol in ["TlsAlloc", "TlsFree", "TlsGetValue", "TlsSetValue"] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        for module in ["api-ms-win-core-fibers-l1-1-0.dll", "kernel32.dll"] {
            for symbol in ["FlsAlloc", "FlsFree", "FlsGetValue", "FlsSetValue"] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        self.win32_stub(m, "CloseHandle", false, "CloseHandle — no-op stub");
        self.win32_stub(
            m,
            "InterlockedIncrement",
            false,
            "Interlocked increment — stub",
        );
        self.win32_stub(
            m,
            "InterlockedDecrement",
            false,
            "Interlocked decrement — stub",
        );
        self.win32_stub(
            m,
            "InterlockedCompareExchange",
            false,
            "InterlockedCompareExchange — stub",
        );
        self.win32_stub(
            m,
            "InterlockedExchange",
            false,
            "InterlockedExchange — stub",
        );

        let m2 = "api-ms-win-core-synch-l1-2-1.dll";
        self.ins(
            m2,
            "WaitForMultipleObjects",
            Classification::StandardService,
        );
    }

    // ---- Win32 library loader ----------------------------------------------

    fn populate_win32_loader(&mut self) {
        let m = "api-ms-win-core-libraryloader-l1-2-0.dll";
        self.win32_stub(
            m,
            "LoadLibraryExA",
            false,
            "LoadLibrary — stub returning null (no dynamic loading)",
        );
        self.win32_stub(
            m,
            "LoadLibraryExW",
            false,
            "LoadLibraryW — stub returning null",
        );
        self.win32_stub(m, "FreeLibrary", false, "FreeLibrary — no-op stub");
        for module in [m, "kernel32.dll"] {
            for symbol in ["GetModuleHandleA", "GetModuleHandleW", "GetProcAddress"] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        self.win32_stub(
            m,
            "GetModuleFileNameA",
            false,
            "GetModuleFileName — stub returning empty path",
        );
        self.win32_stub(
            m,
            "GetModuleFileNameW",
            false,
            "GetModuleFileNameW — stub returning empty path",
        );
    }

    // ---- Win32 performance -------------------------------------------------

    fn populate_win32_profile(&mut self) {
        for module in ["kernel32.dll", "api-ms-win-core-profile-l1-1-0.dll"] {
            for symbol in ["QueryPerformanceCounter", "QueryPerformanceFrequency"] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        for module in [
            "kernel32.dll",
            "api-ms-win-core-processenvironment-l1-1-0.dll",
        ] {
            for symbol in ["GetEnvironmentVariableA", "GetEnvironmentVariableW"] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
    }

    // ---- Win32 system info -------------------------------------------------

    fn populate_win32_sysinfo(&mut self) {
        let m = "api-ms-win-core-sysinfo-l1-1-0.dll";
        for (module, symbols) in [
            (
                m,
                &[
                    "GetSystemTimeAsFileTime",
                    "GetTickCount",
                    "GetTickCount64",
                    "GetSystemInfo",
                    "GlobalMemoryStatusEx",
                    "GetSystemTime",
                    "GetLocalTime",
                ][..],
            ),
            (
                "api-ms-win-core-sysinfo-l1-2-0.dll",
                &["GetNativeSystemInfo", "GetSystemTimePreciseAsFileTime"],
            ),
            (
                "api-ms-win-core-timezone-l1-1-0.dll",
                &["FileTimeToSystemTime", "SystemTimeToFileTime"],
            ),
            (
                "api-ms-win-core-realtime-l1-1-0.dll",
                &["QueryUnbiasedInterruptTime"],
            ),
            ("api-ms-win-core-synch-l1-2-0.dll", &["Sleep"]),
            ("api-ms-win-core-synch-l1-1-0.dll", &["SleepEx"]),
            (
                "kernel32.dll",
                &[
                    "GetSystemTimeAsFileTime",
                    "GetTickCount",
                    "GetTickCount64",
                    "GetSystemInfo",
                    "GetNativeSystemInfo",
                    "GlobalMemoryStatusEx",
                    "GetSystemTime",
                    "GetLocalTime",
                    "GetSystemTimePreciseAsFileTime",
                    "FileTimeToSystemTime",
                    "SystemTimeToFileTime",
                    "QueryUnbiasedInterruptTime",
                    "Sleep",
                    "SleepEx",
                ],
            ),
        ] {
            for symbol in symbols {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        self.win32_stub(
            m,
            "GetProcessorCount",
            false,
            "returns 1 for single-threaded guest",
        );
    }

    // ---- Win32 file I/O ----------------------------------------------------

    fn populate_win32_file(&mut self) {
        let m = "api-ms-win-core-file-l1-1-0.dll";
        for module in [m, "kernel32.dll"] {
            for symbol in [
                "CreateFileA",
                "CreateFileW",
                "ReadFile",
                "WriteFile",
                "GetFileSize",
                "GetFileSizeEx",
                "GetFileType",
                "GetFileTime",
                "SetFileTime",
                "FlushFileBuffers",
                "SetFilePointerEx",
                "SetEndOfFile",
                "CreateDirectoryW",
                "RemoveDirectoryW",
                "DeleteFileA",
                "DeleteFileW",
                "GetFileAttributesA",
                "GetFileAttributesW",
                "GetFileAttributesExA",
                "GetFileAttributesExW",
                "GetTempPathA",
                "GetTempPathW",
                "GetTempFileNameW",
                "MoveFileW",
                "MoveFileExW",
                "FindFirstFileW",
                "FindNextFileW",
                "FindClose",
            ] {
                self.ins(module, symbol, Classification::StandardService);
            }
        }
        self.win32_stub(m, "CloseHandle", false, "CloseHandle — no-op stub");
    }

    // ---- Proprietary / unavailable -----------------------------------------

    fn populate_unsupported(&mut self) {
        // Xbox-specific DLLs — no public equivalent
        for sym in &[
            "Ordinal#1",
            "Ordinal#2",
            "Ordinal#3",
            "Ordinal#4",
            "Ordinal#5",
        ] {
            for m in &[
                "libHttpClient.GDK.dll",
                "Bink2Scarlett.dll",
                "AcpHal.dll",
                "XboxBvhBuilder.dll",
                "xapu.dll",
                "chad.dll",
                "amd_fidelityfx_loader_xbox.dll",
                "xcrdapi.dll",
            ] {
                self.unsupported(m, sym, "Xbox-proprietary DLL — no public documentation");
            }
        }
        // General unsupported modules
        for m in &[
            "libHttpClient.GDK.dll",
            "Bink2Scarlett.dll",
            "AcpHal.dll",
            "XboxBvhBuilder.dll",
            "xapu.dll",
            "chad.dll",
            "amd_fidelityfx_loader_xbox.dll",
            "xcrdapi.dll",
            "WINHTTP.dll",
            "MFPlat.DLL",
            "d3d12_xs.dll",
            "ext-ms-win-rtcore-ntuser-window-ansi-l1-1-0.dll",
            "api-ms-win-core-com-l1-1-0.dll",
            "WS2_32.dll",
            "MSVCP140.dll", // C++ classes — require vtable layout knowledge
        ] {
            self.ins(m, "__wildcard__", Classification::Unsupported {
                reason: "Xbox-proprietary, COM, networking, or C++ ABI — requires manual engineering",
            });
        }
    }
}
