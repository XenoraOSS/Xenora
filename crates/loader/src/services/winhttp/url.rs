// SPDX-License-Identifier: GPL-3.0-or-later

//! WinHTTP's guest-memory URL_COMPONENTS boundary.

pub(super) mod parser;

use super::{ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, MODULE};
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::GuestMemory;
use crate::services::ServicesState;
use std::ops::Range;
use std::sync::Arc;

const ICU_ESCAPE: u32 = 0x8000_0000;
const ICU_REJECT_USERPWD: u32 = 0x4000;
const MAX_URL_UNITS: usize = 32_768;

// The public Windows SDK uses eight-byte packing for Win64 URL_COMPONENTS.
// Each string slot contains an eight-byte pointer and a four-byte capacity.
const STRUCT_SIZE: usize = 104;
const SCHEME: usize = 8;
const SCHEME_KIND: usize = 20;
const HOST: usize = 24;
const PORT: usize = 36;
const USER: usize = 40;
const PASSWORD: usize = 56;
const PATH: usize = 72;
const EXTRA: usize = 88;

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let state = Arc::clone(state);
    let create_state = Arc::clone(&state);
    registry.register_with_status(
        MODULE,
        "WinHttpCrackUrl",
        ServiceStatus::Partial,
        move |args, cpu, memory| match crack(
            memory,
            args.rcx,
            args.rdx as u32,
            args.r8 as u32,
            args.r9,
        ) {
            Ok(()) => Ok(1),
            Err(error) => state.fail(cpu.gs_base, error, 0),
        },
    );
    registry.register_with_status(
        MODULE,
        "WinHttpCreateUrl",
        ServiceStatus::Partial,
        move |args, cpu, memory| match create(memory, args.rcx, args.rdx as u32, args.r8, args.r9) {
            Ok(()) => Ok(1),
            Err(error) => create_state.fail(cpu.gs_base, error, 0),
        },
    );
}

/// One string component of a creation record. A null pointer omits the
/// component, a zero length means a zero-terminated string and a nonzero
/// length counts the units exactly, as the URL_COMPONENTS remarks document.
fn read_component(
    memory: &GuestMemory,
    record: &[u8; STRUCT_SIZE],
    field: usize,
) -> Result<Option<Vec<u16>>, u32> {
    let pointer = read_pointer(record, field);
    if pointer == 0 {
        return Ok(None);
    }
    let length = read_dword(record, field + 8);
    if length != 0 {
        if length as usize > MAX_URL_UNITS {
            return Err(ERROR_INVALID_PARAMETER);
        }
        let mut bytes = vec![0; length as usize * 2];
        memory
            .read_bytes(pointer, &mut bytes)
            .map_err(|_| ERROR_INVALID_PARAMETER)?;
        let (words, _) = bytes.as_chunks::<2>();
        return Ok(Some(
            words.iter().copied().map(u16::from_le_bytes).collect(),
        ));
    }
    let mut result = Vec::new();
    for index in 0..=MAX_URL_UNITS {
        let unit = memory
            .read_u16(
                pointer
                    .checked_add(index as u64 * 2)
                    .ok_or(ERROR_INVALID_PARAMETER)?,
            )
            .map_err(|_| ERROR_INVALID_PARAMETER)?;
        if unit == 0 {
            return Ok(Some(result));
        }
        if index == MAX_URL_UNITS {
            break;
        }
        result.push(unit);
    }
    Err(ERROR_INVALID_PARAMETER)
}

/// Escapes the unsafe set the public "Uniform Resource Locators in WinHTTP"
/// page lists, in path and extra-info text: control characters, DEL, space,
/// percent, `"< > # { } | \ ^ ~ [ ] ` '"` and every non-ASCII character.
/// Non-ASCII characters become their UTF-8 bytes; Windows converts through
/// the system code page, which Xenora does not model. Unpaired surrogates
/// have no encoding and are an invalid URL. Percent is escaped even inside a
/// valid escape triplet, the literal reading of the published list.
fn escape(units: &[u16]) -> Result<Vec<u16>, u32> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out: Vec<u16> = Vec::new();
    for decoded in char::decode_utf16(units.iter().copied()) {
        let ch = decoded.map_err(|_| parser::ERROR_WINHTTP_INVALID_URL)?;
        if ch.is_ascii() {
            let byte = ch as u8;
            let unsafe_ascii = byte < 0x20
                || byte == 0x7f
                || matches!(
                    byte,
                    b' ' | b'%'
                        | b'<'
                        | b'>'
                        | b'"'
                        | b'#'
                        | b'{'
                        | b'}'
                        | b'|'
                        | b'\\'
                        | b'^'
                        | b'~'
                        | b'['
                        | b']'
                        | b'`'
                        | b'\''
                );
            if unsafe_ascii {
                out.push(u16::from(b'%'));
                out.push(u16::from(HEX[(byte >> 4) as usize]));
                out.push(u16::from(HEX[(byte & 0xf) as usize]));
            } else {
                out.push(u16::from(byte));
            }
        } else {
            let mut encoded = [0u8; 4];
            for &byte in ch.encode_utf8(&mut encoded).as_bytes() {
                out.push(u16::from(b'%'));
                out.push(u16::from(HEX[(byte >> 4) as usize]));
                out.push(u16::from(HEX[(byte & 0xf) as usize]));
            }
        }
    }
    Ok(out)
}

fn ascii_units(text: &str) -> Vec<u16> {
    text.bytes().map(u16::from).collect()
}

fn create(
    memory: &mut GuestMemory,
    components: u64,
    flags: u32,
    url_out: u64,
    length_ptr: u64,
) -> Result<(), u32> {
    if flags & !(ICU_ESCAPE | ICU_REJECT_USERPWD) != 0 {
        return Err(ERROR_NOT_SUPPORTED);
    }
    if components == 0 || length_ptr == 0 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let mut record = [0; STRUCT_SIZE];
    memory
        .read_bytes(components, &mut record)
        .map_err(|_| ERROR_INVALID_PARAMETER)?;
    if read_dword(&record, 0) != STRUCT_SIZE as u32 || !memory.can_write(length_ptr, 4) {
        return Err(ERROR_INVALID_PARAMETER);
    }

    let scheme_text = read_component(memory, &record, SCHEME)?;
    let host = read_component(memory, &record, HOST)?;
    let user = read_component(memory, &record, USER)?;
    let password = read_component(memory, &record, PASSWORD)?;
    let path = read_component(memory, &record, PATH)?;
    let extra = read_component(memory, &record, EXTRA)?;
    let scheme_kind = read_dword(&record, SCHEME_KIND);
    let port = u16::from_le_bytes(record[PORT..PORT + 2].try_into().unwrap());

    // The wininet twin documents the creation precedence: a scheme string
    // wins over the nScheme enumeration. Xenora composes only the HTTP and
    // HTTPS schemes WinHTTP itself recognizes, as cracking does.
    let (scheme_units, default_port) = match scheme_text.filter(|text| !text.is_empty()) {
        Some(text) => {
            let (_, default_port) = parser::parse_scheme(&text)?;
            (text, default_port)
        }
        None => match scheme_kind {
            1 => (ascii_units("http"), 80),
            2 => (ascii_units("https"), 443),
            _ => return Err(parser::ERROR_WINHTTP_UNRECOGNIZED_SCHEME),
        },
    };
    let host = host
        .filter(|text| !text.is_empty())
        .ok_or(parser::ERROR_WINHTTP_INVALID_URL)?;
    let user = user.filter(|text| !text.is_empty());
    let password = password.filter(|text| !text.is_empty());
    if flags & ICU_REJECT_USERPWD != 0 && (user.is_some() || password.is_some()) {
        return Err(parser::ERROR_WINHTTP_INVALID_URL);
    }
    if password.is_some() && user.is_none() {
        return Err(parser::ERROR_WINHTTP_INVALID_URL);
    }

    let mut url: Vec<u16> = Vec::new();
    url.extend_from_slice(&scheme_units);
    url.extend_from_slice(&ascii_units("://"));
    if let Some(user) = user {
        url.extend_from_slice(&user);
        if let Some(password) = password {
            url.push(u16::from(b':'));
            url.extend_from_slice(&password);
        }
        url.push(u16::from(b'@'));
    }
    url.extend_from_slice(&host);
    // Canonical round-trip form: cracking a URL without an explicit port
    // reports the scheme default, so composing that port adds nothing.
    if port != 0 && port != default_port {
        url.push(u16::from(b':'));
        url.extend_from_slice(&ascii_units(&port.to_string()));
    }
    let mut tail: Vec<u16> = Vec::new();
    if let Some(mut path) = path.filter(|text| !text.is_empty()) {
        if flags & ICU_ESCAPE != 0 {
            path = escape(&path)?;
        }
        if path.first() != Some(&u16::from(b'/')) {
            tail.push(u16::from(b'/'));
        }
        tail.extend_from_slice(&path);
    }
    if let Some(extra) = extra.filter(|text| !text.is_empty()) {
        if flags & ICU_ESCAPE != 0 {
            tail.extend_from_slice(&escape(&extra)?);
        } else {
            tail.extend_from_slice(&extra);
        }
    }
    url.extend_from_slice(&tail);

    let length = url.len();
    let required = length + 1;
    let capacity = memory
        .read_u32(length_ptr)
        .map_err(|_| ERROR_INVALID_PARAMETER)? as usize;
    if url_out == 0 || capacity < required {
        memory
            .write_u32(length_ptr, required as u32)
            .map_err(|_| ERROR_INVALID_PARAMETER)?;
        return Err(ERROR_INSUFFICIENT_BUFFER);
    }
    let output_span = byte_span(url_out, required * 2)?;
    let length_span = byte_span(length_ptr, 4)?;
    if overlaps(&output_span, &length_span) || !memory.can_write(url_out, required * 2) {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let bytes: Vec<u8> = url
        .iter()
        .copied()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    memory
        .write_bytes(url_out, &bytes)
        .map_err(|_| ERROR_INVALID_PARAMETER)?;
    memory
        .write_u32(length_ptr, length as u32)
        .map_err(|_| ERROR_INVALID_PARAMETER)
}

fn read_input(memory: &GuestMemory, address: u64, count: u32) -> Result<Vec<u16>, u32> {
    if address == 0 || count as usize > MAX_URL_UNITS {
        return Err(ERROR_INVALID_PARAMETER);
    }
    if count != 0 {
        let mut bytes = vec![0; count as usize * 2];
        memory
            .read_bytes(address, &mut bytes)
            .map_err(|_| ERROR_INVALID_PARAMETER)?;
        let (words, _) = bytes.as_chunks::<2>();
        return Ok(words.iter().copied().map(u16::from_le_bytes).collect());
    }

    let mut url = Vec::new();
    for index in 0..=MAX_URL_UNITS {
        let unit = memory
            .read_u16(
                address
                    .checked_add(index as u64 * 2)
                    .ok_or(ERROR_INVALID_PARAMETER)?,
            )
            .map_err(|_| ERROR_INVALID_PARAMETER)?;
        if unit == 0 {
            return Ok(url);
        }
        if index == MAX_URL_UNITS {
            break;
        }
        url.push(unit);
    }
    Err(ERROR_INVALID_PARAMETER)
}

fn byte_span(address: u64, size: usize) -> Result<Range<u64>, u32> {
    Ok(address
        ..address
            .checked_add(size as u64)
            .ok_or(ERROR_INVALID_PARAMETER)?)
}

fn overlaps(a: &Range<u64>, b: &Range<u64>) -> bool {
    a.start < b.end && b.start < a.end
}

fn read_pointer(record: &[u8; STRUCT_SIZE], offset: usize) -> u64 {
    u64::from_le_bytes(record[offset..offset + 8].try_into().unwrap())
}

fn read_dword(record: &[u8; STRUCT_SIZE], offset: usize) -> u32 {
    u32::from_le_bytes(record[offset..offset + 4].try_into().unwrap())
}

fn crack(
    memory: &mut GuestMemory,
    url_address: u64,
    count: u32,
    flags: u32,
    output: u64,
) -> Result<(), u32> {
    if flags & !ICU_REJECT_USERPWD != 0 {
        return Err(ERROR_NOT_SUPPORTED);
    }
    if output == 0 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let mut record = [0; STRUCT_SIZE];
    memory
        .read_bytes(output, &mut record)
        .map_err(|_| ERROR_INVALID_PARAMETER)?;
    if read_dword(&record, 0) != STRUCT_SIZE as u32 || !memory.can_write(output, STRUCT_SIZE) {
        return Err(ERROR_INVALID_PARAMETER);
    }

    let url = read_input(memory, url_address, count)?;
    let parsed = parser::parse(&url, flags & ICU_REJECT_USERPWD != 0)?;
    let input_span = byte_span(url_address, (url.len() + usize::from(count == 0)) * 2)?;
    let output_span = byte_span(output, STRUCT_SIZE)?;
    // Views refer to the live source bytes. Preserve those bytes and keep
    // overlapping outputs from corrupting each other's successful results.
    if overlaps(&input_span, &output_span) {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let separate_extra = read_dword(&record, EXTRA + 8) != 0;
    let path = parsed.path.start..if separate_extra {
        parsed.path.end
    } else {
        url.len()
    };
    let mut copies: Vec<(Range<u64>, Vec<u8>)> = Vec::new();
    for (field, range) in [
        (SCHEME, Some(parsed.scheme)),
        (HOST, Some(parsed.host)),
        (USER, parsed.username),
        (PASSWORD, parsed.password),
        (PATH, Some(path)),
        (EXTRA, Some(parsed.extra)),
    ] {
        let pointer = read_pointer(&record, field);
        let capacity = read_dword(&record, field + 8);
        if capacity == 0 && (field == EXTRA || pointer == 0) {
            continue;
        }
        let part = range.as_ref().map_or(&[][..], |range| &url[range.clone()]);
        if pointer == 0 {
            let view = range
                .filter(|range| !range.is_empty())
                .map_or(0, |range| url_address + range.start as u64 * 2);
            record[field..field + 8].copy_from_slice(&view.to_le_bytes());
        } else {
            if part.len() >= capacity as usize {
                return Err(ERROR_INSUFFICIENT_BUFFER);
            }
            let size = (part.len() + 1) * 2;
            let destination = byte_span(pointer, size)?;
            if !memory.can_write(pointer, size)
                || overlaps(&destination, &input_span)
                || overlaps(&destination, &output_span)
                || copies
                    .iter()
                    .any(|(other, _)| overlaps(other, &destination))
            {
                return Err(ERROR_INVALID_PARAMETER);
            }
            let bytes = part
                .iter()
                .copied()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect();
            copies.push((destination, bytes));
        }
        record[field + 8..field + 12].copy_from_slice(&(part.len() as u32).to_le_bytes());
    }

    record[SCHEME_KIND..SCHEME_KIND + 4].copy_from_slice(&parsed.scheme_id.to_le_bytes());
    record[PORT..PORT + 2].copy_from_slice(&parsed.port.to_le_bytes());
    // All spans were checked against the same exclusive GuestMemory before
    // the first write. No operation in this commit loop can change mappings.
    for (destination, bytes) in copies {
        memory
            .write_bytes(destination.start, &bytes)
            .map_err(|_| ERROR_INVALID_PARAMETER)?;
    }
    memory
        .write_bytes(output, &record)
        .map_err(|_| ERROR_INVALID_PARAMETER)
}
