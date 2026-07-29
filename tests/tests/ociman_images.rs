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

/// `ociman images --filter dangling=true|false` (0268), matching real
/// `podman images --filter dangling=true`'s own literal help-text
/// example: `dangling=true` shows only untagged images, `dangling=
/// false` shows only tagged ones.
#[test]
fn images_filter_dangling_selects_only_untagged_or_only_tagged_images() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/filter-dangling-tagged:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    // A second, untagged image: build on top of the first without a
    // resulting tag, the same technique `ociman_prune.rs`'s own
    // dangling tests use.
    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        context_dir.path().join("Containerfile"),
        "FROM ociman-test/filter-dangling-tagged:latest\nRUN true\n",
    )
    .unwrap();
    let build = ociman(
        storage_dir.path(),
        &["build", context_dir.path().to_str().unwrap()],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let dangling_only = ociman(
        storage_dir.path(),
        &["images", "-q", "--filter", "dangling=true"],
    );
    assert!(dangling_only.status.success());
    assert_eq!(
        String::from_utf8_lossy(&dangling_only.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "exactly the one untagged image: {dangling_only:?}"
    );

    let tagged_only = ociman(
        storage_dir.path(),
        &["images", "-q", "--filter", "dangling=false"],
    );
    assert!(tagged_only.status.success());
    assert_eq!(
        String::from_utf8_lossy(&tagged_only.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "exactly the one tagged image: {tagged_only:?}"
    );

    // Sanity: the two filtered sets are actually disjoint digests.
    assert_ne!(dangling_only.stdout, tagged_only.stdout);
}

/// `ociman images --filter label=<key>=<value>`, matching real
/// `podman images --filter label=`'s own semantics -- shared parsing
/// with `ociman prune --filter label=` (`try_parse_label_filter`),
/// checked here at the `images` call site instead.
#[test]
fn images_filter_label_only_lists_images_with_a_matching_label() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/filter-label-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        context_dir.path().join("Containerfile"),
        "FROM ociman-test/filter-label-base:latest\nLABEL env=prod\n",
    )
    .unwrap();
    let build = ociman(
        storage_dir.path(),
        &["build", context_dir.path().to_str().unwrap()],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let full_digest = String::from_utf8_lossy(&build.stdout)
        .lines()
        .next()
        .unwrap()
        .to_string();
    let digest = full_digest.strip_prefix("sha256:").unwrap_or(&full_digest)[..12].to_string();

    // A mismatched value: the labeled image is excluded.
    let no_match = ociman(
        storage_dir.path(),
        &["images", "-q", "--filter", "label=env=staging"],
    );
    assert!(no_match.status.success());
    assert!(
        !String::from_utf8_lossy(&no_match.stdout).contains(&digest),
        "a mismatched label value should never match: {no_match:?}"
    );

    // The exact matching value: only the labeled image is listed.
    let matched = ociman(
        storage_dir.path(),
        &["images", "-q", "--filter", "label=env=prod"],
    );
    assert!(matched.status.success());
    let lines: Vec<String> = String::from_utf8_lossy(&matched.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines, vec![digest]);
}

/// An unrecognized `--filter` value is a clear, immediate error rather
/// than a silently-ignored no-op (matching `ociman prune`'s own
/// identical rule for its own unrecognized filters).
#[test]
fn images_filter_with_an_unrecognized_kind_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(
        storage_dir.path(),
        &["images", "--filter", "before=some-image"],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not yet supported"),
        "{out:?}"
    );
}
