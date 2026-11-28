// SPDX-License-Identifier: GPL-3.0-or-later

//! WinHTTP's guest-memory URL_COMPONENTS boundary.

mod parser;

use super::{ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, MODULE};
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::GuestMemory;
use crate::services::ServicesState;
use std::ops::Range;
use std::sync::Arc;

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
