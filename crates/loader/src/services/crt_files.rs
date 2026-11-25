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
pub(super) const ENOMEM: u32 = 12;
pub(super) const EACCES: u32 = 13;
pub(super) const EINVAL: u32 = 22;
pub(super) const EMFILE: u32 = 24;
pub(super) const ERANGE: u32 = 34;
pub(super) const ENOSPC: u32 = 28;

/// The most streams `fopen` keeps open at once.
pub(super) const MAX_OPEN_STREAMS: usize = 64;
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

struct FileStream {
    path: String,
    /// Where the next read, or the next write of a stream that does not
    /// append, takes place.
    position: u64,
    readable: bool,
    writable: bool,
    append: bool,
    text: bool,
}

#[derive(Default)]
pub(super) struct FileStreams {
    /// Open streams by the guest address of their `FILE` object.
    open: BTreeMap<u64, FileStream>,
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
            },
        );
        Ok(file)
    }

    /// `fclose`. False when `file` is not an open stream.
    pub(super) fn close(&mut self, memory: &mut GuestMemory, file: u64) -> bool {
        if self.open.remove(&file).is_none() {
            return false;
        }
        memory.release_heap_allocation(PROCESS_HEAP, file);
        true
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
        Ok(())
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
        stream.position = at as u64;
        Ok((!line.is_empty()).then_some(line))
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
