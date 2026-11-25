// SPDX-License-Identifier: GPL-3.0-or-later
//
//! String comparison, classification and case mapping for the runtime's one
//! locale, English (United States). This is package 7, third part, of #459.
//!
//! What is implemented, and the honest scope:
//!
//! - `lstrlenA`/`lstrlenW`, `lstrcpyA`, `lstrcmpA` and `lstrcmpiA` are the
//!   simple byte and wide-unit operations their pages describe. The narrow
//!   forms read bytes; a program running with Xenora's UTF-8 process code
//!   page that passes non-ASCII text gets the byte-wise answer, which is
//!   documented here rather than hidden.
//! - `lstrcmpiA` folds case with the ASCII letters only. Windows uses the
//!   locale's case tables; for en-US every ASCII letter folds the same way,
//!   so the difference exists only for non-ASCII input, which no fixture
//!   uses yet.
//! - `CompareStringA`/`CompareStringW` compare in byte order with the
//!   ASCII-only fold when `NORM_IGNORECASE` is passed. Real Windows en-US
//!   comparison is linguistic: punctuation and case rank differently. No
//!   fixture depends on that yet, so Xenora documents the simplification
//!   instead of pretending to sort like Windows. `NORM_IGNORESYMBOLS` and
//!   the other modifier flags are refused with `ERROR_INVALID_PARAMETER`
//!   rather than silently ignored.
//! - `GetStringTypeW` answers `CT_CTYPE1` classification for ASCII input
//!   with the flags the winnls header defines; `CT_CTYPE2` and `CT_CTYPE3`
//!   are refused rather than answered wrong. Wide units above ASCII are
//!   classified zero except space-like controls, documented.
//! - `LCMapStringW` maps ASCII case for `LCMAP_UPPERCASE`/`LCMAP_LOWERCASE`.
//!   `LCMAP_SORTKEY` and the byte reversal are refused. `-1` source counts
//!   include the terminator in both input processing and output, as the
//!   page documents.

/// `CT_CTYPE1` flags from winnls.h.
pub(super) const C1_UPPER: u16 = 0x0001;
pub(super) const C1_LOWER: u16 = 0x0002;
pub(super) const C1_DIGIT: u16 = 0x0004;
pub(super) const C1_SPACE: u16 = 0x0008;
pub(super) const C1_PUNCT: u16 = 0x0010;
pub(super) const C1_CNTRL: u16 = 0x0020;
pub(super) const C1_BLANK: u16 = 0x0040;
pub(super) const C1_XDIGIT: u16 = 0x0080;
pub(super) const C1_ALPHA: u16 = 0x0100;

/// Classifies one wide unit for `CT_CTYPE1`. The ASCII rows are exactly the
/// en-US answers; units above ASCII get only what is true of them in every
/// locale (nothing), which the module documents rather than invents.
pub(super) fn ctype1(unit: u16) -> u16 {
    let byte = match u8::try_from(unit) {
        Ok(byte) => byte,
        Err(_) => return 0,
    };
    let xdigit = if byte.is_ascii_hexdigit() {
        C1_XDIGIT
    } else {
        0
    };
    match byte {
        b'A'..=b'Z' => C1_UPPER | C1_ALPHA | xdigit,
        b'a'..=b'z' => C1_LOWER | C1_ALPHA | xdigit,
        b'0'..=b'9' => C1_DIGIT | C1_XDIGIT,
        b' ' | b'\t' => C1_SPACE | C1_BLANK,
        b'\n' | b'\r' | 0x0b | 0x0c => C1_SPACE | C1_CNTRL,
        0x00..=0x1f | 0x7f => C1_CNTRL,
        _ => C1_PUNCT,
    }
}

/// ASCII letter fold shared by the case-insensitive comparisons.
pub(super) fn fold(byte: u8) -> u8 {
    byte.to_ascii_lowercase()
}

/// Ordinal comparison of two byte strings, optionally ASCII case-folded.
/// Returns negative, zero or positive like the pages document.
pub(super) fn compare_bytes(left: &[u8], right: &[u8], ignore_case: bool) -> i32 {
    for (a, b) in left.iter().zip(right.iter()) {
        let (a, b) = if ignore_case {
            (fold(*a), fold(*b))
        } else {
            (*a, *b)
        };
        if a != b {
            return if a < b { -1 } else { 1 };
        }
    }
    (left.len() as i64 - right.len() as i64).signum() as i32
}
