//! `ociman inspect` integration tests: real docker/podman's own
//! default resolution order — a container (by id or `--name`) is
//! tried first, falling back to an image if no such container exists
//! (checked directly against `~/git/podman/cmd/podman/inspect/
//! inspect.go`'s own `inspectAll`, see `docs/design/0094`). Same fully
//! offline seeded-image approach `ociman_run.rs` established.

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

fn only_container_id(storage_root: &Path) -> String {
    let out = ociman(storage_root, &["ps", "-a", "-q"]);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn inspect_by_container_name_returns_the_real_container_state() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-basic:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 5".to_string(),
            ]),
            ..Default::default()
        },
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "inspect-me",
            "ociman-test/inspect-basic:latest",
        ],
    );
    assert_eq!(run.status.code(), Some(5));

    let inspect = ociman(storage_dir.path(), &["inspect", "inspect-me"]);
    assert!(
        inspect.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(view["name"], "inspect-me");
    assert_eq!(view["status"], "stopped");
    assert_eq!(view["pid"], 0);
    assert_eq!(view["exit_code"], 5);
    assert_eq!(
        view["image"], "docker.io/ociman-test/inspect-basic:latest",
        "{view:?}"
    );
    assert!(
        view["bundle"]
            .as_str()
            .unwrap()
            .contains(view["id"].as_str().unwrap()),
        "{view:?}"
    );
}

#[test]
fn inspect_by_container_id_returns_the_same_data_as_by_name() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-by-id:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 0".to_string(),
            ]),
            ..Default::default()
        },
    );

    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/inspect-by-id:latest"],
    );
    assert!(run.status.success());
    let id = only_container_id(storage_dir.path());
    assert!(!id.is_empty());

    let inspect = ociman(storage_dir.path(), &["inspect", &id]);
    assert!(inspect.status.success());
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(view["id"], id);
    // No `--name` given, so the field is omitted entirely (matches
    // `ContainerView`'s own established `skip_serializing_if` for the
    // same field).
    assert!(view.get("name").is_none(), "{view:?}");
}

#[test]
fn inspect_falls_back_to_an_image_when_no_such_container_exists() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-image-only:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    // No container ever created -- only the image exists.
    let inspect = ociman(
        storage_dir.path(),
        &["inspect", "ociman-test/inspect-image-only:latest"],
    );
    assert!(
        inspect.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let config: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    // A real `ImageConfig`, not a `ContainerInspectView` -- has
    // `architecture`/`os`, not `status`/`pid`.
    assert!(config.get("architecture").is_some(), "{config:?}");
    assert!(config.get("status").is_none(), "{config:?}");
}

#[test]
fn inspect_of_an_unknown_reference_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(
        storage_dir.path(),
        &["inspect", "nothing-matches-this-at-all"],
    );
    assert!(!out.status.success());
}

/// Real docker/podman rule, checked directly: `inspect` (and `rmi`)
/// resolve by image ID too, not just a tag reference -- the exact
/// short digest `ociman images`' own `DIGEST` column already prints.
#[test]
fn inspect_resolves_a_real_image_by_its_own_short_id() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-by-id:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/inspect-by-id:latest")
        .unwrap()
        .unwrap();
    let full_hex = record.manifest_digest.hex().to_string();
    let short_id = &full_hex[..12];

    let inspect = ociman(storage_dir.path(), &["inspect", short_id]);
    assert!(
        inspect.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let config: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert!(config.get("architecture").is_some(), "{config:?}");

    // The full digest (with and without the `sha256:` prefix) works
    // too, matching real `docker inspect <full-id>`.
    let inspect_full = ociman(storage_dir.path(), &["inspect", &full_hex]);
    assert!(inspect_full.status.success());
    let inspect_prefixed = ociman(
        storage_dir.path(),
        &["inspect", &format!("sha256:{full_hex}")],
    );
    assert!(inspect_prefixed.status.success());
}

/// Two tags pointing at the exact same image (`ociman tag`) must never
/// make resolving that image by its own (now doubly-referenced) ID
/// ambiguous -- only two genuinely *different* images sharing a
/// digest prefix should be.
#[test]
fn inspect_by_id_is_not_ambiguous_when_multiple_tags_share_the_same_digest() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/inspect-by-id-aliased:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let tag = ociman(
        storage_dir.path(),
        &[
            "tag",
            "ociman-test/inspect-by-id-aliased:latest",
            "ociman-test/inspect-by-id-aliased:v2",
        ],
    );
    assert!(tag.status.success());

    let record = store
        .resolve_image("docker.io/ociman-test/inspect-by-id-aliased:latest")
        .unwrap()
        .unwrap();
    let short_id = &record.manifest_digest.hex()[..12];

    let inspect = ociman(storage_dir.path(), &["inspect", short_id]);
    assert!(
        inspect.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
}
