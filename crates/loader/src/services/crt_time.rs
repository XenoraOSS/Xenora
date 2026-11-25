// SPDX-License-Identifier: GPL-3.0-or-later

//! CRT calendar time against the injectable guest clock.
//!
//! `time` / `_time64` report Unix seconds derived from the same FILETIME clock
//! as `GetSystemTimeAsFileTime`. `gmtime_s` / `localtime_s` and their 64-bit
//! forms fill a Windows `tm` in UTC; there is no guest timezone database, so
//! local time matches UTC and `tm_isdst` is always zero.

use super::ServicesState;
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError};
use std::sync::Arc;

const UCRT: &str = "ucrtbase.dll";
const CRT_TIME_API: &str = "api-ms-win-crt-time-l1-1-0.dll";
const EINVAL: u32 = 22;
const FILETIME_TICKS_PER_SEC: u64 = 10_000_000;
/// Seconds from 1601-01-01 to 1970-01-01.
const SECS_1601_TO_1970: u64 = 11_644_473_600;

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

fn set_errno(memory: &mut GuestMemory, teb: u64, error: u32) -> Result<(), ExitReason> {
    let address = memory.crt_errno_address(teb).map_err(fault)?;
    memory.write_u32(address, error).map_err(fault)
}

fn unix_from_filetime(filetime: u64) -> Option<i64> {
    let secs = filetime / FILETIME_TICKS_PER_SEC;
    if secs < SECS_1601_TO_1970 {
        return Some(0);
    }
    i64::try_from(secs - SECS_1601_TO_1970).ok()
}

/// Proleptic Gregorian date for a day count relative to 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u16, u16) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month as u16, day as u16)
}

fn day_of_year(year: i64, month: u16, day: u16) -> i32 {
    const CUMULATIVE: [i32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let extra = i32::from(leap && month > 2);
    CUMULATIVE[month as usize - 1] + i32::from(day) - 1 + extra
}

/// Windows `struct tm` field order (nine 32-bit ints).
fn write_tm(memory: &mut GuestMemory, dest: u64, unix: i64) -> Result<(), ExitReason> {
    let days = unix.div_euclid(86_400);
    let of_day = unix.rem_euclid(86_400) as i32;
    let (year, month, day) = civil_from_days(days);
    let fields = [
        of_day % 60,                       // tm_sec
        (of_day / 60) % 60,                // tm_min
        of_day / 3600,                     // tm_hour
        i32::from(day),                    // tm_mday
        i32::from(month) - 1,              // tm_mon
        (year - 1900) as i32,              // tm_year
        ((days + 4).rem_euclid(7)) as i32, // tm_wday (1970-01-01 was Thursday)
        day_of_year(year, month, day),     // tm_yday
        0,                                 // tm_isdst
    ];
    for (index, field) in fields.into_iter().enumerate() {
        memory
            .write_u32(dest + index as u64 * 4, field as u32)
            .map_err(fault)?;
    }
    Ok(())
}

fn store_time(memory: &mut GuestMemory, dest: u64, value: i64) -> Result<(), ExitReason> {
    if dest == 0 {
        return Ok(());
    }
    memory.write_u64(dest, value as u64).map_err(fault)
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let time64 = Arc::clone(state);
    registry.register(UCRT, "_time64", move |args, _, memory| {
        let now = unix_from_filetime(time64.clock.now_filetime()).unwrap_or(0);
        store_time(memory, args.rcx, now)?;
        Ok(now as u64)
    });

    let time32 = Arc::clone(state);
    registry.register(UCRT, "time", move |args, _, memory| {
        let now = unix_from_filetime(time32.clock.now_filetime()).unwrap_or(0);
        // 32-bit `time_t` saturates rather than wrapping past 2038.
        let clipped = now.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        if args.rcx != 0 {
            memory.write_u32(args.rcx, clipped as u32).map_err(fault)?;
        }
        Ok(u64::from(clipped as u32))
    });

    for (symbol, wide) in [
        ("gmtime_s", false),
        ("localtime_s", false),
        ("_gmtime64_s", true),
        ("_localtime64_s", true),
    ] {
        let clock = Arc::clone(state);
        registry.register(UCRT, symbol, move |args, cpu, memory| {
            let (tm, time_ptr) = (args.rcx, args.rdx);
            if tm == 0 || time_ptr == 0 {
                set_errno(memory, cpu.gs_base, EINVAL)?;
                return Ok(u64::from(EINVAL));
            }
            let unix = if wide {
                memory.read_u64(time_ptr).map_err(fault)? as i64
            } else {
                memory.read_u32(time_ptr).map_err(fault)? as i32 as i64
            };
            let _ = clock; // timezone-free: local == UTC
            write_tm(memory, tm, unix)?;
            Ok(0)
        });
    }

    for symbol in [
        "time",
        "_time64",
        "gmtime_s",
        "localtime_s",
        "_gmtime64_s",
        "_localtime64_s",
    ] {
        registry.alias(CRT_TIME_API, UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{CpuState, Register};
    use crate::import_table::ImportName;
    use crate::imports::Win64Args;
    use crate::memory::PROCESS_HEAP;
    use crate::services::MockClock;

    fn call(
        registry: &ImportRegistry,
        symbol: &str,
        args: [u64; 4],
        cpu: &mut CpuState,
        memory: &mut GuestMemory,
    ) -> u64 {
        cpu.set(Register::Rcx, args[0]);
        cpu.set(Register::Rdx, args[1]);
        cpu.set(Register::R8, args[2]);
        cpu.set(Register::R9, args[3]);
        let service = registry
            .resolve(UCRT, &ImportName::Name(symbol.into()))
            .unwrap();
        (service.handler)(&Win64Args::new(cpu), cpu, memory).unwrap()
    }

    #[test]
    fn time64_and_gmtime_s_match_unix_epoch() {
        // 1970-01-01 00:00:00 UTC as FILETIME.
        let filetime = SECS_1601_TO_1970 * FILETIME_TICKS_PER_SEC;
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(filetime))));
        let mut registry = ImportRegistry::new();
        register(&mut registry, &state);
        let mut memory = GuestMemory::new();
        let mut cpu = CpuState {
            gs_base: 0x10000,
            ..CpuState::default()
        };
        let slot = memory.map_heap_allocation(PROCESS_HEAP, 8).unwrap();
        assert_eq!(
            call(&registry, "_time64", [slot, 0, 0, 0], &mut cpu, &mut memory),
            0
        );
        assert_eq!(memory.read_u64(slot).unwrap(), 0);
        let tm = memory.map_heap_allocation(PROCESS_HEAP, 36).unwrap();
        assert_eq!(
            call(
                &registry,
                "_gmtime64_s",
                [tm, slot, 0, 0],
                &mut cpu,
                &mut memory
            ),
            0
        );
        assert_eq!(memory.read_u32(tm).unwrap(), 0); // sec
        assert_eq!(memory.read_u32(tm + 12).unwrap(), 1); // mday
        assert_eq!(memory.read_u32(tm + 16).unwrap(), 0); // mon
        assert_eq!(memory.read_u32(tm + 20).unwrap(), 70); // year
        assert_eq!(memory.read_u32(tm + 24).unwrap(), 4); // Thursday
    }
}
