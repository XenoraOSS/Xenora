// SPDX-License-Identifier: GPL-3.0-or-later
//
//! The console identity and the standard handles, package 8 first part of
//! #459. What is implemented, and the honest scope:
//!
//! - `GetStdHandle` answers one fixed pseudo-handle per standard device.
//!   They are Xenora's choice: Windows hands out small values that vary
//!   with the machine state, and nothing here depends on the exact number.
//! - `SetStdHandle` stores the replacement, and `GetStdHandle` then answers
//!   it, as documented. A write or read through a handle that is not one of
//!   the console devices fails with `ERROR_INVALID_HANDLE` - the runtime
//!   has no file handles to redirect to yet, so redirection can only point
//!   at another console device or break, which is the honest model until
//!   the Win32 file layer exists.
//! - `GetConsoleMode`/`SetConsoleMode` round-trip mode bits per handle.
//!   The modes do not alter the capture behavior yet; that is documented
//!   here rather than hidden, and the round-trip itself is the implemented
//!   semantics.
//! - `GetConsoleCP`/`GetConsoleOutputCP` answer UTF-8 (65001), coherent
//!   with the process code page choice documented in `codepages`.
//! - `WriteConsoleW` converts its wide text to UTF-8 and appends it to the
//!   same capture `stdout`/`stderr` use, so a program mixing `printf` and
//!   `WriteConsole` produces one coherent output. `WriteConsoleA` appends
//!   its bytes as they are: the console code page is UTF-8, so no
//!   conversion applies. Both report the characters consumed.
//! - `ReadConsoleA` reads one line of the captured standard input. The
//!   input was captured with CR LF replaced by LF (the CRT text mode), and
//!   the console line input hands the line ending back as CR LF, as the
//!   page documents. Reading without a line feed in the rest of the input
//!   ends at the end of the input.

/// Xenora's fixed pseudo-handles for the three standard devices.
pub(super) const INPUT_HANDLE: u64 = 0x101;
pub(super) const OUTPUT_HANDLE: u64 = 0x102;
pub(super) const ERROR_HANDLE: u64 = 0x103;

pub(super) const STD_INPUT: u32 = 0xFFFF_FFF6; // -10
pub(super) const STD_OUTPUT: u32 = 0xFFFF_FFF5; // -11
pub(super) const STD_ERROR: u32 = 0xFFFF_FFF4; // -12

/// The default modes: line input with echo and processing, processed output
/// with wrap. They round-trip through Set but alter nothing yet.
pub(super) const DEFAULT_INPUT_MODE: u32 = 0x1 | 0x2 | 0x4;
pub(super) const DEFAULT_OUTPUT_MODE: u32 = 0x1 | 0x2;
