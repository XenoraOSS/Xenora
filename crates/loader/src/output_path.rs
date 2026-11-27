// SPDX-License-Identifier: GPL-3.0-or-later

//! Preflight for derived files. Validation assumes a stable, trusted directory
//! tree. It does not protect against concurrent changes to parent directories.

use std::env;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const ALLOWED_CHECKOUT_DIRS: &[&str] = &["artifacts", "diagnostics/probe"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    Export,
    PersistentCache,
}

/// Default macOS cache root. Callers provide the current user's absolute home
/// directory and must still validate the result before creating any files.
pub fn macos_cache_directory(home: &Path) -> io::Result<PathBuf> {
    if !home.is_absolute() {
        return Err(refused(home, "expected an absolute home directory"));
    }
    Ok(home.join("Library/Caches/local.xenora.Xenora"))
}

fn refused(path: &Path, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("output path '{}' refused: {reason}", path.display()),
    )
}

/// Resolve an existing prefix component by component, retaining a missing
/// suffix. An existing symlink is followed only if its target resolves. Do not
/// normalize a parent traversal through a path that does not exist.
fn resolve_directory(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(refused(path, "empty output directory"));
    }
    let mut resolved = if path.is_absolute() {
        PathBuf::new()
    } else {
        env::current_dir()?.canonicalize()?
    };
    let mut missing = false;
    for component in path.components() {
        match component {
            Component::CurDir | Component::ParentDir if missing => {
                return Err(refused(
                    path,
                    "parent traversal through a missing directory",
                ));
            }
            Component::Prefix(_) | Component::RootDir => resolved.push(component),
            Component::CurDir | Component::ParentDir | Component::Normal(_) => {
                resolved.push(component);
                if missing {
                    continue;
                }
                match fs::symlink_metadata(&resolved) {
                    Ok(_) => {
                        // Canonicalization failure here includes broken or looping symlinks.
                        resolved = resolved.canonicalize().map_err(|error| {
                            refused(&resolved, &format!("cannot resolve directory: {error}"))
                        })?;
                        if !resolved.is_dir() {
                            return Err(refused(&resolved, "expected a directory"));
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        missing = true;
                    }
                    Err(error) => return Err(refused(&resolved, &error.to_string())),
                }
            }
        }
    }
    Ok(resolved)
}

fn check_checkout_path(path: &Path, kind: OutputKind) -> io::Result<()> {
    // Only the nearest enclosing checkout governs the path. A checkout that
    // merely contains that one - a home directory kept in Git, for example -
    // does not own another repository's directories and cannot withhold
    // access to them. Every ancestor is still checked for `.git` metadata,
    // which the walk reaches before the owning checkout for such a path.
    for ancestor in path.ancestors() {
        if ancestor
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
        {
            return Err(refused(path, "inside Git metadata"));
        }
        match fs::symlink_metadata(ancestor.join(".git")) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(refused(ancestor, &error.to_string())),
        }

        if kind == OutputKind::PersistentCache {
            return Err(refused(path, "persistent caches belong outside checkouts"));
        }
        // The owning checkout must be a Xenora tree, and the target must sit
        // in one of the two directories the policy allows.
        if !ancestor.join("crates/loader/Cargo.toml").is_file()
            || !ancestor.join("scripts/build-app.sh").is_file()
            || !ALLOWED_CHECKOUT_DIRS
                .iter()
                .any(|allowed| path.starts_with(ancestor.join(allowed)))
        {
            return Err(refused(
                path,
                "checkout exports are limited to Xenora's artifacts and diagnostics/probe directories",
            ));
        }
        return Ok(());
    }
    Ok(())
}

/// Validate every target before creating the output directory or any files.
/// Relative directories are relative to the current working directory.
pub fn validate_output_paths(
    directory: &Path,
    names: &[&str],
    kind: OutputKind,
) -> io::Result<PathBuf> {
    let directory = resolve_directory(directory)?;
    check_checkout_path(&directory, kind)?;
    for name in names {
        let leaf = Path::new(name);
        if !matches!(
            leaf.components().next(),
            Some(Component::Normal(component))
                if component == leaf.as_os_str() && !component.eq_ignore_ascii_case(".git")
        ) {
            return Err(refused(leaf, "expected one output filename"));
        }
        let path = directory.join(leaf);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => {
                return Err(refused(
                    &path,
                    "expected a regular file, not a link or directory",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(refused(&path, &error.to_string())),
        }
    }
    Ok(directory)
}

/// Stage a batch in the destination filesystem. Once all files are written,
/// publish each by rename so a failed write cannot truncate a previous report.
pub(crate) fn publish_batch(directory: &Path, files: &[(&str, &str)]) -> io::Result<()> {
    let names: Vec<_> = files.iter().map(|(name, _)| *name).collect();
    let directory = validate_output_paths(directory, &names, OutputKind::Export)?;
    fs::create_dir_all(&directory)?;

    let mut directory_builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory_builder.mode(0o700);
    }
    static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
    let staging = (0..64)
        .find_map(|_| {
            let id = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!(".xenora-output-{}-{id}", std::process::id()));
            match directory_builder.create(&path) {
                Ok(()) => Some(Ok(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()?
        .ok_or_else(|| refused(&directory, "could not reserve a staging directory"))?;

    let result = (|| {
        for (name, contents) in files {
            fs::write(staging.join(name), contents)?;
        }
        for (name, _) in files {
            fs::rename(staging.join(name), directory.join(name))?;
        }
        Ok(())
    })();
    let cleanup = fs::remove_dir_all(staging);
    result.and(cleanup)
}
