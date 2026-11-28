// SPDX-License-Identifier: GPL-3.0-or-later

//! Exercise the documented non-alertable SignalObjectAndWait contract through
//! the exact kernel32.dll import and the guest scheduler.

mod support;

use std::sync::Arc;
use support::{Guest, SCRATCH, THREAD_A, THREAD_B};
use xenora_loader::cpu::Register;
use xenora_loader::scheduler::Outcome;
use xenora_loader::services::{Clock, MockClock, ServicesState};

const KERNEL32: &str = "kernel32.dll";
const SIGNAL_WAIT: &str = "SignalObjectAndWait";
const WAIT_OBJECT_0: u64 = 0;
const WAIT_TIMEOUT: u64 = 258;
const WAIT_FAILED: u64 = u32::MAX as u64;
const ERROR_ACCESS_DENIED: u64 = 5;
const ERROR_INVALID_HANDLE: u64 = 6;
const ERROR_NOT_SUPPORTED: u64 = 50;
const ERROR_NOT_OWNER: u64 = 288;
const ERROR_TOO_MANY_POSTS: u64 = 298;
const SYNCHRONIZE: u64 = 0x0010_0000;
const EVENT_MODIFY_STATE: u64 = 2;

const IMPORTS: &[(&str, &str)] = &[
    (KERNEL32, SIGNAL_WAIT),
    (KERNEL32, "CreateEventW"),
    (KERNEL32, "CreateMutexW"),
    (KERNEL32, "CreateSemaphoreW"),
    (KERNEL32, "DuplicateHandle"),
    (KERNEL32, "GetLastError"),
    (KERNEL32, "ReleaseMutex"),
    (KERNEL32, "WaitForSingleObject"),
];

fn event(guest: &mut Guest, signaled: bool) -> u64 {
    let handle = guest.call("CreateEventW", &[0, 0, u64::from(signaled), 0]);
    assert_ne!(handle, 0);
    handle
}

fn semaphore(guest: &mut Guest, count: u64, maximum: u64) -> u64 {
    let handle = guest.call("CreateSemaphoreW", &[0, count, maximum, 0]);
    assert_ne!(handle, 0);
    handle
}

fn duplicate(guest: &mut Guest, handle: u64, access: u64) -> u64 {
    assert_eq!(
        guest.call(
            "DuplicateHandle",
            &[u64::MAX, handle, u64::MAX, SCRATCH, access, 0, 0],
        ),
        1
    );
    let duplicate = guest.u64_at(SCRATCH);
    assert!(![0, handle].contains(&duplicate));
    duplicate
}

fn poll(guest: &mut Guest, handle: u64) -> u64 {
    guest.call("WaitForSingleObject", &[handle, 0])
}

fn assert_failure(guest: &mut Guest, args: [u64; 4], error: u64) {
    assert_eq!(guest.call(SIGNAL_WAIT, &args), WAIT_FAILED, "{args:X?}");
    assert_eq!(guest.error(), error, "{args:X?}");
}

#[test]
fn a_zero_timeout_still_signals_the_first_object() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let signal = event(&mut guest, false);
    let wait = event(&mut guest, false);

    assert_eq!(guest.call(SIGNAL_WAIT, &[signal, wait, 0, 0]), WAIT_TIMEOUT);
    assert_eq!(poll(&mut guest, signal), WAIT_OBJECT_0);
    assert_eq!(poll(&mut guest, signal), WAIT_TIMEOUT);
    assert_eq!(poll(&mut guest, wait), WAIT_TIMEOUT);
}

#[test]
fn a_scheduled_timeout_resumes_without_signaling_again() {
    let clock = Arc::new(MockClock::new(0));
    let state = ServicesState::new(clock.clone());
    let mut guest = Guest::with_state(IMPORTS, &[], state);
    let signal = semaphore(&mut guest, 0, 8);
    let wait = event(&mut guest, false);
    let report = guest.scheduled(SIGNAL_WAIT, &[signal, wait, 7, 0], 1, 128);

    assert_eq!(report.outcome, Outcome::Halted);
    assert_eq!(report.main.get(Register::Rax), WAIT_TIMEOUT);
    assert_eq!(clock.performance_count(), 70_000);
    assert_eq!(poll(&mut guest, signal), WAIT_OBJECT_0);
    assert_eq!(poll(&mut guest, signal), WAIT_TIMEOUT);
}

#[test]
fn a_duplicate_can_wait_on_the_object_just_signaled() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let signal = semaphore(&mut guest, 0, 2);
    let wait = duplicate(&mut guest, signal, SYNCHRONIZE);

    assert_eq!(
        guest.call(SIGNAL_WAIT, &[signal, wait, 0, 0]),
        WAIT_OBJECT_0
    );
    assert_eq!(poll(&mut guest, signal), WAIT_TIMEOUT);
}

#[test]
fn signaling_an_owned_mutex_releases_one_recursion_level() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let mutex = guest.on(THREAD_A).call("CreateMutexW", &[0, 1, 0]);
    assert_ne!(mutex, 0);
    assert_eq!(poll(guest.on(THREAD_A), mutex), WAIT_OBJECT_0);

    for expected in [WAIT_TIMEOUT, WAIT_OBJECT_0] {
        let wait = event(&mut guest, true);
        assert_eq!(
            guest.on(THREAD_A).call(SIGNAL_WAIT, &[mutex, wait, 0, 0]),
            WAIT_OBJECT_0
        );
        assert_eq!(poll(guest.on(THREAD_B), mutex), expected);
        assert_eq!(poll(&mut guest, wait), WAIT_TIMEOUT);
    }
    assert_eq!(guest.on(THREAD_B).call("ReleaseMutex", &[mutex]), 1);
}

#[test]
fn invalid_handles_access_and_alertable_waits_leave_objects_unchanged() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let signal = event(&mut guest, false);
    let wait = event(&mut guest, true);
    let signal_without_modify = duplicate(&mut guest, signal, SYNCHRONIZE);
    let wait_without_synchronize = duplicate(&mut guest, wait, EVENT_MODIFY_STATE);

    for (args, error) in [
        ([0, wait, 0, 0], ERROR_INVALID_HANDLE),
        ([signal, 0, 0, 0], ERROR_INVALID_HANDLE),
        ([signal_without_modify, wait, 0, 0], ERROR_ACCESS_DENIED),
        (
            [signal, wait_without_synchronize, 0, 0],
            ERROR_ACCESS_DENIED,
        ),
        ([signal, wait, 0, 1], ERROR_NOT_SUPPORTED),
    ] {
        assert_failure(&mut guest, args, error);
        assert_eq!(poll(&mut guest, signal), WAIT_TIMEOUT);
    }
    assert_eq!(poll(&mut guest, wait), WAIT_OBJECT_0);
    assert_eq!(poll(&mut guest, wait), WAIT_TIMEOUT);
}

#[test]
fn semaphore_overflow_and_nonowner_mutex_release_fail_before_waiting() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let full = semaphore(&mut guest, 1, 1);
    let wait = event(&mut guest, true);
    assert_failure(&mut guest, [full, wait, 0, 0], ERROR_TOO_MANY_POSTS);
    assert_eq!(poll(&mut guest, full), WAIT_OBJECT_0);
    assert_eq!(poll(&mut guest, full), WAIT_TIMEOUT);

    let mutex = guest.on(THREAD_A).call("CreateMutexW", &[0, 1, 0]);
    assert_ne!(mutex, 0);
    assert_failure(guest.on(THREAD_B), [mutex, wait, 0, 0], ERROR_NOT_OWNER);
    assert_eq!(poll(&mut guest, mutex), WAIT_TIMEOUT);
    assert_eq!(poll(&mut guest, wait), WAIT_OBJECT_0);
    assert_eq!(guest.on(THREAD_A).call("ReleaseMutex", &[mutex]), 1);
}
