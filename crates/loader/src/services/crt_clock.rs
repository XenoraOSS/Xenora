// SPDX-License-Identifier: GPL-3.0-or-later

//! The C time services of package 5 of #459.
//!
//! The arithmetic is exact: the civil calendar conversions are Howard
//! Hinnant's `days_from_civil` and `civil_from_days`, which are public
//! domain, re-derived here from his paper "chrono-Compatible Low-Level
//! Date Algorithms" rather than copied from any emulator.
//!
//! The one honest clock the emulator has is the injected [`Clock`], and
//! it reads UTC. What zone the machine stands in is not something Xenora
//! can answer, so every local-time service is the UTC one, and the docs
//! say so. `tm_isdst` is always 0, and `mktime` composes what `gmtime`
//! breaks down.

use super::crt::set_errno;
use super::crt_files::EINVAL;
use super::crt_streams::{c_string, fault};
use crate::cpu::ExitReason;
use crate::memory::{GuestMemory, PROCESS_HEAP};

/// Seconds in a day; the conversions count in days.
const SECONDS_PER_DAY: i64 = 86_400;

/// The last second `_gmtime64` accepts: 23:59:59, December 31, 3000 UTC,
/// per the Microsoft page.
const MAX_CRT_TIME: i64 = 32_535_215_999;

/// The nine fields of `struct tm`, in the order the CRT lays them out.
#[derive(Clone, Copy, Default)]
struct Tm {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
    isdst: i32,
}

impl Tm {
    fn read(memory: &GuestMemory, address: u64) -> Result<Self, crate::memory::MemoryError> {
        let mut fields = [0i32; 9];
        for (index, field) in fields.iter_mut().enumerate() {
            *field = memory.read_u32(address + 4 * index as u64)? as i32;
        }
        Ok(Self {
            sec: fields[0],
            min: fields[1],
            hour: fields[2],
            mday: fields[3],
            mon: fields[4],
            year: fields[5],
            wday: fields[6],
            yday: fields[7],
            isdst: fields[8],
        })
    }

    fn write(
        &self,
        memory: &mut GuestMemory,
        address: u64,
    ) -> Result<(), crate::memory::MemoryError> {
        for (index, field) in [
            self.sec, self.min, self.hour, self.mday, self.mon, self.year, self.wday, self.yday,
            self.isdst,
        ]
        .iter()
        .enumerate()
        {
            memory.write_u32(address + 4 * index as u64, *field as u32)?;
        }
        Ok(())
    }

    /// Seconds since the epoch, the inverse of `break_down`. Fields outside
    /// their usual range are meaningful: the CRT normalizes them, and the
    /// arithmetic below does too, because the month count is folded into
    /// the year before the days are counted.
    fn compose(&self) -> i64 {
        let year = i64::from(self.year) + 1900 + i64::from(self.mon).div_euclid(12);
        let month = i64::from(self.mon).rem_euclid(12) + 1;
        let days = days_from_civil(year, month, i64::from(self.mday));
        days * SECONDS_PER_DAY
            + i64::from(self.hour) * 3600
            + i64::from(self.min) * 60
            + i64::from(self.sec)
    }
}

/// Days since 1970-01-01 of `year-month-day` in the proleptic Gregorian
/// calendar. `month` counts from 1. Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// `(year, month, day)`, the inverse of `days_from_civil`. `month` counts
/// from 1. Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    // The year counts from March: the month index is 0 for March.
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

/// Breaks `time` down in UTC. `tm_isdst` is 0: the clock has no daylight
/// saving, as it has no zone.
fn break_down(time: i64) -> Tm {
    let days = time.div_euclid(SECONDS_PER_DAY);
    let seconds = time.rem_euclid(SECONDS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let yday = days - days_from_civil(year, 1, 1);
    Tm {
        sec: (seconds % 60) as i32,
        min: ((seconds / 60) % 60) as i32,
        hour: (seconds / 3600) as i32,
        mday: day as i32,
        mon: (month - 1) as i32,
        year: (year - 1900) as i32,
        // 1970-01-01 was a Thursday, the fourth day counting from Sunday.
        wday: (days + 4).rem_euclid(7) as i32,
        yday: yday as i32,
        isdst: 0,
    }
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const WEEKDAYS_LONG: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHS_LONG: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// The shape `asctime` writes: `Www Mmm dd hh:mm:ss yyyy` and a newline,
/// 24 characters, with the day of the month padded with a space.
fn ascii_time(tm: &Tm) -> Option<Vec<u8>> {
    let weekday = usize::try_from(tm.wday).ok().filter(|day| *day < 7)?;
    let month = usize::try_from(tm.mon).ok().filter(|month| *month < 12)?;
    let text = format!(
        "{} {} {:2} {:02}:{:02}:{:02} {}\n",
        WEEKDAYS[weekday],
        MONTHS[month],
        tm.mday,
        tm.hour,
        tm.min,
        tm.sec,
        tm.year + 1900,
    );
    let mut bytes = text.into_bytes();
    bytes.push(0);
    Some(bytes)
}

/// State the time services keep: the static storage the CRT shares between
/// calls. `tm` is one buffer for both `gmtime` and `localtime`, and the
/// string one for both `asctime` and `ctime`, exactly as the CRT reuses
/// them.
#[derive(Default)]
pub(crate) struct ClockServices {
    tm_static: u64,
    ascii_static: u64,
}

impl ClockServices {
    fn tm_static(&mut self, memory: &mut GuestMemory) -> Result<u64, crate::memory::MemoryError> {
        if self.tm_static == 0 {
            self.tm_static = memory.map_heap_allocation(PROCESS_HEAP, 36)?;
        }
        Ok(self.tm_static)
    }

    fn ascii_static(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<u64, crate::memory::MemoryError> {
        if self.ascii_static == 0 {
            self.ascii_static = memory.map_heap_allocation(PROCESS_HEAP, 26)?;
        }
        Ok(self.ascii_static)
    }

    /// `_gmtime64` and `gmtime`, and `_localtime64` and `localtime`: the
    /// same arithmetic here, as the machine's local time is UTC. Null or
    /// out-of-range times are `EINVAL` with a null return.
    pub(super) fn break_time(
        &mut self,
        memory: &mut GuestMemory,
        teb: u64,
        time_address: u64,
    ) -> Result<u64, ExitReason> {
        if time_address == 0 {
            let _ = set_errno(memory, teb, EINVAL);
            return Ok(0);
        }
        let time = memory.read_u64(time_address).map_err(fault)? as i64;
        if !(0..=MAX_CRT_TIME).contains(&time) {
            let _ = set_errno(memory, teb, EINVAL);
            return Ok(0);
        }
        let tm = break_down(time);
        let buffer = self.tm_static(memory).map_err(fault)?;
        tm.write(memory, buffer).map_err(fault)?;
        Ok(buffer)
    }

    /// `_mktime64` and `mktime`: the inverse. Years outside the CRT's
    /// 1970-to-3000 range fail with `EINVAL` and -1.
    pub(super) fn compose_time(
        &mut self,
        memory: &mut GuestMemory,
        teb: u64,
        tm_address: u64,
    ) -> Result<u64, ExitReason> {
        if tm_address == 0 {
            let _ = set_errno(memory, teb, EINVAL);
            return Ok(u64::MAX);
        }
        let tm = Tm::read(memory, tm_address).map_err(fault)?;
        let year = i64::from(tm.year) + 1900 + i64::from(tm.mon).div_euclid(12);
        if !(1970..=3000).contains(&year) {
            let _ = set_errno(memory, teb, EINVAL);
            return Ok(u64::MAX);
        }
        let time = tm.compose();
        if !(0..=MAX_CRT_TIME).contains(&time) {
            let _ = set_errno(memory, teb, EINVAL);
            return Ok(u64::MAX);
        }
        // The CRT normalizes the caller's fields and fills in the day of
        // the week and year.
        break_down(time).write(memory, tm_address).map_err(fault)?;
        Ok(time as u64)
    }

    /// `asctime` into the shared static buffer. Out-of-range fields are
    /// `EINVAL` with a null return, per the Microsoft page.
    pub(super) fn ascii_time(
        &mut self,
        memory: &mut GuestMemory,
        teb: u64,
        tm_address: u64,
    ) -> Result<u64, ExitReason> {
        if tm_address == 0 {
            let _ = set_errno(memory, teb, EINVAL);
            return Ok(0);
        }
        let tm = Tm::read(memory, tm_address).map_err(fault)?;
        // The Microsoft page fails out-of-range fields; the year itself is
        // formatted as the number it is.
        let valid = (0..=59).contains(&tm.sec)
            && (0..=59).contains(&tm.min)
            && (0..=23).contains(&tm.hour)
            && (1..=31).contains(&tm.mday)
            && (0..=11).contains(&tm.mon)
            && (0..=6).contains(&tm.wday)
            && (0..=365).contains(&tm.yday);
        if !valid {
            let _ = set_errno(memory, teb, EINVAL);
            return Ok(0);
        }
        let Some(bytes) = ascii_time(&tm) else {
            let _ = set_errno(memory, teb, EINVAL);
            return Ok(0);
        };
        let buffer = self.ascii_static(memory).map_err(fault)?;
        memory.write_bytes(buffer, &bytes).map_err(fault)?;
        Ok(buffer)
    }

    /// `_ctime64` and `ctime`: `asctime` of the broken-down time, in one
    /// call, sharing the same static buffer.
    pub(super) fn ascii_clock(
        &mut self,
        memory: &mut GuestMemory,
        teb: u64,
        time_address: u64,
    ) -> Result<u64, ExitReason> {
        if time_address == 0 {
            let _ = set_errno(memory, teb, EINVAL);
            return Ok(0);
        }
        let time = memory.read_u64(time_address).map_err(fault)? as i64;
        if !(0..=MAX_CRT_TIME).contains(&time) {
            let _ = set_errno(memory, teb, EINVAL);
            return Ok(0);
        }
        let tm = break_down(time);
        let buffer = self.tm_static(memory).map_err(fault)?;
        tm.write(memory, buffer).map_err(fault)?;
        self.ascii_time(memory, teb, buffer)
    }

    /// `asctime_s`: the checked form. Errors empty the buffer's first byte
    /// and return the error number, which is also the errno.
    pub(super) fn ascii_time_s(
        &mut self,
        memory: &mut GuestMemory,
        teb: u64,
        buffer: u64,
        size: u64,
        tm_address: u64,
    ) -> Result<u64, ExitReason> {
        let fail = |memory: &mut GuestMemory, teb: u64| {
            if buffer != 0 && size > 0 {
                let _ = memory.write_bytes(buffer, &[0]);
            }
            let _ = set_errno(memory, teb, EINVAL);
            u64::from(EINVAL)
        };
        if buffer == 0 || size < 26 || tm_address == 0 {
            return Ok(fail(memory, teb));
        }
        let tm = Tm::read(memory, tm_address).map_err(fault)?;
        let Some(bytes) = ascii_time(&tm) else {
            return Ok(fail(memory, teb));
        };
        memory.write_bytes(buffer, &bytes).map_err(fault)?;
        Ok(0)
    }

    /// `_ctime64_s` and `ctime_s`.
    pub(super) fn ascii_clock_s(
        &mut self,
        memory: &mut GuestMemory,
        teb: u64,
        buffer: u64,
        size: u64,
        time_address: u64,
    ) -> Result<u64, ExitReason> {
        if time_address == 0 {
            let _ = set_errno(memory, teb, EINVAL);
            return Ok(u64::from(EINVAL));
        }
        let time = memory.read_u64(time_address).map_err(fault)? as i64;
        if !(0..=MAX_CRT_TIME).contains(&time) {
            let _ = set_errno(memory, teb, EINVAL);
            return Ok(u64::from(EINVAL));
        }
        let tm = break_down(time);
        let scratch = self.tm_static(memory).map_err(fault)?;
        tm.write(memory, scratch).map_err(fault)?;
        self.ascii_time_s(memory, teb, buffer, size, scratch)
    }

    /// `strftime` for the C locale, the only locale the emulator has. The
    /// conversions are the ones the Microsoft page documents; an unknown
    /// conversion is copied verbatim, as the CRT does for the characters it
    /// does not know. Returns the length without the terminator, or 0 when
    /// the result does not fit — the contents are then indeterminate, which
    /// here means empty.
    pub(super) fn format_time(
        &mut self,
        memory: &mut GuestMemory,
        teb: u64,
        out: u64,
        size: u64,
        format: u64,
        tm_address: u64,
    ) -> Result<u64, ExitReason> {
        if out == 0 || format == 0 || tm_address == 0 {
            let _ = set_errno(memory, teb, EINVAL);
            return Ok(0);
        }
        let tm = Tm::read(memory, tm_address).map_err(fault)?;
        if !(0..=59).contains(&tm.sec)
            || !(0..=59).contains(&tm.min)
            || !(0..=23).contains(&tm.hour)
            || !(1..=31).contains(&tm.mday)
            || !(0..=11).contains(&tm.mon)
            || !(0..=6).contains(&tm.wday)
            || !(0..=365).contains(&tm.yday)
        {
            let _ = set_errno(memory, teb, EINVAL);
            return Ok(0);
        }
        let format_bytes = c_string(memory, format, 65_536).map_err(fault)?;
        let text = render_strftime(&format_bytes, &tm);
        if text.len() + 1 > size as usize {
            return Ok(0);
        }
        let mut bytes = text.into_bytes();
        bytes.push(0);
        memory.write_bytes(out, &bytes).map_err(fault)?;
        Ok((bytes.len() - 1) as u64)
    }
}

/// Renders the strftime conversions. Separated from the service so the
/// table stays readable.
fn render_strftime(format: &[u8], tm: &Tm) -> String {
    let mut out = String::new();
    let mut index = 0;
    let weekday = usize::try_from(tm.wday).unwrap_or(0).min(6);
    let month = usize::try_from(tm.mon).unwrap_or(0).min(11);
    while index < format.len() {
        if format[index] != b'%' {
            out.push(format[index] as char);
            index += 1;
            continue;
        }
        index += 1;
        if index >= format.len() {
            break;
        }
        let conversion = format[index];
        index += 1;
        let year = tm.year + 1900;
        match conversion {
            b'a' => out.push_str(WEEKDAYS[weekday]),
            b'A' => out.push_str(WEEKDAYS_LONG[weekday]),
            b'b' | b'h' => out.push_str(MONTHS[month]),
            b'B' => out.push_str(MONTHS_LONG[month]),
            b'c' => out.push_str(&format!(
                "{} {} {:2} {:02}:{:02}:{:02} {}",
                WEEKDAYS[weekday], MONTHS[month], tm.mday, tm.hour, tm.min, tm.sec, year
            )),
            b'd' => out.push_str(&format!("{:02}", tm.mday)),
            b'e' => out.push_str(&format!("{:2}", tm.mday)),
            b'H' => out.push_str(&format!("{:02}", tm.hour)),
            b'I' => {
                let hour = if tm.hour % 12 == 0 { 12 } else { tm.hour % 12 };
                out.push_str(&format!("{hour:02}"));
            }
            b'j' => out.push_str(&format!("{:03}", tm.yday + 1)),
            b'm' => out.push_str(&format!("{:02}", tm.mon + 1)),
            b'M' => out.push_str(&format!("{:02}", tm.min)),
            b'p' => out.push_str(if tm.hour < 12 { "AM" } else { "PM" }),
            b'S' => out.push_str(&format!("{:02}", tm.sec)),
            b'U' => out.push_str(&format!("{:02}", week_of_year(tm, 0))),
            b'w' => out.push_str(&format!("{weekday}")),
            b'W' => out.push_str(&format!("{:02}", week_of_year(tm, 1))),
            b'x' => out.push_str(&format!(
                "{:02}/{:02}/{:02}",
                tm.mon + 1,
                tm.mday,
                year % 100
            )),
            b'X' => out.push_str(&format!("{:02}:{:02}:{:02}", tm.hour, tm.min, tm.sec)),
            b'y' => out.push_str(&format!("{:02}", year % 100)),
            b'Y' => out.push_str(&format!("{year}")),
            b'z' => out.push_str("+0000"),
            b'Z' => out.push_str("UTC"),
            b'%' => out.push('%'),
            other => {
                out.push('%');
                out.push(other as char);
            }
        }
    }
    out
}

/// The week number for `%U` (first Sunday starts week 1) or `%W` (first
/// Monday). `first` is the weekday that starts a week: 0 or 1.
fn week_of_year(tm: &Tm, first: i32) -> i32 {
    // Days before the first `first`-weekday of the year count as week 0.
    let offset = (tm.wday - tm.yday).rem_euclid(7);
    let first_of_year = if first == 0 {
        // Days up to and including the first Sunday.
        (7 - offset) % 7
    } else {
        (8 - offset) % 7
    };
    if tm.yday < first_of_year {
        0
    } else {
        1 + (tm.yday - first_of_year) / 7
    }
}

impl crate::services::ServicesState {
    pub(crate) fn crt_clock(&self) -> std::sync::MutexGuard<'_, ClockServices> {
        self.crt_clock.lock().expect("CRT clock state")
    }
}
