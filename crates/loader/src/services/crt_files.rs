// SPDX-License-Identifier: GPL-3.0-or-later

//! The C runtime's file streams: what `fopen` opens.
//!
//! A stream reads and writes a file on the in-memory `C:` drive that the
//! handle services in `files.rs` also use, as the C runtime sits on Win32
//! files on Windows. The rules are from the Microsoft Learn pages "fopen,
//! _wfopen", "fclose, _fcloseall", "fgets, fgetws" and "perror, _wperror".
//!
//! Modes: `r`, `w` or `a`, then any of `+`, `b` and `t`. In text mode, which
//! is the default as `_fmode` starts as `_O_TEXT`, "LF characters are
//! translated to CR-LF combinations on output", "CR-LF combinations are
//! translated into single line feeds on input", and "CTRL+Z is interpreted as
//! an end-of-file character on input".
//!
//! Nothing is buffered: a write reaches the drive at once. A position is the
//! byte offset in the file on the drive, in text mode too, so a position
//! that `_ftelli64` or `fgetpos` returned can be given back to `_fseeki64` or
//! `fsetpos` ("fseek, _fseeki64", "ftell, _ftelli64", "fgetpos", "fsetpos").

use super::ObjectState;
use super::files::DriveEntry;
use crate::memory::{GuestMemory, PROCESS_HEAP};
use std::collections::BTreeMap;

/// errno.h values.
pub(super) const ENOENT: u32 = 2;
pub(super) const EBADF: u32 = 9;
/// A wide character the C locale cannot write.
pub(super) const EILSEQ: u32 = 42;
pub(super) const ENOMEM: u32 = 12;
pub(super) const EACCES: u32 = 13;
pub(super) const EINVAL: u32 = 22;
pub(super) const EMFILE: u32 = 24;
pub(super) const ERANGE: u32 = 34;
pub(super) const ENOSPC: u32 = 28;

/// The most streams `fopen` keeps open at once.
pub(super) const MAX_OPEN_STREAMS: usize = 64;
/// `EEXIST`, for `_open` with `_O_CREAT | _O_EXCL`.
pub(super) const EEXIST: u32 = 17;
/// The most file descriptors, as the CRT's default table size is 8192.
pub(super) const MAX_DESCRIPTORS: u32 = 8192;
/// `CTRL+Z`, which ends the input of a text-mode stream.
const CTRL_Z: u8 = 0x1A;

/// The text `perror` prints for an `errno` value. The page gives the text for
/// a missing file in its example; the others are the usual C library wording
/// and are Xenora's choice.
pub(super) fn error_text(errno: u32) -> &'static str {
    match errno {
        0 => "No error",
        ENOENT => "No such file or directory",
        EBADF => "Bad file descriptor",
        ENOMEM => "Not enough space",
        EACCES => "Permission denied",
        EINVAL => "Invalid argument",
        EMFILE => "Too many open files",
        ENOSPC => "No space left on device",
        ERANGE => "Result too large",
        _ => "Unknown error",
    }
}

struct Mode {
    readable: bool,
    writable: bool,
    /// Every write goes to the end of the file.
    append: bool,
    /// The file must already exist.
    existing: bool,
    /// The file's contents are destroyed on opening.
    truncate: bool,
    text: bool,
}

/// `r`, `w` or `a` first, then `+`, `b` and `t` in any order, each at most
/// once and not both `b` and `t`. Anything else is not a mode this runtime
/// opens, including the other characters the page lists.
fn parse_mode(mode: &[u8]) -> Option<Mode> {
    let (&kind, rest) = mode.split_first()?;
    let (mut update, mut binary, mut text) = (false, false, false);
    for &byte in rest {
        let seen = match byte {
            b'+' => &mut update,
            b'b' => &mut binary,
            b't' => &mut text,
            _ => return None,
        };
        if std::mem::replace(seen, true) {
            return None;
        }
    }
    if binary && text {
        return None;
    }
    let (readable, writable, append, existing, truncate) = match kind {
        b'r' => (true, update, false, true, false),
        b'w' => (update, true, false, false, true),
        b'a' => (update, true, true, false, false),
        _ => return None,
    };
    Some(Mode {
        readable,
        writable,
        append,
        existing,
        truncate,
        text: !binary,
    })
}

/// The first io operation on a stream fixes its orientation, as the CRT
/// fixes it: byte or wide. The other kind then fails. Returns false for a
/// conflict.
fn orient(stream: &mut FileStream, wide: bool) -> bool {
    let wanted = if wide { 2 } else { 1 };
    if stream.orientation == 0 {
        stream.orientation = wanted;
    }
    stream.orientation == wanted
}

struct FileStream {
    path: String,
    /// Where the next read, or the next write of a stream that does not
    /// append, takes place.
    position: u64,
    readable: bool,
    writable: bool,
    append: bool,
    text: bool,
    /// `tmpfile` opened it or `_O_TEMPORARY` was passed: the file is
    /// deleted when the stream or its descriptor closes.
    temporary: bool,
    /// The end-of-file indicator, set when a read meets the end (or the
    /// CTRL+Z of a text stream) and cleared by `clearerr` or a seek.
    eof: bool,
    /// The error indicator. Nothing sets it yet, as a write that fails
    /// reports its `errno` at once; `clearerr` clears it all the same.
    error: bool,
    /// The C orientation: 0 unoriented, 1 byte, 2 wide. The first io
    /// operation sets it; the other kind of operation then fails.
    orientation: u8,
}

/// A file descriptor, as `_open` and `_fileno` hand out. 0, 1 and 2 are
/// the standard streams and never enter this table.
enum Descriptor {
    /// `_open` opened it: the state is its own.
    Owned(FileStream),
    /// `_fileno` gave it out for a stream: the state is the stream's, as
    /// the CRT shares one file between a stream and its descriptor.
    Shared(u64),
}

#[derive(Default)]
pub(super) struct FileStreams {
    /// Open streams by the guest address of their `FILE` object.
    open: BTreeMap<u64, FileStream>,
    /// The counter `tmpnam` and `tmpfile` names come from.
    temporary_counter: u32,
    /// Open file descriptors by their number, from 3.
    descriptors: BTreeMap<u32, Descriptor>,
}

impl FileStreams {
    pub(super) fn contains(&self, file: u64) -> bool {
        self.open.contains_key(&file)
    }

    /// `fopen`. Returns the guest address of a new `FILE` object, or the
    /// `errno` value of the failure.
    pub(super) fn open(
        &mut self,
        memory: &mut GuestMemory,
        drive: &mut ObjectState,
        name: &[u8],
        mode: &[u8],
    ) -> Result<u64, u32> {
        // An empty name or mode is an invalid parameter: neither is a mode
        // nor a name the drive accepts. Xenora's choice: so is any other mode
        // it does not open and any other name the drive rejects.
        let mode = parse_mode(mode).ok_or(EINVAL)?;
        let path = ObjectState::drive_path(name).ok_or(EINVAL)?;
        if self.open.len() >= MAX_OPEN_STREAMS {
            return Err(EMFILE);
        }
        let create = match drive.drive_entry(&path) {
            DriveEntry::Directory => return Err(EACCES),
            DriveEntry::File => false,
            DriveEntry::Missing { .. } if mode.existing => return Err(ENOENT),
            DriveEntry::Missing { parent: false } => return Err(ENOENT),
            DriveEntry::Missing { parent: true } => true,
        };
        // The FILE object is allocated before the drive is touched, so a
        // failed fopen neither creates a file nor destroys the contents of
        // one.
        let file = memory
            .map_heap_allocation(PROCESS_HEAP, 8)
            .map_err(|_| ENOMEM)?;
        if create && !drive.drive_create(&path) {
            memory.release_heap_allocation(PROCESS_HEAP, file);
            return Err(ENOSPC);
        }
        if mode.truncate {
            drive.drive_truncate(&path);
        }
        self.open.insert(
            file,
            FileStream {
                path,
                position: 0,
                readable: mode.readable,
                writable: mode.writable,
                append: mode.append,
                text: mode.text,
                temporary: false,
                eof: false,
                error: false,
                orientation: 0,
            },
        );
        Ok(file)
    }

    /// `fclose`. False when `file` is not an open stream. A stream
    /// `tmpfile` opened takes its file with it.
    pub(super) fn close(
        &mut self,
        memory: &mut GuestMemory,
        drive: &mut ObjectState,
        file: u64,
    ) -> bool {
        let Some(stream) = self.open.remove(&file) else {
            return false;
        };
        if stream.temporary {
            drive.drive_remove(&stream.path);
        }
        self.drop_descriptors_of(file);
        memory.release_heap_allocation(PROCESS_HEAP, file);
        true
    }

    /// A name `tmpnam` and `tmpfile` can use: `tmp` and a decimal counter,
    /// never one the drive holds. The CRT counts from 1 for the lifetime of
    /// the process, and so does this.
    pub(super) fn temporary_name(&mut self, drive: &ObjectState) -> String {
        loop {
            self.temporary_counter += 1;
            let name = format!("tmp{}", self.temporary_counter);
            if matches!(drive.drive_entry(&name), DriveEntry::Missing { .. }) {
                return name;
            }
        }
    }

    /// `tmpfile`: a stream open for reading and writing in binary, on a
    /// file no other stream uses, deleted when the stream closes.
    pub(super) fn open_temporary(
        &mut self,
        memory: &mut GuestMemory,
        drive: &mut ObjectState,
    ) -> Result<u64, u32> {
        if self.open.len() >= MAX_OPEN_STREAMS {
            return Err(EMFILE);
        }
        let file = memory
            .map_heap_allocation(PROCESS_HEAP, 8)
            .map_err(|_| ENOMEM)?;
        let path = self.temporary_name(drive);
        if !drive.drive_create(&path) {
            memory.release_heap_allocation(PROCESS_HEAP, file);
            return Err(ENOSPC);
        }
        self.open.insert(
            file,
            FileStream {
                path,
                position: 0,
                readable: true,
                writable: true,
                append: false,
                text: false,
                temporary: true,
                eof: false,
                error: false,
                orientation: 0,
            },
        );
        Ok(file)
    }

    /// Write to an open stream, in its mode. Returns the `errno` value of a
    /// failure, with nothing written.
    pub(super) fn write(
        &mut self,
        drive: &mut ObjectState,
        file: u64,
        bytes: &[u8],
    ) -> Result<(), u32> {
        let stream = self.open.get_mut(&file).ok_or(EBADF)?;
        if !stream.writable {
            return Err(EBADF);
        }
        if !orient(stream, false) {
            return Err(EBADF);
        }
        self.write_bytes(drive, file, bytes)
    }

    /// The write itself, the orientation already taken care of.
    fn write_bytes(&mut self, drive: &mut ObjectState, file: u64, bytes: &[u8]) -> Result<(), u32> {
        let stream = self.open.get_mut(&file).ok_or(EBADF)?;
        let stored: Vec<u8> = if stream.text {
            let mut stored = Vec::with_capacity(bytes.len());
            for &byte in bytes {
                if byte == b'\n' {
                    stored.push(b'\r');
                }
                stored.push(byte);
            }
            stored
        } else {
            bytes.to_vec()
        };
        // "All write operations occur at the end of the file."
        if stream.append {
            stream.position = drive.drive_file(&stream.path).ok_or(EBADF)?.len() as u64;
        }
        match drive.drive_write(&stream.path, stream.position, &stored) {
            Some(true) => {
                stream.position += stored.len() as u64;
                Ok(())
            }
            Some(false) => Err(ENOSPC),
            // The file was deleted or moved while the stream was open.
            None => Err(EBADF),
        }
    }

    /// `fgets`: up to `limit` characters, ending after a newline. `None` is
    /// end of file with nothing read.
    /// `fputws` on a file stream: every wide character converts in the C
    /// locale — one byte of the same value, or nothing when it is above
    /// 255, which fails the write. Returns false for an orientation
    /// conflict, as the CRT returns `WEOF` for one.
    pub(super) fn write_wide(
        &mut self,
        drive: &mut ObjectState,
        file: u64,
        wide: &[u16],
    ) -> Result<bool, u32> {
        let stream = self.open.get_mut(&file).ok_or(EBADF)?;
        if !stream.writable {
            return Err(EBADF);
        }
        if !orient(stream, true) {
            return Ok(false);
        }
        let mut bytes = Vec::with_capacity(wide.len());
        for unit in wide {
            let Some(byte) = u8::try_from(*unit).ok() else {
                return Err(EILSEQ);
            };
            bytes.push(byte);
        }
        self.write_bytes(drive, file, &bytes)?;
        Ok(true)
    }

    /// `fgetws` on a file stream: bytes read as `fgets` reads them, every
    /// one zero-extended to its wide character. `None` is end of file with
    /// nothing read; the inner `Ok(false)` is an orientation conflict.
    pub(super) fn read_wide_line(
        &mut self,
        drive: &ObjectState,
        file: u64,
        limit: usize,
    ) -> Result<Option<Vec<u16>>, u32> {
        // Orient before delegating, so a conflict does not consume input.
        let stream = self.open.get_mut(&file).ok_or(EBADF)?;
        // An orientation conflict reads nothing, exactly what end of file
        // reads: `fgetws` answers null to both.
        if !orient(stream, true) {
            return Ok(None);
        }
        if !stream.readable {
            return Err(EBADF);
        }
        let data = drive.drive_file(&stream.path).ok_or(EBADF)?;
        let mut at = usize::try_from(stream.position).map_or(data.len(), |at| at.min(data.len()));
        if limit == 0 {
            let ended = at == data.len() || (stream.text && data[at] == CTRL_Z);
            return Ok((!ended).then(Vec::new));
        }
        let mut line = Vec::new();
        while line.len() < limit && at < data.len() {
            let mut byte = data[at];
            if stream.text {
                if byte == CTRL_Z {
                    break;
                }
                if byte == b'\r' && data.get(at + 1) == Some(&b'\n') {
                    at += 1;
                    byte = b'\n';
                }
            }
            at += 1;
            line.push(u16::from(byte));
            if byte == b'\n' {
                break;
            }
        }
        stream.position = at as u64;
        Ok((!line.is_empty()).then_some(line))
    }

    /// The position of a stream: where its next read takes place, or its
    /// next write unless it appends.
    pub(super) fn tell(&self, file: u64) -> Result<u64, u32> {
        self.open
            .get(&file)
            .map(|stream| stream.position)
            .ok_or(EBADF)
    }

    /// Move a stream to `offset` from the start (`origin` 0), the current
    /// position (1) or the end of the file (2). A position before the start
    /// is `EINVAL`; one past the end is allowed, and a write there fills the
    /// gap with zeros. A stream whose file was deleted fails with `EBADF`.
    pub(super) fn seek(
        &mut self,
        drive: &ObjectState,
        file: u64,
        offset: i64,
        origin: u32,
    ) -> Result<(), u32> {
        let stream = self.open.get_mut(&file).ok_or(EBADF)?;
        let base = match origin {
            0 => 0,
            1 => stream.position,
            2 => drive.drive_file(&stream.path).ok_or(EBADF)?.len() as u64,
            _ => return Err(EINVAL),
        };
        let target = i128::from(base) + i128::from(offset);
        if !(0..=i128::from(i64::MAX)).contains(&target) {
            return Err(EINVAL);
        }
        drive.drive_file(&stream.path).ok_or(EBADF)?;
        stream.position = target as u64;
        // A successful seek or fsetpos clears the end-of-file indicator
        // (ISO C 7.21.9.2 and 7.21.9.3).
        stream.eof = false;
        Ok(())
    }

    /// Raw bytes from a stream's file, for `_read`. Text mode translates
    /// CR LF into one line feed and ends at CTRL+Z, exactly as a line read
    /// does.
    pub(super) fn read_bytes(
        &mut self,
        drive: &ObjectState,
        file: u64,
        limit: usize,
    ) -> Result<Vec<u8>, u32> {
        let stream = self.open.get_mut(&file).ok_or(EBADF)?;
        if !stream.readable {
            return Err(EBADF);
        }
        let data = drive.drive_file(&stream.path).ok_or(EBADF)?;
        let mut at = usize::try_from(stream.position).map_or(data.len(), |at| at.min(data.len()));
        let start = at;
        let mut bytes = Vec::new();
        while bytes.len() < limit && at < data.len() {
            let mut byte = data[at];
            if stream.text {
                if byte == CTRL_Z {
                    break;
                }
                if byte == b'\r' && data.get(at + 1) == Some(&b'\n') {
                    at += 1;
                    byte = b'\n';
                }
            }
            at += 1;
            bytes.push(byte);
        }
        // The position advances by the bytes read, from where it was: a read
        // past the end reads nothing and leaves it there, as POSIX leaves it.
        stream.position += (at - start) as u64;
        Ok(bytes)
    }

    /// Whether a descriptor number is open, for `_isatty`.
    pub(super) fn has_descriptor(&self, fd: u32) -> bool {
        self.descriptors.contains_key(&fd)
    }

    /// The descriptor of a stream, for `_fileno`: the one it already has,
    /// or a new one that shares the stream's state, as the CRT's streams
    /// and descriptors share a file.
    pub(super) fn descriptor_of(&mut self, file: u64) -> Result<u32, u32> {
        if !self.open.contains_key(&file) {
            return Err(EBADF);
        }
        if let Some((&fd, _)) = self
            .descriptors
            .iter()
            .find(|(_, entry)| matches!(entry, Descriptor::Shared(shared) if *shared == file))
        {
            return Ok(fd);
        }
        self.insert_descriptor(Descriptor::Shared(file))
    }

    /// The lowest free descriptor from 3, or `EMFILE` when the table is as
    /// full as the CRT's default allows.
    /// Whether one more descriptor fits, so a failing open can refuse
    /// before it creates or truncates anything.
    fn has_descriptor_room(&self) -> bool {
        let mut fd = 3;
        while self.descriptors.contains_key(&fd) {
            fd += 1;
            if fd >= MAX_DESCRIPTORS {
                return false;
            }
        }
        true
    }

    fn insert_descriptor(&mut self, entry: Descriptor) -> Result<u32, u32> {
        let mut fd = 3;
        while self.descriptors.contains_key(&fd) {
            fd += 1;
            if fd >= MAX_DESCRIPTORS {
                return Err(EMFILE);
            }
        }
        self.descriptors.insert(fd, entry);
        Ok(fd)
    }

    /// The stream state a descriptor works on: its own for one `_open`
    /// made, or the stream's for one `_fileno` gave out.
    fn descriptor_stream(&mut self, fd: u32) -> Result<&mut FileStream, u32> {
        match self.descriptors.get(&fd).ok_or(EBADF)? {
            Descriptor::Shared(file) => {
                let file = *file;
                self.open.get_mut(&file).ok_or(EBADF)
            }
            Descriptor::Owned(_) => match self.descriptors.get_mut(&fd) {
                Some(Descriptor::Owned(stream)) => Ok(stream),
                _ => Err(EBADF),
            },
        }
    }

    /// `_open`. The flags are the `<fcntl.h>` values: the access mode in the
    /// low two bits, `_O_APPEND` 8, `_O_CREAT` 0x100, `_O_TRUNC` 0x200,
    /// `_O_EXCL` 0x400, `_O_TEXT` 0x4000, `_O_BINARY` 0x8000. The wide
    /// text modes stop the guest before this; the hint bits are accepted
    /// and ignored.
    pub(super) fn open_descriptor(
        &mut self,
        drive: &mut ObjectState,
        name: &[u8],
        flags: u32,
    ) -> Result<u32, u32> {
        let path = ObjectState::drive_path(name).ok_or(EINVAL)?;
        let (readable, writable) = match flags & 3 {
            0 => (true, false),
            1 => (false, true),
            2 => (true, true),
            _ => return Err(EINVAL),
        };
        let append = flags & 0x8 != 0;
        let create = flags & 0x100 != 0;
        let truncate = flags & 0x200 != 0;
        let exclusive = flags & 0x400 != 0;
        // Text unless binary, as `_fmode` starts as `_O_TEXT`.
        let text = flags & 0x8000 == 0;
        if !writable && truncate {
            return Err(EINVAL);
        }
        let create = match drive.drive_entry(&path) {
            DriveEntry::Directory => return Err(EACCES),
            DriveEntry::File => {
                if create && exclusive {
                    return Err(EEXIST);
                }
                false
            }
            DriveEntry::Missing { .. } if !create => return Err(ENOENT),
            DriveEntry::Missing { parent: false } => return Err(ENOENT),
            DriveEntry::Missing { parent: true } => true,
        };
        // A full table fails before anything is created or truncated: the
        // EMFILE of the CRT leaves the file as it was.
        if !self.has_descriptor_room() {
            return Err(EMFILE);
        }
        if create && !drive.drive_create(&path) {
            return Err(ENOSPC);
        }
        if truncate {
            drive.drive_truncate(&path);
        }
        let mut stream = FileStream {
            path,
            position: 0,
            readable,
            writable,
            append,
            text,
            temporary: flags & 0x40 != 0,
            eof: false,
            error: false,
            orientation: 0,
        };
        if append && let Some(data) = drive.drive_file(&stream.path) {
            stream.position = data.len() as u64;
        }
        self.insert_descriptor(Descriptor::Owned(stream))
    }

    /// `_read`. The bytes, or the `errno` value. An empty read at the end
    /// is no error.
    pub(super) fn descriptor_read(
        &mut self,
        drive: &ObjectState,
        fd: u32,
        limit: usize,
    ) -> Result<Vec<u8>, u32> {
        // A shared descriptor reads through the stream, indicators included.
        if let Some(Descriptor::Shared(file)) = self.descriptors.get(&fd) {
            let file = *file;
            return self.read_bytes(drive, file, limit);
        }
        let stream = self.descriptor_stream(fd)?;
        if !stream.readable {
            return Err(EBADF);
        }
        let position_path = stream.path.clone();
        let data = drive.drive_file(&position_path).ok_or(EBADF)?;
        let mut at = usize::try_from(stream.position).map_or(data.len(), |at| at.min(data.len()));
        let start = at;
        let mut bytes = Vec::new();
        while bytes.len() < limit && at < data.len() {
            let mut byte = data[at];
            if stream.text {
                if byte == CTRL_Z {
                    break;
                }
                if byte == b'\r' && data.get(at + 1) == Some(&b'\n') {
                    at += 1;
                    byte = b'\n';
                }
            }
            at += 1;
            bytes.push(byte);
        }
        // The position advances by the bytes read, from where it was: a read
        // past the end reads nothing and leaves it there, as POSIX leaves it.
        stream.position += (at - start) as u64;
        Ok(bytes)
    }

    /// `_write`. Nothing is written when the drive has no room.
    pub(super) fn descriptor_write(
        &mut self,
        drive: &mut ObjectState,
        fd: u32,
        bytes: &[u8],
    ) -> Result<(), u32> {
        if let Some(Descriptor::Shared(file)) = self.descriptors.get(&fd) {
            let file = *file;
            return self.write(drive, file, bytes);
        }
        let path = self.descriptor_stream(fd)?.path.clone();
        let stream = self.descriptor_stream(fd)?;
        if !stream.writable {
            return Err(EBADF);
        }
        let mut stored: Vec<u8> = Vec::new();
        if stream.text {
            for &byte in bytes {
                if byte == b'\n' {
                    stored.push(b'\r');
                }
                stored.push(byte);
            }
        } else {
            stored.extend_from_slice(bytes);
        }
        let position = if stream.append {
            drive.drive_file(&path).map(|data| data.len() as u64)
        } else {
            Some(stream.position)
        };
        let Some(position) = position else {
            return Err(EBADF);
        };
        // A zero-byte write touches nothing: it must not extend the file to
        // the cursor, as a write of zero bytes has no effect.
        if !stored.is_empty() {
            let written = drive.drive_write(&path, position, &stored).ok_or(EBADF)?;
            if !written {
                return Err(ENOSPC);
            }
        }
        stream.position = position + stored.len() as u64;
        Ok(())
    }

    /// `_lseek`. A position before the start, one that no 32-bit `long` can
    /// hold, or an unknown origin is -1 with `EINVAL`. A seek clears the
    /// end-of-file indicator of a shared stream, as `_fseeki64` does.
    pub(super) fn descriptor_seek(
        &mut self,
        drive: &ObjectState,
        fd: u32,
        offset: i32,
        origin: u32,
    ) -> Result<i64, u32> {
        if origin > 2 {
            return Err(EINVAL);
        }
        // A shared descriptor seeks the stream.
        if let Some(Descriptor::Shared(file)) = self.descriptors.get(&fd) {
            let file = *file;
            self.seek(drive, file, i64::from(offset), origin)?;
            return self.tell(file).map(|position| position as i64);
        }
        let path = self.descriptor_stream(fd)?.path.clone();
        let stream = self.descriptor_stream(fd)?;
        let base = match origin {
            0 => 0,
            1 => stream.position,
            2 => drive.drive_file(&path).ok_or(EBADF)?.len() as u64,
            _ => return Err(EINVAL),
        };
        let target = i128::from(base) + i128::from(offset);
        if !(0..=i128::from(i32::MAX)).contains(&target) {
            return Err(EINVAL);
        }
        drive.drive_file(&stream.path).ok_or(EBADF)?;
        stream.position = target as u64;
        Ok(target as i64)
    }

    /// `_close`. Closing a shared descriptor closes the underlying file, so
    /// the stream it came from fails afterwards, as it does in the CRT.
    pub(super) fn descriptor_close(
        &mut self,
        memory: &mut GuestMemory,
        drive: &mut ObjectState,
        fd: u32,
    ) -> bool {
        match self.descriptors.remove(&fd) {
            Some(Descriptor::Owned(stream)) => {
                if stream.temporary {
                    drive.drive_remove(&stream.path);
                }
                true
            }
            Some(Descriptor::Shared(file)) => {
                self.open.remove(&file);
                memory.release_heap_allocation(PROCESS_HEAP, file);
                true
            }
            None => false,
        }
    }

    /// Drop a stream's shared descriptors when the stream itself is closed,
    /// so a later `_fileno` of a recycled stream address cannot pick them up.
    pub(super) fn drop_descriptors_of(&mut self, file: u64) {
        self.descriptors
            .retain(|_, entry| !matches!(entry, Descriptor::Shared(shared) if *shared == file));
    }

    /// The end-of-file indicator of a stream.
    pub(super) fn end_of_file(&self, file: u64) -> bool {
        self.open.get(&file).is_some_and(|stream| stream.eof)
    }

    /// `clearerr`: both indicators off. False when `file` is not open.
    pub(super) fn clear_errors(&mut self, file: u64) -> bool {
        let Some(stream) = self.open.get_mut(&file) else {
            return false;
        };
        stream.eof = false;
        stream.error = false;
        true
    }

    pub(super) fn read_line(
        &mut self,
        drive: &ObjectState,
        file: u64,
        limit: usize,
    ) -> Result<Option<Vec<u8>>, u32> {
        let stream = self.open.get_mut(&file).ok_or(EBADF)?;
        if !stream.readable {
            return Err(EBADF);
        }
        if !orient(stream, false) {
            return Err(EBADF);
        }
        let data = drive.drive_file(&stream.path).ok_or(EBADF)?;
        let mut at = usize::try_from(stream.position).map_or(data.len(), |at| at.min(data.len()));
        let ended = at == data.len() || (stream.text && data[at] == CTRL_Z);
        if limit == 0 {
            // No room to read a character: an empty line unless the stream
            // has ended.
            return Ok((!ended).then(Vec::new));
        }
        let mut line = Vec::new();
        while line.len() < limit && at < data.len() {
            let mut byte = data[at];
            if stream.text {
                if byte == CTRL_Z {
                    break;
                }
                if byte == b'\r' && data.get(at + 1) == Some(&b'\n') {
                    at += 1;
                    byte = b'\n';
                }
            }
            at += 1;
            line.push(byte);
            if byte == b'\n' {
                break;
            }
        }
        // The position advances by the bytes read, from where it was: a read
        // past the end reads nothing and leaves it there, as POSIX leaves it.
        stream.position = at as u64;
        if line.is_empty() {
            // End of file: the indicator is set, as ISO C says of the read
            // functions.
            stream.eof = true;
            return Ok(None);
        }
        Ok(Some(line))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_mode;

    /// The six access types of the `fopen` page, each alone and with `t` or
    /// `b`, and what the page says of each.
    #[test]
    fn modes_follow_the_fopen_page() {
        // (mode, readable, writable, append, must exist, truncates)
        for (mode, readable, writable, append, existing, truncate) in [
            ("r", true, false, false, true, false),
            ("w", false, true, false, false, true),
            ("a", false, true, true, false, false),
            ("r+", true, true, false, true, false),
            ("w+", true, true, false, false, true),
            ("a+", true, true, true, false, false),
        ] {
            for (suffix, text) in [("", true), ("t", true), ("b", false)] {
                let mut spellings = vec![format!("{mode}{suffix}")];
                if mode.len() == 2 && !suffix.is_empty() {
                    // "rb+" is the same mode as "r+b".
                    spellings.push(format!("{}{suffix}+", &mode[..1]));
                }
                for spelling in spellings {
                    let parsed = parse_mode(spelling.as_bytes())
                        .unwrap_or_else(|| panic!("{spelling} is a mode"));
                    assert_eq!(
                        (
                            parsed.readable,
                            parsed.writable,
                            parsed.append,
                            parsed.existing,
                            parsed.truncate,
                            parsed.text
                        ),
                        (readable, writable, append, existing, truncate, text),
                        "{spelling}"
                    );
                }
            }
        }
        // "If t or b is prefixed to the argument, the function fails." The
        // other mode characters the page lists are not opened here, and
        // neither is anything that is not a mode.
        for mode in [
            "",
            "t",
            "b",
            "tr",
            "br",
            "+r",
            "x",
            "rw",
            "wr",
            "ra",
            "rbt",
            "rtb",
            "r++",
            "rbb",
            "wtt",
            "wx",
            "w+x",
            "rc",
            "rn",
            "rN",
            "rS",
            "rR",
            "wT",
            "wD",
            "r,ccs=UTF-8",
            "R",
            "W",
            "r ",
            " r",
            "rb+t",
        ] {
            assert!(parse_mode(mode.as_bytes()).is_none(), "{mode:?}");
        }
    }
}
