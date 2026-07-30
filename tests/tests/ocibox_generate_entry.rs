//! `ocibox generate-entry` integration tests (`docs/design/0364`): a
//! real, standalone desktop launcher for entering a whole box —
//! distinct from `export --app` (`ocibox_export.rs`), which exports
//! one specific application *inside* a box. Same fully offline
//! seeded-image approach `ocibox_export.rs` already established.

use std::path::Path;
use std::process::Command;

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image};

fn ocibox(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ocibox")
}

/// Same as [`ocibox`], but with `$HOME` overridden to `home` — a
/// generated entry's own destination is always computed from `$HOME`
/// (`$HOME/.local/share/applications`), the same real convention
/// `ocibox_export.rs`'s own identical helper already established.
fn ocibox_with_home(storage_root: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .env("HOME", home)
        .args(args)
        .output()
        .expect("failed to spawn ocibox")
}

/// Seeds a real busybox-based image and `create`s a box from it —
/// same technique `ocibox_export.rs`'s own `make_box` already
/// established.
fn make_box(storage_dir: &tempfile::TempDir, name: &str) {
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/generate-entry-base:latest",
        &busybox_path().expect("busybox not found on $PATH"),
        &["sh", "echo"],
        ContainerConfig::default(),
    );
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/generate-entry-base:latest",
            "--name",
            name,
        ],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
}

#[test]
fn generate_entry_writes_a_real_desktop_launcher_with_the_default_icon() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "genentry-basic");

    let generate = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
        &["generate-entry", "genentry-basic"],
    );
    assert!(
        generate.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&generate.stderr)
    );

    let entry_path = home_dir
        .path()
        .join(".local/share/applications/genentry-basic.desktop");
    let content = std::fs::read_to_string(&entry_path).unwrap();
    assert!(content.contains("Name=genentry-basic"), "{content}");
    assert!(
        content.contains("Exec=ocibox enter genentry-basic"),
        "{content}"
    );
    assert!(content.contains("Icon=utilities-terminal"), "{content}");
    assert!(content.contains("TryExec=ocibox"), "{content}");
    assert!(content.contains("[Desktop Action Remove]"), "{content}");
    assert!(
        content.contains("Exec=ocibox rm genentry-basic"),
        "{content}"
    );
}

#[test]
fn generate_entry_icon_overrides_the_default() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "genentry-icon");

    let generate = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
        &["generate-entry", "genentry-icon", "--icon", "fedora-logo"],
    );
    assert!(generate.status.success(), "{generate:?}");

    let entry_path = home_dir
        .path()
        .join(".local/share/applications/genentry-icon.desktop");
    let content = std::fs::read_to_string(&entry_path).unwrap();
    assert!(content.contains("Icon=fedora-logo"), "{content}");
}

#[test]
fn generate_entry_delete_removes_the_launcher_and_tolerates_a_missing_one() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "genentry-delete");
    let entry_path = home_dir
        .path()
        .join(".local/share/applications/genentry-delete.desktop");

    let generate = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
        &["generate-entry", "genentry-delete"],
    );
    assert!(generate.status.success(), "{generate:?}");
    assert!(entry_path.is_file());

    let delete = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
        &["generate-entry", "genentry-delete", "--delete"],
    );
    assert!(
        delete.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&delete.stderr)
    );
    assert!(!entry_path.exists());

    // A second delete, with nothing left to remove, is a real,
    // tolerant no-op success -- matching real `distrobox generate-
    // entry --delete`'s own identical `os.IsNotExist` tolerance.
    let delete_again = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
        &["generate-entry", "genentry-delete", "--delete"],
    );
    assert!(delete_again.status.success(), "{delete_again:?}");
}

/// `--all` generates (or deletes) an entry for every existing box,
/// ignoring any `NAME` also given — matching real `distrobox
/// generate-entry --all`'s own identical priority.
#[test]
fn generate_entry_all_covers_every_existing_box() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "genentry-all-1");
    make_box(&storage_dir, "genentry-all-2");

    let generate = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
        &["generate-entry", "--all", "ignored-name"],
    );
    assert!(generate.status.success(), "{generate:?}");

    let apps_dir = home_dir.path().join(".local/share/applications");
    assert!(apps_dir.join("genentry-all-1.desktop").is_file());
    assert!(apps_dir.join("genentry-all-2.desktop").is_file());
    assert!(!apps_dir.join("ignored-name.desktop").exists());

    let delete_all = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
        &["generate-entry", "--all", "--delete"],
    );
    assert!(delete_all.status.success(), "{delete_all:?}");
    assert!(!apps_dir.join("genentry-all-1.desktop").exists());
    assert!(!apps_dir.join("genentry-all-2.desktop").exists());
}

#[test]
fn generate_entry_of_an_unknown_box_is_a_clear_error_but_delete_tolerates_it() {
    let storage_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    let generate = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
        &["generate-entry", "never-created"],
    );
    assert!(!generate.status.success());
    assert!(
        String::from_utf8_lossy(&generate.stderr).contains("cannot find box"),
        "{}",
        String::from_utf8_lossy(&generate.stderr)
    );

    // Deleting an entry for a box that doesn't exist at all (or never
    // had one generated) is still a real, tolerant success -- the
    // box's own existence is never checked on the delete path.
    let delete = ocibox_with_home(
        storage_dir.path(),
        home_dir.path(),
        &["generate-entry", "never-created", "--delete"],
    );
    assert!(delete.status.success(), "{delete:?}");
}

#[test]
fn generate_entry_requires_either_a_name_or_all() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ocibox(storage_dir.path(), &["generate-entry"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("either NAME or --all"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
