// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded synthetic C: drive with no host filesystem mapping, plus drives
//! backed by explicitly configured host directories (see `host_files`).

use super::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_NOACCESS,
    ERROR_NOT_ENOUGH_MEMORY, ERROR_NOT_SUPPORTED, GuestObject, KERNEL32, ServicesState, host_files,
};
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{GuestMemory, MemoryError};
use std::collections::BTreeSet;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

const FILE_API: &str = "api-ms-win-core-file-l1-1-0.dll";
const INVALID_HANDLE: u64 = u64::MAX;
const READ: u32 = 0x8000_0000;
const WRITE: u32 = 0x4000_0000;
const FILE_NOT_FOUND: u32 = 2;
const PATH_NOT_FOUND: u32 = 3;
const DIRECTORY_NOT_EMPTY: u32 = 145;
const FILE_EXISTS: u32 = 80;
const ALREADY_EXISTS: u32 = 183;
const INVALID_NAME: u32 = 123;
const NEGATIVE_SEEK: u32 = 131;
const SHARING_VIOLATION: u32 = 32;
const DISK_FULL: u32 = 112;
const MAX_FILES: usize = 256;
const MAX_DIRECTORIES: usize = 256;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_TRANSFER: usize = 1024 * 1024;

pub(super) struct SyntheticFile {
    data: Vec<u8>,
    times: [u64; 3],
}

impl SyntheticFile {
    fn new(now: u64) -> Self {
        Self {
            data: Vec::new(),
            times: [now; 3],
        }
    }

    /// The file's contents.
    pub(super) fn data(&self) -> &[u8] {
        &self.data
    }
}

#[derive(Clone)]
pub(super) struct OpenFile {
    path: String,
    instance: Arc<FileInstance>,
    open_access: u32,
    pub(super) access: u32,
    share: u32,
    /// Set for files on a host mount; `path` is then a label only.
    host: Option<Arc<HostFile>>,
}

/// Duplicated handles share this state. The object table serializes I/O.
#[derive(Default)]
struct FileInstance {
    position: AtomicU64,
    suppressed_times: AtomicU8,
}

/// An open host file and the size limit of the mount it came from.
pub(super) struct HostFile {
    file: File,
    max_size: u64,
    budget: Arc<host_files::Budget>,
}

impl HostFile {
    fn len(&self) -> Result<u64, u32> {
        self.file
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|error| host_files::win32_error(&error))
    }

    /// Change the file to `length` bytes, charging growth to the mount.
    fn set_len(&self, length: u64) -> Result<(), u32> {
        let old = self.len()?;
        self.budget.resize(old, length)?;
        if let Err(error) = self.file.set_len(length) {
            let now = self.len().unwrap_or(old);
            let _ = self.budget.resize(length, now);
            return Err(host_files::win32_error(&error));
        }
        Ok(())
    }
}

impl OpenFile {
    fn updates_time(&self, index: usize) -> bool {
        self.instance.suppressed_times.load(Ordering::Relaxed) & (1 << index) == 0
    }

    fn host_len(&self) -> Option<Result<u64, u32>> {
        let file = &self.host.as_ref()?.file;
        Some(
            file.metadata()
                .map(|metadata| metadata.len())
                .map_err(|error| host_files::win32_error(&error)),
        )
    }
}

/// What a sharing check compares against: a synthetic path, or the current
/// identity of a host file.
enum Target<'a> {
    Path(&'a str),
    Host(host_files::FileId),
}

impl Target<'_> {
    fn matches(&self, file: &OpenFile) -> bool {
        match (self, &file.host) {
            (Self::Path(path), None) => file.path == *path,
            // An unreadable identity counts as a match, so checks fail closed.
            (Self::Host(id), Some(host)) => {
                host_files::current_id(&host.file).is_none_or(|current| current == *id)
            }
            _ => false,
        }
    }
}

fn check_sharing(
    objects: &super::ObjectState,
    target: &Target<'_>,
    access: u32,
    share: u32,
) -> bool {
    !objects.handles.values().any(|object| {
        matches!(object, GuestObject::File(file)
            if target.matches(file)
                && (share_mask(access) & !file.share != 0
                    || share_mask(file.open_access) & !share != 0))
    })
}

fn is_open(objects: &super::ObjectState, target: &Target<'_>) -> bool {
    objects
        .handles
        .values()
        .any(|object| matches!(object, GuestObject::File(file) if target.matches(file)))
}

fn create_host_file(
    state: &ServicesState,
    teb: u64,
    resolved: Result<host_files::Resolved<'_>, u32>,
    access: u32,
    share: u32,
    disposition: u32,
) -> Result<u64, ExitReason> {
    let resolved = match resolved {
        Ok(resolved) => resolved,
        Err(error) => return state.fail(teb, error, INVALID_HANDLE),
    };
    let Some(name) = resolved.components.last().cloned() else {
        return state.fail(teb, ERROR_ACCESS_DENIED, INVALID_HANDLE);
    };
    let (parent, entry) = match resolved.lookup() {
        Ok(found) => found,
        Err(error) => return state.fail(teb, error, INVALID_HANDLE),
    };
    let exists = match &entry {
        Some(entry) if entry.kind == host_files::Kind::Directory => {
            return state.fail(teb, ERROR_ACCESS_DENIED, INVALID_HANDLE);
        }
        Some(_) => true,
        None => false,
    };
    if exists && disposition == 1 {
        return state.fail(teb, FILE_EXISTS, INVALID_HANDLE);
    }
    if !exists && matches!(disposition, 3 | 5) {
        return state.fail(teb, FILE_NOT_FOUND, INVALID_HANDLE);
    }
    let modifies = access & WRITE != 0 || !exists || matches!(disposition, 2 | 5);
    if modifies && !resolved.mount.writable() {
        return state.fail(teb, ERROR_ACCESS_DENIED, INVALID_HANDLE);
    }
    let mut objects = state.objects.lock().expect("guest object state");
    // Sharing is decided by host file identity. An existing file is checked
    // before it is opened, so a refused open never truncates it.
    let expected = entry.as_ref().map(|entry| entry.id);
    if let Some(id) = expected
        && !check_sharing(&objects, &Target::Host(id), access, share)
    {
        return state.fail(teb, SHARING_VIOLATION, INVALID_HANDLE);
    }
    let Some(handle) = objects.next_handle() else {
        return state.fail(teb, ERROR_NOT_ENOUGH_MEMORY, INVALID_HANDLE);
    };
    let request = host_files::OpenRequest {
        read: access & READ != 0 || access & WRITE == 0,
        write: access & WRITE != 0,
        create: !exists,
    };
    let host_name = entry.map_or(name, |entry| entry.name);
    let budget = Arc::clone(resolved.mount.budget());
    if !exists && let Err(error) = budget.add_entry() {
        return state.fail(teb, error, INVALID_HANDLE);
    }
    let (file, id) = match host_files::open_file(&parent, &host_name, &request, expected) {
        Ok(opened) => opened,
        Err(error) => {
            if !exists {
                budget.remove_entry(0);
            }
            return state.fail(teb, host_files::win32_error(&error), INVALID_HANDLE);
        }
    };
    let host = HostFile {
        file,
        max_size: resolved.mount.max_file_size(),
        budget,
    };
    let key = id.key();
    if !check_sharing(&objects, &Target::Host(id), access, share) {
        return state.fail(teb, SHARING_VIOLATION, INVALID_HANDLE);
    }
    if exists
        && matches!(disposition, 2 | 5)
        && let Err(error) = host.set_len(0)
    {
        return state.fail(teb, error, INVALID_HANDLE);
    }
    if matches!(disposition, 2 | 4) {
        state.fail(teb, if exists { ALREADY_EXISTS } else { 0 }, 0)?;
    }
    objects.insert_handle(
        handle,
        GuestObject::File(OpenFile {
            path: key,
            instance: Arc::default(),
            open_access: access,
            access,
            share,
            host: Some(Arc::new(host)),
        }),
    );
    Ok(handle)
}

/// Open a search handle over `entries` and return the first through `output`.
fn open_search(
    state: &ServicesState,
    teb: u64,
    memory: &mut GuestMemory,
    output: u64,
    entries: Vec<SearchEntry>,
) -> Result<u64, ExitReason> {
    let Some(first) = entries.first() else {
        return state.fail(teb, FILE_NOT_FOUND, INVALID_HANDLE);
    };
    let mut objects = state.objects.lock().expect("guest object state");
    if objects
        .handles
        .values()
        .filter(|object| matches!(object, GuestObject::Search(_)))
        .count()
        >= 32
    {
        return state.fail(teb, ERROR_NOT_ENOUGH_MEMORY, INVALID_HANDLE);
    }
    let Some(handle) = objects.next_handle() else {
        return state.fail(teb, ERROR_NOT_ENOUGH_MEMORY, INVALID_HANDLE);
    };
    if write_find_data(memory, output, first).is_err() {
        return state.fail(teb, ERROR_NOACCESS, INVALID_HANDLE);
    }
    objects.insert_handle(handle, GuestObject::Search(Search { entries, next: 1 }));
    Ok(handle)
}

#[derive(Clone, Copy)]
enum HostChange {
    CreateDirectory,
    RemoveDirectory,
    DeleteFile,
}

fn change_host_namespace(
    state: &ServicesState,
    teb: u64,
    resolved: Result<host_files::Resolved<'_>, u32>,
    change: HostChange,
) -> Result<u64, ExitReason> {
    const ERROR_DIRECTORY: u32 = 267;
    let resolved = match resolved {
        Ok(resolved) => resolved,
        Err(error) => return state.fail(teb, error, 0),
    };
    let Some(name) = resolved.components.last().cloned() else {
        return state.fail(teb, ERROR_ACCESS_DENIED, 0);
    };
    if !resolved.mount.writable() {
        return state.fail(teb, ERROR_ACCESS_DENIED, 0);
    }
    let (parent, entry) = match resolved.lookup() {
        Ok(found) => found,
        Err(error) => return state.fail(teb, error, 0),
    };
    use host_files::Kind::{Directory, File};
    let budget = resolved.mount.budget();
    let result = match (change, entry) {
        (HostChange::CreateDirectory, Some(_)) => return state.fail(teb, ALREADY_EXISTS, 0),
        (HostChange::CreateDirectory, None) => {
            if let Err(error) = budget.add_entry() {
                return state.fail(teb, error, 0);
            }
            match host_files::create_directory(&parent, &name) {
                Ok(()) => Ok(()),
                Err(error) => {
                    budget.remove_entry(0);
                    if error.raw_os_error() == Some(libc::EEXIST) {
                        return state.fail(teb, ALREADY_EXISTS, 0);
                    }
                    Err(error)
                }
            }
        }
        (_, None) => return state.fail(teb, FILE_NOT_FOUND, 0),
        (HostChange::RemoveDirectory, Some(entry)) if entry.kind == File => {
            return state.fail(teb, ERROR_DIRECTORY, 0);
        }
        (HostChange::RemoveDirectory, Some(entry)) => {
            host_files::remove(&parent, &entry.name, &entry).map(|()| budget.remove_entry(0))
        }
        (HostChange::DeleteFile, Some(entry)) if entry.kind == Directory => {
            return state.fail(teb, ERROR_ACCESS_DENIED, 0);
        }
        (HostChange::DeleteFile, Some(entry)) => {
            let objects = state.objects.lock().expect("guest object state");
            if is_open(&objects, &Target::Host(entry.id)) {
                return state.fail(teb, SHARING_VIOLATION, 0);
            }
            host_files::remove(&parent, &entry.name, &entry)
                .map(|()| budget.remove_entry(entry.size))
        }
    };
    match result {
        Ok(()) => Ok(1),
        Err(error) => state.fail(teb, host_files::win32_error(&error), 0),
    }
}

fn host_attributes(entry: &host_files::Entry, writable: bool) -> u32 {
    match (entry.kind, writable) {
        (host_files::Kind::Directory, _) => 0x10, // FILE_ATTRIBUTE_DIRECTORY
        (host_files::Kind::File, true) => 0x80,   // FILE_ATTRIBUTE_NORMAL
        (host_files::Kind::File, false) => 0x01,  // FILE_ATTRIBUTE_READONLY
    }
}

fn file_metadata(state: &ServicesState, raw: &str) -> Result<(u32, u64, [u64; 3]), u32> {
    if let Some(resolved) = host_files::resolve(&state.mounts, raw, valid_component) {
        let resolved = resolved?;
        let (_, entry) = resolved.lookup()?;
        let entry = entry.ok_or(FILE_NOT_FOUND)?;
        return Ok((
            host_attributes(&entry, resolved.mount.writable()),
            entry.size,
            entry.times,
        ));
    }

    let objects = state.objects.lock().expect("guest object state");
    let path = resolve_file_path(raw, &objects.current_directory).ok_or(INVALID_NAME)?;
    if objects.directories.contains(&path) {
        return Ok((0x10, 0, [0; 3])); // FILE_ATTRIBUTE_DIRECTORY
    }
    if let Some(file) = objects.files.get(&path) {
        return Ok((0x80, file.data.len() as u64, file.times)); // FILE_ATTRIBUTE_NORMAL
    }
    let parent = parent(&path);
    Err(
        if parent.is_empty() || objects.directories.contains(parent) {
            FILE_NOT_FOUND
        } else {
            PATH_NOT_FOUND
        },
    )
}

/// FindFirstFileW on a host mount: a sorted snapshot of matching names.
fn find_host_entries(state: &ServicesState, raw: &str) -> Option<Result<Vec<SearchEntry>, u32>> {
    let (directory, pattern) = raw.rsplit_once('\\')?;
    let directory = if directory.len() == 2 {
        format!("{directory}\\")
    } else {
        directory.to_owned()
    };
    let resolved = host_files::resolve(&state.mounts, &directory, valid_component)?;
    Some((|| {
        let resolved = resolved?;
        let pattern = pattern.to_ascii_lowercase();
        if !valid_component(&pattern.replace(['*', '?'], "x")) {
            return Err(INVALID_NAME);
        }
        let pattern = if pattern == "*.*" {
            "*".to_owned()
        } else {
            pattern
        };
        let directory = resolved.directory()?;
        let writable = resolved.mount.writable();
        let mut entries: Vec<_> = host_files::list(&directory)
            .map_err(|error| host_files::win32_error(&error))?
            .into_iter()
            .filter(|entry| wildcard_match(&pattern, &entry.name.to_ascii_lowercase()))
            .map(|entry| SearchEntry {
                attributes: host_attributes(&entry, writable),
                size: entry.size,
                times: entry.times,
                name: entry.name,
            })
            .collect();
        entries.sort_by_key(|entry| entry.name.to_ascii_lowercase());
        Ok(entries)
    })())
}

/// Read or write a host file at `position`, transferring as much as the file
/// allows. Reads stop early only at the end of the file.
fn host_transfer(
    file: &File,
    position: u64,
    buffer: &mut [u8],
    writing: bool,
) -> Result<usize, u32> {
    let result = if writing {
        file.write_all_at(buffer, position).map(|()| buffer.len())
    } else {
        let mut done = 0;
        loop {
            match file.read_at(&mut buffer[done..], position + done as u64) {
                Ok(0) => break Ok(done),
                Ok(read) => {
                    done += read;
                    if done == buffer.len() {
                        break Ok(done);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => break Err(error),
            }
        }
    };
    result.map_err(|error| host_files::win32_error(&error))
}

/// A NUL-terminated UTF-16 path of at most 259 units, with `/` read as `\`
/// and its case preserved.
fn read_raw_path(memory: &GuestMemory, address: u64) -> Result<Option<String>, MemoryError> {
    if address == 0 {
        return Ok(None);
    }
    let mut units = Vec::new();
    for index in 0..260 {
        let slot = address
            .checked_add(index * 2)
            .ok_or(MemoryError::Overflow)?;
        let unit = memory.read_u16(slot)?;
        if unit == 0 {
            let Ok(path) = String::from_utf16(&units) else {
                return Ok(None);
            };
            return Ok(Some(path.replace('/', "\\")));
        }
        units.push(unit);
    }
    Ok(None)
}

/// A path on the synthetic C: drive, folded to lowercase.
fn read_guest_path(
    memory: &GuestMemory,
    address: u64,
    cwd: &str,
) -> Result<Option<String>, MemoryError> {
    Ok(read_raw_path(memory, address)?.and_then(|path| resolve_synthetic_path(&path, cwd)))
}

fn resolve_synthetic_path(path: &str, cwd: &str) -> Option<String> {
    if path.is_empty() || path.starts_with("\\\\") {
        return None;
    }
    let path = path.to_ascii_lowercase();
    Some(
        if let Some(path) = path
            .strip_prefix("c:\\")
            .or_else(|| path.strip_prefix('\\'))
        {
            path.to_owned()
        } else if cwd.is_empty() {
            path
        } else {
            format!("{cwd}\\{path}")
        },
    )
}

fn read_ascii_path(memory: &GuestMemory, address: u64) -> Result<Option<String>, MemoryError> {
    if address == 0 {
        return Ok(None);
    }
    let mut path = String::new();
    for offset in 0..260 {
        let byte = memory.read_u8(address.checked_add(offset).ok_or(MemoryError::Overflow)?)?;
        match byte {
            0 => return Ok(Some(path)),
            b'/' => path.push('\\'),
            byte if byte.is_ascii() => path.push(char::from(byte)),
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// The resolved path when `address` names a drive backed by a host mount.
const MOVEFILE_REPLACE_EXISTING: u32 = 1;
const MOVEFILE_COPY_ALLOWED: u32 = 2;
const MOVEFILE_WRITE_THROUGH: u32 = 8;
const ERROR_NOT_SAME_DEVICE: u32 = 17;

/// The failure for a move between drives: Windows copies when
/// `MOVEFILE_COPY_ALLOWED` is set, which is not supported here.
fn cross_drive(flags: u32) -> u32 {
    if flags & MOVEFILE_COPY_ALLOWED != 0 {
        ERROR_NOT_SUPPORTED
    } else {
        ERROR_NOT_SAME_DEVICE
    }
}

/// MoveFileExW within one writable mount: a file or directory keeps its
/// contents and identity under the new name. With
/// `MOVEFILE_REPLACE_EXISTING` a closed file may replace another closed file;
/// `MOVEFILE_WRITE_THROUGH` flushes both directories before returning. An
/// open file can neither move nor be replaced.
fn move_host_entry(
    state: &ServicesState,
    teb: u64,
    source: Result<host_files::Resolved<'_>, u32>,
    target: Result<host_files::Resolved<'_>, u32>,
    flags: u32,
) -> Result<u64, ExitReason> {
    let (source, target) = match (source, target) {
        (Ok(source), Ok(target)) => (source, target),
        (Err(error), _) | (_, Err(error)) => return state.fail(teb, error, 0),
    };
    if !std::ptr::eq(source.mount, target.mount) {
        return state.fail(teb, cross_drive(flags), 0);
    }
    let Some(to) = target.components.last() else {
        return state.fail(teb, ERROR_ACCESS_DENIED, 0);
    };
    if source.components.is_empty() || !source.mount.writable() {
        return state.fail(teb, ERROR_ACCESS_DENIED, 0);
    }
    let (from_directory, entry) = match source.lookup() {
        Ok((directory, Some(entry))) => (directory, entry),
        Ok((_, None)) => return state.fail(teb, FILE_NOT_FOUND, 0),
        Err(error) => return state.fail(teb, error, 0),
    };
    let (to_directory, existing) = match target.lookup() {
        Ok(found) => found,
        Err(error) => return state.fail(teb, error, 0),
    };
    let objects = state.objects.lock().expect("guest object state");
    if entry.kind == host_files::Kind::File && is_open(&objects, &Target::Host(entry.id)) {
        return state.fail(teb, SHARING_VIOLATION, 0);
    }
    let replacing = match existing {
        Some(old) if old.id != entry.id => {
            if flags & MOVEFILE_REPLACE_EXISTING == 0 {
                return state.fail(teb, ALREADY_EXISTS, 0);
            }
            if old.kind == host_files::Kind::Directory || entry.kind == host_files::Kind::Directory
            {
                return state.fail(teb, ERROR_ACCESS_DENIED, 0);
            }
            if is_open(&objects, &Target::Host(old.id)) {
                return state.fail(teb, SHARING_VIOLATION, 0);
            }
            Some(old)
        }
        _ => None,
    };
    let renamed = host_files::rename(
        &from_directory,
        &entry.name,
        &entry,
        &to_directory,
        to,
        replacing.as_ref(),
    );
    match renamed {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {
            return state.fail(teb, ALREADY_EXISTS, 0);
        }
        Err(error) => return state.fail(teb, host_files::win32_error(&error), 0),
    }
    if let Some(old) = &replacing {
        source.mount.budget().remove_entry(old.size);
    }
    if flags & MOVEFILE_WRITE_THROUGH != 0 {
        let synced = host_files::sync_directory(&to_directory)
            .and_then(|()| host_files::sync_directory(&from_directory));
        if let Err(error) = synced {
            return state.fail(teb, host_files::win32_error(&error), 0);
        }
    }
    Ok(1)
}

fn move_entry(
    state: &ServicesState,
    teb: u64,
    memory: &GuestMemory,
    from: u64,
    to: u64,
    flags: u32,
) -> Result<u64, ExitReason> {
    if flags & !(MOVEFILE_REPLACE_EXISTING | MOVEFILE_COPY_ALLOWED | MOVEFILE_WRITE_THROUGH) != 0 {
        return state.fail(teb, ERROR_NOT_SUPPORTED, 0);
    }
    match (
        host_path(state, memory, from)?,
        host_path(state, memory, to)?,
    ) {
        (None, None) => {}
        (Some(source), Some(target)) => {
            return move_host_entry(state, teb, source, target, flags);
        }
        _ => return state.fail(teb, cross_drive(flags), 0),
    }
    let mut objects = state.objects.lock().expect("guest object state");
    let Some(source) =
        guest_path(memory, from, &objects.current_directory).map_err(ExitReason::MemoryFault)?
    else {
        return state.fail(teb, INVALID_NAME, 0);
    };
    let Some(target) =
        guest_path(memory, to, &objects.current_directory).map_err(ExitReason::MemoryFault)?
    else {
        return state.fail(teb, INVALID_NAME, 0);
    };
    if [parent(&source), parent(&target)]
        .into_iter()
        .any(|path| !path.is_empty() && !objects.directories.contains(path))
    {
        return state.fail(teb, PATH_NOT_FOUND, 0);
    }
    if objects.directories.contains(&source) {
        return state.fail(teb, ERROR_NOT_SUPPORTED, 0);
    }
    if !objects.files.contains_key(&source) {
        return state.fail(teb, FILE_NOT_FOUND, 0);
    }
    let open = |path: &str| {
        objects
            .handles
            .values()
            .any(|object| matches!(object, GuestObject::File(file) if file.path == path))
    };
    if open(&source) {
        return state.fail(teb, SHARING_VIOLATION, 0);
    }
    if source != target {
        if objects.directories.contains(&target) {
            let error = if flags & MOVEFILE_REPLACE_EXISTING == 0 {
                ALREADY_EXISTS
            } else {
                ERROR_ACCESS_DENIED
            };
            return state.fail(teb, error, 0);
        }
        if objects.files.contains_key(&target) {
            if flags & MOVEFILE_REPLACE_EXISTING == 0 {
                return state.fail(teb, ALREADY_EXISTS, 0);
            }
            if open(&target) {
                return state.fail(teb, SHARING_VIOLATION, 0);
            }
        }
    }
    // The synthetic drive lives in memory, so write-through needs no flush.
    let data = objects.files.remove(&source).expect("source file exists");
    objects.files.insert(target, data);
    Ok(1)
}

fn host_path<'a>(
    state: &'a ServicesState,
    memory: &GuestMemory,
    address: u64,
) -> Result<Option<Result<host_files::Resolved<'a>, u32>>, ExitReason> {
    let Some(raw) = read_raw_path(memory, address).map_err(ExitReason::MemoryFault)? else {
        return Ok(None);
    };
    Ok(host_files::resolve(&state.mounts, &raw, valid_component))
}

fn guest_path(
    memory: &GuestMemory,
    address: u64,
    cwd: &str,
) -> Result<Option<String>, MemoryError> {
    Ok(read_raw_path(memory, address)?.and_then(|raw| resolve_file_path(&raw, cwd)))
}

fn resolve_file_path(raw: &str, cwd: &str) -> Option<String> {
    resolve_synthetic_path(raw, cwd).filter(|path| path.split('\\').all(valid_component))
}

struct SearchEntry {
    times: [u64; 3],
    name: String,
    attributes: u32,
    size: u64,
}

pub(super) struct Search {
    entries: Vec<SearchEntry>,
    next: usize,
}

fn write_file_times(bytes: &mut [u8], times: [u64; 3]) {
    for (slot, time) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(times) {
        slot.copy_from_slice(&time.to_le_bytes());
    }
}

fn write_find_data(
    memory: &mut GuestMemory,
    output: u64,
    entry: &SearchEntry,
) -> Result<(), MemoryError> {
    if output == 0 {
        return Err(MemoryError::UnmappedAddress(0));
    }
    // WIN32_FIND_DATAW: attributes, three FILETIMEs, size high/low,
    // two reserved DWORDs, WCHAR[260], then WCHAR[14].
    let mut bytes = [0u8; 592];
    bytes[..4].copy_from_slice(&entry.attributes.to_le_bytes());
    write_file_times(&mut bytes[4..28], entry.times);
    bytes[28..32].copy_from_slice(&((entry.size >> 32) as u32).to_le_bytes());
    bytes[32..36].copy_from_slice(&(entry.size as u32).to_le_bytes());
    for (slot, unit) in bytes[44..564]
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(entry.name.encode_utf16())
    {
        slot.copy_from_slice(&unit.to_le_bytes());
    }
    memory.write_bytes(output, &bytes)
}

fn wildcard_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<_> = pattern.encode_utf16().collect();
    let name: Vec<_> = name.encode_utf16().collect();
    let (mut p, mut n, mut retry) = (0, 0, None);
    while n < name.len() {
        match pattern.get(p).copied() {
            Some(42) => {
                p += 1;
                retry = Some((p, n));
            }
            Some(unit) if unit == 63 || unit == name[n] => {
                p += 1;
                n += 1;
            }
            _ => {
                let Some((after_star, previous)) = retry else {
                    return false;
                };
                n = previous + 1;
                p = after_star;
                retry = Some((p, n));
            }
        }
    }
    pattern[p..].iter().all(|&unit| unit == 42)
}

fn valid_component(name: &str) -> bool {
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

fn parent(path: &str) -> &str {
    path.rsplit_once('\\').map_or("", |(parent, _)| parent)
}

fn share_mask(access: u32) -> u32 {
    u32::from(access & READ != 0) | (u32::from(access & WRITE != 0) << 1)
}

fn temp_file_name(
    state: &ServicesState,
    memory: &mut GuestMemory,
    teb: u64,
    path: u64,
    prefix: u64,
    unique: u32,
    output: u64,
) -> Result<u64, ExitReason> {
    let Some(path) = read_raw_path(memory, path).map_err(ExitReason::MemoryFault)? else {
        return state.fail(teb, INVALID_NAME, 0);
    };
    if path.encode_utf16().count() > 246 {
        return state.fail(teb, 111, 0); // ERROR_BUFFER_OVERFLOW
    }
    let Some(prefix) = read_raw_path(memory, prefix).map_err(ExitReason::MemoryFault)? else {
        return state.fail(teb, INVALID_NAME, 0);
    };
    let prefix: String = prefix.chars().take(3).collect();
    if !prefix.is_ascii() || !valid_component(&format!("{prefix}0000.TMP")) {
        return state.fail(teb, INVALID_NAME, 0);
    }
    if let Some(resolved) = host_files::resolve(&state.mounts, &path, valid_component) {
        let result = resolved.and_then(|resolved| {
            if unique == 0 && !resolved.mount.writable() {
                return Err(ERROR_ACCESS_DENIED);
            }
            let directory = resolved.directory()?;
            let budget = resolved.mount.budget();
            let occupied = if unique == 0 {
                host_files::list(&directory)
                    .map_err(|error| host_files::win32_error(&error))?
                    .into_iter()
                    .map(|entry| entry.name.to_ascii_lowercase())
                    .collect::<BTreeSet<_>>()
            } else {
                BTreeSet::new()
            };
            write_temp_file_name(
                memory,
                output,
                &path,
                &prefix,
                unique,
                state.clock.now_filetime(),
                |name| {
                    if occupied.contains(&name.to_ascii_lowercase()) {
                        return Ok(false);
                    }
                    let request = host_files::OpenRequest {
                        read: false,
                        write: true,
                        create: true,
                    };
                    budget.add_entry()?;
                    let created = match host_files::open_file(&directory, name, &request, None) {
                        Ok(_) => return Ok(true),
                        Err(error) if error.raw_os_error() == Some(libc::EEXIST) => Ok(false),
                        Err(error) => Err(host_files::win32_error(&error)),
                    };
                    budget.remove_entry(0);
                    created
                },
            )
        });
        return match result {
            Ok(number) => Ok(number),
            Err(error) => state.fail(teb, error, 0),
        };
    }

    let mut objects = state.objects.lock().expect("guest object state");
    let current = &objects.current_directory;
    let directory = if path == "." {
        current.as_str().to_owned()
    } else {
        let Some(directory) = resolve_synthetic_path(&path, current) else {
            return state.fail(teb, INVALID_NAME, 0);
        };
        directory.trim_end_matches('\\').to_owned()
    };
    if !directory.is_empty() && !objects.directories.contains(&directory) {
        return state.fail(teb, PATH_NOT_FOUND, 0);
    }
    if unique == 0 && objects.files.len() >= MAX_FILES {
        return state.fail(teb, DISK_FULL, 0);
    }

    let path = if path == "." {
        format!("C:\\{directory}")
    } else {
        path
    };
    let result = write_temp_file_name(
        memory,
        output,
        &path,
        &prefix,
        unique,
        state.clock.now_filetime(),
        |name| {
            let name = name.to_ascii_lowercase();
            let key = if directory.is_empty() {
                name
            } else {
                format!("{directory}\\{name}")
            };
            if objects.files.contains_key(&key) || objects.directories.contains(&key) {
                return Ok(false);
            }
            objects
                .files
                .insert(key, SyntheticFile::new(state.clock.now_filetime()));
            Ok(true)
        },
    );
    match result {
        Ok(number) => Ok(number),
        Err(error) => state.fail(teb, error, 0),
    }
}

fn write_temp_file_name(
    memory: &mut GuestMemory,
    output: u64,
    path: &str,
    prefix: &str,
    unique: u32,
    now: u64,
    mut create: impl FnMut(&str) -> Result<bool, u32>,
) -> Result<u64, u32> {
    let separator = if path.ends_with('\\') { "" } else { "\\" };
    let start = (now as u16).max(1);
    let count = if unique == 0 { u32::from(u16::MAX) } else { 1 };
    for offset in 0..count {
        let number = if unique == 0 {
            ((u32::from(start) - 1 + offset) % u32::from(u16::MAX) + 1) as u16
        } else {
            unique as u16
        };
        let name = format!("{prefix}{number:04X}.TMP");
        let units = format!("{path}{separator}{name}");
        if units.encode_utf16().count() >= 260 {
            return Err(111); // ERROR_BUFFER_OVERFLOW
        }
        let bytes: Vec<_> = units
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        if output == 0 || memory.check_write(output, bytes.len()).is_err() {
            return Err(ERROR_NOACCESS);
        }
        if unique == 0 && !create(&name)? {
            continue;
        }
        memory
            .write_bytes(output, &bytes)
            .map_err(|_| ERROR_NOACCESS)?;
        return Ok(if unique == 0 {
            u64::from(number)
        } else {
            u64::from(unique)
        });
    }
    Err(FILE_EXISTS)
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let temp = Arc::clone(state);
    registry.register_with_status(
        KERNEL32,
        "GetTempFileNameW",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            temp_file_name(
                &temp,
                memory,
                cpu.gs_base,
                args.rcx,
                args.rdx,
                args.r8 as u32,
                args.r9,
            )
        },
    );
    registry.alias(FILE_API, KERNEL32, "GetTempFileNameW");

    for (symbol, wide) in [("GetTempPathA", false), ("GetTempPathW", true)] {
        let state = Arc::clone(state);
        registry.register_with_status(
            KERNEL32,
            symbol,
            ServiceStatus::Partial,
            move |args, cpu, memory| {
                let mut path = {
                    let env = state.env.lock().expect("guest environment");
                    ["TMP", "TEMP", "USERPROFILE"]
                        .into_iter()
                        .find_map(|key| env.get(key).filter(|value| !value.is_empty()))
                        .unwrap_or(r"C:\")
                        .replace('/', "\\")
                };
                if !path.ends_with('\\') {
                    path.push('\\');
                }
                let (bytes, required) = if wide {
                    let bytes: Vec<_> = path
                        .encode_utf16()
                        .chain([0])
                        .flat_map(u16::to_le_bytes)
                        .collect();
                    let required = bytes.len() / 2;
                    (bytes, required)
                } else {
                    if !path.is_ascii() {
                        return state.fail(cpu.gs_base, INVALID_NAME, 0);
                    }
                    let mut bytes = path.into_bytes();
                    bytes.push(0);
                    let required = bytes.len();
                    (bytes, required)
                };
                if required > 261 {
                    return state.fail(cpu.gs_base, host_files::ERROR_FILENAME_EXCED_RANGE, 0);
                }
                if (args.rcx as u32 as usize) < required {
                    return Ok(required as u64);
                }
                if args.rdx == 0 || memory.write_bytes(args.rdx, &bytes).is_err() {
                    return state.fail(cpu.gs_base, ERROR_NOACCESS, 0);
                }
                Ok((required - 1) as u64)
            },
        );
        registry.alias(FILE_API, KERNEL32, symbol);
    }

    for (get, set, wide) in [
        ("GetCurrentDirectoryW", "SetCurrentDirectoryW", true),
        ("GetCurrentDirectoryA", "SetCurrentDirectoryA", false),
    ] {
        let get_cwd = Arc::clone(state);
        registry.register_with_status(
            KERNEL32,
            get,
            ServiceStatus::Partial,
            move |args, cpu, memory| {
                let objects = get_cwd.objects.lock().expect("guest object state");
                let path = format!("C:\\{}", objects.current_directory);
                let (bytes, required) = if wide {
                    let bytes: Vec<_> = path
                        .encode_utf16()
                        .chain([0])
                        .flat_map(u16::to_le_bytes)
                        .collect();
                    let required = bytes.len() / 2;
                    (bytes, required)
                } else {
                    if !path.is_ascii() {
                        return get_cwd.fail(cpu.gs_base, INVALID_NAME, 0);
                    }
                    let mut bytes = path.into_bytes();
                    bytes.push(0);
                    let required = bytes.len();
                    (bytes, required)
                };
                if (args.rcx as u32 as usize) < required {
                    return Ok(required as u64);
                }
                if args.rdx == 0 || memory.write_bytes(args.rdx, &bytes).is_err() {
                    return get_cwd.fail(cpu.gs_base, ERROR_NOACCESS, 0);
                }
                Ok((required - 1) as u64)
            },
        );
        let set_cwd = Arc::clone(state);
        registry.register_with_status(
            KERNEL32,
            set,
            ServiceStatus::Partial,
            move |args, cpu, memory| {
                let raw = if wide {
                    read_raw_path(memory, args.rcx)
                } else {
                    read_ascii_path(memory, args.rcx)
                }
                .map_err(ExitReason::MemoryFault)?;
                let mut objects = set_cwd.objects.lock().expect("guest object state");
                let Some(path) = raw
                    .as_deref()
                    .and_then(|path| resolve_synthetic_path(path, &objects.current_directory))
                else {
                    return set_cwd.fail(cpu.gs_base, INVALID_NAME, 0);
                };
                let path = path.trim_end_matches('\\');
                if !path.is_empty()
                    && (!path.split('\\').all(valid_component)
                        || !objects.directories.contains(path))
                {
                    return set_cwd.fail(cpu.gs_base, PATH_NOT_FOUND, 0);
                }
                objects.current_directory = path.to_owned();
                Ok(1)
            },
        );
    }

    for (symbol, read_path) in [
        ("CreateFileA", read_ascii_path as fn(&GuestMemory, u64) -> _),
        ("CreateFileW", read_raw_path),
    ] {
        let open = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let access = args.rdx as u32;
            let share = args.r8 as u32;
            let disposition = args.get(4, memory).map_err(ExitReason::MemoryFault)? as u32;
            let flags = args.get(5, memory).map_err(ExitReason::MemoryFault)? as u32;
            let template = args.get(6, memory).map_err(ExitReason::MemoryFault)?;
            if args.r9 != 0
                || template != 0
                || !matches!(flags, 0 | 0x80)
                || access & !(READ | WRITE) != 0
                || share & !3 != 0
            {
                return open.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, INVALID_HANDLE);
            }
            if !(1..=5).contains(&disposition) {
                return open.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, INVALID_HANDLE);
            }
            if matches!(disposition, 2 | 5) && access & WRITE == 0 {
                return open.fail(cpu.gs_base, ERROR_ACCESS_DENIED, INVALID_HANDLE);
            }
            let raw = read_path(memory, args.rcx).map_err(ExitReason::MemoryFault)?;
            let Some(raw) = raw else {
                return open.fail(cpu.gs_base, INVALID_NAME, INVALID_HANDLE);
            };
            if let Some(resolved) = host_files::resolve(&open.mounts, &raw, valid_component) {
                return create_host_file(&open, cpu.gs_base, resolved, access, share, disposition);
            }
            let mut objects = open.objects.lock().expect("guest object state");
            let Some(path) = resolve_file_path(&raw, &objects.current_directory) else {
                return open.fail(cpu.gs_base, INVALID_NAME, INVALID_HANDLE);
            };
            let parent = parent(&path);
            if !parent.is_empty() && !objects.directories.contains(parent) {
                return open.fail(cpu.gs_base, PATH_NOT_FOUND, INVALID_HANDLE);
            }
            if objects.directories.contains(&path) {
                return open.fail(cpu.gs_base, ERROR_ACCESS_DENIED, INVALID_HANDLE);
            }
            let exists = objects.files.contains_key(&path);
            if exists && disposition == 1 {
                return open.fail(cpu.gs_base, FILE_EXISTS, INVALID_HANDLE);
            }
            if !exists && matches!(disposition, 3 | 5) {
                return open.fail(cpu.gs_base, FILE_NOT_FOUND, INVALID_HANDLE);
            }
            if !check_sharing(&objects, &Target::Path(&path), access, share) {
                return open.fail(cpu.gs_base, SHARING_VIOLATION, INVALID_HANDLE);
            }
            let Some(handle) = objects.next_handle() else {
                return open.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, INVALID_HANDLE);
            };
            if !exists {
                if objects.files.len() >= MAX_FILES {
                    return open.fail(cpu.gs_base, DISK_FULL, INVALID_HANDLE);
                }
                objects
                    .files
                    .insert(path.clone(), SyntheticFile::new(open.clock.now_filetime()));
            } else if matches!(disposition, 2 | 5) {
                let file = objects.files.get_mut(&path).expect("existing file");
                file.data.clear();
                file.times[2] = open.clock.now_filetime();
            }
            if matches!(disposition, 2 | 4) {
                open.fail(cpu.gs_base, if exists { ALREADY_EXISTS } else { 0 }, 0)?;
            }
            objects.insert_handle(
                handle,
                GuestObject::File(OpenFile {
                    path,
                    instance: Arc::default(),
                    open_access: access,
                    access,
                    share,
                    host: None,
                }),
            );
            Ok(handle)
        });
    }

    let create = Arc::clone(state);
    registry.register(KERNEL32, "CreateDirectoryW", move |args, cpu, memory| {
        if args.rdx != 0 {
            return create.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
        }
        if let Some(resolved) = host_path(&create, memory, args.rcx)? {
            return change_host_namespace(
                &create,
                cpu.gs_base,
                resolved,
                HostChange::CreateDirectory,
            );
        }
        let mut objects = create.objects.lock().expect("guest object state");
        let Some(path) = guest_path(memory, args.rcx, &objects.current_directory)
            .map_err(ExitReason::MemoryFault)?
        else {
            return create.fail(cpu.gs_base, INVALID_NAME, 0);
        };
        let parent = parent(&path);
        if !parent.is_empty() && !objects.directories.contains(parent) {
            return create.fail(cpu.gs_base, PATH_NOT_FOUND, 0);
        }
        if objects.directories.contains(&path) || objects.files.contains_key(&path) {
            return create.fail(cpu.gs_base, ALREADY_EXISTS, 0);
        }
        if objects.directories.len() >= MAX_DIRECTORIES {
            return create.fail(cpu.gs_base, DISK_FULL, 0);
        }
        objects.directories.insert(path);
        Ok(1)
    });

    let remove = Arc::clone(state);
    registry.register(KERNEL32, "RemoveDirectoryW", move |args, cpu, memory| {
        if let Some(resolved) = host_path(&remove, memory, args.rcx)? {
            return change_host_namespace(
                &remove,
                cpu.gs_base,
                resolved,
                HostChange::RemoveDirectory,
            );
        }
        let mut objects = remove.objects.lock().expect("guest object state");
        let Some(path) = guest_path(memory, args.rcx, &objects.current_directory)
            .map_err(ExitReason::MemoryFault)?
        else {
            return remove.fail(cpu.gs_base, INVALID_NAME, 0);
        };
        if !objects.directories.contains(&path) {
            return remove.fail(cpu.gs_base, PATH_NOT_FOUND, 0);
        }
        if objects.directories.iter().any(|key| parent(key) == path)
            || objects.files.keys().any(|key| parent(key) == path)
        {
            return remove.fail(cpu.gs_base, DIRECTORY_NOT_EMPTY, 0);
        }
        if objects.current_directory == path {
            return remove.fail(cpu.gs_base, ERROR_ACCESS_DENIED, 0);
        }
        objects.directories.remove(&path);
        Ok(1)
    });

    for (symbol, read_path) in [
        ("DeleteFileA", read_ascii_path as fn(&GuestMemory, u64) -> _),
        ("DeleteFileW", read_raw_path),
    ] {
        let delete = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let raw = read_path(memory, args.rcx).map_err(ExitReason::MemoryFault)?;
            let Some(raw) = raw else {
                return delete.fail(cpu.gs_base, INVALID_NAME, 0);
            };
            if let Some(resolved) = host_files::resolve(&delete.mounts, &raw, valid_component) {
                return change_host_namespace(
                    &delete,
                    cpu.gs_base,
                    resolved,
                    HostChange::DeleteFile,
                );
            }
            let mut objects = delete.objects.lock().expect("guest object state");
            let Some(path) = resolve_file_path(&raw, &objects.current_directory) else {
                return delete.fail(cpu.gs_base, INVALID_NAME, 0);
            };
            if objects.directories.contains(&path) {
                return delete.fail(cpu.gs_base, ERROR_ACCESS_DENIED, 0);
            }
            let parent = parent(&path);
            if !parent.is_empty() && !objects.directories.contains(parent) {
                return delete.fail(cpu.gs_base, PATH_NOT_FOUND, 0);
            }
            if objects
                .handles
                .values()
                .any(|object| matches!(object, GuestObject::File(file) if file.path == path))
            {
                return delete.fail(cpu.gs_base, SHARING_VIOLATION, 0);
            }
            if objects.files.remove(&path).is_none() {
                return delete.fail(cpu.gs_base, FILE_NOT_FOUND, 0);
            }
            Ok(1)
        });
    }

    for (symbol, wide, extended) in [
        ("GetFileAttributesA", false, false),
        ("GetFileAttributesW", true, false),
        ("GetFileAttributesExA", false, true),
        ("GetFileAttributesExW", true, true),
    ] {
        let attributes = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let failure = if extended { 0 } else { u64::from(u32::MAX) };
            if extended && args.rdx as u32 != 0 {
                return attributes.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, failure);
            }
            let raw = if wide {
                read_raw_path(memory, args.rcx)
            } else {
                read_ascii_path(memory, args.rcx)
            }
            .map_err(ExitReason::MemoryFault)?;
            let Some(raw) = raw else {
                return attributes.fail(cpu.gs_base, INVALID_NAME, failure);
            };
            let (flags, size, times) = match file_metadata(&attributes, &raw) {
                Ok(metadata) => metadata,
                Err(error) => return attributes.fail(cpu.gs_base, error, failure),
            };
            if !extended {
                return Ok(u64::from(flags));
            }

            // WIN32_FILE_ATTRIBUTE_DATA: flags, three FILETIMEs, then size high and low.
            let mut data = [0; 36];
            data[..4].copy_from_slice(&flags.to_le_bytes());
            write_file_times(&mut data[4..28], times);
            data[28..32].copy_from_slice(&((size >> 32) as u32).to_le_bytes());
            data[32..36].copy_from_slice(&(size as u32).to_le_bytes());
            if args.r8 == 0 || memory.write_bytes(args.r8, &data).is_err() {
                return attributes.fail(cpu.gs_base, ERROR_NOACCESS, failure);
            }
            Ok(1)
        });
    }

    for (symbol, extended) in [("MoveFileW", false), ("MoveFileExW", true)] {
        let rename = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            // MoveFileW behaves as MoveFileExW with MOVEFILE_COPY_ALLOWED.
            let flags = if extended {
                args.r8 as u32
            } else {
                MOVEFILE_COPY_ALLOWED
            };
            move_entry(&rename, cpu.gs_base, memory, args.rcx, args.rdx, flags)
        });
    }

    let find = Arc::clone(state);
    registry.register(KERNEL32, "FindFirstFileW", move |args, cpu, memory| {
        let raw = read_raw_path(memory, args.rcx).map_err(ExitReason::MemoryFault)?;
        if let Some(found) = raw.as_deref().and_then(|raw| find_host_entries(&find, raw)) {
            let entries = match found {
                Ok(entries) => entries,
                Err(error) => return find.fail(cpu.gs_base, error, INVALID_HANDLE),
            };
            return open_search(&find, cpu.gs_base, memory, args.rdx, entries);
        }
        let objects = find.objects.lock().expect("guest object state");
        let Some(path) = read_guest_path(memory, args.rcx, &objects.current_directory)
            .map_err(ExitReason::MemoryFault)?
        else {
            return find.fail(cpu.gs_base, INVALID_NAME, INVALID_HANDLE);
        };
        let directory = parent(&path);
        let pattern = path.rsplit('\\').next().unwrap_or_default();
        if path.starts_with('\\')
            || (!directory.is_empty() && !directory.split('\\').all(valid_component))
            || !valid_component(&pattern.replace(['*', '?'], "x"))
        {
            return find.fail(cpu.gs_base, INVALID_NAME, INVALID_HANDLE);
        }
        let pattern = if pattern == "*.*" { "*" } else { pattern };
        if !directory.is_empty() && !objects.directories.contains(directory) {
            return find.fail(cpu.gs_base, PATH_NOT_FOUND, INVALID_HANDLE);
        }
        let candidates = objects
            .directories
            .iter()
            .map(|path| (path, 0x10, 0, [0; 3]))
            .chain(
                objects
                    .files
                    .iter()
                    .map(|(path, file)| (path, 0x80, file.data.len() as u64, file.times)),
            );
        let mut entries = Vec::new();
        for (path, attributes, size, times) in
            candidates.filter(|(path, _, _, _)| parent(path) == directory)
        {
            let name = path.rsplit('\\').next().expect("candidate name");
            if wildcard_match(pattern, name) {
                entries.push(SearchEntry {
                    name: name.to_owned(),
                    times,
                    attributes,
                    size,
                });
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        drop(objects);
        open_search(&find, cpu.gs_base, memory, args.rdx, entries)
    });

    let find_next = Arc::clone(state);
    registry.register(KERNEL32, "FindNextFileW", move |args, cpu, memory| {
        let mut objects = find_next.objects.lock().expect("guest object state");
        let Some(GuestObject::Search(search)) = objects.handles.get_mut(&args.rcx) else {
            return find_next.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        };
        let Some(entry) = search.entries.get(search.next) else {
            return find_next.fail(cpu.gs_base, 18, 0); // ERROR_NO_MORE_FILES
        };
        if write_find_data(memory, args.rdx, entry).is_err() {
            return find_next.fail(cpu.gs_base, ERROR_NOACCESS, 0);
        }
        search.next += 1;
        Ok(1)
    });

    let find_close = Arc::clone(state);
    registry.register(KERNEL32, "FindClose", move |args, cpu, _| {
        let mut objects = find_close.objects.lock().expect("guest object state");
        if !matches!(objects.handles.get(&args.rcx), Some(GuestObject::Search(_))) {
            return find_close.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        }
        objects.handles.remove(&args.rcx);
        Ok(1)
    });

    for (symbol, extended) in [("GetFileSize", false), ("GetFileSizeEx", true)] {
        let size = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            let failure = if extended { 0 } else { u64::from(u32::MAX) };
            let objects = size.objects.lock().expect("guest object state");
            let Some(GuestObject::File(file)) = objects.handles.get(&args.rcx) else {
                return size.fail(cpu.gs_base, ERROR_INVALID_HANDLE, failure);
            };
            let bytes = match file.host_len() {
                Some(Ok(bytes)) => bytes,
                Some(Err(error)) => return size.fail(cpu.gs_base, error, failure),
                None => objects
                    .files
                    .get(&file.path)
                    .expect("open file exists")
                    .data
                    .len() as u64,
            };
            if extended {
                if args.rdx == 0 || memory.write_u64(args.rdx, bytes).is_err() {
                    return size.fail(cpu.gs_base, ERROR_NOACCESS, failure);
                }
                return Ok(1);
            }
            if args.rdx != 0 && memory.write_u32(args.rdx, (bytes >> 32) as u32).is_err() {
                return size.fail(cpu.gs_base, ERROR_NOACCESS, failure);
            }
            let low = bytes as u32;
            if low == u32::MAX {
                size.set_last_error(cpu.gs_base, 0);
            }
            Ok(u64::from(low))
        });
    }

    let times = Arc::clone(state);
    registry.register(KERNEL32, "GetFileTime", move |args, cpu, memory| {
        let objects = times.objects.lock().expect("guest object state");
        let Some(GuestObject::File(file)) = objects.handles.get(&args.rcx) else {
            return times.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        };
        if file.access & READ == 0 {
            return times.fail(cpu.gs_base, ERROR_ACCESS_DENIED, 0);
        }
        let addresses = [args.rdx, args.r8, args.r9];
        if addresses
            .iter()
            .any(|&address| address != 0 && memory.check_write(address, 8).is_err())
        {
            return times.fail(cpu.gs_base, ERROR_NOACCESS, 0);
        }
        let values = if let Some(host) = &file.host {
            match host_files::open_file_times(&host.file) {
                Ok(values) => values,
                Err(error) => return times.fail(cpu.gs_base, host_files::win32_error(&error), 0),
            }
        } else {
            objects
                .files
                .get(&file.path)
                .expect("open file exists")
                .times
        };
        for (address, value) in addresses.into_iter().zip(values) {
            if address != 0 {
                memory
                    .write_u64(address, value)
                    .map_err(ExitReason::MemoryFault)?;
            }
        }
        Ok(1)
    });

    let times = Arc::clone(state);
    registry.register(KERNEL32, "SetFileTime", move |args, cpu, memory| {
        let mut objects = times.objects.lock().expect("guest object state");
        let super::ObjectState { handles, files, .. } = &mut *objects;
        let Some(GuestObject::File(file)) = handles.get(&args.rcx) else {
            return times.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        };
        if file.access & WRITE == 0 {
            return times.fail(cpu.gs_base, ERROR_ACCESS_DENIED, 0);
        }
        let mut values = [0; 3];
        for (slot, address) in values.iter_mut().zip([args.rdx, args.r8, args.r9]) {
            if address != 0 {
                match memory.read_u64(address) {
                    Ok(value) => *slot = value,
                    Err(_) => return times.fail(cpu.gs_base, ERROR_NOACCESS, 0),
                }
            }
        }
        if values == [0; 3] {
            return Ok(1);
        }
        let Some(host) = &file.host else {
            let synthetic = files.get_mut(&file.path).expect("open file exists");
            for (index, (slot, value)) in synthetic.times.iter_mut().zip(values).enumerate() {
                match value {
                    0 => {}
                    u64::MAX if index != 0 => {
                        file.instance
                            .suppressed_times
                            .fetch_or(1 << index, Ordering::Relaxed);
                    }
                    _ => *slot = value,
                }
            }
            return Ok(1);
        };
        if values[0] != 0 || values[1..].contains(&u64::MAX) {
            return times.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
        }
        match host_files::set_open_file_times(&host.file, values[1], values[2]) {
            Ok(()) => Ok(1),
            Err(error) => times.fail(cpu.gs_base, host_files::win32_error(&error), 0),
        }
    });

    let file_type = Arc::clone(state);
    registry.register(KERNEL32, "GetFileType", move |args, cpu, _| {
        const FILE_TYPE_DISK: u64 = 1;
        let objects = file_type.objects.lock().expect("guest object state");
        if matches!(objects.handles.get(&args.rcx), Some(GuestObject::File(_))) {
            return Ok(FILE_TYPE_DISK);
        }
        file_type.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0)
    });

    let flush = Arc::clone(state);
    registry.register(KERNEL32, "FlushFileBuffers", move |args, cpu, _| {
        let objects = flush.objects.lock().expect("guest object state");
        let Some(GuestObject::File(file)) = objects.handles.get(&args.rcx) else {
            return flush.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        };
        if file.access & WRITE == 0 {
            return flush.fail(cpu.gs_base, ERROR_ACCESS_DENIED, 0);
        }
        if let Some(host) = &file.host
            && let Err(error) = host.file.sync_all()
        {
            return flush.fail(cpu.gs_base, host_files::win32_error(&error), 0);
        }
        // Synthetic writes update the in-memory file immediately.
        Ok(1)
    });

    let seek = Arc::clone(state);
    registry.register(KERNEL32, "SetFilePointerEx", move |args, cpu, memory| {
        let mut objects = seek.objects.lock().expect("guest object state");
        let super::ObjectState { handles, files, .. } = &mut *objects;
        let Some(GuestObject::File(file)) = handles.get_mut(&args.rcx) else {
            return seek.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        };
        if file.access & (READ | WRITE) == 0 {
            return seek.fail(cpu.gs_base, ERROR_ACCESS_DENIED, 0);
        }
        let origin = match args.r9 as u32 {
            0 => None,
            1 => Some(file.instance.position.load(Ordering::Relaxed)),
            2 => match file.host_len() {
                Some(Ok(bytes)) => Some(bytes),
                Some(Err(error)) => return seek.fail(cpu.gs_base, error, 0),
                None => Some(files.get(&file.path).expect("open file exists").data.len() as u64),
            },
            _ => return seek.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0),
        };
        let position = match origin {
            None => args.rdx,
            Some(origin) => {
                let distance = args.rdx as i64;
                if distance.is_negative() && distance.unsigned_abs() > origin {
                    return seek.fail(cpu.gs_base, NEGATIVE_SEEK, 0);
                }
                let Some(position) = origin.checked_add_signed(distance) else {
                    return seek.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
                };
                position
            }
        };
        if args.r8 != 0 && memory.write_u64(args.r8, position).is_err() {
            return seek.fail(cpu.gs_base, ERROR_NOACCESS, 0);
        }
        file.instance.position.store(position, Ordering::Relaxed);
        Ok(1)
    });

    let truncate = Arc::clone(state);
    registry.register(KERNEL32, "SetEndOfFile", move |args, cpu, _| {
        let mut objects = truncate.objects.lock().expect("guest object state");
        let super::ObjectState { handles, files, .. } = &mut *objects;
        let Some(GuestObject::File(file)) = handles.get(&args.rcx) else {
            return truncate.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        };
        if file.access & WRITE == 0 {
            return truncate.fail(cpu.gs_base, ERROR_ACCESS_DENIED, 0);
        }
        let position = file.instance.position.load(Ordering::Relaxed);
        if let Some(host) = &file.host {
            if position > host.max_size {
                return truncate.fail(cpu.gs_base, DISK_FULL, 0);
            }
            return match host.set_len(position) {
                Ok(()) => Ok(1),
                Err(error) => truncate.fail(cpu.gs_base, error, 0),
            };
        }
        if position > MAX_BYTES as u64 {
            return truncate.fail(cpu.gs_base, DISK_FULL, 0);
        }
        let used = files.values().map(|file| file.data.len()).sum::<usize>();
        let synthetic = files.get_mut(&file.path).expect("open file exists");
        let data = &mut synthetic.data;
        let length = position as usize;
        let growth = length.saturating_sub(data.len());
        if growth > MAX_BYTES - used {
            return truncate.fail(cpu.gs_base, DISK_FULL, 0);
        }
        data.try_reserve_exact(growth)
            .map_err(|_| ExitReason::MemoryFault(MemoryError::AllocationFailed))?;
        if length < data.len() {
            data.truncate(length);
            data.shrink_to_fit();
        } else {
            data.resize(length, 0);
        }
        if file.updates_time(2) {
            synthetic.times[2] = truncate.clock.now_filetime();
        }
        Ok(1)
    });

    for (symbol, writing) in [("ReadFile", false), ("WriteFile", true)] {
        let state = Arc::clone(state);
        registry.register(KERNEL32, symbol, move |args, cpu, memory| {
            if args.get(4, memory).map_err(ExitReason::MemoryFault)? != 0 {
                return state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            let count = args.r8 as u32 as usize;
            if count > MAX_TRANSFER {
                return state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            let mut objects = state.objects.lock().expect("guest object state");
            let super::ObjectState { handles, files, .. } = &mut *objects;
            let Some(GuestObject::File(file)) = handles.get_mut(&args.rcx) else {
                return state.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
            };
            let required = if writing { WRITE } else { READ };
            if file.access & required == 0 {
                return state.fail(cpu.gs_base, ERROR_ACCESS_DENIED, 0);
            }
            if args.r9 == 0 || memory.check_write(args.r9, 4).is_err() {
                return state.fail(cpu.gs_base, ERROR_NOACCESS, 0);
            }
            if let Some(host) = file.host.clone() {
                let position = file.instance.position.load(Ordering::Relaxed);
                let Some(end) = position.checked_add(count as u64) else {
                    return state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
                };
                if writing && count != 0 && end > host.max_size {
                    return state.fail(cpu.gs_base, DISK_FULL, 0);
                }
                let mut buffer = vec![0; count];
                if writing
                    && count != 0
                    && (args.rdx == 0 || memory.read_bytes(args.rdx, &mut buffer).is_err())
                {
                    return state.fail(cpu.gs_base, ERROR_NOACCESS, 0);
                }
                if !writing
                    && count != 0
                    && (args.rdx == 0 || memory.check_write(args.rdx, count).is_err())
                {
                    return state.fail(cpu.gs_base, ERROR_NOACCESS, 0);
                }
                // Charge growth before writing; settle on what the file holds
                // if the write fails part way.
                let grown = if writing && count != 0 {
                    let old = match host.len() {
                        Ok(old) => old,
                        Err(error) => return state.fail(cpu.gs_base, error, 0),
                    };
                    if let Err(error) = host.budget.resize(old, old.max(end)) {
                        return state.fail(cpu.gs_base, error, 0);
                    }
                    Some((old, old.max(end)))
                } else {
                    None
                };
                let transferred = match host_transfer(&host.file, position, &mut buffer, writing) {
                    Ok(transferred) => transferred,
                    Err(error) => {
                        if let Some((old, charged)) = grown {
                            let now = host.len().unwrap_or(old);
                            let _ = host.budget.resize(charged, now);
                        }
                        return state.fail(cpu.gs_base, error, 0);
                    }
                };
                if !writing && transferred != 0 {
                    memory
                        .write_bytes(args.rdx, &buffer[..transferred])
                        .map_err(ExitReason::MemoryFault)?;
                }
                memory
                    .write_u32(args.r9, transferred as u32)
                    .map_err(ExitReason::MemoryFault)?;
                file.instance
                    .position
                    .store(position + transferred as u64, Ordering::Relaxed);
                return Ok(1);
            }

            let used = if writing {
                files.values().map(|file| file.data.len()).sum::<usize>()
            } else {
                0
            };
            let synthetic = files.get_mut(&file.path).expect("open file exists");
            let data = &mut synthetic.data;
            let position = file.instance.position.load(Ordering::Relaxed);
            let transferred = if writing {
                if count == 0 {
                    0
                } else {
                    let Some(end) = position
                        .checked_add(count as u64)
                        .filter(|end| *end <= MAX_BYTES as u64)
                    else {
                        return state.fail(cpu.gs_base, DISK_FULL, 0);
                    };
                    let end = end as usize;
                    let growth = end.saturating_sub(data.len());
                    if growth > MAX_BYTES - used {
                        return state.fail(cpu.gs_base, DISK_FULL, 0);
                    }
                    let mut input = vec![0; count];
                    if args.rdx == 0 || memory.read_bytes(args.rdx, &mut input).is_err() {
                        return state.fail(cpu.gs_base, ERROR_NOACCESS, 0);
                    }
                    if growth != 0 {
                        data.try_reserve_exact(growth)
                            .map_err(|_| ExitReason::MemoryFault(MemoryError::AllocationFailed))?;
                        data.resize(end, 0);
                    }
                    data[position as usize..end].copy_from_slice(&input);
                    count
                }
            } else {
                let start = position.min(data.len() as u64) as usize;
                let transferred = count.min(data.len() - start);
                if transferred != 0
                    && (args.rdx == 0
                        || memory
                            .write_bytes(args.rdx, &data[start..start + transferred])
                            .is_err())
                {
                    return state.fail(cpu.gs_base, ERROR_NOACCESS, 0);
                }
                transferred
            };
            memory
                .write_u32(args.r9, transferred as u32)
                .map_err(ExitReason::MemoryFault)?;
            let time_index = if writing { 2 } else { 1 };
            if transferred != 0 && file.updates_time(time_index) {
                synthetic.times[time_index] = state.clock.now_filetime();
            }
            file.instance
                .position
                .store(position + transferred as u64, Ordering::Relaxed);
            Ok(1)
        });
    }

    for symbol in [
        "CreateFileA",
        "CreateFileW",
        "GetFileSize",
        "GetFileSizeEx",
        "GetFileType",
        "GetFileTime",
        "SetFileTime",
        "FlushFileBuffers",
        "ReadFile",
        "WriteFile",
        "SetFilePointerEx",
        "SetEndOfFile",
        "CreateDirectoryW",
        "RemoveDirectoryW",
        "DeleteFileA",
        "DeleteFileW",
        "GetFileAttributesA",
        "GetFileAttributesW",
        "GetFileAttributesExA",
        "GetFileAttributesExW",
        "MoveFileW",
        "MoveFileExW",
        "FindFirstFileW",
        "FindNextFileW",
        "FindClose",
    ] {
        registry.alias(FILE_API, KERNEL32, symbol);
        registry.set_status(KERNEL32, symbol, ServiceStatus::Partial);
    }
}
