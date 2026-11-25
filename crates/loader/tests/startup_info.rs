// SPDX-License-Identifier: GPL-3.0-or-later

//! What a process was started with: `GetStartupInfo` and `GetCommandLine`.
//!
//! `tests/compat_fixtures/build-startup-info.sh` compiles an original C program
//! with no C library and an original hosted C++ program with mingw-w64, so this
//! test needs it on the host and fails, not skips, when it is missing.
//!
//! The expected values are the ones this file gives the process. The structure
//! layout is asserted three times over: here by offset, in the C program from
//! the documented field list, and in the C++ program from the public headers.
//! The C program splits the command line by the documented rules, checked first
//! against Microsoft's examples.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use support::{Guest, SCRATCH, SCRATCH_LEN};
use xenora_loader::cpu::ExitReason;
use xenora_loader::imports::ImportRegistry;
use xenora_loader::services::{
    MockClock, ProcessStartupInfo, ProgramArgumentsError, ServicesState, StartupInfoError,
    register_standard_services,
};
use xenora_loader::startup::{StartupLayout, StartupResult, run_synthetic_startup};

/// The status of the C program's run in which every check passed.
const STATUS: u32 = 0x5157;
const SIZE: usize = 104;

fn built() -> &'static Path {
    static OUT: OnceLock<PathBuf> = OnceLock::new();
    OUT.get_or_init(|| {
        let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("startup-info-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/compat_fixtures/build-startup-info.sh"
        );
        let build = Command::new("sh").arg(script).arg(&out).output().unwrap();
        assert!(
            build.status.success(),
            "the startup programs did not build; they need mingw-w64:\n{}{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        out
    })
}

fn state() -> Arc<ServicesState> {
    Arc::new(ServicesState::new(Arc::new(MockClock::new(0))))
}

fn start(name: &str, state: &Arc<ServicesState>) -> Result<StartupResult, String> {
    let bytes = std::fs::read(built().join(format!("{name}.exe"))).unwrap();
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(state));
    let layout = StartupLayout {
        image_base: 0x2_5000_0000,
        stack_top: 0x2_0001_0000,
        tls_base: 0x3_0000_0000,
    };
    run_synthetic_startup(&bytes, layout, registry, 5_000_000).map_err(|error| error.to_string())
}

fn exit(name: &str, state: &Arc<ServicesState>) -> ExitReason {
    start(name, state).unwrap().execution.exit_reason
}

fn file(state: &ServicesState, name: &str) -> Option<Vec<u8>> {
    state.file_contents(&format!("C:\\{name}.bin"))
}

/// Zero-terminated UTF-16 strings, one after another.
fn wide(bytes: &[u8]) -> Vec<String> {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|unit| u16::from_le_bytes(*unit))
        .collect();
    let mut strings: Vec<String> = units
        .split(|unit| *unit == 0)
        .map(|text| String::from_utf16(text).unwrap())
        .collect();
    assert_eq!(strings.pop().as_deref(), Some(""), "terminated");
    strings
}

fn narrow(bytes: &[u8]) -> String {
    let (last, text) = bytes.split_last().unwrap();
    assert!(*last == 0 && !text.contains(&0));
    String::from_utf8(text.to_vec()).unwrap()
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn pointer(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

/// Every field, each with a different value.
fn full() -> ProcessStartupInfo {
    ProcessStartupInfo {
        desktop: Some("WinSta0\\Default".into()),
        title: Some("caf\u{e9} \u{2713} \u{1f600}".into()),
        x: 11,
        y: 22,
        x_size: 33,
        y_size: 44,
        x_count_chars: 55,
        y_count_chars: 66,
        fill_attribute: 77,
        flags: 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x100,
        show_window: 3,
        std_input: 0x1111_2222_3333_4444,
        std_output: u64::MAX,
        std_error: 0x99,
    }
}

const LEVELS: [&str; 4] = ["O0", "O1", "O2", "Os"];

#[test]
fn a_c_program_reads_the_startup_information_it_was_given() {
    for level in LEVELS {
        let state = state();
        let given = full();
        state.set_startup_info(given.clone()).unwrap();
        let name = format!("startup-info-{level}");
        assert_eq!(exit(&name, &state), ExitReason::ProcessExit(STATUS));
        for kind in ["w", "a"] {
            let info = file(&state, &format!("info-{kind}")).unwrap();
            assert_eq!(info.len(), SIZE);
            assert_eq!(word(&info, 0), SIZE as u32);
            assert_eq!(pointer(&info, 8), 0, "lpReserved");
            let words: Vec<u32> = (32..64).step_by(4).map(|at| word(&info, at)).collect();
            assert_eq!(words, [11, 22, 33, 44, 55, 66, 77, given.flags]);
            // wShowWindow, then cbReserved2 and lpReserved2.
            assert_eq!(word(&info, 64), 3);
            assert_eq!(pointer(&info, 72), 0);
            let handles: Vec<u64> = (80..104).step_by(8).map(|at| pointer(&info, at)).collect();
            assert_eq!(handles, [given.std_input, u64::MAX, 0x99]);
        }
        for (what, text) in [("desktop", given.desktop), ("title", given.title)] {
            let text = text.unwrap();
            let got = file(&state, &format!("{what}-w")).unwrap();
            assert_eq!(wide(&got), std::slice::from_ref(&text), "{level}");
            let got = file(&state, &format!("{what}-a")).unwrap();
            assert_eq!(narrow(&got), text, "{level}");
        }
    }
}

#[test]
fn a_process_started_with_nothing_gets_only_the_size() {
    let state = state();
    assert_eq!(
        exit("startup-info-O1", &state),
        ExitReason::ProcessExit(STATUS)
    );
    for kind in ["info-w", "info-a"] {
        let info = file(&state, kind).unwrap();
        assert_eq!(word(&info, 0), SIZE as u32);
        assert!(info[4..].iter().all(|byte| *byte == 0));
    }
    assert_eq!(file(&state, "desktop-w"), None);
    assert_eq!(file(&state, "title-a"), None);
    // No program name is available: an empty one, quoted.
    assert_eq!(narrow(&file(&state, "line-a").unwrap()), "\"\"");
    assert_eq!(wide(&file(&state, "arguments").unwrap()), [""]);
}

#[test]
fn the_command_line_splits_into_the_programs_arguments() {
    let cases: &[&[&str]] = &[
        &["prog.exe"],
        &["prog.exe", "one", "two words", "", "tab\there"],
        &["C:\\Program Files\\x y\\prog.exe", "plain"],
        &["C:\\dir\\", "a\\", "b c\\", "d\\\\", "e f\\\\"],
        &["p", "say \"hi\"", "\"", "\\\"", "\\\\\"x", "a\"\"b", "\"\""],
        &["p", "a\\\\b", "c\\d e", "\\\\server\\share\\", "^%*?&|<>"],
        &["caf\u{e9}.exe", "\u{2713}", "\u{1f600} x"],
    ];
    for (index, arguments) in cases.iter().enumerate() {
        let level = LEVELS[index % 4];
        let state = state();
        state.set_program_arguments(arguments).unwrap();
        let name = format!("startup-info-{level}");
        assert_eq!(exit(&name, &state), ExitReason::ProcessExit(STATUS));
        let got = wide(&file(&state, "arguments").unwrap());
        assert_eq!(got, *arguments, "{level}");
        // One string in two encodings.
        let line = wide(&file(&state, "line-w").unwrap());
        let bytes = narrow(&file(&state, "line-a").unwrap());
        assert_eq!(line, [bytes]);
    }
}

#[test]
fn plain_arguments_are_not_quoted() {
    let state = state();
    state
        .set_program_arguments(&["prog.exe", "one", "C:\\a\\b", "x y"])
        .unwrap();
    exit("startup-info-O2", &state);
    let line = narrow(&file(&state, "line-a").unwrap());
    assert_eq!(line, "prog.exe one C:\\a\\b \"x y\"");
}

#[test]
fn a_hosted_cpp_program_sees_them_before_main() {
    for level in LEVELS {
        let state = state();
        let given = ProcessStartupInfo {
            title: Some("a title".into()),
            ..full()
        };
        state.set_startup_info(given).unwrap();
        let arguments = ["cpp.exe", "two words", "q\"uote"];
        state.set_program_arguments(&arguments).unwrap();
        let name = format!("startup-info-cpp-{level}");
        assert_eq!(exit(&name, &state), ExitReason::ProcessExit(0), "{level}");
        let output = String::from_utf8(state.standard_output()).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(
            lines,
            [
                "line cpp.exe \"two words\" \"q\\\"uote\"",
                "argv 0 cpp.exe",
                "argv 1 two words",
                "argv 2 q\"uote",
                "title a title",
                "show 3",
            ],
            "{level}"
        );

        // Without STARTF_USESHOWWINDOW the program shows its window by default.
        let state = self::state();
        state.set_program_arguments(&["cpp.exe"]).unwrap();
        assert_eq!(exit(&name, &state), ExitReason::ProcessExit(0), "{level}");
        let output = String::from_utf8(state.standard_output()).unwrap();
        assert!(output.contains("title (none)"), "{output}");
        assert!(output.contains("show 10"), "{output}");
    }
}

#[test]
fn the_listed_api_sets_forward_and_an_unlisted_one_does_not() {
    let state = state();
    state.set_startup_info(full()).unwrap();
    state.set_program_arguments(&["p", "a b"]).unwrap();
    assert_eq!(
        exit("startup-info-apiset", &state),
        ExitReason::ProcessExit(STATUS)
    );
    assert_eq!(wide(&file(&state, "arguments").unwrap()), ["p", "a b"]);

    let error = start("startup-info-unlisted", &self::state())
        .err()
        .unwrap();
    assert!(error.contains("GetStartupInfoW"), "{error}");
}

#[test]
fn a_wrong_expectation_stops_at_its_check() {
    assert_eq!(
        exit("startup-info-break", &state()),
        ExitReason::ProcessExit(1)
    );
}

const IMPORTS: &[(&str, &str)] = &[
    ("KERNEL32.dll", "GetStartupInfoA"),
    ("KERNEL32.dll", "GetStartupInfoW"),
    ("KERNEL32.dll", "GetCommandLineA"),
    ("KERNEL32.dll", "GetCommandLineW"),
];

/// The API sets Microsoft's pages list for the two pairs of functions.
#[test]
fn every_documented_api_set_resolves_to_the_same_service() {
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state());
    let resolve = |module: &str, symbol: &str| {
        registry
            .resolve(
                module,
                &xenora_loader::import_table::ImportName::Name(symbol.into()),
            )
            .map(|service| service.module.to_ascii_lowercase())
    };
    let mut startup: Vec<String> = (0..=8)
        .map(|version| format!("api-ms-win-core-processthreads-l1-1-{version}.dll"))
        .collect();
    startup.extend((0..=5).map(|v| format!("api-ms-win-core-kernel32-legacy-l1-1-{v}.dll")));
    startup.push("api-ms-win-downlevel-kernel32-l1-1-0.dll".into());
    startup.push("api-ms-win-downlevel-kernel32-l2-1-0.dll".into());
    let command = [
        "api-ms-win-core-processenvironment-l1-1-0.dll",
        "api-ms-win-core-processenvironment-l1-2-0.dll",
        "api-ms-win-downlevel-kernel32-l1-1-0.dll",
    ];
    for suffix in ["A", "W"] {
        let (info, line) = (
            format!("GetStartupInfo{suffix}"),
            format!("GetCommandLine{suffix}"),
        );
        for module in startup.iter().map(String::as_str).chain(["KERNEL32.dll"]) {
            let found = resolve(module, &info);
            assert_eq!(found.as_deref(), Some("kernel32.dll"), "{module}!{info}");
        }
        for module in command.into_iter().chain(["kernel32.dll"]) {
            let found = resolve(module, &line);
            assert_eq!(found.as_deref(), Some("kernel32.dll"), "{module}!{line}");
        }
        assert_eq!(
            resolve("api-ms-win-core-processthreads-l1-1-9.dll", &info),
            None
        );
        assert_eq!(
            resolve("api-ms-win-core-kernel32-legacy-l1-1-6.dll", &info),
            None
        );
        assert_eq!(
            resolve("api-ms-win-core-processthreads-l1-1-0.dll", &line),
            None
        );
        assert_eq!(
            resolve("api-ms-win-core-processenvironment-l1-1-0.dll", &info),
            None
        );
    }
}

#[test]
fn a_structure_that_cannot_be_written_stops_the_guest_and_allocates_nothing() {
    for symbol in ["GetStartupInfoA", "GetStartupInfoW"] {
        let mut guest = Guest::new(IMPORTS, &[]);
        guest.state.set_startup_info(full()).unwrap();
        // The last byte is past the mapping, and the null pointer.
        for output in [SCRATCH + SCRATCH_LEN - SIZE as u64 + 1, 0] {
            let tail = SCRATCH + SCRATCH_LEN - 64;
            guest.memory.write_bytes(tail, &[0xCC; 64]).unwrap();
            let reason = guest.fails(symbol, &[output]);
            assert!(matches!(reason, ExitReason::MemoryFault(_)), "{reason:?}");
            let mut left = [0; 64];
            guest.memory.read_bytes(tail, &mut left).unwrap();
            assert_eq!(left, [0xCC; 64]);
        }
        // The strings were not handed out, so the host may still change them.
        assert_eq!(guest.state.set_startup_info(full()), Ok(()));
        // The last 104 bytes of the mapping are enough.
        let output = SCRATCH + SCRATCH_LEN - SIZE as u64;
        guest.call(symbol, &[output]);
        assert_eq!(guest.memory.read_u32(output).unwrap(), SIZE as u32);
    }
}

#[test]
fn strings_stay_where_they_are_and_the_host_cannot_change_them_later() {
    let mut guest = Guest::new(IMPORTS, &[]);
    guest.state.set_startup_info(full()).unwrap();
    guest.state.set_program_arguments(&["one"]).unwrap();
    guest.state.set_program_arguments(&["two"]).unwrap();
    for symbol in ["GetStartupInfoA", "GetStartupInfoW"] {
        guest.call(symbol, &[SCRATCH]);
        guest.call(symbol, &[SCRATCH + 0x100]);
        for at in [16, 24] {
            let first = guest.memory.read_u64(SCRATCH + at).unwrap();
            assert_ne!(first, 0);
            assert_eq!(guest.memory.read_u64(SCRATCH + 0x100 + at).unwrap(), first);
        }
        assert_eq!(
            guest.state.set_startup_info(ProcessStartupInfo::default()),
            Err(StartupInfoError::AlreadyRead)
        );
    }
    let (a, w) = (
        guest.call("GetCommandLineA", &[]),
        guest.call("GetCommandLineW", &[]),
    );
    assert!(a != 0 && w != 0 && a != w);
    assert_eq!(guest.call("GetCommandLineA", &[]), a);
    assert_eq!(guest.call("GetCommandLineW", &[]), w);
    assert_eq!(
        guest.memory.read_u32(a).unwrap(),
        u32::from_le_bytes(*b"two\0")
    );
    assert_eq!(
        guest.state.set_program_arguments(&["three"]),
        Err(ProgramArgumentsError::AlreadyConfigured)
    );
}

#[test]
fn no_arguments_give_an_empty_command_line() {
    let mut guest = Guest::new(IMPORTS, &[]);
    guest.state.set_program_arguments(&[]).unwrap();
    let line = guest.call("GetCommandLineW", &[]);
    assert_eq!(guest.memory.read_u16(line).unwrap(), 0);
    // A quote cannot be represented in the program name.
    let mut guest = Guest::new(IMPORTS, &[]);
    guest.state.set_program_arguments(&["a\"b"]).unwrap();
    let line = guest.call("GetCommandLineA", &[]);
    let mut text = [0xCC; 3];
    guest.memory.read_bytes(line, &mut text).unwrap();
    assert_eq!(text, *b"ab ");
}

/// STARTUPINFOW documents which flags exist, which exclude each other, and that
/// STARTF_PREVENTPINNING needs STARTF_TITLEISAPPID.
#[test]
fn startup_information_the_documentation_excludes_is_refused() {
    let set = |change: &dyn Fn(&mut ProcessStartupInfo)| {
        let mut info = ProcessStartupInfo::default();
        change(&mut info);
        state().set_startup_info(info)
    };
    for flags in [0x100 | 0x200, 0x800 | 0x1000, 0x2000, 0x2000 | 0x800] {
        assert_eq!(
            set(&|info| info.flags = flags),
            Err(StartupInfoError::ConflictingFlags),
            "{flags:#x}"
        );
    }
    for flags in [0x100, 0x200, 0x800, 0x1000, 0x1000 | 0x2000, 0x8000, 0xFF] {
        assert_eq!(set(&|info| info.flags = flags), Ok(()), "{flags:#x}");
    }
    for flags in [0x400, 0x4000, 0x1_0000, 0x8000_0000] {
        assert_eq!(
            set(&|info| info.flags = flags),
            Err(StartupInfoError::UnknownFlag),
            "{flags:#x}"
        );
    }
    let shown =
        |flags, show_window| set(&|info| (info.flags, info.show_window) = (flags, show_window));
    assert_eq!(shown(1, 10), Err(StartupInfoError::ShowDefault));
    assert_eq!(shown(0, 10), Ok(()));
    assert_eq!(shown(1, 9), Ok(()));
    assert_eq!(
        set(&|info| info.title = Some("a\0b".into())),
        Err(StartupInfoError::ContainsNul)
    );
    assert_eq!(
        set(&|info| info.desktop = Some("a\0".into())),
        Err(StartupInfoError::ContainsNul)
    );
    // 32,767 characters with the terminator, counted in UTF-16 units.
    assert_eq!(set(&|info| info.title = Some("x".repeat(32_766))), Ok(()));
    assert_eq!(
        set(&|info| info.title = Some("x".repeat(32_767))),
        Err(StartupInfoError::TooLong)
    );
    assert_eq!(
        set(&|info| info.desktop = Some("\u{1f600}".repeat(16_384))),
        Err(StartupInfoError::TooLong)
    );
}

/// `GetProcAddress` finds them, as startup code that looks them up expects.
#[test]
fn they_are_exported_from_kernel32_for_lookup() {
    let mut guest = Guest::new(
        &[
            ("KERNEL32.dll", "GetModuleHandleA"),
            ("KERNEL32.dll", "GetProcAddress"),
            ("KERNEL32.dll", "LoadLibraryA"),
        ],
        &[],
    );
    guest
        .memory
        .write_bytes(SCRATCH, b"kernel32.dll\0")
        .unwrap();
    let module = guest.call("LoadLibraryA", &[SCRATCH]);
    assert_ne!(module, 0);
    for (_, symbol) in IMPORTS {
        let name = [symbol.as_bytes(), b"\0"].concat();
        guest.memory.write_bytes(SCRATCH + 64, &name).unwrap();
        assert_ne!(
            guest.call("GetProcAddress", &[module, SCRATCH + 64]),
            0,
            "{symbol}"
        );
    }
}
