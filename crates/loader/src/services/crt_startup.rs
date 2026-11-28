// SPDX-License-Identifier: GPL-3.0-or-later

//! What a C program's startup code configures in the C runtime before `main`.
//!
//! The explicit invalid-parameter entry points consult the recorded handler.
//! Most other startup settings only record values and remain partial.

use crate::cpu::ExitReason;
use crate::memory::{GuestMemory, PROCESS_HEAP};
use std::collections::BTreeMap;

/// `_O_TEXT` (fcntl.h), the documented initial value of `_fmode`.
const O_TEXT: u32 = 0x4000;
/// `_ENABLE_PER_THREAD_LOCALE` and `_DISABLE_PER_THREAD_LOCALE` (locale.h).
const ENABLE_PER_THREAD_LOCALE: i32 = 1;
const DISABLE_PER_THREAD_LOCALE: i32 = 2;
const SIGILL: i32 = 4;
const SIGFPE: i32 = 8;
const SIGSEGV: i32 = 11;
const SIG_DFL: u64 = 0;
const SIG_IGN: u64 = 1;
/// The signals the documentation of `signal` lists, by their signal.h values:
/// SIGINT, SIGILL, SIGFPE, SIGSEGV, SIGTERM and SIGABRT.
const SIGNALS: [i32; 6] = [2, SIGILL, SIGFPE, SIGSEGV, 15, 22];

const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
const STATUS_ILLEGAL_INSTRUCTION: u32 = 0xC000_001D;
const STATUS_PRIVILEGED_INSTRUCTION: u32 = 0xC000_0096;
const STATUS_FLOAT_DENORMAL_OPERAND: u32 = 0xC000_008D;
const STATUS_FLOAT_UNDERFLOW: u32 = 0xC000_0093;

pub(super) enum ExceptionAction {
    Search,
    Resume,
    Call { handler: u64, signal: i32 },
    Unsupported,
}

#[derive(Default)]
pub(super) struct CrtStartup {
    app_type: i32,
    /// Whether `malloc` would call the new handler on failure. It never does.
    new_mode: i32,
    per_thread_locale: bool,
    user_math_error: u64,
    invalid_parameter_handler: u64,
    /// Handlers other than the default, `SIG_DFL`, which is zero.
    signals: BTreeMap<i32, u64>,
    /// Guest address of `_fmode`. `_commode` is the `int` after it.
    file_modes: Option<u64>,
    /// Guest address of the legacy `_timezone` cell.
    timezone: Option<u64>,
}

impl CrtStartup {
    pub(super) fn app_type(&self) -> i32 {
        self.app_type
    }

    pub(super) fn set_app_type(&mut self, kind: i32) {
        self.app_type = kind;
    }

    /// The previous mode, or `None` for a mode that is neither 0 nor 1.
    pub(super) fn set_new_mode(&mut self, mode: i32) -> Option<i32> {
        matches!(mode, 0 | 1).then(|| std::mem::replace(&mut self.new_mode, mode))
    }

    /// The setting before the call, or `None` for a request that is not 0
    /// (query), enable or disable. A new process uses the global locale.
    pub(super) fn configure_thread_locale(&mut self, request: i32) -> Option<i32> {
        let before = if self.per_thread_locale {
            ENABLE_PER_THREAD_LOCALE
        } else {
            DISABLE_PER_THREAD_LOCALE
        };
        match request {
            0 => {}
            ENABLE_PER_THREAD_LOCALE => self.per_thread_locale = true,
            DISABLE_PER_THREAD_LOCALE => self.per_thread_locale = false,
            _ => return None,
        }
        Some(before)
    }

    pub(super) fn user_math_error(&self) -> u64 {
        self.user_math_error
    }

    pub(super) fn set_user_math_error(&mut self, handler: u64) {
        self.user_math_error = handler;
    }

    pub(super) fn set_invalid_parameter_handler(&mut self, handler: u64) -> u64 {
        std::mem::replace(&mut self.invalid_parameter_handler, handler)
    }

    pub(super) fn invalid_parameter_handler(&self) -> u64 {
        self.invalid_parameter_handler
    }

    /// The previous handler, or `None` for a signal the documentation does not
    /// list. One table serves the whole process.
    pub(super) fn signal(&mut self, signal: i32, handler: u64) -> Option<u64> {
        SIGNALS
            .contains(&signal)
            .then(|| self.signals.insert(signal, handler).unwrap_or(SIG_DFL))
    }

    /// Resolve the documented EXE filter mapping and consume a callable
    /// non-floating-point disposition before guest code can reinstall it.
    pub(super) fn exception_action(&mut self, code: u32) -> ExceptionAction {
        let signal = match code {
            STATUS_ACCESS_VIOLATION => SIGSEGV,
            STATUS_ILLEGAL_INSTRUCTION | STATUS_PRIVILEGED_INSTRUCTION => SIGILL,
            STATUS_FLOAT_DENORMAL_OPERAND..=STATUS_FLOAT_UNDERFLOW => SIGFPE,
            _ => return ExceptionAction::Search,
        };
        match self.signals.get(&signal).copied().unwrap_or(SIG_DFL) {
            SIG_DFL => ExceptionAction::Search,
            _ if signal == SIGFPE => ExceptionAction::Unsupported,
            SIG_IGN => ExceptionAction::Resume,
            handler => {
                self.signals.remove(&signal);
                ExceptionAction::Call { handler, signal }
            }
        }
    }

    /// Guest address of `_fmode` (`index` 0) or `_commode` (`index` 1). Both
    /// are allocated together from the process heap on first use. `_fmode`
    /// starts as `_O_TEXT`, as documented. `_commode` starts as zero: its
    /// documentation gives no initial value, and zero is "no commit".
    pub(super) fn file_mode_address(
        &mut self,
        memory: &mut GuestMemory,
        index: u64,
    ) -> Result<u64, ExitReason> {
        let base = match self.file_modes {
            Some(base) => base,
            None => {
                let base = memory
                    .map_heap_allocation(PROCESS_HEAP, 8)
                    .and_then(|base| memory.write_u32(base, O_TEXT).map(|()| base))
                    .map_err(ExitReason::MemoryFault)?;
                self.file_modes = Some(base);
                base
            }
        };
        Ok(base + 4 * index)
    }

    /// Guest address of `_timezone`, allocated from the process heap on first
    /// use. The legacy variable is the seconds west of UTC of the host time
    /// zone; the runtime clock is documented UTC, so the cell stays zero.
    pub(super) fn timezone_address(&mut self, memory: &mut GuestMemory) -> Result<u64, ExitReason> {
        match self.timezone {
            Some(base) => Ok(base),
            None => {
                let base = memory
                    .map_heap_allocation(PROCESS_HEAP, 8)
                    .map_err(ExitReason::MemoryFault)?;
                self.timezone = Some(base);
                Ok(base)
            }
        }
    }
}
