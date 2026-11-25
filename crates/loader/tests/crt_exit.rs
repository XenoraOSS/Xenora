// SPDX-License-Identifier: GPL-3.0-or-later

//! The C runtime's `exit`, `_exit` and `abort` (issue #395), called from
//! translated guest code through a bound import table.
//!
//! Expected behaviour is from the Microsoft Learn pages "exit, _Exit, _exit"
//! (`exit` calls the functions registered by `atexit` "in last-in-first-out
//! (LIFO) order"; `_exit` terminates "without ... processing atexit or _onexit
//! functions") and "abort" ("calls _exit to terminate the process with exit
//! code 3"), and from ISO C 7.22.4.2 and 7.22.4.4 for a function registered
//! while `exit` runs. Calling `exit` again from a registered function is
//! undefined in ISO C; the test for it states Xenora's rule. The order each
//! test expects is the order in which it registered its functions. The guest
//! code is original.

mod support;

use support::*;
use xenora_loader::cpu::ExitReason;
use xenora_loader::import_table::ImportName;
use xenora_loader::imports::{ImportRegistry, ServiceStatus};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};

const KERNEL32: &str = "kernel32.dll";
const UCRT: &str = "ucrtbase.dll";
const RUNTIME: &str = "api-ms-win-crt-runtime-l1-1-0.dll";

const EXIT: usize = 0;
const ATEXIT: usize = 3;
const IMPORTS: &[(&str, &str)] = &[
    (UCRT, "exit"),
    (UCRT, "_exit"),
    (UCRT, "abort"),
    (UCRT, "atexit"),
    (RUNTIME, "_crt_atexit"),
    (RUNTIME, "_cexit"),
    (KERNEL32, "CreateEventW"),
    (KERNEL32, "SetLastError"),
];
/// The same imports as a program built with mingw-w64 names the three: by the
/// runtime API set.
const BY_API_SET: &[(&str, &str)] = &[
    (RUNTIME, "exit"),
    (RUNTIME, "_exit"),
    (RUNTIME, "abort"),
    (UCRT, "atexit"),
    (RUNTIME, "_crt_atexit"),
    (RUNTIME, "_cexit"),
    (KERNEL32, "CreateEventW"),
    (KERNEL32, "SetLastError"),
];

/// A decimal log of the registered functions that ran, oldest digit first.
const LOG: u64 = SCRATCH;
/// Set to one by code placed after a call that must not return.
const RETURNED: u64 = SCRATCH + 0x100;

/// Guest address of the stub `call [slot]; hlt` for import `index`.
fn stub(index: usize) -> u64 {
    BASE + u64::from(CODE_RVA) + 16 * index as u64
}

/// Guest address of the import address table slot of import `index`, read
/// from the displacement of its stub.
fn slot(guest: &Guest, index: usize) -> u64 {
    let displacement = guest.u32_at(stub(index) + 2) as i32;
    (stub(index) + 6).wrapping_add_signed(i64::from(displacement))
}

// Routine indices. Routines are 32 bytes apart.
const APPEND_1: usize = 0;
const APPEND_2: usize = 1;
const APPEND_3: usize = 2;
const APPEND_4: usize = 3;
const APPEND_5: usize = 4;
const REGISTER_4_THEN_APPEND_5: usize = 5;
const EXIT_AGAIN: usize = 6;
/// The status the `EXIT_AGAIN` routine exits with.
const SECOND_STATUS: u32 = 77;

/// `log = log * 10 + digit; return 0`
fn append(digit: u8) -> Vec<u8> {
    let mut code = vec![0x48, 0xB8]; // mov rax, LOG
    code.extend(LOG.to_le_bytes());
    code.extend([
        0x48, 0x8B, 0x08, // mov rcx, [rax]
        0x48, 0x6B, 0xC9, 0x0A, // imul rcx, rcx, 10
        0x48, 0x83, 0xC1, digit, // add rcx, digit
        0x48, 0x89, 0x08, // mov [rax], rcx
        0x31, 0xC0, // xor eax, eax
        0xC3, // ret
    ]);
    code
}

fn rel32(from_end: u64, to: u64) -> [u8; 4] {
    (to.wrapping_sub(from_end) as u32).to_le_bytes()
}

/// A registered function that registers `APPEND_4` with `atexit`, through the
/// import address table as compiled code would, and then appends 5.
fn register_4_then_append_5(atexit_slot: u64) -> Vec<u8> {
    let at = routine(REGISTER_4_THEN_APPEND_5);
    let mut code = vec![0x48, 0x83, 0xEC, 0x28]; // sub rsp, 0x28
    code.extend([0x48, 0x8D, 0x0D]); // lea rcx, [rip + APPEND_4]
    code.extend(rel32(at + 11, routine(APPEND_4)));
    code.extend([0xFF, 0x15]); // call [rip + atexit slot]
    code.extend(rel32(at + 17, atexit_slot));
    code.extend([0x48, 0x83, 0xC4, 0x28]); // add rsp, 0x28
    code.push(0xE9); // jmp APPEND_5
    code.extend(rel32(at + 26, routine(APPEND_5)));
    code
}

/// A registered function that calls `exit(SECOND_STATUS)`. The code after the
/// call would record that `exit` returned.
fn exit_again() -> Vec<u8> {
    let at = routine(EXIT_AGAIN);
    let mut code = vec![0x48, 0x83, 0xEC, 0x28]; // sub rsp, 0x28
    code.push(0xB9); // mov ecx, SECOND_STATUS
    code.extend(SECOND_STATUS.to_le_bytes());
    code.push(0xE8); // call <exit stub>
    code.extend(rel32(at + 14, stub(EXIT)));
    code.extend([0x48, 0xB8]); // mov rax, RETURNED
    code.extend(RETURNED.to_le_bytes());
    code.extend([
        0xC6, 0x00, 0x01, // mov byte ptr [rax], 1
        0x48, 0x83, 0xC4, 0x28, // add rsp, 0x28
        0xC3, // ret
    ]);
    code
}

fn guest_with(imports: &'static [(&'static str, &'static str)]) -> Guest {
    // The slot of `atexit` depends only on the import list, so a guest built
    // without routines tells where it is.
    let atexit_slot = slot(&Guest::new(imports, &[]), ATEXIT);
    let routines = [
        append(1),
        append(2),
        append(3),
        append(4),
        append(5),
        register_4_then_append_5(atexit_slot),
        exit_again(),
    ];
    let guest = Guest::new(imports, &routines);
    assert_eq!(slot(&guest, ATEXIT), atexit_slot);
    guest
}

fn guest() -> Guest {
    guest_with(IMPORTS)
}

fn register(guest: &mut Guest, symbol: &str, index: usize) {
    assert_eq!(guest.call(symbol, &[routine(index)]) as u32, 0, "{symbol}");
}

#[test]
fn exit_runs_registered_functions_most_recent_first_then_ends_the_process() {
    for imports in [IMPORTS, BY_API_SET] {
        let mut guest = guest_with(imports);
        // atexit and _crt_atexit feed one list.
        register(&mut guest, "atexit", APPEND_1);
        register(&mut guest, "_crt_atexit", APPEND_2);
        register(&mut guest, "atexit", APPEND_3);
        assert_eq!(guest.u64_at(LOG), 0);
        assert_eq!(guest.state.exit_status(), None);
        // The status is an int: the upper half of RCX is not part of it.
        let (_, exit) = guest.run("exit", &[0xAAAA_BBBB_1234_5678]);
        assert_eq!(exit, ExitReason::ProcessExit(0x1234_5678));
        assert_eq!(guest.u64_at(LOG), 321, "last registered runs first");
        assert_eq!(guest.state.exit_status(), Some(0x1234_5678));
    }
}

#[test]
fn exit_with_nothing_registered_ends_the_process_with_its_status() {
    for (argument, status) in [
        (0, 0),
        (1, 1),
        (300, 300),
        (0xFFFF_FFFF, 0xFFFF_FFFF),
        (0x1_0000_0000, 0),
    ] {
        let mut guest = guest();
        // The stub is `call [exit]; hlt`: a service that returned would reach
        // the HLT and report Halt.
        let (_, exit) = guest.run("exit", &[argument]);
        assert_eq!(exit, ExitReason::ProcessExit(status), "{argument:#x}");
        assert_eq!(guest.u64_at(LOG), 0);
        assert_eq!(guest.state.exit_status(), Some(status));
    }
}

/// ISO C 7.22.4.4: registered functions are called in reverse order of
/// registration, "except that a function is called after any previously
/// registered functions that had already been called at the time it was
/// registered". A function registered by the second function to run is
/// therefore the third to run, ahead of the one registered first.
#[test]
fn a_function_registered_while_exit_runs_is_called_next() {
    let mut guest = guest();
    register(&mut guest, "atexit", APPEND_1);
    register(&mut guest, "atexit", REGISTER_4_THEN_APPEND_5);
    register(&mut guest, "atexit", APPEND_3);
    let (_, exit) = guest.run("exit", &[6]);
    assert_eq!(exit, ExitReason::ProcessExit(6));
    // 3 ran first; then the function that registered 4 and logged 5; then 4,
    // registered during exit; then 1, the oldest.
    assert_eq!(guest.u64_at(LOG), 3541);
}

/// Calling `exit` from a registered function is undefined in ISO C. Xenora's
/// rule: the functions that have not run yet run once each, and the process
/// ends with the status of the later call. Nothing runs twice and the inner
/// call does not return.
#[test]
fn exit_called_again_from_a_registered_function_finishes_with_the_later_status() {
    let mut guest = guest();
    register(&mut guest, "atexit", APPEND_1);
    register(&mut guest, "atexit", EXIT_AGAIN);
    register(&mut guest, "atexit", APPEND_3);
    let (_, exit) = guest.run("exit", &[5]);
    assert_eq!(exit, ExitReason::ProcessExit(SECOND_STATUS));
    // 3, then the function that called exit again, then 1 from inside that
    // second call. A status of 5 would mean the function never ran.
    assert_eq!(guest.u64_at(LOG), 31);
    assert_eq!(guest.memory.read_u8(RETURNED).unwrap(), 0);
    assert_eq!(guest.state.exit_status(), Some(SECOND_STATUS));
}

#[test]
fn underscore_exit_and_abort_run_no_registered_function() {
    for imports in [IMPORTS, BY_API_SET] {
        // _exit ends the process with its status, an int.
        for (argument, status) in [(9, 9), (0xAAAA_BBBB_0000_0102, 0x102), (0, 0)] {
            let mut guest = guest_with(imports);
            register(&mut guest, "atexit", APPEND_1);
            register(&mut guest, "_crt_atexit", APPEND_2);
            let (_, exit) = guest.run("_exit", &[argument]);
            assert_eq!(exit, ExitReason::ProcessExit(status), "{argument:#x}");
            assert_eq!(guest.u64_at(LOG), 0);
            assert_eq!(guest.state.exit_status(), Some(status));
        }
        // abort takes no argument and ends the process with exit code 3,
        // whatever the argument registers hold.
        for junk in [0, 0x1234] {
            let mut guest = guest_with(imports);
            register(&mut guest, "atexit", APPEND_1);
            let (_, exit) = guest.run("abort", &[junk, junk, junk, junk]);
            assert_eq!(exit, ExitReason::ProcessExit(3));
            assert_eq!(guest.u64_at(LOG), 0);
            assert_eq!(guest.state.exit_status(), Some(3));
        }
    }
}

#[test]
fn each_way_out_closes_the_guest_handles() {
    for symbol in ["exit", "_exit", "abort"] {
        let mut guest = guest();
        assert_ne!(guest.call("CreateEventW", &[0, 1, 0, 0]), 0);
        assert_ne!(guest.call("CreateEventW", &[0, 0, 1, 0]), 0);
        assert_eq!(guest.state.open_handle_count(), 2);
        let (_, exit) = guest.run(symbol, &[4]);
        assert!(matches!(exit, ExitReason::ProcessExit(_)), "{symbol}");
        assert_eq!(guest.state.open_handle_count(), 0, "{symbol}");
    }
    // With registered functions: they run while the handles are still open,
    // and the handles are closed once the last one has returned.
    let mut guest = guest();
    assert_ne!(guest.call("CreateEventW", &[0, 1, 0, 0]), 0);
    register(&mut guest, "atexit", APPEND_1);
    assert_eq!(guest.run("exit", &[0]).1, ExitReason::ProcessExit(0));
    assert_eq!(guest.u64_at(LOG), 1);
    assert_eq!(guest.state.open_handle_count(), 0);
}

#[test]
fn an_exited_guest_runs_nothing_further() {
    for symbol in ["exit", "_exit", "abort"] {
        let mut guest = guest();
        let first = guest.run(symbol, &[8]).1;
        assert!(matches!(first, ExitReason::ProcessExit(_)), "{symbol}");
        // Neither another service nor a second exit takes effect.
        for (later, argument) in [("SetLastError", 5), ("exit", 9), ("_exit", 9), ("abort", 0)] {
            assert_eq!(guest.run(later, &[argument]).1, first, "{symbol}, {later}");
        }
        assert_eq!(guest.state.last_error(THREAD_A), 0, "{symbol}");
    }
}

#[test]
fn functions_already_run_by_cexit_are_not_run_again_by_exit() {
    let mut guest = guest();
    register(&mut guest, "atexit", APPEND_1);
    register(&mut guest, "atexit", APPEND_2);
    // _cexit runs the registered functions and returns to the caller.
    assert_eq!(guest.call("_cexit", &[]), 0);
    assert_eq!(guest.u64_at(LOG), 21);
    // One registered afterwards is the only one exit has left to run.
    register(&mut guest, "atexit", APPEND_3);
    assert_eq!(guest.run("exit", &[2]).1, ExitReason::ProcessExit(2));
    assert_eq!(guest.u64_at(LOG), 213);
}

#[test]
fn the_three_resolve_under_the_library_and_the_runtime_api_set() {
    let state = std::sync::Arc::new(ServicesState::new(std::sync::Arc::new(MockClock::new(0))));
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, state);
    let named = |name: &str| ImportName::Name(name.to_owned());
    for symbol in ["exit", "_exit", "abort"] {
        let service = registry
            .resolve(UCRT, &named(symbol))
            .unwrap_or_else(|| panic!("{symbol}"));
        let aliased = registry.resolve(RUNTIME, &named(symbol)).unwrap();
        assert!(std::ptr::eq(service, aliased), "{symbol}");
        assert_eq!(service.status, ServiceStatus::Partial, "{symbol}");
        // Not exported by kernel32, and not through an unrelated API set.
        assert!(registry.resolve(KERNEL32, &named(symbol)).is_none());
        assert!(
            registry
                .resolve("api-ms-win-crt-heap-l1-1-0.dll", &named(symbol))
                .is_none()
        );
    }
    // The spelling is exact: _Exit is another function and is not provided.
    assert!(registry.resolve(UCRT, &named("_Exit")).is_none());
}
