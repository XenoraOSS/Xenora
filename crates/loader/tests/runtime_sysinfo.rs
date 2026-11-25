// SPDX-License-Identifier: GPL-3.0-or-later

//! System information, calendar conversion and sleeping through the guest
//! import table.
//!
//! Layouts and constants come from the public `SYSTEM_INFO`, `MEMORYSTATUSEX`
//! and `SYSTEMTIME` documentation and winnt.h. Calendar expectations come from
//! the FILETIME epoch (1601-01-01 UTC) and dates computed independently with
//! the host `date` utility, not from the conversion code.

mod support;

use std::sync::Arc;
use support::*;
use xenora_loader::services::sysinfo::{GuestSystemProfile, HostCapabilities};
use xenora_loader::services::{MockClock, ServicesState};

const KERNEL32: &str = "kernel32.dll";
const SYSINFO: &str = "api-ms-win-core-sysinfo-l1-1-0.dll";

const IMPORTS: &[(&str, &str)] = &[
    (KERNEL32, "GetLastError"),
    (SYSINFO, "GetSystemInfo"),
    ("api-ms-win-core-sysinfo-l1-2-0.dll", "GetNativeSystemInfo"),
    (SYSINFO, "GlobalMemoryStatusEx"),
    (SYSINFO, "GetSystemTime"),
    (KERNEL32, "GetLocalTime"),
    (KERNEL32, "GetSystemTimeAsFileTime"),
    (
        "api-ms-win-core-sysinfo-l1-2-0.dll",
        "GetSystemTimePreciseAsFileTime",
    ),
    (
        "api-ms-win-core-timezone-l1-1-0.dll",
        "FileTimeToSystemTime",
    ),
    (KERNEL32, "SystemTimeToFileTime"),
    (
        "api-ms-win-core-realtime-l1-1-0.dll",
        "QueryUnbiasedInterruptTime",
    ),
    ("api-ms-win-core-synch-l1-2-0.dll", "Sleep"),
    (KERNEL32, "SleepEx"),
    (KERNEL32, "GetTickCount64"),
    (KERNEL32, "VirtualAlloc"),
    (KERNEL32, "GetProcessHeap"),
    (KERNEL32, "HeapAlloc"),
];

const ERROR_INVALID_PARAMETER: u64 = 87;
const UNIX_EPOCH_FILETIME: u64 = 116_444_736_000_000_000;
const TICKS_PER_SECOND: u64 = 10_000_000;
/// `date -u -j -f "%Y-%m-%d %H:%M:%S" "2026-11-24 12:34:56" +%s`, a Tuesday.
const NOV_24_2026: u64 = 1_795_523_696;
/// `date -u -j -f "%Y-%m-%d %H:%M:%S" "2024-02-29 23:59:59" +%s`, a Thursday.
const FEB_29_2024: u64 = 1_709_251_199;

fn filetime(unix_seconds: u64, millis: u64) -> u64 {
    UNIX_EPOCH_FILETIME + unix_seconds * TICKS_PER_SECOND + millis * 10_000
}

/// SYSTEMTIME fields: year, month, day of week, day, hour, minute, second, ms.
fn system_time(guest: &Guest, address: u64) -> [u16; 8] {
    std::array::from_fn(|index| guest.u16_at(address + 2 * index as u64))
}

fn write_system_time(guest: &mut Guest, address: u64, fields: [u16; 8]) {
    for (index, field) in fields.into_iter().enumerate() {
        guest
            .memory
            .write_u16(address + 2 * index as u64, field)
            .unwrap();
    }
}

#[test]
fn system_info_reports_the_simulated_guest_machine() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let info = SCRATCH;
    for symbol in ["GetSystemInfo", "GetNativeSystemInfo"] {
        guest.memory.write_bytes(info, &[0xAA; 56]).unwrap();
        assert_eq!(guest.call(symbol, &[info]), 0);
        assert_eq!(guest.u16_at(info), 9); // PROCESSOR_ARCHITECTURE_AMD64
        assert_eq!(guest.u32_at(info + 4), 4096); // dwPageSize
        assert_eq!(guest.u64_at(info + 8), 0x1_0000);
        assert_eq!(guest.u64_at(info + 16), 0x7fff_fffe_ffff);
        assert_eq!(guest.u64_at(info + 24), 1); // dwActiveProcessorMask
        assert_eq!(guest.u32_at(info + 32), 1); // dwNumberOfProcessors
        assert_eq!(guest.u32_at(info + 36), 8664); // PROCESSOR_AMD_X8664
        assert_eq!(guest.u32_at(info + 40), 65_536); // dwAllocationGranularity
        assert_eq!(guest.u32_at(info + 44), 0); // no simulated CPU model
        assert_eq!(guest.u64_at(info + 48), 0xAAAA_AAAA_AAAA_AAAA);
    }

    let mut state = ServicesState::new(Arc::new(MockClock::new(0)));
    state.guest_profile = GuestSystemProfile { processors: 4 };
    let mut guest = Guest::with_state(IMPORTS, &[], state);
    assert_eq!(guest.call("GetSystemInfo", &[info]), 0);
    assert_eq!(guest.u64_at(info + 24), 0b1111);
    assert_eq!(guest.u32_at(info + 32), 4);

    // The guest page size does not follow the host's.
    let host = HostCapabilities::detect();
    assert!(host.page_size.is_some() && host.logical_processors.is_some());
    assert_eq!(guest.u32_at(info + 4), 4096);
}

#[test]
fn memory_status_tracks_guest_allocations_against_the_budget() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let status = SCRATCH;
    // dwLength must hold sizeof(MEMORYSTATUSEX).
    guest.memory.write_u32(status, 0).unwrap();
    assert_eq!(guest.call("GlobalMemoryStatusEx", &[status]), 0);
    assert_eq!(guest.error(), ERROR_INVALID_PARAMETER);

    let query = |guest: &mut Guest| {
        guest.memory.write_u32(status, 64).unwrap();
        assert_eq!(guest.call("GlobalMemoryStatusEx", &[status]), 1);
        let load = guest.u32_at(status + 4);
        let fields: [u64; 7] =
            std::array::from_fn(|index| guest.u64_at(status + 8 + 8 * index as u64));
        (load, fields)
    };
    const MIB: u64 = 1 << 20;
    let (
        load,
        [
            total,
            available,
            page_total,
            page_available,
            total_virtual,
            _,
            extended,
        ],
    ) = query(&mut guest);
    assert_eq!((load, total, available), (0, 128 * MIB, 128 * MIB));
    assert_eq!((page_total, page_available), (total, available));
    assert_eq!(total_virtual, 0x7fff_fffe_0000);
    assert_eq!(extended, 0);

    let region = guest.call("VirtualAlloc", &[0, MIB, 0x3000, 4]);
    assert_ne!(region, 0);
    let heap = guest.call("GetProcessHeap", &[]);
    assert_ne!(guest.call("HeapAlloc", &[heap, 0, 32 * MIB]), 0);
    let (load, [_, available, ..]) = query(&mut guest);
    assert_eq!(available, 128 * MIB - 33 * MIB);
    assert_eq!(load, 25); // 33 of 128 MiB in use, rounded down
}

#[test]
fn calendar_time_comes_from_the_injected_clock() {
    let clock = Arc::new(MockClock::new(filetime(NOV_24_2026, 789)));
    let state = ServicesState::new(clock.clone());
    let mut guest = Guest::with_state(IMPORTS, &[], state);
    let time = SCRATCH;
    for symbol in ["GetSystemTime", "GetLocalTime"] {
        assert_eq!(guest.call(symbol, &[time]), 0);
        assert_eq!(
            system_time(&guest, time),
            [2026, 11, 2, 24, 12, 34, 56, 789]
        );
    }
    let precise = SCRATCH + 0x20;
    assert_eq!(guest.call("GetSystemTimePreciseAsFileTime", &[precise]), 0);
    assert_eq!(guest.u64_at(precise), filetime(NOV_24_2026, 789));
    clock.advance(TICKS_PER_SECOND);
    assert_eq!(guest.call("GetSystemTimeAsFileTime", &[precise]), 0);
    assert_eq!(guest.u64_at(precise), filetime(NOV_24_2026 + 1, 789));
}

#[test]
fn filetime_and_system_time_convert_in_both_directions() {
    let mut guest = Guest::new(IMPORTS, &[]);
    let (input, output, back) = (SCRATCH, SCRATCH + 0x20, SCRATCH + 0x40);
    for (value, expected) in [
        (0, [1601, 1, 1, 1, 0, 0, 0, 0]),                   // a Monday
        (UNIX_EPOCH_FILETIME, [1970, 1, 4, 1, 0, 0, 0, 0]), // a Thursday
        (
            filetime(FEB_29_2024, 999),
            [2024, 2, 4, 29, 23, 59, 59, 999],
        ),
        (
            filetime(NOV_24_2026, 789),
            [2026, 11, 2, 24, 12, 34, 56, 789],
        ),
    ] {
        guest.memory.write_u64(input, value).unwrap();
        assert_eq!(guest.call("FileTimeToSystemTime", &[input, output]), 1);
        assert_eq!(system_time(&guest, output), expected);
        // The day of week is ignored on the way back.
        let mut fields = expected;
        fields[2] = 6;
        write_system_time(&mut guest, output, fields);
        assert_eq!(guest.call("SystemTimeToFileTime", &[output, back]), 1);
        assert_eq!(guest.u64_at(back), value - value % 10_000);
    }

    guest
        .memory
        .write_u64(input, 0x8000_0000_0000_0000)
        .unwrap();
    guest.memory.write_u64(output, u64::MAX).unwrap();
    assert_eq!(guest.call("FileTimeToSystemTime", &[input, output]), 0);
    assert_eq!(guest.error(), ERROR_INVALID_PARAMETER);
    assert_eq!(guest.u64_at(output), u64::MAX);

    for invalid in [
        [2023, 2, 0, 29, 0, 0, 0, 0], // not a leap year
        [1900, 2, 0, 29, 0, 0, 0, 0], // century, not a leap year
        [2026, 13, 0, 1, 0, 0, 0, 0],
        [2026, 4, 0, 31, 0, 0, 0, 0],
        [2026, 1, 0, 1, 24, 0, 0, 0],
        [2026, 1, 0, 1, 0, 0, 0, 1000],
        [1600, 12, 0, 31, 0, 0, 0, 0],
    ] {
        write_system_time(&mut guest, output, invalid);
        guest.memory.write_u64(back, 7).unwrap();
        assert_eq!(
            guest.call("SystemTimeToFileTime", &[output, back]),
            0,
            "{invalid:?}"
        );
        assert_eq!(guest.error(), ERROR_INVALID_PARAMETER);
        assert_eq!(guest.u64_at(back), 7);
    }
    write_system_time(&mut guest, output, [2000, 2, 0, 29, 0, 0, 0, 0]);
    assert_eq!(guest.call("SystemTimeToFileTime", &[output, back]), 1);
    write_system_time(&mut guest, output, [30827, 12, 0, 31, 23, 59, 59, 999]);
    assert_eq!(guest.call("SystemTimeToFileTime", &[output, back]), 1);
}

#[test]
fn sleeping_advances_the_injected_clock() {
    let clock = Arc::new(MockClock::new(0));
    let state = ServicesState::new(clock.clone());
    let mut guest = Guest::with_state(IMPORTS, &[], state);
    let interrupt = SCRATCH;
    assert_eq!(guest.call("GetTickCount64", &[]), 0);
    assert_eq!(guest.call("Sleep", &[250]), 0);
    assert_eq!(guest.call("GetTickCount64", &[]), 250);
    assert_eq!(guest.call("SleepEx", &[5, 1]), 0);
    assert_eq!(guest.call("Sleep", &[0]), 0);
    assert_eq!(guest.call("GetTickCount64", &[]), 255);
    assert_eq!(guest.call("QueryUnbiasedInterruptTime", &[interrupt]), 1);
    assert_eq!(guest.u64_at(interrupt), 2_550_000);
    // A single guest thread sleeping forever can never wake.
    assert_eq!(guest.fails("Sleep", &[u64::from(u32::MAX)]), RUNTIME_LIMIT);
    assert_eq!(guest.fails("SleepEx", &[60_000, 0]), RUNTIME_LIMIT);
    assert_eq!(guest.call("GetTickCount64", &[]), 255);
}

/// `IsProcessorFeaturePresent` must agree with what translated guest code sees
/// when it executes `CPUID`, bit for bit, per the CPUID feature flags in the
/// public Intel and AMD manuals and the `PF_*` numbers in winnt.h.
#[test]
fn processor_features_agree_with_guest_cpuid() {
    const FEATURES: &[(&str, &str)] = &[
        ("kernel32.dll", "IsProcessorFeaturePresent"),
        (
            "api-ms-win-core-processthreads-l1-1-0.dll",
            "IsProcessorFeaturePresent",
        ),
    ];
    // mov eax, leaf; xor ecx, ecx; cpuid; hlt
    let leaf = |n: u8| vec![0xB8, n, 0, 0, 0, 0x31, 0xC9, 0x0F, 0xA2, 0xF4];
    let mut guest = Guest::new(FEATURES, &[leaf(1), leaf(7)]);
    let registers = |guest: &mut Guest, index| {
        use xenora_loader::cpu::{ExitReason, Register};
        let (cpu, exit) = guest.execute(routine(index), 16);
        assert_eq!(exit, ExitReason::Halt);
        (
            cpu.get(Register::Rbx) as u32,
            cpu.get(Register::Rcx) as u32,
            cpu.get(Register::Rdx) as u32,
        )
    };
    let (_, ecx1, edx1) = registers(&mut guest, 0);
    let (ebx7, _, _) = registers(&mut guest, 1);
    let cases = [
        (2, edx1 & (1 << 8) != 0),
        (3, edx1 & (1 << 23) != 0),
        (6, edx1 & (1 << 25) != 0),
        (8, edx1 & (1 << 4) != 0),
        (10, edx1 & (1 << 26) != 0),
        (13, ecx1 & 1 != 0),
        (14, ecx1 & (1 << 13) != 0),
        (17, ecx1 & (3 << 26) == 3 << 26),
        (28, ecx1 & (1 << 30) != 0),
        (36, ecx1 & (1 << 9) != 0),
        (37, ecx1 & (1 << 19) != 0),
        (38, ecx1 & (1 << 20) != 0),
        (39, ecx1 & (1 << 28) != 0),
        (40, ebx7 & (1 << 5) != 0),
    ];
    for (feature, expected) in cases {
        let answer = guest.call("IsProcessorFeaturePresent", &[feature]);
        assert_eq!(answer, u64::from(expected), "PF {feature}");
    }
    // No CPUID test exists for these, so they are not claimed:
    // PF_NX_ENABLED (12), PF_FASTFAIL_AVAILABLE (23), and unknown numbers.
    for feature in [12, 23, 9999, u32::MAX as u64] {
        assert_eq!(guest.call("IsProcessorFeaturePresent", &[feature]), 0);
    }
}

/// `TIME_ZONE_INFORMATION` (172 bytes) and `TIME_ZONE_ID_UNKNOWN` per the
/// public GetTimeZoneInformation page: a zone without daylight saving.
#[test]
fn time_zone_is_utc_without_daylight_saving() {
    const ZONE: &[(&str, &str)] = &[
        (
            "api-ms-win-core-timezone-l1-1-0.dll",
            "GetTimeZoneInformation",
        ),
        ("kernel32.dll", "GetLastError"),
    ];
    let mut guest = Guest::new(ZONE, &[]);
    guest.memory.write_bytes(SCRATCH, &[0xCC; 176]).unwrap();
    assert_eq!(guest.call("GetTimeZoneInformation", &[SCRATCH]), 0);
    let name = |offset: u64| {
        let units: Vec<u16> = (0..32)
            .map(|i| guest.u16_at(SCRATCH + offset + 2 * i))
            .collect();
        String::from_utf16(&units[..units.iter().position(|&u| u == 0).unwrap()]).unwrap()
    };
    assert_eq!(name(4), "Coordinated Universal Time");
    assert_eq!(name(88), "Coordinated Universal Time");
    assert_eq!(guest.u32_at(SCRATCH), 0, "Bias");
    for (offset, length) in [(68, 20), (152, 20)] {
        for byte in 0..length {
            assert_eq!(guest.memory.read_u8(SCRATCH + offset + byte).unwrap(), 0);
        }
    }
    assert_eq!(
        guest.u32_at(SCRATCH + 172),
        0xCCCC_CCCC,
        "nothing past the struct"
    );
    assert_eq!(guest.call("GetTimeZoneInformation", &[0]), u32::MAX as u64);
    assert_eq!(guest.error(), 998);
}

/// SYSTEM_LOGICAL_PROCESSOR_INFORMATION entries are 32 bytes on x64; a short
/// buffer fails with ERROR_INSUFFICIENT_BUFFER and the required length.
#[test]
fn processor_topology_follows_the_guest_profile() {
    const TOPOLOGY: &[(&str, &str)] = &[
        (
            "api-ms-win-core-sysinfo-l1-1-0.dll",
            "GetLogicalProcessorInformation",
        ),
        ("kernel32.dll", "GetLastError"),
    ];
    let length = SCRATCH + 0x800;
    let mut state = ServicesState::new(Arc::new(MockClock::new(0)));
    state.guest_profile = GuestSystemProfile { processors: 2 };
    let mut guest = Guest::with_state(TOPOLOGY, &[], state);
    guest.memory.write_u32(length, 0).unwrap();
    assert_eq!(
        guest.call("GetLogicalProcessorInformation", &[0, length]),
        0
    );
    assert_eq!(guest.error(), 122);
    // Two cores and one package.
    assert_eq!(guest.u32_at(length), 3 * 32);
    assert_eq!(
        guest.call("GetLogicalProcessorInformation", &[SCRATCH, length]),
        1
    );
    let entry = |index: u64| {
        (
            guest.u64_at(SCRATCH + 32 * index),
            guest.u32_at(SCRATCH + 32 * index + 8),
        )
    };
    assert_eq!(entry(0), (0b01, 0), "RelationProcessorCore");
    assert_eq!(entry(1), (0b10, 0), "RelationProcessorCore");
    assert_eq!(entry(2), (0b11, 3), "RelationProcessorPackage");
    assert_eq!(
        guest.call("GetLogicalProcessorInformation", &[SCRATCH, 0]),
        0
    );
    assert_eq!(guest.error(), 998);
}
