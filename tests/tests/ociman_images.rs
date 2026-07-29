//! `ociman images -q`/`--quiet` integration tests (`docs/design/
//! 0265`): matching real `docker images -q`/`podman images -q`
//! exactly, and this project's own `ociman ps -q`'s identical shape
//! for containers — a real self-inconsistency in `ociman`'s own CLI
//! this closes (`ps` already had `-q`; `images` didn't). Same fully
//! offline seeded-image approach `ociman_rmi.rs`/`ociman_system_df.rs`
//! established.

use std::path::Path;
use std::process::Command;

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image};

fn ociman(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociman")
}

#[test]
fn images_quiet_prints_nothing_on_an_empty_store() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let out = ociman(storage_dir.path(), &["images", "-q"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "an empty store prints nothing at all in quiet mode: {out:?}"
    );
}

#[test]
fn images_quiet_prints_the_same_short_digest_the_plain_table_shows() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/images-quiet:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let plain = ociman(storage_dir.path(), &["images"]);
    assert!(plain.status.success());
    let plain_stdout = String::from_utf8_lossy(&plain.stdout);
    let plain_digest = plain_stdout
        .lines()
        .nth(1)
        .expect("one real image row")
        .split_whitespace()
        .nth(1)
        .expect("a DIGEST column")
        .to_string();

    // Both the short `-q` and the long `--quiet` spelling behave
    // identically, and print the exact same 12-hex-char digest the
    // plain table's own `DIGEST` column already showed above -- one
    // shared computation, never two different truncation rules
    // silently drifting apart.
    for flag in ["-q", "--quiet"] {
        let quiet = ociman(storage_dir.path(), &["images", flag]);
        assert!(
            quiet.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&quiet.stderr)
        );
        let quiet_stdout = String::from_utf8_lossy(&quiet.stdout);
        let lines: Vec<&str> = quiet_stdout.lines().collect();
        assert_eq!(lines.len(), 1, "{flag}: {quiet_stdout:?}");
        assert_eq!(lines[0], plain_digest, "{flag}: {quiet_stdout:?}");
        assert_eq!(
            lines[0].len(),
            12,
            "matches real docker/podman's own 12-hex-char short ID: {flag}: {quiet_stdout:?}"
        );
    }
}

#[test]
fn images_quiet_lists_one_line_per_tag_including_two_tags_of_the_same_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/images-quiet-two-tags:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let tag = ociman(
        storage_dir.path(),
        &[
            "tag",
            "ociman-test/images-quiet-two-tags:latest",
            "ociman-test/images-quiet-two-tags:second",
        ],
    );
    assert!(tag.status.success(), "{tag:?}");

    let quiet = ociman(storage_dir.path(), &["images", "-q"]);
    assert!(quiet.status.success());
    // Real `podman images -q` lists one row per *tag*, matching the
    // plain table's own identical one-row-per-tag behavior (this
    // project's own established behavior, unrelated to this new
    // flag) -- both rows here share the same real digest.
    let lines: Vec<String> = String::from_utf8_lossy(&quiet.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0], lines[1], "{lines:?}");
}
