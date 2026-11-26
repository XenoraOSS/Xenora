// SPDX-License-Identifier: GPL-3.0-or-later

//! Synthetic guest calls for the toolhelp process snapshot of a machine with
//! one process. Expectations use Microsoft's public contracts:
//! https://learn.microsoft.com/windows/win32/api/tlhelp32/nf-tlhelp32-createtoolhelp32snapshot
//! https://learn.microsoft.com/windows/win32/api/tlhelp32/nf-tlhelp32-process32firstw
//!
//! The one-process machine is a Xenora fact, not a Windows behavior.

mod support;

use support::{Guest, SCRATCH};

const KERNEL32: &str = "kernel32.dll";

const IMPORTS: &[(&str, &str)] = &[
    (KERNEL32, "GetLastError"),
    (KERNEL32, "CloseHandle"),
    (KERNEL32, "CreateToolhelp32Snapshot"),
    (KERNEL32, "Process32FirstW"),
    (KERNEL32, "Process32NextW"),
    (KERNEL32, "Process32First"),
    (KERNEL32, "Process32Next"),
    (KERNEL32, "IsWow64Process"),
    (KERNEL32, "GetCurrentProcessId"),
    (KERNEL32, "GetProcessTimes"),
    (KERNEL32, "OpenProcess"),
    (KERNEL32, "ReadProcessMemory"),
    (KERNEL32, "CreateRemoteThread"),
];

const ENTRY: u64 = SCRATCH;
const ERROR_NO_MORE_FILES: u64 = 259;
const ERROR_INVALID_HANDLE: u64 = 6;
const TH32CS_SNAPPROCESS: u64 = 2;

#[test]
fn the_snapshot_enumerates_the_one_process() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let own_pid = guest.call("GetCurrentProcessId", &[]) as u32;
    let snapshot = guest.call("CreateToolhelp32Snapshot", &[TH32CS_SNAPPROCESS, 0]);
    assert_ne!(snapshot, 0);
    // First answers the machine's one process with one thread and no parent.
    guest.memory.write_u32(ENTRY, 0x248).unwrap();
    assert_eq!(guest.call("Process32FirstW", &[snapshot, ENTRY]), 1);
    assert_eq!(guest.u32_at(ENTRY + 8), own_pid);
    assert_eq!(guest.u32_at(ENTRY + 0x18), 1, "one thread");
    assert_eq!(guest.u32_at(ENTRY + 0x20), 0, "no parent");
    let mut units = [0u16; 17];
    for (slot, unit) in units.iter_mut().enumerate() {
        *unit = guest.u16_at(ENTRY + 0x28 + slot as u64 * 2);
    }
    let name: String = units
        .iter()
        .take_while(|unit| **unit != 0)
        .map(|unit| char::from(*unit as u8))
        .collect();
    assert_eq!(name, "xenora-guest.exe");
    // Next is the end of the list, honestly: ERROR_NO_MORE_FILES.
    assert_eq!(guest.call("Process32NextW", &[snapshot, ENTRY]), 0);
    assert_eq!(
        guest.call("GetLastError", &[]) & 0xFFFF_FFFF,
        ERROR_NO_MORE_FILES
    );
    assert_eq!(guest.call("CloseHandle", &[snapshot]), 1);
    // The narrow width enumerates the same process.
    let narrow = guest.call("CreateToolhelp32Snapshot", &[TH32CS_SNAPPROCESS, 0]);
    guest.memory.write_u32(ENTRY, 0x128).unwrap();
    assert_eq!(guest.call("Process32First", &[narrow, ENTRY]), 1);
    assert_eq!(guest.u32_at(ENTRY + 8), own_pid);
    assert_eq!(guest.call("Process32Next", &[narrow, ENTRY]), 0);
    assert_eq!(guest.call("CloseHandle", &[narrow]), 1);
    // A file handle is not a snapshot.
    assert_eq!(guest.call("Process32FirstW", &[0xDEAD, ENTRY]), 0);
    assert_eq!(
        guest.call("GetLastError", &[]) & 0xFFFF_FFFF,
        ERROR_INVALID_HANDLE
    );
}

#[test]
fn nothing_runs_under_wow64() {
    let mut guest = Guest::new(IMPORTS, &[]);
    const OUT: u64 = SCRATCH;
    assert_eq!(guest.call("IsWow64Process", &[u64::MAX, OUT]), 1);
    assert_eq!(guest.u32_at(OUT), 0);
}

/// The process times of the machine's one process read the mock clock:
/// a live process has no exit time.
#[test]
fn the_process_times_of_the_one_process() {
    let mut guest = Guest::new(IMPORTS, &[]);
    const CREATION: u64 = SCRATCH;
    const EXIT: u64 = SCRATCH + 8;
    const KERNEL: u64 = SCRATCH + 0x10;
    const USER: u64 = SCRATCH + 0x18;
    // Each output lands at its own pointer: exit at the r8 argument is
    // zero for a live process, creation, kernel and user read the clock.
    for pointer in [CREATION, EXIT, KERNEL, USER] {
        guest.memory.write_u64(pointer, 0xFF).unwrap();
    }
    assert_eq!(
        guest.call("GetProcessTimes", &[u64::MAX, CREATION, EXIT, KERNEL, USER]),
        1
    );
    assert_eq!(guest.u64_at(KERNEL), guest.u64_at(CREATION));
    assert_eq!(guest.u64_at(USER), guest.u64_at(CREATION));
    assert_eq!(guest.u64_at(EXIT), 0, "a live process has no exit time");
    // An unknown handle is refused.
    assert_eq!(
        guest.call("GetProcessTimes", &[0xDEAD, CREATION, EXIT, KERNEL, USER]),
        0
    );
}

/// OpenProcess serves the machine's one process and no other, and
/// ReadProcessMemory copies the guest memory exactly.
#[test]
fn open_process_and_read_process_memory_see_the_one_process() {
    const SOURCE: u64 = SCRATCH + 0x900;
    const TARGET: u64 = SCRATCH + 0xA00;
    const COUNT: u64 = SCRATCH + 0xB00;
    let mut guest = Guest::new(IMPORTS, &[]);
    // The machine's one process answers a real handle.
    let process = guest.call("OpenProcess", &[0x1F0FFF, 0, 0x4100]);
    assert_ne!(process, 0);
    assert_ne!(process, u64::MAX);
    // Any other id does not exist.
    assert_eq!(guest.call("OpenProcess", &[0x1F0FFF, 0, 0x9999]), 0);
    // Read the memory back exactly.
    guest
        .memory
        .write_bytes(SOURCE, b"process-memory-check")
        .unwrap();
    assert_eq!(
        guest.call("ReadProcessMemory", &[process, SOURCE, TARGET, 20, COUNT]),
        1
    );
    assert_eq!(guest.u64_at(COUNT), 20);
    let mut got = Vec::new();
    for index in 0..20u64 {
        got.push(guest.memory.read_u8(TARGET + index).unwrap());
    }
    assert_eq!(got, b"process-memory-check");
    // A closed handle stops reading.
    assert_eq!(guest.call("CloseHandle", &[process]), 1);
    assert_eq!(
        guest.call("ReadProcessMemory", &[process, SOURCE, TARGET, 4, COUNT]),
        0
    );
    // The cross-process forms are honestly unsupported.
    assert_eq!(
        guest.call("CreateRemoteThread", &[process, 0, 0, 0, 0, 0, 0]),
        u64::MAX
    );
    assert_eq!(guest.call("GetLastError", &[]) & 0xFFFF_FFFF, 50);
}
