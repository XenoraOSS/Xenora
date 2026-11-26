// SPDX-License-Identifier: GPL-3.0-or-later

use super::{Direction, MAX_EVENTS, parse_direction};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::Path;

const HEADER: &str = "xenora-session-v1";
const MAX_BYTES: u64 = 256;

pub(super) fn read_directions(path: &Path) -> Result<Vec<Direction>, String> {
    read(path).map_err(|error| format!("replay file {}: {error}", path.display()))
}

fn read(path: &Path) -> Result<Vec<Direction>, String> {
    // Follow symlinks to regular files, but avoid opening known special files.
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("expected a regular file".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // Darwin sys/fcntl.h. The path could have become a FIFO since stat.
        const O_NONBLOCK: i32 = 0x00000004;
        options.custom_flags(O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("expected a regular file".into());
    }

    let too_large = || format!("exceeds {MAX_BYTES} bytes");
    if metadata.len() > MAX_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(too_large());
    }

    let contents = std::str::from_utf8(&bytes)
        .map_err(|error| format!("invalid UTF-8 at byte {}", error.valid_up_to() + 1))?;
    let mut lines = contents.lines();
    if lines.next() != Some(HEADER) {
        return Err(format!("line 1: expected {HEADER}"));
    }
    let mut directions = Vec::new();
    for (index, line) in lines.enumerate() {
        let number = index + 2;
        if directions.len() == MAX_EVENTS {
            return Err(format!("line {number}: at most {MAX_EVENTS} directions"));
        }
        let direction = parse_direction(line)
            .ok_or_else(|| format!("line {number}: expected left or right"))?;
        directions.push(direction);
    }
    if directions.is_empty() {
        return Err("line 2: expected at least one direction".into());
    }
    Ok(directions)
}
