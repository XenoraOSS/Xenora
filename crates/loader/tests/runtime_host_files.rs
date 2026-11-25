// SPDX-License-Identifier: GPL-3.0-or-later

//! Guest drives backed by temporary host directories, used through the guest
//! import table. Each test builds its own directory tree, including files
//! outside the mount and symbolic links that point to them, and checks that
//! the guest can neither read nor change anything outside the mounted root.
//!
//! Error codes and dispositions follow the public CreateFileW, ReadFile,
//! FindFirstFileW and System Error Codes documentation.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use support::*;
use xenora_loader::services::host_files::HostMount;
use xenora_loader::services::{MockClock, ServicesState};

const KERNEL32: &str = "kernel32.dll";
const FILE_API: &str = "api-ms-win-core-file-l1-1-0.dll";
const UCRT: &str = "ucrtbase.dll";

const IMPORTS: &[(&str, &str)] = &[
    (KERNEL32, "GetLastError"),
    (FILE_API, "CreateFileA"),
    (FILE_API, "CreateFileW"),
    (FILE_API, "GetTempFileNameW"),
    (KERNEL32, "ReadFile"),
    (KERNEL32, "WriteFile"),
    (KERNEL32, "CloseHandle"),
    (KERNEL32, "DuplicateHandle"),
    (FILE_API, "GetFileSize"),
    (FILE_API, "GetFileType"),
    (FILE_API, "GetFileTime"),
    (FILE_API, "SetFileTime"),
    (KERNEL32, "GetFileSizeEx"),
    (FILE_API, "FlushFileBuffers"),
    (KERNEL32, "SetFilePointerEx"),
    (KERNEL32, "SetEndOfFile"),
    (FILE_API, "GetFileAttributesA"),
    (FILE_API, "GetFileAttributesW"),
    (FILE_API, "GetFileAttributesExW"),
    (KERNEL32, "GetFileAttributesExA"),
    (KERNEL32, "FindFirstFileW"),
    (KERNEL32, "FindNextFileW"),
    (KERNEL32, "FindClose"),
    (KERNEL32, "CreateDirectoryW"),
    (KERNEL32, "RemoveDirectoryW"),
    (FILE_API, "DeleteFileA"),
    (KERNEL32, "DeleteFileW"),
    (KERNEL32, "MoveFileW"),
    (FILE_API, "MoveFileExW"),
    (UCRT, "fopen"),
    (UCRT, "fclose"),
    (UCRT, "fread"),
    (UCRT, "fwrite"),
];

const GENERIC_READ: u64 = 0x8000_0000;
const GENERIC_WRITE: u64 = 0x4000_0000;
const CREATE_NEW: u64 = 1;
const CREATE_ALWAYS: u64 = 2;
const OPEN_EXISTING: u64 = 3;
const OPEN_ALWAYS: u64 = 4;
const INVALID: u64 = u64::MAX;

const PATH: u64 = SCRATCH;
const OTHER_PATH: u64 = SCRATCH + 0x300;
const BUFFER: u64 = SCRATCH + 0x600;
const COUNT: u64 = SCRATCH + 0x700;
const FIND_DATA: u64 = SCRATCH + 0x800;

/// A fresh directory, removed on drop. It lives under `XENORA_TEST_TMP` or
/// `/tmp`, which must support hard links and POSIX permissions.
struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Self {
        let base = std::env::var_os("XENORA_TEST_TMP")
            .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
        let path = base.join(format!("xenora-host-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn guest_with(mounts: Vec<HostMount>) -> Guest {
    let mut state = ServicesState::new(Arc::new(MockClock::new(0)));
    for mount in mounts {
        assert!(state.mount(mount).is_ok());
    }
    Guest::with_state(IMPORTS, &[], state)
}

fn put_path(guest: &mut Guest, address: u64, path: &str) {
    for (index, unit) in path.encode_utf16().chain([0]).enumerate() {
        guest
            .memory
            .write_u16(address + 2 * index as u64, unit)
            .unwrap();
    }
}

fn open(guest: &mut Guest, path: &str, access: u64, share: u64, disposition: u64) -> u64 {
    put_path(guest, PATH, path);
    guest.call(
        "CreateFileW",
        &[PATH, access, share, 0, disposition, 0x80, 0],
    )
}

fn read(guest: &mut Guest, handle: u64, count: u64) -> Option<Vec<u8>> {
    guest.memory.write_u32(COUNT, u32::MAX).unwrap();
    if guest.call("ReadFile", &[handle, BUFFER, count, COUNT, 0]) == 0 {
        return None;
    }
    let read = guest.u32_at(COUNT) as usize;
    let mut bytes = vec![0; read];
    guest.memory.read_bytes(BUFFER, &mut bytes).unwrap();
    Some(bytes)
}

fn write(guest: &mut Guest, handle: u64, bytes: &[u8]) -> bool {
    guest.memory.write_bytes(BUFFER, bytes).unwrap();
    let written = guest.call("WriteFile", &[handle, BUFFER, bytes.len() as u64, COUNT, 0]);
    written == 1 && guest.u32_at(COUNT) as usize == bytes.len()
}

fn attributes(guest: &mut Guest, path: &str) -> u64 {
    put_path(guest, PATH, path);
    guest.call("GetFileAttributesW", &[PATH])
}

fn path_call(guest: &mut Guest, symbol: &str, path: &str) -> u64 {
    put_path(guest, PATH, path);
    match symbol {
        "CreateDirectoryW" => guest.call(symbol, &[PATH, 0]),
        _ => guest.call(symbol, &[PATH]),
    }
}

fn found_name(guest: &Guest) -> String {
    let units: Vec<u16> = (0..260)
        .map(|index| guest.u16_at(FIND_DATA + 44 + 2 * index))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16(&units).unwrap()
}

fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[test]
fn synthetic_timestamp_suppression_is_scoped_to_the_open_file() {
    let clock = Arc::new(MockClock::new(100));
    let mut guest = Guest::with_state(IMPORTS, &[], ServicesState::new(clock.clone()));
    let name = r"C:\Time.dat";
    let access = GENERIC_READ | GENERIC_WRITE;
    let pinned = open(&mut guest, name, access, 3, CREATE_NEW);
    let normal = open(&mut guest, name, access, 3, OPEN_EXISTING);
    assert_ne!(pinned, INVALID);
    assert_ne!(normal, INVALID);
    guest.memory.write_u64(COUNT, u64::MAX).unwrap();
    assert_eq!(guest.call("SetFileTime", &[pinned, 0, COUNT, COUNT]), 1);
    let process = u64::MAX;
    assert_eq!(
        guest.call(
            "DuplicateHandle",
            &[process, pinned, process, COUNT, 0, 0, 2]
        ),
        1
    );
    let duplicate = guest.u64_at(COUNT);
    assert_eq!(guest.call("CloseHandle", &[pinned]), 1);
    let pinned = duplicate;
    clock.advance(10);
    assert!(write(&mut guest, pinned, b"x"));
    assert_eq!(guest.call("SetFilePointerEx", &[pinned, 0, 0, 0]), 1);
    assert_eq!(read(&mut guest, pinned, 1).unwrap(), b"x");
    assert_eq!(
        guest.call("GetFileTime", &[pinned, 0, BUFFER, BUFFER + 8]),
        1
    );
    assert_eq!(guest.u64_at(BUFFER), 100);
    assert_eq!(guest.u64_at(BUFFER + 8), 100);

    clock.advance(10);
    assert_eq!(read(&mut guest, normal, 1).unwrap(), b"x");
    assert!(write(&mut guest, normal, b"y"));
    assert_eq!(
        guest.call("GetFileTime", &[pinned, 0, BUFFER, BUFFER + 8]),
        1
    );
    assert_eq!(guest.u64_at(BUFFER), 120);
    assert_eq!(guest.u64_at(BUFFER + 8), 120);
}

#[test]
fn synthetic_file_times_follow_the_file_until_deletion() {
    let clock = Arc::new(MockClock::new(100));
    let state = ServicesState::new(clock.clone());
    let mut guest = Guest::with_state(IMPORTS, &[], state);
    let source = r"C:\Before.dat";
    let target = r"C:\After.dat";
    let handle = open(
        &mut guest,
        source,
        GENERIC_READ | GENERIC_WRITE,
        0,
        CREATE_NEW,
    );
    assert_ne!(handle, INVALID);
    assert_eq!(
        guest.call("GetFileTime", &[handle, BUFFER, BUFFER + 8, BUFFER + 16]),
        1
    );
    assert_eq!(guest.u64_at(BUFFER), 100);
    assert_eq!(guest.u64_at(BUFFER + 8), 100);
    assert_eq!(guest.u64_at(BUFFER + 16), 100);
    let created = 133_444_736_001_234_567;
    let modified = 133_444_736_012_345_678;
    guest.memory.write_u64(COUNT, created).unwrap();
    guest.memory.write_u64(COUNT + 8, modified).unwrap();
    assert_eq!(guest.call("SetFileTime", &[handle, COUNT, 0, COUNT + 8]), 1);
    assert_eq!(
        guest.call("GetFileTime", &[handle, BUFFER, BUFFER + 8, BUFFER + 16]),
        1
    );
    assert_eq!(guest.u64_at(BUFFER), created);
    assert_eq!(guest.u64_at(BUFFER + 8), 100);
    assert_eq!(guest.u64_at(BUFFER + 16), modified);
    clock.advance(10);
    assert!(write(&mut guest, handle, b"data"));
    assert_eq!(guest.call("SetFilePointerEx", &[handle, 0, 0, 0]), 1);
    clock.advance(10);
    assert_eq!(read(&mut guest, handle, 4).unwrap(), b"data");
    assert_eq!(
        guest.call("GetFileTime", &[handle, 0, BUFFER, BUFFER + 8]),
        1
    );
    assert_eq!(guest.u64_at(BUFFER), 120);
    assert_eq!(guest.u64_at(BUFFER + 8), 110);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);

    put_path(&mut guest, OTHER_PATH, target);
    assert_eq!(guest.call("MoveFileW", &[PATH, OTHER_PATH]), 1);
    clock.advance(10);
    let handle = open(&mut guest, target, GENERIC_WRITE, 0, CREATE_ALWAYS);
    assert_ne!(handle, INVALID);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);
    assert_eq!(guest.call("GetFileAttributesExW", &[PATH, 0, BUFFER]), 1);
    assert_eq!(guest.u64_at(BUFFER + 4), created);
    assert_eq!(guest.u64_at(BUFFER + 20), 130);

    let search = guest.call("FindFirstFileW", &[PATH, FIND_DATA]);
    assert_ne!(search, INVALID);
    assert_eq!(guest.u64_at(FIND_DATA + 4), created);
    assert_eq!(guest.u64_at(FIND_DATA + 20), 130);
    assert_eq!(guest.call("FindClose", &[search]), 1);
    assert_eq!(guest.call("DeleteFileW", &[PATH]), 1);

    let handle = open(&mut guest, target, GENERIC_READ, 0, CREATE_NEW);
    assert_ne!(handle, INVALID);
    assert_eq!(
        guest.call("GetFileTime", &[handle, BUFFER, 0, BUFFER + 8]),
        1
    );
    assert_eq!(guest.u64_at(BUFFER), 130);
    assert_eq!(guest.u64_at(BUFFER + 8), 130);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);
}

#[test]
fn guest_sets_mounted_write_time_by_handle() {
    let tree = Tree::new("set-filetime");
    let root = tree.path("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("Times.dat"), b"unchanged").unwrap();
    let mount = HostMount::new('D', &root, true).unwrap();
    let mut guest = guest_with(vec![mount]);
    let name = r"D:\Times.dat";
    let readonly = open(&mut guest, name, GENERIC_READ, 0, OPEN_EXISTING);
    assert_ne!(readonly, INVALID);
    assert_eq!(guest.call("SetFileTime", &[readonly, 0, 0, COUNT]), 0);
    assert_eq!(guest.error(), 5);
    assert_eq!(guest.call("CloseHandle", &[readonly]), 1);

    let handle = open(
        &mut guest,
        name,
        GENERIC_READ | GENERIC_WRITE,
        0,
        OPEN_EXISTING,
    );
    assert_ne!(handle, INVALID);
    assert_eq!(guest.call("GetFileTime", &[handle, 0, BUFFER, 0]), 1);
    let accessed = guest.u64_at(BUFFER);
    let modified = 133_444_736_012_345_678;
    guest.memory.write_u64(COUNT, modified).unwrap();
    assert_eq!(guest.call("SetFileTime", &[handle, 0, 0, COUNT]), 1);
    assert_eq!(
        guest.call("GetFileTime", &[handle, 0, BUFFER, BUFFER + 8]),
        1
    );
    assert_eq!(guest.u64_at(BUFFER), accessed);
    assert_eq!(guest.u64_at(BUFFER + 8), modified);
    assert_eq!(guest.call("SetFileTime", &[handle, 0, COUNT, 1]), 0);
    assert_eq!(guest.error(), 998);
    assert_eq!(guest.call("GetFileTime", &[handle, 0, BUFFER, 0]), 1);
    assert_eq!(guest.u64_at(BUFFER), accessed);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);
    assert_eq!(std::fs::read(root.join("Times.dat")).unwrap(), b"unchanged");
}

#[test]
fn mounted_metadata_reports_windows_filetimes() {
    use std::time::{Duration, UNIX_EPOCH};

    let tree = Tree::new("filetimes");
    let root = tree.path("root");
    std::fs::create_dir_all(&root).unwrap();
    let file = std::fs::File::create(root.join("Times.dat")).unwrap();
    let accessed = UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_700);
    let modified = UNIX_EPOCH + Duration::new(1_700_000_001, 234_567_800);
    file.set_times(
        std::fs::FileTimes::new()
            .set_accessed(accessed)
            .set_modified(modified),
    )
    .unwrap();
    let mount = HostMount::new('D', &root, false).unwrap();
    let mut guest = guest_with(vec![mount]);
    put_path(&mut guest, PATH, r"D:\Times.dat");

    assert_eq!(guest.call("GetFileAttributesExW", &[PATH, 0, BUFFER]), 1);
    let created = guest.u64_at(BUFFER + 4);
    #[cfg(target_os = "macos")]
    assert_ne!(created, 0);
    assert_eq!(guest.u64_at(BUFFER + 12), 133_444_736_001_234_567);
    assert_eq!(guest.u64_at(BUFFER + 20), 133_444_736_012_345_678);

    let handle = open(&mut guest, r"D:\Times.dat", GENERIC_READ, 0, OPEN_EXISTING);
    assert_ne!(handle, INVALID);
    guest.memory.write_u64(COUNT, u64::MAX).unwrap();
    assert_eq!(guest.call("GetFileTime", &[handle, COUNT, 0, 1]), 0);
    assert_eq!(guest.error(), 998);
    assert_eq!(guest.u64_at(COUNT), u64::MAX);
    assert_eq!(guest.call("GetFileTime", &[handle, COUNT, 0, COUNT + 8]), 1);
    assert_eq!(guest.u64_at(COUNT), created);
    assert_eq!(guest.u64_at(COUNT + 8), 133_444_736_012_345_678);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);

    let search = guest.call("FindFirstFileW", &[PATH, FIND_DATA]);
    assert_ne!(search, INVALID);
    assert_eq!(guest.u64_at(FIND_DATA + 4), created);
    assert_eq!(guest.u64_at(FIND_DATA + 12), guest.u64_at(BUFFER + 12));
    assert_eq!(guest.u64_at(FIND_DATA + 20), guest.u64_at(BUFFER + 20));
    assert_eq!(guest.call("FindClose", &[search]), 1);
}

#[test]
fn legacy_file_size_splits_large_lengths_and_distinguishes_failure() {
    let tree = Tree::new("legacy-size");
    let root = tree.path("root");
    std::fs::create_dir_all(&root).unwrap();
    let host_file = std::fs::File::create(root.join("Sparse.dat")).unwrap();
    host_file.set_len(0x1_ffff_ffff).unwrap();
    let mut guest = guest_with(vec![HostMount::new('D', &root, false).unwrap()]);
    let file = open(&mut guest, r"D:\Sparse.dat", GENERIC_READ, 0, OPEN_EXISTING);
    assert_ne!(file, INVALID);
    assert_eq!(guest.call("GetFileType", &[file]), 1);
    guest.memory.write_bytes(BUFFER, &[0xff; 40]).unwrap();
    assert_eq!(guest.call("GetFileAttributesExW", &[PATH, 1, BUFFER]), 0);
    assert_eq!(guest.error(), 87);
    assert_eq!(guest.u32_at(BUFFER), u32::MAX);
    assert_eq!(guest.call("GetFileAttributesExW", &[PATH, 0, 1]), 0);
    assert_eq!(guest.error(), 998);
    assert_eq!(guest.call("GetFileAttributesExW", &[PATH, 0, BUFFER]), 1);
    assert_eq!(guest.u32_at(BUFFER), 1);
    assert_eq!(guest.u32_at(BUFFER + 28), 1);
    assert_eq!(guest.u32_at(BUFFER + 32), u32::MAX);
    assert_eq!(guest.u32_at(BUFFER + 36), u32::MAX);

    guest.memory.write_bytes(PATH, b"D:\\Sparse.dat\0").unwrap();
    assert_eq!(guest.call("GetFileAttributesExA", &[PATH, 0, BUFFER]), 1);
    assert_eq!(guest.u32_at(BUFFER + 28), 1);
    let invalid_size = u64::from(u32::MAX);
    guest.memory.write_u64(COUNT, u64::MAX).unwrap();

    assert_eq!(guest.call("GetFileSize", &[file, 1]), invalid_size);
    assert_eq!(guest.error(), 998);
    assert_eq!(guest.call("GetFileSize", &[file, COUNT]), invalid_size);
    assert_eq!(guest.error(), 0);
    assert_eq!(guest.u64_at(COUNT), 0xffff_ffff_0000_0001);
    host_file.set_len(0x1_0000_0007).unwrap();
    assert_eq!(guest.call("GetFileSize", &[file, 0]), 7);
    assert_eq!(guest.call("GetFileSizeEx", &[file, COUNT]), 1);
    assert_eq!(guest.u64_at(COUNT), 0x1_0000_0007);
    assert_eq!(guest.call("CloseHandle", &[file]), 1);
    assert_eq!(guest.call("GetFileSize", &[file, COUNT]), invalid_size);
    assert_eq!(guest.error(), 6);
    assert_eq!(guest.call("GetFileType", &[file]), 0);
    assert_eq!(guest.error(), 6);
    assert_eq!(guest.u64_at(COUNT), 0x1_0000_0007);
}

#[test]
fn ansi_create_file_uses_the_same_mount_and_sharing_rules() {
    let tree = Tree::new("ansi-create");
    let root = tree.path("root");
    std::fs::create_dir_all(&root).unwrap();
    let mut guest = guest_with(vec![HostMount::new('D', &root, true).unwrap()]);
    let open_ansi = |guest: &mut Guest, name: &[u8], disposition| {
        guest.memory.write_bytes(PATH, name).unwrap();
        guest.call(
            "CreateFileA",
            &[PATH, GENERIC_WRITE, 0, 0, disposition, 0x80, 0],
        )
    };

    let file = open_ansi(&mut guest, b"D:\\Mixed.txt\0", CREATE_NEW);
    assert_ne!(file, INVALID);
    assert_eq!(guest.call("GetFileTime", &[file, 0, 0, COUNT]), 0);
    assert_eq!(guest.error(), 5);
    assert!(write(&mut guest, file, b"from ANSI"));
    assert_eq!(guest.call("FlushFileBuffers", &[file]), 1);
    assert_eq!(
        open(&mut guest, r"D:\mixed.txt", GENERIC_READ, 1, OPEN_EXISTING),
        INVALID
    );
    assert_eq!(guest.error(), 32);
    assert_eq!(guest.call("CloseHandle", &[file]), 1);
    assert_eq!(
        open_ansi(&mut guest, b"D:\\mixed.txt\0", CREATE_NEW),
        INVALID
    );
    assert_eq!(guest.error(), 80);
    assert_eq!(
        open_ansi(&mut guest, b"D:\\\xff.txt\0", CREATE_NEW),
        INVALID
    );
    assert_eq!(guest.error(), 123);
    assert_eq!(std::fs::read(root.join("Mixed.txt")).unwrap(), b"from ANSI");
}

#[test]
fn ansi_delete_uses_mount_permissions_and_sharing() {
    let tree = Tree::new("ansi-delete");
    let root = tree.path("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("Mixed.txt"), b"keep until closed").unwrap();
    let mut guest = guest_with(vec![
        HostMount::new('D', &root, true).unwrap(),
        HostMount::new('E', &root, false).unwrap(),
    ]);
    let open_file = open(&mut guest, r"D:\Mixed.txt", GENERIC_READ, 1, OPEN_EXISTING);
    assert_ne!(open_file, INVALID);
    assert_eq!(guest.call("FlushFileBuffers", &[open_file]), 0);
    assert_eq!(guest.error(), 5);
    let delete = |guest: &mut Guest, name: &[u8]| {
        guest.memory.write_bytes(PATH, name).unwrap();
        guest.call("DeleteFileA", &[PATH])
    };
    assert_eq!(delete(&mut guest, b"E:\\mixed.txt\0"), 0);
    assert_eq!(guest.error(), 5);
    assert_eq!(delete(&mut guest, b"D:\\mixed.txt\0"), 0);
    assert_eq!(guest.error(), 32);
    assert!(root.join("Mixed.txt").exists());
    assert_eq!(guest.call("CloseHandle", &[open_file]), 1);
    assert_eq!(guest.call("FlushFileBuffers", &[open_file]), 0);
    assert_eq!(guest.error(), 6);
    assert_eq!(delete(&mut guest, b"D:\\mixed.txt\0"), 1);
    assert!(!root.join("Mixed.txt").exists());
    assert_eq!(delete(&mut guest, b"D:\\mixed.txt\0"), 0);
    assert_eq!(guest.error(), 2);
}

#[test]
fn temporary_files_use_the_mounted_directory_and_skip_existing_names() {
    let tree = Tree::new("temp-file");
    let root = tree.path("root");
    std::fs::create_dir_all(root.join("Temp")).unwrap();
    std::fs::write(root.join("Temp/PRE0001.tmp"), b"original").unwrap();
    let writable = HostMount::new('D', &root, true).unwrap();
    let readonly = HostMount::new('E', &root, false).unwrap();
    let mut guest = guest_with(vec![writable, readonly]);
    let prefix = PATH + 0x200;
    put_path(&mut guest, prefix, "prefix");
    put_path(&mut guest, PATH, r"D:\temp");
    assert_eq!(guest.call("GetTempFileNameW", &[PATH, prefix, 0, 1]), 0);
    assert_eq!(guest.error(), 998);
    assert_eq!(
        guest.call("GetTempFileNameW", &[PATH, prefix, 0, BUFFER]),
        2
    );
    let name: Vec<_> = (0..32)
        .map(|i| guest.u16_at(BUFFER + 2 * i))
        .take_while(|unit| *unit != 0)
        .collect();
    assert_eq!(String::from_utf16(&name).unwrap(), r"D:\temp\pre0002.TMP");
    assert_eq!(
        std::fs::metadata(root.join("Temp/pre0002.TMP"))
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        std::fs::read(root.join("Temp/PRE0001.tmp")).unwrap(),
        b"original"
    );

    put_path(&mut guest, PATH, r"E:\Temp");
    assert_eq!(
        guest.call("GetTempFileNameW", &[PATH, prefix, 0, BUFFER]),
        0
    );
    assert_eq!(guest.error(), 5);
    assert_eq!(
        guest.call("GetTempFileNameW", &[PATH, prefix, 3, BUFFER]),
        3
    );
    assert!(!root.join("Temp/pre0003.TMP").exists());
}

#[test]
fn host_mount_non_ascii_spelling_and_native_create_collisions() {
    let tree = Tree::new("non-ascii-case");
    let root = tree.path("root");
    std::fs::create_dir_all(root.join("Över")).unwrap();
    std::fs::write(root.join("État.txt"), b"original").unwrap();
    let file_collision = root.join("état.txt").exists();
    let directory_collision = root.join("över").exists();
    let mount = HostMount::new('D', &root, true).unwrap();
    let mut guest = guest_with(vec![mount]);

    let file = open(&mut guest, r"D:\ÉTAT.TXT", GENERIC_READ, 0, OPEN_EXISTING);
    assert_ne!(file, INVALID);
    assert_eq!(read(&mut guest, file, 8).unwrap(), b"original");
    assert_eq!(guest.call("CloseHandle", &[file]), 1);
    assert_eq!(
        open(&mut guest, r"D:\état.txt", GENERIC_READ, 0, OPEN_EXISTING),
        INVALID
    );
    assert_eq!(guest.error(), 2);

    let file = open(&mut guest, r"D:\état.txt", GENERIC_WRITE, 0, CREATE_NEW);
    if file_collision {
        assert_eq!(file, INVALID);
        assert_eq!(guest.error(), 80);
    } else {
        assert_ne!(file, INVALID);
        assert_eq!(guest.call("CloseHandle", &[file]), 1);
    }
    assert_eq!(std::fs::read(root.join("État.txt")).unwrap(), b"original");

    let created = path_call(&mut guest, "CreateDirectoryW", r"D:\över");
    if directory_collision {
        assert_eq!(created, 0);
        assert_eq!(guest.error(), 183);
    } else {
        assert_eq!(created, 1);
        assert!(root.join("över").is_dir());
    }
    assert!(root.join("Över").is_dir());
}

#[test]
fn read_only_mounts_serve_files_without_escaping_the_root() {
    let tree = Tree::new("read-only");
    std::fs::create_dir_all(tree.path("root/Data")).unwrap();
    std::fs::write(tree.path("root/Data/Level1.bin"), b"XENORA-LEVEL").unwrap();
    std::fs::write(tree.path("outside.txt"), b"secret").unwrap();
    symlink(&tree.path("outside.txt"), &tree.path("root/Data/escape"));
    symlink(&tree.0, &tree.path("root/link"));
    let mount = HostMount::new('D', &tree.path("root"), false).unwrap();
    let mut guest = guest_with(vec![mount]);

    guest
        .memory
        .write_bytes(PATH, b"D:\\data\\LEVEL1.BIN\0")
        .unwrap();
    assert_eq!(guest.call("GetFileAttributesA", &[PATH]), 1);
    guest
        .memory
        .write_bytes(PATH, b"D:\\data\\missing.bin\0")
        .unwrap();
    assert_eq!(
        guest.call("GetFileAttributesA", &[PATH]),
        u64::from(u32::MAX)
    );
    assert_eq!(guest.error(), 2);
    guest.memory.write_bytes(PATH, b"D:\\data\\\xff\0").unwrap();
    assert_eq!(
        guest.call("GetFileAttributesA", &[PATH]),
        u64::from(u32::MAX)
    );
    assert_eq!(guest.error(), 123);

    // Names match case-insensitively against the host listing.
    let file = open(
        &mut guest,
        "D:\\data\\LEVEL1.BIN",
        GENERIC_READ,
        1,
        OPEN_EXISTING,
    );
    assert_ne!(file, INVALID);
    assert_eq!(read(&mut guest, file, 6).unwrap(), b"XENORA");
    guest.memory.write_u64(COUNT, 0).unwrap();
    assert_eq!(guest.call("GetFileSizeEx", &[file, COUNT]), 1);
    assert_eq!(guest.u64_at(COUNT), 12);
    assert_eq!(
        guest.call("SetFilePointerEx", &[file, (-2i64) as u64, 0, 2]),
        1
    );
    assert_eq!(read(&mut guest, file, 16).unwrap(), b"EL");
    // Reading at the end of the file succeeds with zero bytes.
    assert_eq!(read(&mut guest, file, 16).unwrap(), b"");
    assert!(!write(&mut guest, file, b"x"));
    assert_eq!(guest.error(), 5);

    // Sharing modes apply to host files: the open handle allows only readers.
    let reader = open(
        &mut guest,
        "d:/Data/level1.bin",
        GENERIC_READ,
        1,
        OPEN_EXISTING,
    );
    assert_ne!(reader, INVALID);
    assert_eq!(guest.call("CloseHandle", &[reader]), 1);
    let exclusive = open(
        &mut guest,
        "D:\\Data\\Level1.bin",
        GENERIC_READ,
        0,
        OPEN_EXISTING,
    );
    assert_eq!(exclusive, INVALID);
    assert_eq!(guest.error(), 32); // ERROR_SHARING_VIOLATION
    assert_eq!(guest.call("CloseHandle", &[file]), 1);

    // The mount refuses changes.
    assert_eq!(
        open(
            &mut guest,
            "D:\\Data\\Level1.bin",
            GENERIC_WRITE,
            0,
            OPEN_EXISTING
        ),
        INVALID
    );
    assert_eq!(guest.error(), 5);
    assert_eq!(
        open(&mut guest, "D:\\new.txt", GENERIC_READ, 0, OPEN_ALWAYS),
        INVALID
    );
    assert_eq!(guest.error(), 5);
    assert_eq!(path_call(&mut guest, "CreateDirectoryW", "D:\\Saves"), 0);
    assert_eq!(guest.error(), 5);

    // Nothing outside the root is reachable: traversal is an invalid name,
    // symbolic links are invisible, and drive-relative paths are refused.
    for (path, error) in [
        ("D:\\..\\outside.txt", 123),
        ("D:\\Data\\..\\..\\outside.txt", 123),
        ("D:\\Data\\escape", 2),
        ("D:\\link\\outside.txt", 3),
        ("D:outside.txt", 3),
    ] {
        assert_eq!(
            open(&mut guest, path, GENERIC_READ, 1, OPEN_EXISTING),
            INVALID,
            "{path}"
        );
        assert_eq!(guest.error(), error, "{path}");
    }

    assert_eq!(attributes(&mut guest, "D:\\"), 0x10);
    assert_eq!(attributes(&mut guest, "D:\\DATA"), 0x10);
    assert_eq!(attributes(&mut guest, "D:\\Data\\Level1.bin"), 0x01); // read-only
    assert_eq!(
        attributes(&mut guest, "D:\\Data\\missing"),
        u64::from(u32::MAX)
    );
    assert_eq!(guest.error(), 2);

    // Enumeration reports the host names and hides the symbolic link.
    put_path(&mut guest, PATH, "D:\\data\\*");
    let search = guest.call("FindFirstFileW", &[PATH, FIND_DATA]);
    assert_ne!(search, INVALID);
    assert_eq!(found_name(&guest), "Level1.bin");
    assert_eq!(guest.u32_at(FIND_DATA), 0x01);
    assert_eq!(guest.u32_at(FIND_DATA + 32), 12); // nFileSizeLow
    assert_eq!(guest.call("FindNextFileW", &[search, FIND_DATA]), 0);
    assert_eq!(guest.error(), 18); // ERROR_NO_MORE_FILES
    assert_eq!(guest.call("FindClose", &[search]), 1);
    put_path(&mut guest, PATH, "D:\\*.*");
    let search = guest.call("FindFirstFileW", &[PATH, FIND_DATA]);
    assert_eq!(found_name(&guest), "Data");
    assert_eq!(guest.call("FindNextFileW", &[search, FIND_DATA]), 0);

    assert_eq!(std::fs::read(tree.path("outside.txt")).unwrap(), b"secret");
}

#[test]
fn writable_mounts_create_change_and_remove_only_inside_the_root() {
    let tree = Tree::new("writable");
    std::fs::create_dir_all(tree.path("root")).unwrap();
    std::fs::write(tree.path("outside.txt"), b"secret").unwrap();
    symlink(&tree.path("outside.txt"), &tree.path("root/escape"));
    let mount = HostMount::new('f', &tree.path("root"), true).unwrap();
    let mut guest = guest_with(vec![mount]);
    let saves = tree.path("root/Saves");

    assert_eq!(
        open(
            &mut guest,
            "F:\\Saves\\Slot.dat",
            GENERIC_READ | GENERIC_WRITE,
            0,
            CREATE_NEW
        ),
        INVALID
    );
    assert_eq!(guest.error(), 3);
    assert_eq!(path_call(&mut guest, "CreateDirectoryW", "F:\\Saves"), 1);
    assert_eq!(path_call(&mut guest, "CreateDirectoryW", "F:\\saves"), 0);
    assert_eq!(guest.error(), 183);
    assert!(saves.is_dir());

    let file = open(
        &mut guest,
        "F:\\saves\\Slot.dat",
        GENERIC_READ | GENERIC_WRITE,
        0,
        CREATE_NEW,
    );
    assert_ne!(file, INVALID);
    assert!(write(&mut guest, file, b"hello"));
    // The guest's spelling of a new name is kept on the host.
    assert_eq!(std::fs::read(saves.join("Slot.dat")).unwrap(), b"hello");
    assert_eq!(
        path_call(&mut guest, "DeleteFileW", "F:\\Saves\\slot.dat"),
        0
    );
    assert_eq!(guest.error(), 32);
    assert_eq!(guest.call("SetFilePointerEx", &[file, 2, 0, 0]), 1);
    assert_eq!(guest.call("SetEndOfFile", &[file]), 1);
    assert_eq!(std::fs::read(saves.join("Slot.dat")).unwrap(), b"he");
    assert_eq!(guest.call("CloseHandle", &[file]), 1);

    assert_eq!(
        open(
            &mut guest,
            "F:\\Saves\\slot.dat",
            GENERIC_READ,
            0,
            CREATE_NEW
        ),
        INVALID
    );
    assert_eq!(guest.error(), 80); // ERROR_FILE_EXISTS
    let reopened = open(
        &mut guest,
        "F:\\Saves\\SLOT.DAT",
        GENERIC_READ,
        0,
        OPEN_ALWAYS,
    );
    assert_ne!(reopened, INVALID);
    assert_eq!(guest.error(), 183); // existing file opened
    assert_eq!(read(&mut guest, reopened, 8).unwrap(), b"he");
    assert_eq!(guest.call("CloseHandle", &[reopened]), 1);

    // CREATE_ALWAYS truncates an existing host file and reports ALREADY_EXISTS.
    let truncated = open(
        &mut guest,
        "F:\\Saves\\SLOT.DAT",
        GENERIC_READ | GENERIC_WRITE,
        0,
        CREATE_ALWAYS,
    );
    assert_ne!(truncated, INVALID);
    assert_eq!(guest.error(), 183); // ERROR_ALREADY_EXISTS
    assert_eq!(std::fs::metadata(saves.join("Slot.dat")).unwrap().len(), 0);
    assert!(write(&mut guest, truncated, b"new"));
    assert_eq!(std::fs::read(saves.join("Slot.dat")).unwrap(), b"new");
    assert_eq!(guest.call("CloseHandle", &[truncated]), 1);

    assert_eq!(path_call(&mut guest, "RemoveDirectoryW", "F:\\Saves"), 0);
    assert_eq!(guest.error(), 145); // ERROR_DIR_NOT_EMPTY
    assert_eq!(
        path_call(&mut guest, "DeleteFileW", "F:\\Saves\\slot.dat"),
        1
    );
    assert_eq!(path_call(&mut guest, "RemoveDirectoryW", "F:\\Saves"), 1);
    assert!(!saves.exists());

    // A symbolic link cannot be written through, replaced or moved.
    assert_eq!(
        open(&mut guest, "F:\\escape", GENERIC_WRITE, 0, OPEN_ALWAYS),
        INVALID
    );
    assert_eq!(guest.error(), 80);
    assert_eq!(path_call(&mut guest, "DeleteFileW", "F:\\escape"), 0);
    assert_eq!(guest.error(), 2);
    put_path(&mut guest, PATH, "F:\\escape");
    put_path(&mut guest, OTHER_PATH, "F:\\moved");
    assert_eq!(guest.call("MoveFileW", &[PATH, OTHER_PATH]), 0);
    assert_eq!(guest.error(), 2);

    assert_eq!(std::fs::read(tree.path("outside.txt")).unwrap(), b"secret");
    assert!(tree.path("root/escape").is_symlink());
}

#[test]
fn mounts_are_explicit_and_unique_per_drive() {
    let tree = Tree::new("mounts");
    for drive in ['A', 'b', 'C', 'c', '1', ':', 'é'] {
        assert!(HostMount::new(drive, &tree.0, false).is_err(), "{drive}");
    }
    for drive in ['D', 'd', 'Z', 'z'] {
        assert!(HostMount::new(drive, &tree.0, false).is_ok(), "{drive}");
    }
    assert!(HostMount::new('D', &tree.path("missing"), false).is_err());
    let mut state = ServicesState::new(Arc::new(MockClock::new(0)));
    assert!(
        state
            .mount(HostMount::new('D', &tree.0, false).unwrap())
            .is_ok()
    );
    assert!(
        state
            .mount(HostMount::new('d', &tree.0, true).unwrap())
            .is_err()
    );

    // Without a mount, other drive letters stay unreachable.
    let mut guest = guest_with(Vec::new());
    assert_eq!(
        open(&mut guest, "D:\\anything", GENERIC_READ, 1, OPEN_EXISTING),
        INVALID
    );
    assert_ne!(guest.error(), 0);
}

#[test]
fn hard_links_are_hidden_so_they_cannot_alias_outside_files() {
    let tree = Tree::new("hard-links");
    std::fs::create_dir_all(tree.path("root")).unwrap();
    std::fs::write(tree.path("outside.txt"), b"secret").unwrap();
    std::fs::hard_link(tree.path("outside.txt"), tree.path("root/alias.txt")).unwrap();
    std::fs::write(tree.path("root/plain.txt"), b"plain").unwrap();
    let mount = HostMount::new('D', &tree.path("root"), true).unwrap();
    let mut guest = guest_with(vec![mount]);

    assert_eq!(
        open(&mut guest, r"D:\alias.txt", GENERIC_READ, 1, OPEN_EXISTING),
        INVALID
    );
    assert_eq!(guest.error(), 2);
    // A writable mount cannot replace or delete the alias either.
    assert_eq!(
        open(&mut guest, r"D:\alias.txt", GENERIC_WRITE, 0, OPEN_ALWAYS),
        INVALID
    );
    assert_eq!(guest.error(), 80);
    assert_eq!(path_call(&mut guest, "DeleteFileW", r"D:\alias.txt"), 0);
    assert_eq!(guest.error(), 2);
    put_path(&mut guest, PATH, r"D:\*");
    let search = guest.call("FindFirstFileW", &[PATH, FIND_DATA]);
    assert_eq!(found_name(&guest), "plain.txt");
    assert_eq!(guest.call("FindNextFileW", &[search, FIND_DATA]), 0);
    assert_eq!(std::fs::read(tree.path("outside.txt")).unwrap(), b"secret");
}

#[test]
fn sharing_follows_the_host_file_rather_than_its_name() {
    let tree = Tree::new("identity");
    std::fs::create_dir_all(tree.path("root")).unwrap();
    std::fs::write(tree.path("root/a.txt"), b"first").unwrap();
    let mount = HostMount::new('F', &tree.path("root"), true).unwrap();
    let mut guest = guest_with(vec![mount]);

    let file = open(
        &mut guest,
        r"F:\a.txt",
        GENERIC_READ | GENERIC_WRITE,
        0,
        OPEN_EXISTING,
    );
    assert_ne!(file, INVALID);
    // The host renames the file while the guest holds it exclusively.
    std::fs::rename(tree.path("root/a.txt"), tree.path("root/b.txt")).unwrap();
    assert_eq!(
        open(&mut guest, r"F:\b.txt", GENERIC_READ, 3, OPEN_EXISTING),
        INVALID
    );
    assert_eq!(guest.error(), 32);
    assert_eq!(path_call(&mut guest, "DeleteFileW", r"F:\b.txt"), 0);
    assert_eq!(guest.error(), 32);
    // The handle still refers to the same file under its new name.
    assert!(write(&mut guest, file, b"SECOND"));
    assert_eq!(std::fs::read(tree.path("root/b.txt")).unwrap(), b"SECOND");
    // A different file now using the old name is unrelated to the handle.
    std::fs::write(tree.path("root/a.txt"), b"other").unwrap();
    let other = open(&mut guest, r"F:\a.txt", GENERIC_READ, 0, OPEN_EXISTING);
    assert_ne!(other, INVALID);
    assert_eq!(guest.call("CloseHandle", &[other]), 1);
    assert_eq!(guest.call("CloseHandle", &[file]), 1);
    assert_eq!(path_call(&mut guest, "DeleteFileW", r"F:\b.txt"), 1);
}

#[test]
fn host_changes_between_guest_calls_cannot_redirect_paths() {
    let tree = Tree::new("concurrent");
    std::fs::create_dir_all(tree.path("root/Data")).unwrap();
    std::fs::create_dir_all(tree.path("elsewhere")).unwrap();
    std::fs::write(tree.path("elsewhere/level.bin"), b"secret").unwrap();
    std::fs::write(tree.path("root/Data/level.bin"), b"data").unwrap();
    let mount = HostMount::new('D', &tree.path("root"), false).unwrap();
    let mut guest = guest_with(vec![mount]);
    let file = open(
        &mut guest,
        r"D:\Data\level.bin",
        GENERIC_READ,
        1,
        OPEN_EXISTING,
    );
    assert_ne!(file, INVALID);

    // The directory is replaced by a link to a directory outside the root.
    std::fs::rename(tree.path("root/Data"), tree.path("root/Old")).unwrap();
    symlink(&tree.path("elsewhere"), &tree.path("root/Data"));
    assert_eq!(
        open(
            &mut guest,
            r"D:\Data\level.bin",
            GENERIC_READ,
            1,
            OPEN_EXISTING
        ),
        INVALID
    );
    assert_eq!(guest.error(), 3);
    put_path(&mut guest, PATH, r"D:\Data\*");
    assert_eq!(guest.call("FindFirstFileW", &[PATH, FIND_DATA]), INVALID);
    assert_eq!(guest.error(), 3);
    // The open handle keeps reading the file it opened.
    assert_eq!(read(&mut guest, file, 8).unwrap(), b"data");

    // A file replaced by a directory is refused as a file.
    std::fs::create_dir_all(tree.path("root/Old/level2.bin")).unwrap();
    assert_eq!(
        open(
            &mut guest,
            r"D:\Old\level2.bin",
            GENERIC_READ,
            1,
            OPEN_EXISTING
        ),
        INVALID
    );
    assert_eq!(guest.error(), 5);
    assert_eq!(
        std::fs::read(tree.path("elsewhere/level.bin")).unwrap(),
        b"secret"
    );
}

#[test]
fn read_only_mounts_refuse_every_change() {
    let tree = Tree::new("refuse");
    std::fs::create_dir_all(tree.path("root/Dir")).unwrap();
    std::fs::write(tree.path("root/file.bin"), b"keep").unwrap();
    let mount = HostMount::new('D', &tree.path("root"), false).unwrap();
    let mut guest = guest_with(vec![mount]);
    let reader = open(&mut guest, r"D:\file.bin", GENERIC_READ, 3, OPEN_EXISTING);
    assert_ne!(reader, INVALID);
    assert_eq!(guest.call("SetEndOfFile", &[reader]), 0);
    assert_eq!(guest.error(), 5);
    assert!(!write(&mut guest, reader, b"x"));
    for disposition in [1, 2, 4, 5] {
        assert_eq!(
            open(
                &mut guest,
                r"D:\file.bin",
                GENERIC_READ | GENERIC_WRITE,
                3,
                disposition
            ),
            INVALID
        );
        assert_eq!(
            guest.error(),
            if disposition == 1 { 80 } else { 5 },
            "{disposition}"
        );
    }
    for (symbol, path) in [
        ("DeleteFileW", r"D:\file.bin"),
        ("RemoveDirectoryW", r"D:\Dir"),
        ("CreateDirectoryW", r"D:\New"),
    ] {
        assert_eq!(path_call(&mut guest, symbol, path), 0, "{symbol}");
        assert_eq!(guest.error(), 5, "{symbol}");
    }
    assert_eq!(std::fs::read(tree.path("root/file.bin")).unwrap(), b"keep");
    assert!(tree.path("root/Dir").is_dir());
    assert!(!tree.path("root/New").exists());
}

#[test]
fn enumeration_errors_and_limits_are_reported() {
    use std::os::unix::fs::PermissionsExt;
    let tree = Tree::new("limits");
    std::fs::create_dir_all(tree.path("root/Big")).unwrap();
    std::fs::create_dir_all(tree.path("root/Locked")).unwrap();
    for index in 0..=4096 {
        std::fs::write(tree.path(&format!("root/Big/{index}.bin")), b"").unwrap();
    }
    std::fs::set_permissions(
        tree.path("root/Locked"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let mount = HostMount::new('D', &tree.path("root"), false).unwrap();
    let mut guest = guest_with(vec![mount]);

    // 4,097 entries exceed the 4,096-entry listing bound.
    put_path(&mut guest, PATH, r"D:\Big\*");
    assert_eq!(guest.call("FindFirstFileW", &[PATH, FIND_DATA]), INVALID);
    assert_eq!(guest.error(), 8); // ERROR_NOT_ENOUGH_MEMORY
    assert_eq!(
        open(&mut guest, r"D:\Big\1.bin", GENERIC_READ, 1, OPEN_EXISTING),
        INVALID
    );
    assert_eq!(guest.error(), 8);
    // A directory the host cannot read reports access denied, not emptiness.
    put_path(&mut guest, PATH, r"D:\Locked\*");
    assert_eq!(guest.call("FindFirstFileW", &[PATH, FIND_DATA]), INVALID);
    assert_eq!(guest.error(), 5);
    std::fs::set_permissions(
        tree.path("root/Locked"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
}

#[test]
fn writable_mounts_bound_how_large_guest_writes_make_files() {
    let tree = Tree::new("quota");
    std::fs::create_dir_all(tree.path("root")).unwrap();
    let mount = HostMount::new('F', &tree.path("root"), true)
        .unwrap()
        .with_max_file_size(8);
    let mut guest = guest_with(vec![mount]);
    let file = open(
        &mut guest,
        r"F:\save.bin",
        GENERIC_READ | GENERIC_WRITE,
        0,
        CREATE_NEW,
    );
    assert_ne!(file, INVALID);
    assert!(write(&mut guest, file, b"12345"));
    assert!(!write(&mut guest, file, b"6789"));
    assert_eq!(guest.error(), 112); // ERROR_DISK_FULL
    assert_eq!(std::fs::read(tree.path("root/save.bin")).unwrap(), b"12345");
    // Seeking far ahead and extending is bounded the same way.
    assert_eq!(guest.call("SetFilePointerEx", &[file, 1 << 40, 0, 0]), 1);
    assert_eq!(guest.call("SetEndOfFile", &[file]), 0);
    assert_eq!(guest.error(), 112);
    assert_eq!(guest.call("SetFilePointerEx", &[file, 8, 0, 0]), 1);
    assert_eq!(guest.call("SetEndOfFile", &[file]), 1);
    assert_eq!(
        std::fs::metadata(tree.path("root/save.bin")).unwrap().len(),
        8
    );
    assert_eq!(guest.call("CloseHandle", &[file]), 1);
}

#[test]
fn crt_fopen_writes_through_a_writable_host_mount() {
    let tree = Tree::new("crt-fopen");
    std::fs::create_dir_all(tree.path("root")).unwrap();
    let mount = HostMount::new('D', &tree.path("root"), true).unwrap();
    let mut guest = guest_with(vec![mount]);
    let path = BUFFER;
    let mode_w = BUFFER + 0x40;
    let mode_r = BUFFER + 0x50;
    let payload = BUFFER + 0x60;
    let out = BUFFER + 0x80;
    guest.memory.write_bytes(path, b"D:\\crt.bin\0").unwrap();
    guest.memory.write_bytes(mode_w, b"wb\0").unwrap();
    guest.memory.write_bytes(mode_r, b"rb\0").unwrap();
    guest.memory.write_bytes(payload, b"stdio\0").unwrap();
    let file = guest.call("fopen", &[path, mode_w]);
    assert_ne!(file, 0);
    assert_eq!(guest.call("fwrite", &[payload, 1, 5, file]), 5);
    assert_eq!(guest.call("fclose", &[file]), 0);
    assert_eq!(std::fs::read(tree.path("root/crt.bin")).unwrap(), b"stdio");
    let reader = guest.call("fopen", &[path, mode_r]);
    assert_ne!(reader, 0);
    assert_eq!(guest.call("fread", &[out, 1, 5, reader]), 5);
    let mut got = [0u8; 5];
    guest.memory.read_bytes(out, &mut got).unwrap();
    assert_eq!(&got, b"stdio");
    assert_eq!(guest.call("fclose", &[reader]), 0);

    // A writable stream reaches the host when it is closed, not before.
    let file = guest.call("fopen", &[path, mode_w]);
    assert_eq!(guest.call("fwrite", &[payload, 1, 3, file]), 3);
    assert_eq!(std::fs::read(tree.path("root/crt.bin")).unwrap(), b"");
    assert_eq!(guest.call("fclose", &[file]), 0);
    assert_eq!(std::fs::read(tree.path("root/crt.bin")).unwrap(), b"std");
}

/// A mount quota bounds the net bytes and entries guest operations add,
/// across Win32 file calls, GetTempFileNameW and CRT streams. Exceeding it
/// fails like a full volume: ERROR_DISK_FULL (112) and no host change.
#[test]
fn mount_quota_bounds_net_bytes_and_entries_added_by_the_guest() {
    let tree = Tree::new("mount-quota");
    let root = tree.path("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("old.bin"), b"666666").unwrap();
    let mount = HostMount::new('G', &root, true)
        .unwrap()
        .with_quota(Some(10), Some(2));
    let mut guest = guest_with(vec![mount]);
    let rw = GENERIC_READ | GENERIC_WRITE;

    // First entry: a new file that takes six of the ten bytes.
    let a = open(&mut guest, r"G:\a.bin", rw, 0, CREATE_NEW);
    assert_ne!(a, INVALID);
    assert!(write(&mut guest, a, b"aaaaaa"));
    assert!(!write(&mut guest, a, b"bbbbb"));
    assert_eq!(guest.error(), 112);
    assert_eq!(std::fs::read(root.join("a.bin")).unwrap(), b"aaaaaa");
    // Overwriting existing bytes adds nothing.
    assert_eq!(guest.call("SetFilePointerEx", &[a, 0, 0, 0]), 1);
    assert!(write(&mut guest, a, b"cccccc"));
    // Extending with SetEndOfFile is charged the same way.
    assert_eq!(guest.call("SetFilePointerEx", &[a, 11, 0, 0]), 1);
    assert_eq!(guest.call("SetEndOfFile", &[a]), 0);
    assert_eq!(guest.error(), 112);
    assert_eq!(guest.call("SetFilePointerEx", &[a, 10, 0, 0]), 1);
    assert_eq!(guest.call("SetEndOfFile", &[a]), 1);
    assert_eq!(std::fs::metadata(root.join("a.bin")).unwrap().len(), 10);
    assert_eq!(guest.call("CloseHandle", &[a]), 1);

    // Second entry: a directory. Then every entry is used.
    put_path(&mut guest, PATH, r"G:\dir");
    assert_eq!(guest.call("CreateDirectoryW", &[PATH, 0]), 1);
    assert_eq!(open(&mut guest, r"G:\b.bin", rw, 0, CREATE_NEW), INVALID);
    assert_eq!(guest.error(), 112);
    assert!(!root.join("b.bin").exists());
    put_path(&mut guest, PATH, r"G:\dir2");
    assert_eq!(guest.call("CreateDirectoryW", &[PATH, 0]), 0);
    assert_eq!(guest.error(), 112);
    assert!(!root.join("dir2").exists());
    let prefix = OTHER_PATH;
    put_path(&mut guest, prefix, "tmp");
    put_path(&mut guest, PATH, r"G:\dir");
    assert_eq!(
        guest.call("GetTempFileNameW", &[PATH, prefix, 0, BUFFER]),
        0
    );
    assert_eq!(guest.error(), 112);
    assert_eq!(std::fs::read_dir(root.join("dir")).unwrap().count(), 0);
    // Clear of COUNT and FIND_DATA, which later calls overwrite.
    let path = OTHER_PATH + 0x100;
    let mode = OTHER_PATH + 0x140;
    guest.memory.write_bytes(path, b"G:\\crt.bin\0").unwrap();
    guest.memory.write_bytes(mode, b"wb\0").unwrap();
    assert_eq!(guest.call("fopen", &[path, mode]), 0);
    assert!(!root.join("crt.bin").exists());
    // Opening an existing file needs no new entry.
    let old = open(&mut guest, r"G:\old.bin", rw, 0, OPEN_EXISTING);
    assert_ne!(old, INVALID);

    // Deleting a.bin returns its entry and its ten bytes.
    put_path(&mut guest, PATH, r"G:\a.bin");
    assert_eq!(guest.call("DeleteFileW", &[PATH]), 1);
    let b = open(&mut guest, r"G:\b.bin", rw, 0, CREATE_NEW);
    assert_ne!(b, INVALID);
    assert!(write(&mut guest, b, b"0123456789"));
    assert!(!write(&mut guest, b, b"x"));
    assert_eq!(guest.error(), 112);

    // Shrinking a file that predates the mount frees its bytes for others.
    assert_eq!(guest.call("SetEndOfFile", &[old]), 1);
    assert_eq!(std::fs::metadata(root.join("old.bin")).unwrap().len(), 0);
    assert!(write(&mut guest, b, b"abcdef"));
    assert!(!write(&mut guest, b, b"x"));
    assert_eq!(guest.error(), 112);
    assert_eq!(std::fs::metadata(root.join("b.bin")).unwrap().len(), 16);
    assert_eq!(guest.call("CloseHandle", &[b]), 1);
    assert_eq!(guest.call("CloseHandle", &[old]), 1);

    // Removing the directory frees an entry, so a CRT stream may create a
    // file, but writing past the byte budget fails at fclose.
    put_path(&mut guest, PATH, r"G:\dir");
    assert_eq!(guest.call("RemoveDirectoryW", &[PATH]), 1);
    let payload = OTHER_PATH + 0x180;
    guest.memory.write_bytes(payload, b"z").unwrap();
    let stream = guest.call("fopen", &[path, mode]);
    assert_ne!(stream, 0);
    assert_eq!(guest.call("fwrite", &[payload, 1, 1, stream]), 1);
    assert_eq!(guest.call("fclose", &[stream]) as u32, u32::MAX);
    assert_eq!(std::fs::metadata(root.join("crt.bin")).unwrap().len(), 0);
}

/// MoveFileW renames files and directories within one writable mount,
/// keeping contents and refusing to replace an existing name.
/// Expected codes follow the MoveFileW and System Error Codes pages:
/// ERROR_ALREADY_EXISTS (183), ERROR_FILE_NOT_FOUND (2),
/// ERROR_PATH_NOT_FOUND (3), ERROR_SHARING_VIOLATION (32) and
/// ERROR_ACCESS_DENIED (5). Moves between drives are not supported (50).
#[test]
fn move_file_renames_within_one_writable_mount() {
    let tree = Tree::new("host-move");
    let root = tree.path("root");
    std::fs::create_dir_all(root.join("Saves/Old")).unwrap();
    std::fs::write(root.join("Saves/slot1.dat"), b"one").unwrap();
    std::fs::write(root.join("Saves/slot2.dat"), b"two").unwrap();
    std::fs::write(root.join("Saves/Old/keep.dat"), b"keep").unwrap();
    std::fs::write(tree.path("outside.txt"), b"secret").unwrap();
    let mut guest = guest_with(vec![
        HostMount::new('D', &root, true).unwrap(),
        HostMount::new('E', &root, false).unwrap(),
        HostMount::new('F', &tree.path("root/Saves/Old"), true).unwrap(),
    ]);
    let moved = |guest: &mut Guest, from: &str, to: &str| {
        put_path(guest, PATH, from);
        put_path(guest, OTHER_PATH, to);
        if guest.call("MoveFileW", &[PATH, OTHER_PATH]) == 1 {
            0
        } else {
            guest.error()
        }
    };

    // A file moves into another directory under a new name, any case.
    assert_eq!(
        moved(
            &mut guest,
            r"D:\SAVES\SLOT1.DAT",
            r"d:\saves\old\Slot-A.dat"
        ),
        0
    );
    assert!(!root.join("Saves/slot1.dat").exists());
    assert_eq!(
        std::fs::read(root.join("Saves/Old/Slot-A.dat")).unwrap(),
        b"one"
    );
    // An existing target is never replaced, whatever its case.
    assert_eq!(
        moved(
            &mut guest,
            r"D:\Saves\slot2.dat",
            r"D:\Saves\Old\SLOT-a.DAT"
        ),
        183
    );
    assert_eq!(std::fs::read(root.join("Saves/slot2.dat")).unwrap(), b"two");
    assert_eq!(
        moved(&mut guest, r"D:\Saves\slot2.dat", r"D:\Saves\Old"),
        183
    );
    // A change of case alone renames the entry in place.
    assert_eq!(
        moved(&mut guest, r"D:\Saves\slot2.dat", r"D:\Saves\SLOT2.DAT"),
        0
    );
    let names: Vec<_> = std::fs::read_dir(root.join("Saves"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(names.contains(&"SLOT2.DAT".to_owned()), "{names:?}");
    // Directories move with their contents.
    assert_eq!(moved(&mut guest, r"D:\Saves\Old", r"D:\Archive"), 0);
    assert_eq!(
        std::fs::read(root.join("Archive/keep.dat")).unwrap(),
        b"keep"
    );
    assert_eq!(
        std::fs::read(root.join("Archive/Slot-A.dat")).unwrap(),
        b"one"
    );

    // Missing sources and target directories.
    assert_eq!(moved(&mut guest, r"D:\nothing.dat", r"D:\x.dat"), 2);
    assert_eq!(
        moved(&mut guest, r"D:\Saves\SLOT2.DAT", r"D:\Missing\x.dat"),
        3
    );
    // Neither `..` nor a host path can name somewhere outside the root.
    assert_eq!(
        moved(&mut guest, r"D:\Saves\SLOT2.DAT", r"D:\..\stolen.dat"),
        123
    );
    assert!(!tree.path("stolen.dat").exists());
    // The mount root, read-only mounts and other drives.
    assert_eq!(moved(&mut guest, r"D:\", r"D:\Root2"), 5);
    assert_eq!(moved(&mut guest, r"E:\Saves\SLOT2.DAT", r"E:\x.dat"), 5);
    assert_eq!(moved(&mut guest, r"D:\Saves\SLOT2.DAT", r"F:\x.dat"), 50);
    assert_eq!(moved(&mut guest, r"D:\Saves\SLOT2.DAT", r"C:\x.dat"), 50);
    assert!(root.join("Saves/SLOT2.DAT").exists());

    // An open file cannot be moved until its handle is closed.
    let handle = open(
        &mut guest,
        r"D:\Saves\SLOT2.DAT",
        GENERIC_READ,
        3,
        OPEN_EXISTING,
    );
    assert_ne!(handle, INVALID);
    assert_eq!(moved(&mut guest, r"D:\Saves\SLOT2.DAT", r"D:\x.dat"), 32);
    assert_eq!(guest.call("CloseHandle", &[handle]), 1);
    assert_eq!(moved(&mut guest, r"D:\Saves\SLOT2.DAT", r"D:\x.dat"), 0);
    assert_eq!(std::fs::read(root.join("x.dat")).unwrap(), b"two");
    assert_eq!(std::fs::read(tree.path("outside.txt")).unwrap(), b"secret");
}

/// MoveFileExW flags from the public MoveFileExW page:
/// MOVEFILE_REPLACE_EXISTING (1) replaces a closed file but never a
/// directory, MOVEFILE_COPY_ALLOWED (2) selects the error for moves between
/// drives (ERROR_NOT_SAME_DEVICE, 17, without it) and MOVEFILE_WRITE_THROUGH
/// (8) is accepted. Other flags are not supported (50). The save-file
/// pattern - write a temporary file, then replace the old one - is checked on
/// a host mount and on the synthetic C: drive.
#[test]
fn move_file_ex_replaces_closed_files_on_both_drives() {
    const REPLACE: u64 = 1;
    const COPY_ALLOWED: u64 = 2;
    const WRITE_THROUGH: u64 = 8;
    let tree = Tree::new("host-move-ex");
    let root = tree.path("root");
    std::fs::create_dir_all(root.join("Dir")).unwrap();
    std::fs::write(root.join("save.dat"), b"old save").unwrap();
    std::fs::write(root.join("other.dat"), b"other").unwrap();
    let mut guest = guest_with(vec![
        HostMount::new('D', &root, true).unwrap(),
        HostMount::new('F', &tree.path("root/Dir"), true).unwrap(),
    ]);
    let moved = |guest: &mut Guest, from: &str, to: &str, flags: u64| {
        put_path(guest, PATH, from);
        put_path(guest, OTHER_PATH, to);
        if guest.call("MoveFileExW", &[PATH, OTHER_PATH, flags]) == 1 {
            0
        } else {
            guest.error()
        }
    };
    let rw = GENERIC_READ | GENERIC_WRITE;
    for drive in ["D", "C"] {
        let tmp = format!(r"{drive}:\save.tmp");
        let save = format!(r"{drive}:\save.dat");
        if drive == "C" {
            let old = open(&mut guest, &save, rw, 0, CREATE_NEW);
            assert!(write(&mut guest, old, b"old save"));
            assert_eq!(guest.call("CloseHandle", &[old]), 1);
        }
        let new = open(&mut guest, &tmp, rw, 0, CREATE_NEW);
        assert_ne!(new, INVALID, "{drive}");
        assert!(write(&mut guest, new, b"new save"));
        // Neither the open source nor an open target can be replaced.
        assert_eq!(moved(&mut guest, &tmp, &save, REPLACE), 32, "{drive}");
        assert_eq!(guest.call("CloseHandle", &[new]), 1);
        let reader = open(&mut guest, &save, GENERIC_READ, 3, OPEN_EXISTING);
        assert_eq!(moved(&mut guest, &tmp, &save, REPLACE), 32, "{drive}");
        assert_eq!(guest.call("CloseHandle", &[reader]), 1);
        // Without the flag the target stays; with it the target is replaced.
        assert_eq!(moved(&mut guest, &tmp, &save, 0), 183, "{drive}");
        assert_eq!(moved(&mut guest, &tmp, &save, REPLACE | WRITE_THROUGH), 0);
        let file = open(&mut guest, &save, GENERIC_READ, 0, OPEN_EXISTING);
        assert_eq!(read(&mut guest, file, 64).unwrap(), b"new save", "{drive}");
        assert_eq!(guest.call("CloseHandle", &[file]), 1);
        assert_eq!(
            open(&mut guest, &tmp, GENERIC_READ, 0, OPEN_EXISTING),
            INVALID
        );
        assert_eq!(guest.error(), 2, "{drive}");
        // Unsupported flags are refused before anything changes.
        assert_eq!(moved(&mut guest, &save, &tmp, 4), 50, "{drive}");
    }
    assert_eq!(std::fs::read(root.join("save.dat")).unwrap(), b"new save");
    assert!(!root.join("save.tmp").exists());

    // A directory is never replaced and never replaces a file.
    assert_eq!(moved(&mut guest, r"D:\other.dat", r"D:\Dir", REPLACE), 5);
    assert_eq!(moved(&mut guest, r"D:\Dir", r"D:\other.dat", REPLACE), 5);
    assert_eq!(moved(&mut guest, r"D:\Dir", r"D:\other.dat", 0), 183);
    assert_eq!(std::fs::read(root.join("other.dat")).unwrap(), b"other");
    // Between drives the error depends on MOVEFILE_COPY_ALLOWED.
    assert_eq!(moved(&mut guest, r"D:\other.dat", r"F:\x.dat", 0), 17);
    assert_eq!(moved(&mut guest, r"D:\other.dat", r"C:\x.dat", 0), 17);
    assert_eq!(
        moved(&mut guest, r"D:\other.dat", r"F:\x.dat", COPY_ALLOWED),
        50
    );
    assert!(root.join("other.dat").exists());
}
