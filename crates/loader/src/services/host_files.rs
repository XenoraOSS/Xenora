// SPDX-License-Identifier: GPL-3.0-or-later

//! Guest drive letters backed by explicitly configured host directories.
//!
//! Each mount holds a descriptor for its root. Guest paths are resolved one
//! component at a time with `openat` and `O_NOFOLLOW` from that descriptor,
//! matching names case-insensitively against the directory listing.
//!
//! Policy, enforced on every operation:
//! - `.` and `..` components, device names and absolute host paths cannot be
//!   expressed in a guest path.
//! - Symbolic links, devices, sockets, FIFOs, non-UTF-8 names and regular
//!   files with more than one hard link are hidden from the guest and cannot
//!   be opened, so a link cannot alias a host file outside the root.
//! - After each `openat`, the descriptor is checked with `fstat`: it must be
//!   the expected type (directory or regular file), have one link if it is a
//!   file, and keep the device and inode the listing reported. A concurrent
//!   host change that swaps an entry between the listing and the open fails
//!   with `ERROR_ACCESS_DENIED` instead of reaching the replacement.
//! - Reads, writes and truncation use the verified descriptor, so they reach
//!   the same file however the host renames it afterwards. Read-only mounts
//!   open every file `O_RDONLY`, so the host kernel also refuses writes.
//! - Sharing and deletion checks compare device and inode, not guest paths.
//!
//! Not covered: another host process that moves a directory or file the guest
//! already holds open out of the mount. The guest keeps its handle to that
//! object. Host processes with write access to a mounted directory are
//! trusted. Removal and renaming are name-based after a final listing check, so
//! a concurrent host rename in that window can delete or move a different entry
//! inside the root (never outside it); directories have no post-unlink check.
//! This boundary was agent-reviewed for guest path confinement; it is not a
//! substitute for an independent human security audit.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub(super) const ERROR_FILE_NOT_FOUND: u32 = 2;
pub(super) const ERROR_PATH_NOT_FOUND: u32 = 3;
pub(super) const ERROR_ACCESS_DENIED: u32 = 5;
pub(super) const ERROR_FILE_EXISTS: u32 = 80;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const ERROR_GEN_FAILURE: u32 = 31;
const ERROR_DISK_FULL: u32 = 112;
const ERROR_DIR_NOT_EMPTY: u32 = 145;
pub(super) const ERROR_FILENAME_EXCED_RANGE: u32 = 206;
const ERROR_IO_DEVICE: u32 = 1117;
/// Default largest file guest writes may produce on a writable mount.
pub const DEFAULT_MAX_FILE_SIZE: u64 = 1 << 30;
/// Largest number of entries read from one host directory.
pub const MAX_DIRECTORY_ENTRIES: usize = 4096;

/// How much a guest may add to one writable mount while it is mounted.
///
/// Usage is net growth relative to the mount's contents when it was created:
/// bytes added by writes and extending truncation, minus bytes freed by
/// shrinking or deleting files, and entries created minus entries deleted.
/// Freeing never takes usage below zero, so deleting files that were already
/// there does not raise the limit beyond the configured amount. Changes made
/// by host processes are not counted.
#[derive(Debug)]
pub(super) struct Budget {
    max_bytes: Option<u64>,
    max_entries: Option<u64>,
    used: Mutex<Usage>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub bytes: u64,
    pub entries: u64,
}

impl Budget {
    /// Account for a file changing from `from` to `to` bytes. Growth beyond
    /// the byte limit fails with `ERROR_DISK_FULL` and charges nothing.
    pub(super) fn resize(&self, from: u64, to: u64) -> Result<(), u32> {
        let mut used = self.used.lock().expect("mount usage");
        if to > from {
            let bytes = used.bytes.checked_add(to - from).ok_or(ERROR_DISK_FULL)?;
            if self.max_bytes.is_some_and(|max| bytes > max) {
                return Err(ERROR_DISK_FULL);
            }
            used.bytes = bytes;
        } else {
            used.bytes = used.bytes.saturating_sub(from - to);
        }
        Ok(())
    }

    /// Reserve one new file or directory entry.
    pub(super) fn add_entry(&self) -> Result<(), u32> {
        let mut used = self.used.lock().expect("mount usage");
        if self.max_entries.is_some_and(|max| used.entries >= max) {
            return Err(ERROR_DISK_FULL);
        }
        used.entries += 1;
        Ok(())
    }

    /// Release an entry, and the bytes of a file, that no longer exist.
    pub(super) fn remove_entry(&self, bytes: u64) {
        let mut used = self.used.lock().expect("mount usage");
        used.entries = used.entries.saturating_sub(1);
        used.bytes = used.bytes.saturating_sub(bytes);
    }

    fn usage(&self) -> Usage {
        *self.used.lock().expect("mount usage")
    }
}

/// A guest drive letter backed by a host directory.
pub struct HostMount {
    drive: u8,
    root: Arc<OwnedFd>,
    writable: bool,
    max_file_size: u64,
    budget: Arc<Budget>,
}

impl HostMount {
    /// Mount `root` as guest drive `drive`, which must be D: to Z:. The
    /// directory is opened once; later changes to the host path name do not
    /// move the mount.
    pub fn new(drive: char, root: &Path, writable: bool) -> io::Result<Self> {
        let drive = drive.to_ascii_lowercase();
        if !('d'..='z').contains(&drive) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "host mounts use drive letters D: to Z:",
            ));
        }
        let path = CString::new(root.as_os_str().as_encoded_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: `path` is NUL-terminated; the result is checked.
        // `O_NOFOLLOW` rejects a symlink at the final path component so a
        // mount root cannot silently expand to another tree.
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a newly opened descriptor owned by nothing else.
        let root = unsafe { OwnedFd::from_raw_fd(fd) };
        let st = status(root.as_fd())?;
        if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        Ok(Self {
            drive: drive as u8,
            root: Arc::new(root),
            writable,
            max_file_size: DEFAULT_MAX_FILE_SIZE,
            budget: Arc::new(Budget {
                max_bytes: None,
                max_entries: None,
                used: Mutex::default(),
            }),
        })
    }

    /// Limit the net bytes and the net number of files and directories guest
    /// operations may add to this mount. `None` leaves that amount
    /// unlimited; the per-file size limit still applies. Operations that
    /// would exceed a limit fail with `ERROR_DISK_FULL`.
    pub fn with_quota(mut self, bytes: Option<u64>, entries: Option<u64>) -> Self {
        self.budget = Arc::new(Budget {
            max_bytes: bytes,
            max_entries: entries,
            used: Mutex::default(),
        });
        self
    }

    /// Net bytes and entries guest operations have added so far.
    pub fn usage(&self) -> Usage {
        self.budget.usage()
    }

    pub(super) fn budget(&self) -> &Arc<Budget> {
        &self.budget
    }

    /// Limit how large guest writes and truncation may make any file on this
    /// mount. Files that are already larger stay readable.
    pub fn with_max_file_size(mut self, bytes: u64) -> Self {
        self.max_file_size = bytes;
        self
    }

    pub fn max_file_size(&self) -> u64 {
        self.max_file_size
    }

    pub fn drive(&self) -> char {
        self.drive as char
    }

    pub fn writable(&self) -> bool {
        self.writable
    }
}

/// Win32 error for a host failure. Unknown errors keep a distinct generic code.
pub(super) fn win32_error(error: &io::Error) -> u32 {
    match error.raw_os_error() {
        Some(libc::ENOENT) => ERROR_FILE_NOT_FOUND,
        Some(libc::ENOTDIR) => ERROR_PATH_NOT_FOUND,
        Some(libc::EACCES | libc::EPERM | libc::EROFS | libc::ELOOP | libc::EISDIR) => {
            ERROR_ACCESS_DENIED
        }
        Some(libc::EEXIST) => ERROR_FILE_EXISTS,
        Some(libc::ENOSPC | libc::EDQUOT | libc::EFBIG) => ERROR_DISK_FULL,
        Some(libc::ENOTEMPTY) => ERROR_DIR_NOT_EMPTY,
        Some(libc::ENAMETOOLONG) => ERROR_FILENAME_EXCED_RANGE,
        Some(libc::EINVAL) => 87, // ERROR_INVALID_PARAMETER
        Some(libc::ENOMEM) => ERROR_NOT_ENOUGH_MEMORY,
        Some(libc::EIO) => ERROR_IO_DEVICE,
        _ => ERROR_GEN_FAILURE,
    }
}

fn refused() -> io::Error {
    io::Error::from_raw_os_error(libc::EACCES)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    File,
    Directory,
}

/// The host object a name refers to: device and inode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct FileId {
    device: u64,
    inode: u64,
}

impl FileId {
    /// A label for the handle table. Sharing compares live identities.
    pub(super) fn key(self) -> String {
        format!("host:{:x}:{:x}", self.device, self.inode)
    }
}

#[derive(Debug)]
pub(super) struct Entry {
    /// The host name, with its original case.
    pub(super) name: String,
    pub(super) kind: Kind,
    pub(super) size: u64,
    pub(super) times: [u64; 3],
    pub(super) id: FileId,
}

/// Type, link count, size and identity of `stat`, or `None` for objects the
/// guest may not see.
fn classify(status: &libc::stat) -> Option<(Kind, u64, FileId)> {
    let kind = match status.st_mode & libc::S_IFMT {
        libc::S_IFREG if status.st_nlink == 1 => Kind::File,
        libc::S_IFDIR => Kind::Directory,
        _ => return None,
    };
    let id = FileId {
        device: status.st_dev as u64,
        inode: status.st_ino,
    };
    Some((kind, status.st_size as u64, id))
}

fn file_times(status: &libc::stat) -> [u64; 3] {
    fn filetime(seconds: i64, nanoseconds: i64) -> u64 {
        let ticks =
            (i128::from(seconds) + 11_644_473_600) * 10_000_000 + i128::from(nanoseconds) / 100;
        u64::try_from(ticks).unwrap_or(0)
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let created = if status.st_birthtime == 0 && status.st_birthtime_nsec == 0 {
        0
    } else {
        filetime(status.st_birthtime, status.st_birthtime_nsec)
    };
    #[cfg(target_os = "linux")]
    let created = 0;
    [
        created,
        filetime(status.st_atime, status.st_atime_nsec),
        filetime(status.st_mtime, status.st_mtime_nsec),
    ]
}

fn status(fd: BorrowedFd<'_>) -> io::Result<libc::stat> {
    let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fd` is open and `status` has room for a stat.
    if unsafe { libc::fstat(fd.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstat succeeded and initialized `status`.
    Ok(unsafe { status.assume_init() })
}

pub(super) fn open_file_times(file: &File) -> io::Result<[u64; 3]> {
    status(file.as_fd()).map(|status| file_times(&status))
}

pub(super) fn set_open_file_times(file: &File, accessed: u64, modified: u64) -> io::Result<()> {
    use std::fs::FileTimes;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn system_time(ticks: u64) -> io::Result<SystemTime> {
        const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
        let delta = ticks.abs_diff(UNIX_EPOCH_TICKS);
        let duration = Duration::new(delta / 10_000_000, (delta % 10_000_000) as u32 * 100);
        let result = if ticks >= UNIX_EPOCH_TICKS {
            UNIX_EPOCH.checked_add(duration)
        } else {
            UNIX_EPOCH.checked_sub(duration)
        };
        result.ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))
    }

    let mut times = FileTimes::new();
    if accessed != 0 {
        times = times.set_accessed(system_time(accessed)?);
    }
    if modified != 0 {
        times = times.set_modified(system_time(modified)?);
    }
    file.set_times(times)
}

/// Check that an opened descriptor is still the object the listing reported.
fn verify(fd: &OwnedFd, kind: Kind, expected: Option<FileId>) -> io::Result<FileId> {
    match classify(&status(fd.as_fd())?) {
        Some((found, _, id)) if found == kind && expected.is_none_or(|expected| expected == id) => {
            Ok(id)
        }
        _ => Err(refused()),
    }
}

/// The current identity of an open host file. Some file systems, such as
/// exFAT, change a file's inode number when its storage is first allocated,
/// so identities are compared as they are now rather than as they were when
/// the file was opened.
/// `None` means the identity could not be read; callers must then treat the
/// file as possibly the same one.
pub(super) fn current_id(file: &File) -> Option<FileId> {
    let status = status(file.as_fd()).ok()?;
    Some(FileId {
        device: status.st_dev as u64,
        inode: status.st_ino,
    })
}

fn clear_errno() {
    // SAFETY: errno is thread-local and always writable.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    unsafe {
        *libc::__error() = 0
    };
    // SAFETY: as above.
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location() = 0
    };
}

fn c_name(name: &str) -> io::Result<CString> {
    CString::new(name).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn open_at(directory: &OwnedFd, name: &str, flags: i32, mode: u32) -> io::Result<OwnedFd> {
    let name = c_name(name)?;
    // SAFETY: `name` is NUL-terminated and `directory` is an open descriptor.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a newly opened descriptor owned by nothing else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn open_directory(parent: &OwnedFd, name: &str, expected: Option<FileId>) -> io::Result<OwnedFd> {
    let directory = open_at(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    verify(&directory, Kind::Directory, expected)?;
    Ok(directory)
}

/// Visible entries of `directory`. Errors from reading the directory or from
/// examining an entry are returned, except an entry removed while listing.
pub(super) fn list(directory: &OwnedFd) -> io::Result<Vec<Entry>> {
    // fdopendir takes ownership of its descriptor, so give it a duplicate.
    // SAFETY: `directory` is open; the duplicate is checked.
    let duplicate = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `duplicate` is a directory descriptor owned by nothing else.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: fdopendir failed, so the duplicate is still ours to close.
        unsafe { libc::close(duplicate) };
        return Err(error);
    }
    // SAFETY: fdopendir shares the descriptor's offset with every duplicate.
    unsafe { libc::rewinddir(stream) };
    let mut entries = Vec::new();
    let result = loop {
        // readdir signals errors only through errno, so clear it first.
        clear_errno();
        // SAFETY: `stream` is a valid directory stream until closedir below.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            break match error.raw_os_error() {
                Some(0) | None => Ok(()),
                Some(_) => Err(error),
            };
        }
        // SAFETY: readdir returned a valid entry with a NUL-terminated name.
        let raw = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let Ok(name) = raw.to_str() else { continue };
        if name == "." || name == ".." {
            continue;
        }
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `raw` is NUL-terminated and `status` has room for a stat.
        let found = unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                raw.as_ptr(),
                status.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if found != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOENT) {
                continue;
            }
            break Err(error);
        }
        // SAFETY: fstatat succeeded and initialized `status`.
        let status = unsafe { status.assume_init() };
        let Some((kind, size, id)) = classify(&status) else {
            continue;
        };
        if entries.len() >= MAX_DIRECTORY_ENTRIES {
            break Err(io::Error::from_raw_os_error(libc::ENOMEM));
        }
        entries.push(Entry {
            name: name.to_owned(),
            kind,
            size,
            times: file_times(&status),
            id,
        });
    };
    // SAFETY: `stream` is valid and closed exactly once.
    unsafe { libc::closedir(stream) };
    result.map(|()| entries)
}

/// The entry in `directory` whose name matches `component` ignoring ASCII
/// case. Two entries that differ only by case are ambiguous and refused.
fn find(directory: &OwnedFd, component: &str) -> io::Result<Option<Entry>> {
    let mut matches = list(directory)?
        .into_iter()
        .filter(|entry| entry.name.eq_ignore_ascii_case(component));
    let first = matches.next();
    if matches.next().is_some() {
        return Err(refused());
    }
    Ok(first)
}

/// A guest path on a mounted drive: the mount and its validated components.
pub(super) struct Resolved<'a> {
    pub(super) mount: &'a HostMount,
    pub(super) components: Vec<String>,
}

impl Resolved<'_> {
    /// Open the directory holding the final component.
    pub(super) fn parent(&self) -> Result<OwnedFd, u32> {
        let parents = self
            .components
            .split_last()
            .map_or(&[][..], |(_, parents)| parents);
        self.walk(parents)
    }

    /// Open the directory the whole path names.
    pub(super) fn directory(&self) -> Result<OwnedFd, u32> {
        self.walk(&self.components)
    }

    fn walk(&self, components: &[String]) -> Result<OwnedFd, u32> {
        let mut directory =
            open_directory(&self.mount.root, ".", None).map_err(|error| win32_error(&error))?;
        for component in components {
            match find(&directory, component).map_err(|error| win32_error(&error))? {
                Some(entry) if entry.kind == Kind::Directory => {
                    directory = open_directory(&directory, &entry.name, Some(entry.id))
                        .map_err(|error| win32_error(&error))?;
                }
                _ => return Err(ERROR_PATH_NOT_FOUND),
            }
        }
        Ok(directory)
    }

    /// The final component's directory and existing entry, if any. The root
    /// itself resolves to a directory with no name.
    pub(super) fn lookup(&self) -> Result<(OwnedFd, Option<Entry>), u32> {
        let parent = self.parent()?;
        let Some(name) = self.components.last() else {
            let status = status(parent.as_fd()).map_err(|error| win32_error(&error))?;
            let Some((Kind::Directory, _, id)) = classify(&status) else {
                return Err(win32_error(&refused()));
            };
            let root = Entry {
                name: String::new(),
                kind: Kind::Directory,
                size: 0,
                times: file_times(&status),
                id,
            };
            return Ok((parent, Some(root)));
        };
        let entry = find(&parent, name).map_err(|error| win32_error(&error))?;
        Ok((parent, entry))
    }
}

/// Resolve `path`, which keeps its case, when it names a mounted drive.
/// `valid` checks each component against Windows naming rules.
pub(super) fn resolve<'a>(
    mounts: &'a [HostMount],
    path: &str,
    valid: impl Fn(&str) -> bool,
) -> Option<Result<Resolved<'a>, u32>> {
    let bytes = path.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' {
        return None;
    }
    let drive = bytes[0].to_ascii_lowercase();
    let mount = mounts.iter().find(|mount| mount.drive == drive)?;
    let Some(rest) = path[2..].strip_prefix('\\') else {
        // Drive-relative paths such as "D:file" depend on a current directory.
        return Some(Err(ERROR_PATH_NOT_FOUND));
    };
    let components: Vec<String> = rest
        .split('\\')
        .filter(|component| !component.is_empty())
        .map(str::to_owned)
        .collect();
    if !components
        .iter()
        .all(|component| valid(&component.to_ascii_lowercase()))
    {
        return Some(Err(123)); // ERROR_INVALID_NAME
    }
    Some(Ok(Resolved { mount, components }))
}

pub(super) struct OpenRequest {
    pub(super) read: bool,
    pub(super) write: bool,
    pub(super) create: bool,
}

/// Open `name` in `directory`, creating it exclusively when it did not exist,
/// and check the result is the regular file `expected` names.
pub(super) fn open_file(
    directory: &OwnedFd,
    name: &str,
    request: &OpenRequest,
    expected: Option<FileId>,
) -> io::Result<(File, FileId)> {
    let mut flags = match (request.read, request.write) {
        (_, false) => libc::O_RDONLY,
        (false, true) => libc::O_WRONLY,
        (true, true) => libc::O_RDWR,
    };
    if request.create {
        flags |= libc::O_CREAT | libc::O_EXCL;
    }
    let fd = open_at(directory, name, flags, 0o644)?;
    let id = verify(&fd, Kind::File, expected)?;
    Ok((File::from(fd), id))
}

pub(super) fn create_directory(directory: &OwnedFd, name: &str) -> io::Result<()> {
    let name = c_name(name)?;
    // SAFETY: `name` is NUL-terminated and `directory` is open.
    if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o755) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Remove the entry `name` after checking it is still the object `expected`.
///
/// Removal is always name-based `unlinkat` after re-checking the directory
/// listing. A concurrent host rename between that check and the unlink can
/// still remove a different in-tree entry (never one outside the root). Host
/// writers under the mount are trusted for that residual race. After a file
/// unlink, the held descriptor must show `nlink == 0` or the call fails, so a
/// lost race on a file is not reported as success; directories have no such
/// post-check.
pub(super) fn remove(directory: &OwnedFd, name: &str, expected: &Entry) -> io::Result<()> {
    let opened = match expected.kind {
        Kind::Directory => open_directory(directory, name, Some(expected.id))?,
        Kind::File => open_at(directory, name, libc::O_RDONLY, 0)?,
    };
    verify(&opened, expected.kind, Some(expected.id))?;

    // Confirm the directory entry still names this object before unlinking.
    match find(directory, name)? {
        Some(entry) if entry.id == expected.id && entry.kind == expected.kind => {}
        _ => return Err(refused()),
    }
    let cname = c_name(name)?;
    let flags = match expected.kind {
        Kind::Directory => libc::AT_REMOVEDIR,
        Kind::File => 0,
    };
    // SAFETY: `cname` is NUL-terminated and `directory` is open.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), cname.as_ptr(), flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if expected.kind == Kind::File {
        let st = status(opened.as_fd())?;
        if st.st_nlink != 0 {
            // Name-based unlink hit a different dentry; the intended file is
            // still linked.
            return Err(refused());
        }
    }
    Ok(())
}

/// Rename `from` in `source` to `to` in `target`, after checking the source
/// entry is still the object `expected`. The target name must not exist,
/// except when it names the source itself (a change of case only) or the
/// regular file `replacing`, which the rename then replaces atomically.
///
/// The exclusive rename is atomic on the host, so a concurrent host change
/// cannot make the guest replace an existing entry. As with [`remove`], the
/// source is matched by name after the final listing check, so a concurrent
/// host rename in that window can move a different in-tree entry.
pub(super) fn rename(
    source: &OwnedFd,
    from: &str,
    expected: &Entry,
    target: &OwnedFd,
    to: &str,
    replacing: Option<&Entry>,
) -> io::Result<()> {
    match find(source, from)? {
        Some(entry) if entry.id == expected.id && entry.kind == expected.kind => {}
        _ => return Err(refused()),
    }
    let overwrite = match find(target, to)? {
        Some(entry) if entry.id == expected.id => true,
        Some(entry)
            if replacing.is_some_and(|old| old.id == entry.id)
                && entry.kind == Kind::File
                && expected.kind == Kind::File =>
        {
            true
        }
        Some(_) => return Err(io::Error::from_raw_os_error(libc::EEXIST)),
        None => false,
    };
    let from = c_name(from)?;
    let to = c_name(to)?;
    // SAFETY: both names are NUL-terminated and both directories are open.
    let result = unsafe {
        if overwrite {
            libc::renameat(
                source.as_raw_fd(),
                from.as_ptr(),
                target.as_raw_fd(),
                to.as_ptr(),
            )
        } else {
            exclusive_rename(source.as_raw_fd(), &from, target.as_raw_fd(), &to)
        }
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Flush a directory's entries to stable storage.
pub(super) fn sync_directory(directory: &OwnedFd) -> io::Result<()> {
    // SAFETY: `directory` is an open descriptor.
    if unsafe { libc::fsync(directory.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `renameat` that fails with `EEXIST` instead of replacing the target.
///
/// # Safety
/// Both descriptors must be open directories.
unsafe fn exclusive_rename(source: i32, from: &CStr, target: i32, to: &CStr) -> i32 {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    // SAFETY: guaranteed by the caller; the names are NUL-terminated.
    unsafe {
        libc::renameatx_np(
            source,
            from.as_ptr(),
            target,
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    }
    #[cfg(target_os = "linux")]
    // SAFETY: as above.
    unsafe {
        libc::renameat2(
            source,
            from.as_ptr(),
            target,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str) -> std::path::PathBuf {
        let base = std::env::var_os("XENORA_TEST_TMP").map_or_else(
            || std::path::PathBuf::from("/tmp"),
            std::path::PathBuf::from,
        );
        let path = base.join(format!("xenora-host-unit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn root(path: &Path) -> OwnedFd {
        open_directory(&HostMount::new('d', path, true).unwrap().root, ".", None).unwrap()
    }

    #[test]
    fn an_entry_swapped_after_listing_is_refused() {
        let path = tree("swap");
        std::fs::create_dir(path.join("Data")).unwrap();
        std::fs::write(path.join("file.bin"), b"a").unwrap();
        std::fs::create_dir(path.join("outside")).unwrap();
        let fd = root(&path);
        let data = find(&fd, "data").unwrap().unwrap();
        let file = find(&fd, "FILE.BIN").unwrap().unwrap();

        // Swap both entries between the listing and the open.
        std::fs::rename(path.join("Data"), path.join("Moved")).unwrap();
        std::fs::rename(path.join("outside"), path.join("Data")).unwrap();
        std::fs::rename(path.join("file.bin"), path.join("old.bin")).unwrap();
        std::fs::write(path.join("file.bin"), b"replacement").unwrap();

        let error = open_directory(&fd, &data.name, Some(data.id)).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
        let request = OpenRequest {
            read: true,
            write: false,
            create: false,
        };
        let error = open_file(&fd, &file.name, &request, Some(file.id)).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
        let error = remove(&fd, &file.name, &file).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
        assert_eq!(
            std::fs::read(path.join("file.bin")).unwrap(),
            b"replacement"
        );
        std::fs::remove_dir_all(&path).unwrap();
    }

    #[test]
    fn read_only_opens_cannot_write_at_the_host_level() {
        use std::os::unix::fs::FileExt;
        let path = tree("read-only");
        std::fs::write(path.join("file.bin"), b"keep").unwrap();
        let fd = root(&path);
        let entry = find(&fd, "file.bin").unwrap().unwrap();
        let request = OpenRequest {
            read: true,
            write: false,
            create: false,
        };
        let (file, _) = open_file(&fd, &entry.name, &request, Some(entry.id)).unwrap();
        assert!(file.write_at(b"x", 0).is_err());
        assert!(file.set_len(0).is_err());
        assert_eq!(std::fs::read(path.join("file.bin")).unwrap(), b"keep");
        std::fs::remove_dir_all(&path).unwrap();
    }

    #[test]
    fn ambiguous_case_variants_are_refused() {
        let path = tree("case");
        std::fs::write(path.join("a.txt"), b"1").unwrap();
        if std::fs::write(path.join("A.TXT"), b"2").is_ok()
            && std::fs::read(path.join("a.txt")).unwrap() == b"1"
        {
            // Only a case-sensitive file system can hold both names.
            assert_eq!(
                find(&root(&path), "a.txt").unwrap_err().raw_os_error(),
                Some(libc::EACCES)
            );
        }
        std::fs::remove_dir_all(&path).unwrap();
    }

    #[test]
    fn budgets_charge_net_growth_and_never_credit_below_zero() {
        let budget = Budget {
            max_bytes: Some(10),
            max_entries: Some(1),
            used: Mutex::default(),
        };
        assert_eq!(budget.resize(0, 10), Ok(()));
        assert_eq!(budget.resize(10, 11), Err(ERROR_DISK_FULL));
        assert_eq!(budget.usage().bytes, 10);
        // Shrinking a larger file that predates the mount frees at most what
        // was charged.
        assert_eq!(budget.resize(100, 0), Ok(()));
        assert_eq!(budget.usage().bytes, 0);
        assert_eq!(budget.resize(u64::MAX - 1, u64::MAX), Ok(()));
        assert_eq!(budget.resize(0, u64::MAX), Err(ERROR_DISK_FULL));
        assert_eq!(budget.add_entry(), Ok(()));
        assert_eq!(budget.add_entry(), Err(ERROR_DISK_FULL));
        budget.remove_entry(5);
        budget.remove_entry(5);
        assert_eq!(
            budget.usage(),
            Usage {
                bytes: 0,
                entries: 0
            }
        );
        let unlimited = HostMount::new('d', Path::new("/"), true).unwrap();
        assert_eq!(unlimited.budget().resize(0, u64::MAX), Ok(()));
    }

    #[test]
    fn symlink_mount_roots_are_rejected() {
        let path = tree("symlink-root");
        std::fs::create_dir(path.join("real")).unwrap();
        std::os::unix::fs::symlink(path.join("real"), path.join("link")).unwrap();
        match HostMount::new('d', &path.join("link"), false) {
            Ok(_) => panic!("symlink mount root should be rejected"),
            // Darwin may report ENOTDIR instead of ELOOP for
            // O_DIRECTORY|O_NOFOLLOW on a symlink.
            Err(error) => assert!(
                matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)),
                "{error}"
            ),
        }
        assert!(HostMount::new('d', &path.join("real"), false).is_ok());
        std::fs::remove_dir_all(&path).unwrap();
    }
}
