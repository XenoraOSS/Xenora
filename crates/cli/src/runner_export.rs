// SPDX-License-Identifier: GPL-3.0-or-later

//! One bounded synthetic guest file, staged by the supervisor before launching the worker.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Seek, Write};
use std::os::fd::RawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use xenora_loader::ServicesState;
use xenora_loader::output_path::{OutputKind, validate_output_paths};

use super::diagnostics::{adopt_endpoint, inherit_endpoint};

const MAX_BYTES: u64 = 16 * 1024 * 1024;
static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);

pub(super) struct SupervisorExport {
    destination: PathBuf,
    staging: PathBuf,
}

impl SupervisorExport {
    /// Reserve a private stage next to the destination and pass its open descriptor.
    pub(super) fn attach(command: &mut Command, destination: &Path) -> io::Result<(Self, RawFd)> {
        // Read the literal last component. Path::file_name skips a trailing ".".
        let leaf = destination
            .as_os_str()
            .as_encoded_bytes()
            .rsplit(|byte| *byte == b'/')
            .next()
            .filter(|leaf| !leaf.is_empty())
            .and_then(|leaf| std::str::from_utf8(leaf).ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "expected a UTF-8 export filename",
                )
            })?;
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = validate_output_paths(parent, &[leaf], OutputKind::Export)?;
        if !fs::metadata(&parent)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "expected an existing export directory",
            ));
        }
        let destination = parent.join(leaf);
        match fs::symlink_metadata(&destination) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "export destination '{}' already exists",
                        destination.display()
                    ),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let staging = reserve_stage(&parent)?;
        let export = Self {
            destination,
            staging,
        };
        let endpoint = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(export.staging.join("data"))?;
        let fd = inherit_endpoint(
            command,
            endpoint.into(),
            "guest export requires non-stdio descriptors",
        )?;
        Ok((export, fd))
    }

    /// Publish only after the worker has exited successfully and been reaped.
    pub(super) fn publish(self) -> io::Result<()> {
        let staged = self.staging.join("data");
        let metadata = fs::symlink_metadata(&staged)?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "staged guest export must be a regular file of at most 16 MiB",
            ));
        }
        // Linking installs the finished file atomically and refuses any existing leaf.
        fs::hard_link(staged, &self.destination).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "cannot publish guest file to '{}': {error}",
                    self.destination.display()
                ),
            )
        })
    }
}

impl Drop for SupervisorExport {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.staging.join("data"));
        let _ = fs::remove_dir(&self.staging);
    }
}

fn reserve_stage(parent: &Path) -> io::Result<PathBuf> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    for _ in 0..64 {
        let id = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".xenora-export-{}-{id}", std::process::id()));
        match builder.create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not reserve a guest export staging directory",
    ))
}

/// Adopt the file inherited from the supervisor.
///
/// # Safety
/// A live non-stdio descriptor must be unclaimed and owned by this worker.
pub(super) unsafe fn from_inherited(fd: RawFd) -> io::Result<File> {
    // SAFETY: the caller owns the unclaimed inherited descriptor.
    let mut file =
        File::from(unsafe { adopt_endpoint(fd, "guest export descriptor must be outside stdio")? });
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != 0 || file.stream_position()? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "guest export descriptor must refer to a new empty regular file",
        ));
    }
    Ok(file)
}

fn guest_file_key(path: &str) -> Option<String> {
    let separators = ['\\', '/'];
    let relative = match path.split_once(':') {
        Some((drive, tail)) if drive.eq_ignore_ascii_case("c") => tail.strip_prefix(separators)?,
        Some(_) => return None,
        None => path.strip_prefix(separators).unwrap_or(path),
    };

    // Export paths are anchored at the synthetic drive root. Reject extra
    // separators and drive syntax before a later dot component can erase them.
    let mut components = Vec::new();
    for component in relative.split(separators) {
        match component {
            "" => return None,
            "." => {}
            ".." => {
                components.pop();
            }
            name if name.contains(':') => return None,
            name => components.push(name),
        }
    }
    (!components.is_empty()).then(|| components.join("\\"))
}

pub(super) fn stage_copy(
    mut file: File,
    state: &ServicesState,
    guest_path: &str,
) -> io::Result<()> {
    let contents = guest_file_key(guest_path)
        .and_then(|name| state.file_contents(&name))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("guest file {guest_path:?} does not exist"),
            )
        })?;
    if contents.len() as u64 > MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guest file exceeds the 16 MiB export limit",
        ));
    }
    file.write_all(&contents)
}
