// SPDX-License-Identifier: GPL-3.0-or-later

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use xenora_loader::output_path::{OutputKind, macos_cache_directory, validate_output_paths};
use xenora_loader::probe::{ProbeConfig, run_probe_and_export};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir();
        loop {
            let path = root.join(format!(
                "xenora-output-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path.canonicalize().unwrap()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot make scratch directory: {error}"),
            }
        }
    }

    fn checkout(&self) -> PathBuf {
        let path = self.0.join("checkout");
        fs::create_dir_all(path.join("crates/loader")).unwrap();
        fs::create_dir(path.join("scripts")).unwrap();
        fs::write(path.join(".git"), "gitdir: elsewhere\n").unwrap();
        fs::write(path.join("crates/loader/Cargo.toml"), "").unwrap();
        fs::write(path.join("scripts/build-app.sh"), "").unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

const SYNTHETIC_PE: &[u8] = include_bytes!("../../../fixtures/pe/delay-imports.exe");

fn export(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    run_probe_and_export(SYNTHETIC_PE, path, &ProbeConfig::default()).map(|_| ())
}

#[test]
fn exports_to_ignored_checkout_paths_and_explicit_external_paths() {
    let scratch = Scratch::new();
    let checkout = scratch.checkout();
    for path in [
        checkout.join("artifacts/report"),
        checkout.join("diagnostics/probe"),
        scratch.0.join("explicit/new/report"),
    ] {
        export(&path).unwrap();
        assert_eq!(fs::read_dir(&path).unwrap().count(), 8);
        assert!(
            fs::read_to_string(path.join("imports.csv"))
                .unwrap()
                .contains("OrdinaryByName")
        );

        fs::write(path.join("imports.csv"), "previous report").unwrap();
        export(&path).unwrap();
        assert_eq!(fs::read_dir(&path).unwrap().count(), 8);
        assert!(
            fs::read_to_string(path.join("imports.csv"))
                .unwrap()
                .contains("OrdinaryByName")
        );
    }
}

#[test]
fn refuses_checkout_sources_before_creating_missing_directories() {
    let scratch = Scratch::new();
    let checkout = scratch.checkout();
    for path in [
        checkout.join("crates/loader"),
        checkout.join("new/deeper"),
        checkout.join("diagnostics/probe-sibling"),
    ] {
        let error = export(&path).unwrap_err();
        assert!(error.to_string().contains(&path.display().to_string()));
        assert!(!path.join("imports.csv").exists());
    }
    assert!(!checkout.join("new").exists());
    assert!(!checkout.join("diagnostics").exists());

    let other = scratch.0.join("other-project");
    fs::create_dir_all(other.join(".git")).unwrap();
    assert!(export(&other.join("artifacts")).is_err());
    assert!(!other.join("artifacts").exists());
    assert!(export(&other.join(".git")).is_err());
}

#[test]
fn a_checkout_inside_another_repository_governs_its_own_exports() {
    let scratch = Scratch::new();
    // An enclosing repository stands in for a home directory kept in Git.
    let home = scratch.0.join("home");
    fs::create_dir(&home).unwrap();
    fs::write(home.join(".git"), "gitdir: elsewhere\n").unwrap();
    let checkout = home.join("Developer/Xenora");
    fs::create_dir_all(checkout.join("crates/loader")).unwrap();
    fs::create_dir_all(checkout.join("scripts")).unwrap();
    fs::write(checkout.join(".git"), "gitdir: elsewhere\n").unwrap();
    fs::write(checkout.join("crates/loader/Cargo.toml"), "").unwrap();
    fs::write(checkout.join("scripts/build-app.sh"), "").unwrap();

    export(&checkout.join("artifacts/report")).unwrap();
    assert!(checkout.join("artifacts/report/imports.csv").is_file());

    // The enclosing repository still governs its own tree.
    assert!(export(&home.join("artifacts")).is_err());
    assert!(!home.join("artifacts").exists());
    assert!(export(&home.join("artifacts/report")).is_err());
    assert!(!home.join("artifacts").exists());
}

#[cfg(unix)]
#[test]
fn resolves_directory_aliases_and_rejects_unsafe_output_leaves() {
    use std::os::unix::fs::symlink;

    let scratch = Scratch::new();
    let checkout = scratch.checkout();
    let safe = checkout.join("artifacts");
    fs::create_dir(&safe).unwrap();
    symlink(&safe, scratch.0.join("safe-alias")).unwrap();
    export(&scratch.0.join("safe-alias/new")).unwrap();
    assert!(safe.join("new/imports.csv").is_file());

    symlink(checkout.join("crates"), scratch.0.join("source-alias")).unwrap();
    assert!(export(&scratch.0.join("source-alias/new")).is_err());
    assert!(!checkout.join("crates/new").exists());
    assert!(export(&scratch.0.join("source-alias/../new")).is_err());

    symlink(scratch.0.join("missing"), scratch.0.join("dangling")).unwrap();
    assert!(export(&scratch.0.join("dangling/new")).is_err());
    assert!(!scratch.0.join("missing").exists());
    assert!(export(&scratch.0.join("missing/../new")).is_err());
    assert!(!scratch.0.join("new").exists());

    let output = safe.join("partial");
    fs::create_dir(&output).unwrap();
    let protected = checkout.join("crates/loader/Cargo.toml");
    fs::write(&protected, "protected").unwrap();
    fs::write(output.join("imports.csv"), "prior report").unwrap();
    symlink(&protected, output.join("test_commands.txt")).unwrap();
    let error = export(&output).unwrap_err();
    assert!(error.to_string().contains("test_commands.txt"));
    assert_eq!(fs::read_to_string(&protected).unwrap(), "protected");
    assert_eq!(
        fs::read_to_string(output.join("imports.csv")).unwrap(),
        "prior report"
    );
    assert_eq!(fs::read_dir(&output).unwrap().count(), 2);
    fs::remove_file(output.join("test_commands.txt")).unwrap();
    fs::create_dir(output.join("test_commands.txt")).unwrap();
    assert!(export(&output).is_err());
    assert_eq!(fs::read_dir(&output).unwrap().count(), 2);

    let linked = safe.join("hard-link");
    fs::create_dir(&linked).unwrap();
    fs::hard_link(&protected, linked.join("imports.csv")).unwrap();
    export(&linked).unwrap();
    assert_eq!(fs::read_to_string(&protected).unwrap(), "protected");
}

#[test]
fn cache_uses_a_per_user_root_and_never_writes_to_a_checkout() {
    let scratch = Scratch::new();
    let checkout = scratch.checkout();
    let home = scratch.0.join("home");
    let cache = macos_cache_directory(&home).unwrap();
    assert_eq!(cache, home.join("Library/Caches/local.xenora.Xenora"));
    assert_eq!(
        validate_output_paths(&cache, &["pipeline"], OutputKind::PersistentCache).unwrap(),
        cache
    );
    assert!(!home.exists());
    assert!(macos_cache_directory(Path::new("relative-home")).is_err());
    assert!(
        validate_output_paths(
            &checkout.join("artifacts/cache"),
            &["pipeline"],
            OutputKind::PersistentCache
        )
        .is_err()
    );
    for name in [
        "",
        "../pipeline",
        "dir/pipeline",
        "/pipeline",
        "./pipeline",
        "pipeline/",
        ".git",
    ] {
        assert!(
            validate_output_paths(&cache, &[name], OutputKind::PersistentCache).is_err(),
            "{name}"
        );
    }
    assert!(!checkout.join("artifacts").exists());
}
