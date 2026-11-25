// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded C-locale `sprintf`/`snprintf` and a FILE stream subset.
//!
//! Names that are a single path component (no separators) use an in-memory
//! store. Paths with a mounted drive letter (`D:\…`) open through the host
//! mount table: the file is read into a bounded buffer, and writable streams
//! write the buffer back on `fclose`. Standard streams from
//! `__acrt_iob_func` are permanent cookies (stdin empty, stdout/stderr discard).

use super::{ServicesState, host_files};
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus, Win64Args};
use crate::memory::{GuestMemory, MemoryError, PROCESS_HEAP};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

const UCRT: &str = "ucrtbase.dll";
const STDIO_API: &str = "api-ms-win-crt-stdio-l1-1-0.dll";
const EINVAL: u32 = 22;
/// `EOF` as the C `int` -1, in the low 32 bits of RAX.
const EOF: u64 = u32::MAX as u64;
const ENOENT: u32 = 2;
const ENOMEM: u32 = 12;
const EACCES: u32 = 13;
const ENOSPC: u32 = 28;
const MAX_FORMAT: usize = 4096;
const MAX_OUTPUT: usize = 1 << 20;
const MAX_PATH: usize = 260;
const MAX_STREAMS: usize = 32;
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const COOKIE_BYTES: usize = 16;
const COOKIE_MAGIC: u32 = 0x5845_4E46; // "FENX" LE visual as XENF bytes
const SEEK_SET: i32 = 0;
const SEEK_CUR: i32 = 1;
const SEEK_END: i32 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    StdIn,
    StdOut,
    StdErr,
}

struct Stream {
    kind: Kind,
    name: String,
    data: Vec<u8>,
    position: usize,
    readable: bool,
    writable: bool,
    append: bool,
    eof: bool,
    error: bool,
    /// Host file kept open so a writable stream can replace its contents on
    /// `fclose`. Read-only host opens drop the descriptor after the initial
    /// read into `data`.
    /// The mount's quota is charged when `fclose` writes the contents back.
    host: Option<(File, Arc<host_files::Budget>)>,
    /// True when the stream was opened on a mounted drive (even if `host` was
    /// dropped after a read-only load). Host-backed streams skip the in-memory
    /// name store on `fclose`.
    host_backed: bool,
    dirty: bool,
    max_bytes: usize,
    /// A byte `ungetc` pushed back: the next read at `position` returns it
    /// instead of `data[position]`. It is never written to the file, and any
    /// seek or write discards it.
    pushback: Option<u8>,
}

#[derive(Default)]
pub(super) struct StdioState {
    files: BTreeMap<String, Vec<u8>>,
    streams: BTreeMap<u32, Stream>,
    next_id: u32,
    stdin: u64,
    stdout: u64,
    stderr: u64,
}

fn fault(error: MemoryError) -> ExitReason {
    ExitReason::MemoryFault(error)
}

fn set_errno(memory: &mut GuestMemory, teb: u64, error: u32) -> Result<(), ExitReason> {
    let address = memory.crt_errno_address(teb).map_err(fault)?;
    memory.write_u32(address, error).map_err(fault)
}

fn read_c_string(memory: &GuestMemory, address: u64, limit: usize) -> Result<Vec<u8>, ExitReason> {
    let mut bytes = Vec::new();
    for index in 0..limit {
        let byte = memory.read_u8(address + index as u64).map_err(fault)?;
        if byte == 0 {
            return Ok(bytes);
        }
        bytes.push(byte);
    }
    Err(ExitReason::IllegalInstruction(0xFE))
}

fn unsupported() -> ExitReason {
    ExitReason::IllegalInstruction(0xFE)
}

fn normalize_name(raw: &[u8]) -> Result<String, u32> {
    let name = std::str::from_utf8(raw).map_err(|_| EINVAL)?;
    if name.is_empty()
        || name.len() > MAX_PATH
        || name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
        || name == "."
        || name == ".."
    {
        return Err(EINVAL);
    }
    Ok(name.to_ascii_lowercase())
}

fn valid_host_component(name: &str) -> bool {
    let device = name.split('.').next().unwrap_or_default();
    !name.is_empty()
        && !name.ends_with([' ', '.'])
        && !name
            .chars()
            .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
        && !matches!(device, "con" | "prn" | "aux" | "nul")
        && !(device.len() == 4
            && (device.starts_with("com") || device.starts_with("lpt"))
            && device.as_bytes()[3].is_ascii_digit())
}

/// Drive-letter guest path for a configured mount (`D:\file.txt`).
fn host_drive_path(raw: &[u8]) -> Option<String> {
    let path = std::str::from_utf8(raw).ok()?;
    if path.len() < 3 || path.as_bytes()[1] != b':' {
        return None;
    }
    let mut normalized = String::with_capacity(path.len());
    for (index, ch) in path.chars().enumerate() {
        let mapped = match ch {
            '/' => '\\',
            c => c,
        };
        if index >= 2 && mapped == '\\' && normalized.ends_with('\\') {
            continue;
        }
        normalized.push(mapped);
    }
    Some(normalized)
}

fn parse_mode(raw: &[u8]) -> Result<(bool, bool, bool, bool), u32> {
    let mode = std::str::from_utf8(raw).map_err(|_| EINVAL)?;
    let mut readable = false;
    let mut writable = false;
    let mut append = false;
    let mut truncate = false;
    let bytes = mode.as_bytes();
    if bytes.is_empty() {
        return Err(EINVAL);
    }
    match bytes[0] {
        b'r' => readable = true,
        b'w' => {
            writable = true;
            truncate = true;
        }
        b'a' => {
            writable = true;
            append = true;
        }
        _ => return Err(EINVAL),
    }
    for &byte in &bytes[1..] {
        match byte {
            b'+' => {
                readable = true;
                writable = true;
            }
            b'b' | b't' => {}
            _ => return Err(EINVAL),
        }
    }
    Ok((readable, writable, append, truncate))
}

fn write_cookie(memory: &mut GuestMemory, id: u32) -> Result<u64, ExitReason> {
    let cookie = memory
        .map_heap_allocation(PROCESS_HEAP, COOKIE_BYTES)
        .map_err(fault)?;
    let mut bytes = [0u8; COOKIE_BYTES];
    bytes[..4].copy_from_slice(&COOKIE_MAGIC.to_le_bytes());
    bytes[4..8].copy_from_slice(&id.to_le_bytes());
    memory.write_bytes(cookie, &bytes).map_err(fault)?;
    Ok(cookie)
}

fn read_cookie(memory: &GuestMemory, cookie: u64) -> Result<u32, ExitReason> {
    if cookie == 0 {
        return Err(ExitReason::IllegalInstruction(0xFE));
    }
    let mut bytes = [0u8; 8];
    memory.read_bytes(cookie, &mut bytes).map_err(fault)?;
    let magic = u32::from_le_bytes(bytes[..4].try_into().unwrap());
    if magic != COOKIE_MAGIC {
        return Err(ExitReason::IllegalInstruction(0xFE));
    }
    Ok(u32::from_le_bytes(bytes[4..8].try_into().unwrap()))
}

fn ensure_std_cookies(
    state: &Arc<ServicesState>,
    memory: &mut GuestMemory,
) -> Result<(), ExitReason> {
    let mut stdio = state.stdio.lock().expect("stdio state");
    if stdio.stdin != 0 {
        return Ok(());
    }
    let kinds = [Kind::StdIn, Kind::StdOut, Kind::StdErr];
    let mut cookies = [0u64; 3];
    for (index, kind) in kinds.into_iter().enumerate() {
        let id = stdio.next_id;
        stdio.next_id = id
            .checked_add(1)
            .ok_or(ExitReason::MemoryFault(MemoryError::AllocationFailed))?;
        drop(stdio);
        let cookie = write_cookie(memory, id)?;
        cookies[index] = cookie;
        let mut locked = state.stdio.lock().expect("stdio state");
        locked.streams.insert(
            id,
            Stream {
                kind,
                name: match kind {
                    Kind::StdIn => "stdin".into(),
                    Kind::StdOut => "stdout".into(),
                    Kind::StdErr => "stderr".into(),
                    Kind::File => unreachable!(),
                },
                data: Vec::new(),
                position: 0,
                readable: kind == Kind::StdIn,
                writable: kind != Kind::StdIn,
                append: false,
                eof: kind == Kind::StdIn,
                error: false,
                host: None,
                host_backed: false,
                dirty: false,
                max_bytes: MAX_FILE_BYTES,
                pushback: None,
            },
        );
        stdio = locked;
    }
    stdio.stdin = cookies[0];
    stdio.stdout = cookies[1];
    stdio.stderr = cookies[2];
    Ok(())
}

fn errno_from_host(code: u32) -> u32 {
    match code {
        2 | 3 => ENOENT, // FILE_NOT_FOUND / PATH_NOT_FOUND
        5 => EACCES,     // ACCESS_DENIED
        112 => ENOSPC,   // DISK_FULL
        _ => EINVAL,
    }
}

type HostStream = Result<
    (
        String,
        Vec<u8>,
        Option<(File, Arc<host_files::Budget>)>,
        usize,
    ),
    u32,
>;

fn open_host_stream(
    state: &Arc<ServicesState>,
    path: &str,
    readable: bool,
    writable: bool,
    truncate: bool,
) -> HostStream {
    let resolved = match host_files::resolve(&state.mounts, path, valid_host_component) {
        Some(Ok(resolved)) => resolved,
        Some(Err(code)) => return Err(errno_from_host(code)),
        None => return Err(ENOENT),
    };
    if writable && !resolved.mount.writable() {
        return Err(EACCES);
    }
    let max_bytes = (resolved.mount.max_file_size() as usize).min(MAX_FILE_BYTES);
    let (parent, entry) = resolved.lookup().map_err(errno_from_host)?;
    let name = match resolved.components.last() {
        Some(name) => name.clone(),
        None => return Err(EINVAL),
    };
    if let Some(entry) = &entry
        && entry.kind != host_files::Kind::File
    {
        return Err(EACCES);
    }
    let exists = entry.is_some();
    if !exists && readable && !writable {
        return Err(ENOENT);
    }
    let request = host_files::OpenRequest {
        read: readable || !writable,
        write: writable,
        create: !exists && writable,
    };
    let host_name = entry
        .as_ref()
        .map_or(name.as_str(), |entry| entry.name.as_str());
    let expected = entry.as_ref().map(|entry| entry.id);
    let budget = Arc::clone(resolved.mount.budget());
    if request.create {
        budget.add_entry().map_err(errno_from_host)?;
    }
    let (mut file, _) = match host_files::open_file(&parent, host_name, &request, expected) {
        Ok(opened) => opened,
        Err(error) => {
            if request.create {
                budget.remove_entry(0);
            }
            return Err(errno_from_host(host_files::win32_error(&error)));
        }
    };
    if exists && truncate {
        let length = file.metadata().map_err(|_| EACCES)?.len();
        file.set_len(0).map_err(|_| EACCES)?;
        budget.resize(length, 0).map_err(errno_from_host)?;
    }
    let mut data = Vec::new();
    if !truncate {
        file.read_to_end(&mut data).map_err(|_| EACCES)?;
        if data.len() > max_bytes {
            return Err(ENOMEM);
        }
    }
    let host = if writable {
        file.seek(SeekFrom::Start(0)).map_err(|_| EACCES)?;
        Some((file, budget))
    } else {
        None
    };
    Ok((path.to_owned(), data, host, max_bytes))
}

fn open_stream(
    state: &Arc<ServicesState>,
    memory: &mut GuestMemory,
    teb: u64,
    path: u64,
    mode: u64,
) -> Result<u64, ExitReason> {
    let path = read_c_string(memory, path, MAX_PATH)?;
    let mode = read_c_string(memory, mode, 16)?;
    let (readable, writable, append, truncate) = match parse_mode(&mode) {
        Ok(mode) => mode,
        Err(error) => {
            set_errno(memory, teb, error)?;
            return Ok(0);
        }
    };
    let host_path = host_drive_path(&path);
    let opened = if let Some(ref drive_path) = host_path {
        match open_host_stream(state, drive_path, readable, writable, truncate) {
            Ok(opened) => Ok(opened),
            Err(error) => {
                set_errno(memory, teb, error)?;
                return Ok(0);
            }
        }
    } else {
        let name = match normalize_name(&path) {
            Ok(name) => name,
            Err(error) => {
                set_errno(memory, teb, error)?;
                return Ok(0);
            }
        };
        Err(name)
    };

    let mut stdio = state.stdio.lock().expect("stdio state");
    if stdio.streams.len() >= MAX_STREAMS {
        set_errno(memory, teb, ENOMEM)?;
        return Ok(0);
    }

    let (name, data, host, host_backed, max_bytes, dirty) = match opened {
        Ok((name, data, host, max_bytes)) => {
            (name, data, host, true, max_bytes, truncate && writable)
        }
        Err(name) => {
            let existing = stdio.files.get(&name).cloned();
            let data = if truncate {
                Vec::new()
            } else if let Some(data) = existing {
                data
            } else if readable && !writable {
                set_errno(memory, teb, ENOENT)?;
                return Ok(0);
            } else {
                Vec::new()
            };
            if data.len() > MAX_FILE_BYTES {
                set_errno(memory, teb, ENOMEM)?;
                return Ok(0);
            }
            if truncate || !stdio.files.contains_key(&name) {
                stdio.files.insert(name.clone(), Vec::new());
            }
            (name, data, None, false, MAX_FILE_BYTES, false)
        }
    };
    let position = if append { data.len() } else { 0 };
    let id = stdio.next_id;
    stdio.next_id = id
        .checked_add(1)
        .ok_or(ExitReason::MemoryFault(MemoryError::AllocationFailed))?;
    drop(stdio);
    let cookie = write_cookie(memory, id)?;
    let mut stdio = state.stdio.lock().expect("stdio state");
    stdio.streams.insert(
        id,
        Stream {
            kind: Kind::File,
            name,
            data,
            position,
            readable,
            writable,
            append,
            eof: false,
            error: false,
            host,
            host_backed,
            dirty,
            max_bytes,
            pushback: None,
        },
    );
    Ok(cookie)
}

/// Deliver a stream's contents: a dirty host-backed stream replaces its host
/// file (charging the mount quota), and an in-memory file is published under
/// its name so a later `fopen` sees it. Returns the `errno` on failure.
fn flush_stream(stream: &mut Stream, files: &mut BTreeMap<String, Vec<u8>>) -> Result<(), u32> {
    if stream.kind != Kind::File {
        return Ok(());
    }
    if !stream.host_backed {
        files.insert(stream.name.clone(), stream.data.clone());
        return Ok(());
    }
    if !stream.dirty {
        return Ok(());
    }
    let Some((file, budget)) = stream.host.as_mut() else {
        return Ok(());
    };
    let target = stream.data.len() as u64;
    let length = file.metadata().map_err(|_| EACCES)?.len();
    budget.resize(length, target).map_err(errno_from_host)?;
    let failed = file
        .set_len(target)
        .and_then(|_| file.seek(SeekFrom::Start(0)))
        .and_then(|_| file.write_all(&stream.data))
        .is_err();
    if failed {
        // Charge what the host file now holds.
        let now = file.metadata().map_or(length, |metadata| metadata.len());
        let _ = budget.resize(target, now);
        return Err(EACCES);
    }
    stream.dirty = false;
    Ok(())
}

/// Read one byte from a file stream, honouring an `ungetc` push-back.
fn read_byte(stream: &mut Stream) -> Option<u8> {
    if stream.kind != Kind::File {
        return None;
    }
    let byte = match stream.pushback.take() {
        Some(byte) => byte,
        None => *stream.data.get(stream.position)?,
    };
    stream.position += 1;
    Some(byte)
}

/// Write `bytes` at the stream position, as `fwrite` does. Standard output
/// and error accept and discard output. Returns whether it was written.
fn put_bytes(stream: &mut Stream, bytes: &[u8]) -> Result<bool, ExitReason> {
    if !stream.writable || stream.kind == Kind::StdIn {
        stream.error = true;
        return Ok(false);
    }
    if matches!(stream.kind, Kind::StdOut | Kind::StdErr) {
        return Ok(true);
    }
    stream.pushback = None;
    if stream.append {
        stream.position = stream.data.len();
    }
    let end = stream
        .position
        .checked_add(bytes.len())
        .ok_or(ExitReason::IllegalInstruction(0xFE))?;
    if end > stream.max_bytes {
        stream.error = true;
        return Ok(false);
    }
    if end > stream.data.len() {
        stream.data.resize(end, 0);
    }
    stream.data[stream.position..end].copy_from_slice(bytes);
    stream.position = end;
    stream.eof = false;
    stream.dirty = stream.host_backed;
    Ok(true)
}

fn with_stream<T>(
    state: &Arc<ServicesState>,
    memory: &mut GuestMemory,
    cookie: u64,
    body: impl FnOnce(&mut Stream, &mut GuestMemory) -> Result<T, ExitReason>,
) -> Result<T, ExitReason> {
    let id = read_cookie(memory, cookie)?;
    let mut stdio = state.stdio.lock().expect("stdio state");
    let stream = stdio
        .streams
        .get_mut(&id)
        .ok_or(ExitReason::IllegalInstruction(0xFE))?;
    body(stream, memory)
}

#[derive(Clone, Copy)]
enum Length {
    Default,
    Long,
    LongLong,
}

fn parse_length(format: &[u8], index: &mut usize) -> Length {
    if format.get(*index) == Some(&b'l') {
        *index += 1;
        if format.get(*index) == Some(&b'l') {
            *index += 1;
            Length::LongLong
        } else {
            Length::Long
        }
    } else if format.get(*index) == Some(&b'z') || format.get(*index) == Some(&b't') {
        *index += 1;
        Length::LongLong
    } else {
        Length::Default
    }
}

fn format_into(
    memory: &GuestMemory,
    args: &Win64Args,
    format_va: u64,
    first_arg: usize,
) -> Result<Vec<u8>, ExitReason> {
    let format = read_c_string(memory, format_va, MAX_FORMAT)?;
    let mut out = Vec::new();
    let mut index = 0;
    let mut arg = first_arg;
    while index < format.len() {
        if out.len() > MAX_OUTPUT {
            return Err(unsupported());
        }
        let byte = format[index];
        index += 1;
        if byte != b'%' {
            out.push(byte);
            continue;
        }
        if format.get(index) == Some(&b'%') {
            index += 1;
            out.push(b'%');
            continue;
        }
        let mut width = 0usize;
        let mut pad = b' ';
        if format.get(index) == Some(&b'0') {
            pad = b'0';
            index += 1;
        }
        while let Some(digit @ b'0'..=b'9') = format.get(index).copied() {
            width = width
                .checked_mul(10)
                .and_then(|value| value.checked_add((digit - b'0') as usize))
                .ok_or_else(unsupported)?;
            index += 1;
        }
        let length = parse_length(&format, &mut index);
        let spec = *format.get(index).ok_or_else(unsupported)?;
        index += 1;
        let value = args.get(arg, memory).map_err(fault)?;
        arg += 1;
        let rendered = match spec {
            b'c' => vec![value as u8],
            b's' => {
                if value == 0 {
                    b"(null)".to_vec()
                } else {
                    read_c_string(memory, value, MAX_OUTPUT)?
                }
            }
            b'd' | b'i' => {
                let number = match length {
                    Length::Default | Length::Long => value as i32 as i64,
                    Length::LongLong => value as i64,
                };
                number.to_string().into_bytes()
            }
            b'u' => {
                let number = match length {
                    Length::Default | Length::Long => value as u32 as u64,
                    Length::LongLong => value,
                };
                number.to_string().into_bytes()
            }
            b'x' | b'X' => {
                let number = match length {
                    Length::Default | Length::Long => value as u32 as u64,
                    Length::LongLong => value,
                };
                let text = if spec == b'X' {
                    format!("{number:X}")
                } else {
                    format!("{number:x}")
                };
                text.into_bytes()
            }
            b'p' => format!("{value:x}").into_bytes(),
            _ => return Err(unsupported()),
        };
        if width > rendered.len() {
            let padding = width - rendered.len();
            if pad == b'0' && rendered.first() == Some(&b'-') {
                out.push(b'-');
                out.extend(std::iter::repeat_n(b'0', padding));
                out.extend_from_slice(&rendered[1..]);
            } else {
                out.extend(std::iter::repeat_n(pad, padding));
                out.extend_from_slice(&rendered);
            }
        } else {
            out.extend_from_slice(&rendered);
        }
    }
    Ok(out)
}

fn write_formatted(
    memory: &mut GuestMemory,
    teb: u64,
    buffer: u64,
    capacity: Option<u64>,
    args: &Win64Args,
    format_index: usize,
    first_arg: usize,
) -> Result<u64, ExitReason> {
    if buffer == 0 {
        set_errno(memory, teb, EINVAL)?;
        return Ok(u64::from(u32::MAX));
    }
    let format = args.get(format_index, memory).map_err(fault)?;
    if format == 0 {
        set_errno(memory, teb, EINVAL)?;
        return Ok(u64::from(u32::MAX));
    }
    let formatted = format_into(memory, args, format, first_arg)?;
    let needed = formatted.len() as u64;
    match capacity {
        None => {
            let mut bytes = formatted;
            bytes.push(0);
            memory.write_bytes(buffer, &bytes).map_err(fault)?;
            Ok(needed)
        }
        Some(0) => Ok(needed),
        Some(count) => {
            let room = count as usize;
            if room == 0 {
                return Ok(needed);
            }
            let copy = formatted.len().min(room.saturating_sub(1));
            memory
                .write_bytes(buffer, &formatted[..copy])
                .map_err(fault)?;
            memory.write_u8(buffer + copy as u64, 0).map_err(fault)?;
            Ok(needed)
        }
    }
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    registry.register(UCRT, "sprintf", |args, cpu, memory| {
        write_formatted(memory, cpu.gs_base, args.rcx, None, args, 1, 2)
    });
    registry.register(UCRT, "snprintf", |args, cpu, memory| {
        write_formatted(memory, cpu.gs_base, args.rcx, Some(args.rdx), args, 2, 3)
    });

    let fopen = Arc::clone(state);
    registry.register(UCRT, "fopen", move |args, cpu, memory| {
        open_stream(&fopen, memory, cpu.gs_base, args.rcx, args.rdx)
    });

    let fclose = Arc::clone(state);
    registry.register(UCRT, "fclose", move |args, cpu, memory| {
        if args.rcx == 0 {
            set_errno(memory, cpu.gs_base, EINVAL)?;
            return Ok(u64::from(u32::MAX));
        }
        let id = read_cookie(memory, args.rcx)?;
        let mut stdio = fclose.stdio.lock().expect("stdio state");
        let Some(mut stream) = stdio.streams.remove(&id) else {
            set_errno(memory, cpu.gs_base, EINVAL)?;
            return Ok(u64::from(u32::MAX));
        };
        if let Err(error) = flush_stream(&mut stream, &mut stdio.files) {
            set_errno(memory, cpu.gs_base, error)?;
            return Ok(u64::from(u32::MAX));
        }
        let _ = memory.release_heap_allocation(PROCESS_HEAP, args.rcx);
        Ok(0)
    });

    let fread = Arc::clone(state);
    registry.register(UCRT, "fread", move |args, _cpu, memory| {
        let (buffer, size, count, cookie) = (args.rcx, args.rdx, args.r8, args.r9);
        if size == 0 || count == 0 {
            return Ok(0);
        }
        let total = size
            .checked_mul(count)
            .ok_or(ExitReason::IllegalInstruction(0xFE))?;
        if total as usize > MAX_OUTPUT {
            return Err(ExitReason::IllegalInstruction(0xFE));
        }
        with_stream(&fread, memory, cookie, |stream, memory| {
            if !stream.readable {
                stream.error = true;
                return Ok(0);
            }
            if stream.kind == Kind::StdIn {
                stream.eof = true;
                return Ok(0);
            }
            let available = stream.data.len().saturating_sub(stream.position);
            let take = (total as usize).min(available);
            let complete = take / size as usize * size as usize;
            if complete == 0 {
                stream.eof = true;
                return Ok(0);
            }
            let mut bytes = stream.data[stream.position..stream.position + complete].to_vec();
            if let Some(byte) = stream.pushback.take() {
                bytes[0] = byte;
            }
            memory.write_bytes(buffer, &bytes).map_err(fault)?;
            stream.position += complete;
            if stream.position >= stream.data.len() {
                stream.eof = true;
            }
            Ok((complete / size as usize) as u64)
        })
    });

    let fwrite = Arc::clone(state);
    registry.register(UCRT, "fwrite", move |args, _cpu, memory| {
        let (buffer, size, count, cookie) = (args.rcx, args.rdx, args.r8, args.r9);
        if size == 0 || count == 0 {
            return Ok(0);
        }
        let total = size
            .checked_mul(count)
            .ok_or(ExitReason::IllegalInstruction(0xFE))?;
        if total as usize > MAX_OUTPUT {
            return Err(ExitReason::IllegalInstruction(0xFE));
        }
        let mut bytes = vec![0u8; total as usize];
        memory.read_bytes(buffer, &mut bytes).map_err(fault)?;
        with_stream(&fwrite, memory, cookie, |stream, _| {
            Ok(if put_bytes(stream, &bytes)? { count } else { 0 })
        })
    });

    for symbol in ["fputc", "putc"] {
        let putc = Arc::clone(state);
        registry.register(UCRT, symbol, move |args, _, memory| {
            let byte = args.rcx as u8;
            with_stream(&putc, memory, args.rdx, |stream, _| {
                Ok(if put_bytes(stream, &[byte])? {
                    u64::from(byte)
                } else {
                    EOF
                })
            })
        });
    }

    let puts = Arc::clone(state);
    registry.register(UCRT, "puts", move |args, _, memory| {
        let mut text = read_c_string(memory, args.rcx, MAX_OUTPUT)?;
        text.push(b'\n');
        ensure_std_cookies(&puts, memory)?;
        let stdout = puts.stdio.lock().expect("stdio state").stdout;
        with_stream(&puts, memory, stdout, |stream, _| {
            Ok(if put_bytes(stream, &text)? { 0 } else { EOF })
        })
    });

    for symbol in ["fgetc", "getc"] {
        let getc = Arc::clone(state);
        registry.register(UCRT, symbol, move |args, _, memory| {
            with_stream(&getc, memory, args.rcx, |stream, _| {
                if !stream.readable {
                    stream.error = true;
                    return Ok(EOF);
                }
                match read_byte(stream) {
                    Some(byte) => Ok(u64::from(byte)),
                    None => {
                        stream.eof = true;
                        Ok(EOF)
                    }
                }
            })
        });
    }

    let ungetc = Arc::clone(state);
    registry.register(UCRT, "ungetc", move |args, _, memory| {
        let c = args.rcx as i32;
        if c == -1 {
            // Pushing back EOF fails and leaves the stream unchanged.
            return Ok(EOF);
        }
        with_stream(&ungetc, memory, args.rdx, |stream, _| {
            // One byte of push-back, after at least one byte has been read:
            // the byte replaces the one before the position, as far as reads
            // are concerned. A second push-back or one at the start fails.
            if !stream.readable
                || stream.kind != Kind::File
                || stream.pushback.is_some()
                || stream.position == 0
            {
                return Ok(EOF);
            }
            let byte = c as u8;
            stream.position -= 1;
            if stream.data.get(stream.position) != Some(&byte) {
                stream.pushback = Some(byte);
            }
            stream.eof = false;
            Ok(u64::from(byte))
        })
    });

    let fseek = Arc::clone(state);
    registry.register(UCRT, "fseek", move |args, cpu, memory| {
        let cookie = args.rcx;
        let offset = args.rdx as i32 as i64;
        let whence = args.r8 as i32;
        with_stream(&fseek, memory, cookie, |stream, memory| {
            if stream.kind != Kind::File {
                set_errno(memory, cpu.gs_base, EINVAL)?;
                return Ok(u64::from(u32::MAX));
            }
            let base = match whence {
                SEEK_SET => 0i64,
                SEEK_CUR => stream.position as i64,
                SEEK_END => stream.data.len() as i64,
                _ => {
                    set_errno(memory, cpu.gs_base, EINVAL)?;
                    return Ok(u64::from(u32::MAX));
                }
            };
            let Some(position) = base.checked_add(offset) else {
                set_errno(memory, cpu.gs_base, EINVAL)?;
                return Ok(u64::from(u32::MAX));
            };
            if position < 0 {
                set_errno(memory, cpu.gs_base, EINVAL)?;
                return Ok(u64::from(u32::MAX));
            }
            stream.position = position as usize;
            stream.pushback = None;
            stream.eof = false;
            Ok(0)
        })
    });

    let ftell = Arc::clone(state);
    registry.register(UCRT, "ftell", move |args, cpu, memory| {
        with_stream(&ftell, memory, args.rcx, |stream, memory| {
            if stream.kind != Kind::File {
                set_errno(memory, cpu.gs_base, EINVAL)?;
                return Ok(u64::from(u32::MAX));
            }
            Ok(stream.position as u64)
        })
    });

    let feof = Arc::clone(state);
    registry.register(UCRT, "feof", move |args, _, memory| {
        with_stream(&feof, memory, args.rcx, |stream, _| {
            Ok(u64::from(stream.eof))
        })
    });

    let ferror = Arc::clone(state);
    registry.register(UCRT, "ferror", move |args, _, memory| {
        with_stream(&ferror, memory, args.rcx, |stream, _| {
            Ok(u64::from(stream.error))
        })
    });

    let rewind = Arc::clone(state);
    registry.register(UCRT, "rewind", move |args, _, memory| {
        with_stream(&rewind, memory, args.rcx, |stream, _| {
            stream.position = 0;
            stream.pushback = None;
            stream.eof = false;
            stream.error = false;
            Ok(0)
        })
    });

    let iob = Arc::clone(state);
    registry.register(UCRT, "__acrt_iob_func", move |args, _, memory| {
        ensure_std_cookies(&iob, memory)?;
        let stdio = iob.stdio.lock().expect("stdio state");
        Ok(match args.rcx as u32 {
            0 => stdio.stdin,
            1 => stdio.stdout,
            2 => stdio.stderr,
            _ => 0,
        })
    });

    let fprintf = Arc::clone(state);
    registry.register(UCRT, "fprintf", move |args, cpu, memory| {
        ensure_std_cookies(&fprintf, memory)?;
        let cookie = args.rcx;
        let formatted = format_into(memory, args, args.rdx, 2)?;
        if cookie == 0 {
            set_errno(memory, cpu.gs_base, EINVAL)?;
            return Ok(u64::from(u32::MAX));
        }
        let count = formatted.len() as u64;
        with_stream(&fprintf, memory, cookie, |stream, _| {
            if !stream.writable || stream.kind == Kind::StdIn {
                stream.error = true;
                return Ok(u64::from(u32::MAX));
            }
            if matches!(stream.kind, Kind::StdOut | Kind::StdErr) {
                return Ok(count);
            }
            let end = stream
                .position
                .checked_add(formatted.len())
                .ok_or(ExitReason::IllegalInstruction(0xFE))?;
            if end > stream.max_bytes {
                stream.error = true;
                return Ok(u64::from(u32::MAX));
            }
            if end > stream.data.len() {
                stream.data.resize(end, 0);
            }
            stream.pushback = None;
            stream.data[stream.position..end].copy_from_slice(&formatted);
            stream.position = end;
            stream.eof = false;
            stream.dirty = stream.host_backed;
            Ok(count)
        })
    });

    let printf = Arc::clone(state);
    registry.register(UCRT, "printf", move |args, cpu, memory| {
        ensure_std_cookies(&printf, memory)?;
        let stdout = {
            let stdio = printf.stdio.lock().expect("stdio state");
            stdio.stdout
        };
        let formatted = format_into(memory, args, args.rcx, 1)?;
        let count = formatted.len() as u64;
        let writable = with_stream(&printf, memory, stdout, |stream, _| {
            if !stream.writable {
                stream.error = true;
                return Ok(false);
            }
            // stdout discards payload and reports the formatted length.
            Ok(true)
        })?;
        if !writable {
            set_errno(memory, cpu.gs_base, EINVAL)?;
            return Ok(u64::from(u32::MAX));
        }
        Ok(count)
    });

    for symbol in [
        "sprintf",
        "snprintf",
        "fopen",
        "fclose",
        "fread",
        "fwrite",
        "fseek",
        "ftell",
        "feof",
        "ferror",
        "rewind",
        "__acrt_iob_func",
        "fprintf",
        "printf",
        "fputc",
        "putc",
        "puts",
        "fgetc",
        "getc",
        "ungetc",
    ] {
        registry.alias(STDIO_API, UCRT, symbol);
        registry.set_status(UCRT, symbol, ServiceStatus::Partial);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{CpuState, Register};
    use crate::import_table::ImportName;
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
    fn sprintf_formats_the_supported_subset() {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register(&mut registry, &state);
        let mut memory = GuestMemory::new();
        let mut cpu = CpuState {
            gs_base: 0x10000,
            ..CpuState::default()
        };
        let buffer = memory.map_heap_allocation(PROCESS_HEAP, 64).unwrap();
        let format = memory.map_heap_allocation(PROCESS_HEAP, 32).unwrap();
        memory.write_bytes(format, b"%s-%d-%x\0").unwrap();
        let text = memory.map_heap_allocation(PROCESS_HEAP, 8).unwrap();
        memory.write_bytes(text, b"ok\0").unwrap();
        let stack = memory.map_heap_allocation(PROCESS_HEAP, 64).unwrap();
        cpu.set(Register::Rsp, stack);
        memory.write_u64(stack + 40, 0x2a).unwrap();
        let written = call(
            &registry,
            "sprintf",
            [buffer, format, text, 7],
            &mut cpu,
            &mut memory,
        );
        assert_eq!(written, 7);
        let mut out = [0u8; 8];
        memory.read_bytes(buffer, &mut out).unwrap();
        assert_eq!(&out, b"ok-7-2a\0");
    }

    #[test]
    fn fopen_fwrite_fread_round_trip_in_memory() {
        let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        let mut registry = ImportRegistry::new();
        register(&mut registry, &state);
        let mut memory = GuestMemory::new();
        let mut cpu = CpuState {
            gs_base: 0x10000,
            ..CpuState::default()
        };
        let path = memory.map_heap_allocation(PROCESS_HEAP, 16).unwrap();
        let mode_w = memory.map_heap_allocation(PROCESS_HEAP, 8).unwrap();
        let mode_r = memory.map_heap_allocation(PROCESS_HEAP, 8).unwrap();
        let payload = memory.map_heap_allocation(PROCESS_HEAP, 16).unwrap();
        let out = memory.map_heap_allocation(PROCESS_HEAP, 16).unwrap();
        memory.write_bytes(path, b"save.dat\0").unwrap();
        memory.write_bytes(mode_w, b"wb\0").unwrap();
        memory.write_bytes(mode_r, b"rb\0").unwrap();
        memory.write_bytes(payload, b"hello\0").unwrap();
        let file = call(
            &registry,
            "fopen",
            [path, mode_w, 0, 0],
            &mut cpu,
            &mut memory,
        );
        assert_ne!(file, 0);
        assert_eq!(
            call(
                &registry,
                "fwrite",
                [payload, 1, 5, file],
                &mut cpu,
                &mut memory
            ),
            5
        );
        assert_eq!(
            call(&registry, "fclose", [file, 0, 0, 0], &mut cpu, &mut memory),
            0
        );
        let reader = call(
            &registry,
            "fopen",
            [path, mode_r, 0, 0],
            &mut cpu,
            &mut memory,
        );
        assert_ne!(reader, 0);
        assert_eq!(
            call(
                &registry,
                "fread",
                [out, 1, 5, reader],
                &mut cpu,
                &mut memory
            ),
            5
        );
        let mut got = [0u8; 5];
        memory.read_bytes(out, &mut got).unwrap();
        assert_eq!(&got, b"hello");
        assert_eq!(
            call(
                &registry,
                "fclose",
                [reader, 0, 0, 0],
                &mut cpu,
                &mut memory
            ),
            0
        );
    }

    #[test]
    fn fopen_host_mount_round_trip() {
        let root =
            std::env::temp_dir().join(format!("xenora-crt-stdio-host-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut state = ServicesState::new(Arc::new(MockClock::new(0)));
        assert!(
            state
                .mount(host_files::HostMount::new('D', &root, true).unwrap())
                .is_ok()
        );
        let state = Arc::new(state);
        let mut registry = ImportRegistry::new();
        register(&mut registry, &state);
        let mut memory = GuestMemory::new();
        let mut cpu = CpuState {
            gs_base: 0x10000,
            ..CpuState::default()
        };
        let path = memory.map_heap_allocation(PROCESS_HEAP, 32).unwrap();
        let mode_w = memory.map_heap_allocation(PROCESS_HEAP, 8).unwrap();
        let mode_r = memory.map_heap_allocation(PROCESS_HEAP, 8).unwrap();
        let payload = memory.map_heap_allocation(PROCESS_HEAP, 16).unwrap();
        let out = memory.map_heap_allocation(PROCESS_HEAP, 16).unwrap();
        memory.write_bytes(path, b"D:\\guest.bin\0").unwrap();
        memory.write_bytes(mode_w, b"wb\0").unwrap();
        memory.write_bytes(mode_r, b"rb\0").unwrap();
        memory.write_bytes(payload, b"host!\0").unwrap();
        let file = call(
            &registry,
            "fopen",
            [path, mode_w, 0, 0],
            &mut cpu,
            &mut memory,
        );
        assert_ne!(file, 0);
        assert_eq!(
            call(
                &registry,
                "fwrite",
                [payload, 1, 5, file],
                &mut cpu,
                &mut memory
            ),
            5
        );
        assert_eq!(
            call(&registry, "fclose", [file, 0, 0, 0], &mut cpu, &mut memory),
            0
        );
        let on_disk = std::fs::read(root.join("guest.bin")).unwrap();
        assert_eq!(on_disk, b"host!");
        let reader = call(
            &registry,
            "fopen",
            [path, mode_r, 0, 0],
            &mut cpu,
            &mut memory,
        );
        assert_ne!(reader, 0);
        assert_eq!(
            call(
                &registry,
                "fread",
                [out, 1, 5, reader],
                &mut cpu,
                &mut memory
            ),
            5
        );
        let mut got = [0u8; 5];
        memory.read_bytes(out, &mut got).unwrap();
        assert_eq!(&got, b"host!");
        assert_eq!(
            call(
                &registry,
                "fclose",
                [reader, 0, 0, 0],
                &mut cpu,
                &mut memory
            ),
            0
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
