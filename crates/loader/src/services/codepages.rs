// SPDX-License-Identifier: GPL-3.0-or-later

//! The Windows encoding conversions: `MultiByteToWideChar` and
//! `WideCharToMultiByte` for the pages Xenora serves.
//!
//! Three code pages exist here, each a documented public standard:
//!
//! - UTF-8 (65001), converted exactly: every sequence of one to four bytes
//!   and its scalar value, the malformed sequences replaced with U+FFFD — or
//!   refused when the caller passes `MB_ERR_INVALID_CHARS`, as Microsoft's
//!   page describes.
//! - Windows-1252, the Western ANSI page, available by its number: bytes
//!   below 128 are ASCII, bytes 160 to 255 are Latin-1, and the 27 entries
//!   between them are the documented punctuation of the page.
//! - IBM 437, the OEM page Xenora answers for `CP_OEMCP`: the original PC
//!   character set, whose upper half is documented public data. Xenora's
//!   choice: the OEM page is always 437, as on a Western installation.
//!
//! `CP_ACP` resolves to UTF-8, not to 1252: `ansi_page` says why.

/// The Windows error values these conversions set.
pub(super) const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
pub(super) const ERROR_INVALID_FLAGS: u32 = 1004;
pub(super) const ERROR_INVALID_PARAMETER: u32 = 87;
pub(super) const ERROR_NO_UNICODE_TRANSLATION: u32 = 1113;

/// The code pages by their numbers.
pub(super) const CP_UTF8: u32 = 65001;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CodePage {
    Utf8,
    Cp1252,
    Cp437,
}

/// Resolve the `CodePage` argument: `CP_ACP` and `CP_OEMCP` name the
/// runtime's ANSI and OEM pages, and a number itself is one when Xenora
/// serves it.
pub(super) fn resolve(number: u32) -> Option<CodePage> {
    match number {
        0 => Some(CodePage::Utf8),
        1 | 437 => Some(CodePage::Cp437),
        1252 => Some(CodePage::Cp1252),
        CP_UTF8 => Some(CodePage::Utf8),
        _ => None,
    }
}

/// What `GetACP` answers. Xenora's choice: UTF-8, the value a Windows
/// installation in its documented UTF-8 mode answers, because the process
/// arguments of `docs/interfaces.md` are already UTF-8 — a program decoding
/// them with the ANSI page must agree with what it was handed. Windows-1252
/// stays available by its own number.
pub(super) fn ansi_page() -> u32 {
    CP_UTF8
}

/// What `GetOEMCP` answers: the OEM page of this runtime.
pub(super) fn oem_page() -> u32 {
    437
}

/// The replacements of the malformed input: U+FFFD, the replacement
/// character Windows writes when the caller does not ask for the error.
const REPLACEMENT: u16 = 0xFFFD;

/// The Windows-1252 upper entries, for bytes 0x80 to 0x9F. A zero is a byte
/// the page leaves undefined: input with it is malformed.
const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0, 0x017D, 0, 0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC,
    0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
];

/// The IBM 437 upper half, for bytes 0x80 to 0xFF: the documented PC
/// character set.
const CP437_HIGH: [u16; 128] = [
    0x00C7, 0x00FC, 0x00E9, 0x00E2, 0x00E4, 0x00E0, 0x00E5, 0x00E7, 0x00EA, 0x00EB, 0x00E8, 0x00EF,
    0x00EE, 0x00EC, 0x00C4, 0x00C5, 0x00C9, 0x00E6, 0x00C6, 0x00F4, 0x00F6, 0x00F2, 0x00FB, 0x00F9,
    0x00FF, 0x00D6, 0x00DC, 0x00A2, 0x00A3, 0x00A5, 0x20A7, 0x0192, 0x00E1, 0x00ED, 0x00F3, 0x00FA,
    0x00F1, 0x00D1, 0x00AA, 0x00BA, 0x00BF, 0x2310, 0x00AC, 0x00BD, 0x00BC, 0x00A1, 0x00AB, 0x00BB,
    0x2591, 0x2592, 0x2593, 0x2502, 0x2524, 0x2561, 0x2562, 0x2556, 0x2555, 0x2563, 0x2551, 0x2557,
    0x255D, 0x255C, 0x255B, 0x2510, 0x2514, 0x2534, 0x252C, 0x251C, 0x2500, 0x253C, 0x255E, 0x255F,
    0x255A, 0x2554, 0x2569, 0x2566, 0x2560, 0x2550, 0x256C, 0x2567, 0x2568, 0x2564, 0x2565, 0x2559,
    0x2558, 0x2552, 0x2553, 0x256B, 0x256A, 0x2518, 0x250C, 0x2588, 0x2584, 0x258C, 0x2590, 0x2580,
    0x03B1, 0x00DF, 0x0393, 0x03C0, 0x03A3, 0x03C3, 0x00B5, 0x03C4, 0x03A6, 0x0398, 0x03A9, 0x03B4,
    0x221E, 0x03C6, 0x03B5, 0x2229, 0x2261, 0x00B1, 0x2265, 0x2264, 0x2320, 0x2321, 0x00F7, 0x2248,
    0x00B0, 0x2219, 0x00B7, 0x221A, 0x207F, 0x00B2, 0x25A0, 0x00A0,
];

/// One byte of a single-byte page as its wide character, or none when the
/// byte is one the page leaves undefined.
fn single_byte_unit(page: CodePage, byte: u8) -> Option<u16> {
    if byte < 0x80 {
        return Some(u16::from(byte));
    }
    match page {
        CodePage::Cp1252 => {
            if byte >= 0xA0 {
                Some(u16::from(byte))
            } else {
                match CP1252_HIGH[usize::from(byte - 0x80)] {
                    0 => None,
                    unit => Some(unit),
                }
            }
        }
        CodePage::Cp437 => Some(CP437_HIGH[usize::from(byte - 0x80)]),
        CodePage::Utf8 => unreachable!(),
    }
}

/// One wide character as the bytes of a single-byte page, or none when the
/// page cannot represent it.
fn unit_single_byte(page: CodePage, unit: u16) -> Option<u8> {
    if unit < 0x80 {
        return Some(unit as u8);
    }
    match page {
        CodePage::Cp1252 => {
            if (0xA0..=0xFF).contains(&unit) {
                return Some(unit as u8);
            }
            for (index, &high) in CP1252_HIGH.iter().enumerate() {
                if high == unit && high != 0 {
                    return Some(0x80 + index as u8);
                }
            }
            None
        }
        CodePage::Cp437 => {
            for (index, &high) in CP437_HIGH.iter().enumerate() {
                if high == unit {
                    return Some(0x80 + index as u8);
                }
            }
            None
        }
        CodePage::Utf8 => unreachable!(),
    }
}

/// Decode UTF-8 exactly, every malformed sequence producing one error
/// position (the caller turns them into replacements or a failure). A
/// sequence is malformed when it is overlong, when it encodes a surrogate
/// or a value past the maximum scalar, or when it ends or breaks early —
/// the malformed byte is consumed and the next one starts afresh, as the
/// Unicode standard's substitution of maximal subparts describes.
fn decode_utf8(bytes: &[u8]) -> (Vec<u16>, Vec<usize>) {
    let mut units = Vec::new();
    let mut malformed = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte < 0x80 {
            units.push(u16::from(byte));
            at += 1;
            continue;
        }
        // The length the lead byte announces, and the smallest value it may
        // hold: longer forms of small values are overlong.
        let (len, mut bits, minimum) = match byte {
            0xC0..=0xDF => (2usize, u32::from(byte & 0x1F), 0x80u32),
            0xE0..=0xEF => (3, u32::from(byte & 0x0F), 0x800),
            0xF0..=0xF4 => (4, u32::from(byte & 0x07), 0x10000),
            _ => {
                malformed.push(units.len());
                units.push(REPLACEMENT);
                at += 1;
                continue;
            }
        };
        let mut valid = true;
        let mut used = 1;
        for offset in 1..len {
            match bytes.get(at + offset) {
                Some(cont) if cont & 0xC0 == 0x80 => {
                    bits = bits << 6 | u32::from(cont & 0x3F);
                    used += 1;
                }
                _ => {
                    valid = false;
                    break;
                }
            }
        }
        if valid {
            valid = bits >= minimum && !(0xD800..=0xDFFF).contains(&bits) && bits <= 0x10FFFF;
        }
        if valid {
            // Values past the BMP take two units: surrogates are a UTF-16
            // fact, never malformed.
            if bits > 0xFFFF {
                let high = 0xD800 + ((bits - 0x10000) >> 10) as u16;
                let low = 0xDC00 + ((bits - 0x10000) & 0x3FF) as u16;
                units.push(high);
                units.push(low);
            } else {
                units.push(bits as u16);
            }
            at += len;
        } else {
            malformed.push(units.len());
            units.push(REPLACEMENT);
            at += used.max(1);
        }
    }
    (units, malformed)
}

/// Decode the input of `MultiByteToWideChar` for a page: the wide units, and
/// the positions that were malformed in the input (already replaced with
/// U+FFFD in the units, so a lenient caller is done).
pub(super) fn decode(page: CodePage, bytes: &[u8]) -> (Vec<u16>, Vec<usize>) {
    match page {
        CodePage::Utf8 => decode_utf8(bytes),
        _ => {
            let mut units = Vec::with_capacity(bytes.len());
            let mut malformed = Vec::new();
            for &byte in bytes {
                match single_byte_unit(page, byte) {
                    Some(unit) => units.push(unit),
                    None => {
                        malformed.push(units.len());
                        units.push(REPLACEMENT);
                    }
                }
            }
            (units, malformed)
        }
    }
}

/// Encode the wide units of `WideCharToMultiByte` for a page: the bytes, and
/// the positions whose character the page cannot represent (already written
/// as the caller's default byte, so a lenient caller is done).
pub(super) fn encode(page: CodePage, units: &[u16], default: u8) -> (Vec<u8>, Vec<usize>) {
    match page {
        CodePage::Utf8 => {
            let mut bytes = Vec::new();
            let mut malformed = Vec::new();
            let mut at = 0;
            while at < units.len() {
                let unit = units[at];
                let bits = if (0xD800..=0xDBFF).contains(&unit) {
                    // A surrogate pair is one scalar; a lone surrogate is
                    // malformed input.
                    match units.get(at + 1) {
                        Some(low) if (0xDC00..=0xDFFF).contains(low) => {
                            at += 1;
                            0x10000 + (((unit as u32 - 0xD800) << 10) | (u32::from(*low) - 0xDC00))
                        }
                        _ => {
                            malformed.push(bytes.len());
                            bytes.push(b'?');
                            at += 1;
                            continue;
                        }
                    }
                } else if (0xDC00..=0xDFFF).contains(&unit) {
                    malformed.push(bytes.len());
                    bytes.push(b'?');
                    at += 1;
                    continue;
                } else {
                    u32::from(unit)
                };
                // Encode the scalar as UTF-8.
                if bits < 0x80 {
                    bytes.push(bits as u8);
                } else if bits < 0x800 {
                    bytes.push(0xC0 | (bits >> 6) as u8);
                    bytes.push(0x80 | (bits & 0x3F) as u8);
                } else if bits < 0x10000 {
                    bytes.push(0xE0 | (bits >> 12) as u8);
                    bytes.push(0x80 | ((bits >> 6) & 0x3F) as u8);
                    bytes.push(0x80 | (bits & 0x3F) as u8);
                } else {
                    bytes.push(0xF0 | (bits >> 18) as u8);
                    bytes.push(0x80 | ((bits >> 12) & 0x3F) as u8);
                    bytes.push(0x80 | ((bits >> 6) & 0x3F) as u8);
                    bytes.push(0x80 | (bits & 0x3F) as u8);
                }
                at += 1;
            }
            (bytes, malformed)
        }
        _ => {
            let mut bytes = Vec::with_capacity(units.len());
            let mut unrepresentable = Vec::new();
            for &unit in units {
                match unit_single_byte(page, unit) {
                    Some(byte) => bytes.push(byte),
                    None => {
                        unrepresentable.push(bytes.len());
                        bytes.push(default);
                    }
                }
            }
            (bytes, unrepresentable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_decode_encode() {
        let (units, malformed) = decode(CodePage::Utf8, "h\u{e9}\u{20ac}\0".as_bytes());
        eprintln!("utf8 units: {:x?} malformed: {:?}", units, malformed);
        assert_eq!(units, vec![0x68, 0xE9, 0x20AC, 0]);
        let (bytes, unrep) = encode(CodePage::Utf8, &[0x68, 0xE9, 0], b'?');
        eprintln!("utf8 bytes: {:x?} unrep: {:?}", bytes, unrep);
        let (units1252, m2) = decode(CodePage::Cp1252, &[0x80, 0x81]);
        eprintln!("1252 units: {:x?} malformed: {:?}", units1252, m2);
        assert_eq!(units1252[0], 0x20AC);
    }
}
