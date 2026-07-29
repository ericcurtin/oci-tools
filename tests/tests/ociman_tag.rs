//! `ociman tag` integration tests: giving an already-stored image a
//! second reference pointing at the exact same manifest digest,
//! matching real `docker tag`/`podman tag` (see `docs/design/0103`).
//! Same fully offline seeded-image approach `ociman_run.rs`/
//! `ociman_rmi.rs` established.

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
fn tag_creates_a_second_pointer_at_the_same_manifest_digest() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/tag-source:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let source_record = store
        .resolve_image("docker.io/ociman-test/tag-source:latest")
        .unwrap()
        .unwrap();

    let tag = ociman(
        storage_dir.path(),
        &[
            "tag",
            "ociman-test/tag-source:latest",
            "ociman-test/tag-target:v2",
        ],
    );
    assert!(
        tag.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&tag.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&tag.stdout).trim(),
        "docker.io/ociman-test/tag-target:v2"
    );

    // The real, on-disk store resolves the new reference to the exact
    // same manifest digest -- not a copy, a second pointer.
    let target_record = store
        .resolve_image("docker.io/ociman-test/tag-target:v2")
        .unwrap()
        .unwrap();
    assert_eq!(target_record.manifest_digest, source_record.manifest_digest);

    // The source reference still resolves too -- tagging never
    // removes or renames the original.
    assert!(
        store
            .resolve_image("docker.io/ociman-test/tag-source:latest")
            .unwrap()
            .is_some()
    );

    // Both are independently usable: `ociman run` against the new tag
    // works exactly like the original.
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/tag-target:v2",
            "--",
            "/bin/sh",
            "-c",
            "true",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn tag_of_an_unknown_source_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let tag = ociman(
        storage_dir.path(),
        &["tag", "ociman-test/never-pulled:latest", "whatever:latest"],
    );
    assert!(!tag.status.success());
    assert!(
        String::from_utf8_lossy(&tag.stderr).contains("no such image"),
        "{}",
        String::from_utf8_lossy(&tag.stderr)
    );
}

#[test]
fn tag_overwrites_an_existing_target_reference() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/tag-overwrite-old:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    seed_image(
        &store,
        "ociman-test/tag-overwrite-new:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let old_record = store
        .resolve_image("docker.io/ociman-test/tag-overwrite-old:latest")
        .unwrap()
        .unwrap();
    let new_record = store
        .resolve_image("docker.io/ociman-test/tag-overwrite-new:latest")
        .unwrap()
        .unwrap();
    assert_ne!(old_record.manifest_digest, new_record.manifest_digest);

    // Point "moving:latest" at the old image first...
    let tag1 = ociman(
        storage_dir.path(),
        &[
            "tag",
            "ociman-test/tag-overwrite-old:latest",
            "moving:latest",
        ],
    );
    assert!(tag1.status.success());
    assert_eq!(
        store
            .resolve_image("docker.io/library/moving:latest")
            .unwrap()
            .unwrap()
            .manifest_digest,
        old_record.manifest_digest
    );

    // ...then retag it at the new image -- real docker/podman both
    // silently move the tag rather than refusing.
    let tag2 = ociman(
        storage_dir.path(),
        &[
            "tag",
            "ociman-test/tag-overwrite-new:latest",
            "moving:latest",
        ],
    );
    assert!(tag2.status.success());
    assert_eq!(
        store
            .resolve_image("docker.io/library/moving:latest")
            .unwrap()
            .unwrap()
            .manifest_digest,
        new_record.manifest_digest
    );
}

#[test]
fn tag_json_reports_the_canonical_source_and_target() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/tag-json:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let tag = ociman(
        storage_dir.path(),
        &[
            "--json",
            "tag",
            "ociman-test/tag-json:latest",
            "tag-json-out:v1",
        ],
    );
    assert!(
        tag.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&tag.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&tag.stdout).unwrap();
    assert_eq!(view["source"], "docker.io/ociman-test/tag-json:latest");
    assert_eq!(view["target"], "docker.io/library/tag-json-out:v1");
}

/// Real docker/podman rule, checked directly: `tag`'s own source
/// resolves by image ID too, not just a tag reference -- the exact
/// short digest `ociman images`' own `DIGEST` column already prints.
/// Unlike `ociman rmi`'s own by-ID case, tagging has no removal-
/// ambiguity question at all (it only ever adds a pointer).
#[test]
fn tag_resolves_its_own_source_by_a_real_image_id() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/tag-by-id:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/tag-by-id:latest")
        .unwrap()
        .unwrap();
    let short_id = record.manifest_digest.hex()[..12].to_string();

    let tag = ociman(
        storage_dir.path(),
        &["--json", "tag", &short_id, "tag-by-id-out:v1"],
    );
    assert!(
        tag.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&tag.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&tag.stdout).unwrap();
    // The resolved (real, canonical) reference is reported back, not
    // the raw ID the user typed.
    assert_eq!(view["source"], "docker.io/ociman-test/tag-by-id:latest");
    assert_eq!(view["target"], "docker.io/library/tag-by-id-out:v1");

    let new_record = store
        .resolve_image("docker.io/library/tag-by-id-out:v1")
        .unwrap()
        .unwrap();
    assert_eq!(new_record.manifest_digest, record.manifest_digest);
}

/// `ociman untag <image> <reference>` (0283): with an explicit
/// reference given, only that one reference is removed -- the image
/// used to resolve the target, and any other sibling tag, both
/// survive untouched. Matches real `podman untag`'s own checked-
/// directly behavior exactly.
#[test]
fn untag_with_an_explicit_reference_removes_only_that_one() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/untag-explicit:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let tag1 = ociman(
        storage_dir.path(),
        &[
            "tag",
            "ociman-test/untag-explicit:latest",
            "untag-alias1:latest",
        ],
    );
    assert!(tag1.status.success());
    let tag2 = ociman(
        storage_dir.path(),
        &[
            "tag",
            "ociman-test/untag-explicit:latest",
            "untag-alias2:latest",
        ],
    );
    assert!(tag2.status.success());

    let untag = ociman(
        storage_dir.path(),
        &["untag", "untag-alias1:latest", "untag-alias2:latest"],
    );
    assert!(
        untag.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&untag.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&untag.stdout).trim(),
        "docker.io/library/untag-alias2:latest"
    );

    // Only untag-alias2 is gone; the resolving reference (untag-alias1)
    // and the original source both survive untouched.
    assert!(
        store
            .resolve_image("docker.io/library/untag-alias2:latest")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .resolve_image("docker.io/library/untag-alias1:latest")
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/untag-explicit:latest")
            .unwrap()
            .is_some()
    );
}

/// `ociman untag <image>` (a single argument, 0283): untags *every*
/// real reference currently pointing at that image, matching real
/// `podman untag <image>`'s own checked-directly behavior exactly --
/// confirmed the hard way (an initial assumption that only the given
/// reference would be removed was wrong; real podman untags
/// everything when no further references are given).
#[test]
fn untag_with_only_one_argument_removes_every_tag_of_that_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/untag-all:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let tag1 = ociman(
        storage_dir.path(),
        &["tag", "ociman-test/untag-all:latest", "untag-all-a:latest"],
    );
    assert!(tag1.status.success());
    let tag2 = ociman(
        storage_dir.path(),
        &["tag", "ociman-test/untag-all:latest", "untag-all-b:latest"],
    );
    assert!(tag2.status.success());

    let untag = ociman(storage_dir.path(), &["untag", "untag-all-a:latest"]);
    assert!(
        untag.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&untag.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&untag.stdout)
            .trim()
            .lines()
            .count(),
        3,
        "{untag:?}"
    );

    assert!(
        store
            .resolve_image("docker.io/ociman-test/untag-all:latest")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .resolve_image("docker.io/library/untag-all-a:latest")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .resolve_image("docker.io/library/untag-all-b:latest")
            .unwrap()
            .is_none()
    );
}

/// An explicit reference that belongs to a *different* image is a
/// clear error, and nothing at all is removed -- matching real
/// `podman untag <image> <unrelated-tag>`'s own checked-directly
/// "tag not known" refusal exactly.
#[test]
fn untag_an_unrelated_reference_is_a_clear_error_and_removes_nothing() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/untag-unrelated-a:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    seed_image(
        &store,
        "ociman-test/untag-unrelated-b:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    let untag = ociman(
        storage_dir.path(),
        &[
            "untag",
            "ociman-test/untag-unrelated-a:latest",
            "ociman-test/untag-unrelated-b:latest",
        ],
    );
    assert!(!untag.status.success());
    assert!(
        String::from_utf8_lossy(&untag.stderr).contains("does not currently point"),
        "{}",
        String::from_utf8_lossy(&untag.stderr)
    );

    // Both images are completely untouched.
    assert!(
        store
            .resolve_image("docker.io/ociman-test/untag-unrelated-a:latest")
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/untag-unrelated-b:latest")
            .unwrap()
            .is_some()
    );
}

/// `ociman untag` of an unresolvable image reference is a clear
/// error.
#[test]
fn untag_of_an_unknown_image_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let untag = ociman(storage_dir.path(), &["untag", "does-not-exist:latest"]);
    assert!(!untag.status.success());
    assert!(
        String::from_utf8_lossy(&untag.stderr).contains("no such image"),
        "{}",
        String::from_utf8_lossy(&untag.stderr)
    );
}

/// `ociman untag`'s own image argument resolves by a real or short
/// image ID too, not just a tag reference -- same convention `tag`'s
/// own source resolution already established.
#[test]
fn untag_resolves_the_image_argument_by_a_real_image_id() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/untag-by-id:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/untag-by-id:latest")
        .unwrap()
        .unwrap();
    let short_id = record.manifest_digest.hex()[..12].to_string();

    let untag = ociman(storage_dir.path(), &["untag", &short_id]);
    assert!(
        untag.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&untag.stderr)
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/untag-by-id:latest")
            .unwrap()
            .is_none()
    );
}
