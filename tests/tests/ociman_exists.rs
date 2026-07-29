//! `ociman container exists`/`ociman image exists`/`ociman volume
//! exists` (`docs/design/0287`): silent, exit-code-only existence
//! checks matching real `podman container/image/volume exists`
//! exactly (0 = found, 1 = not found, no output either way — real
//! docker has no equivalent of any of the three).

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

fn assert_silent(out: &std::process::Output) {
    assert!(
        out.stdout.is_empty() && out.stderr.is_empty(),
        "exists must never print anything either way, matching real podman exactly: \
         stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn volume_exists_reports_the_real_exit_code_and_prints_nothing() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let missing = ociman(storage_dir.path(), &["volume", "exists", "novol"]);
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");
    assert_silent(&missing);

    let create = ociman(storage_dir.path(), &["volume", "create", "myvol"]);
    assert!(create.status.success(), "{create:?}");

    let present = ociman(storage_dir.path(), &["volume", "exists", "myvol"]);
    assert_eq!(present.status.code(), Some(0), "{present:?}");
    assert_silent(&present);
}

#[test]
fn container_exists_reports_the_real_exit_code_and_prints_nothing() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exists:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let missing = ociman(storage_dir.path(), &["container", "exists", "nope"]);
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");
    assert_silent(&missing);

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "exists-test",
            "ociman-test/exists:latest",
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

    // Findable both by its own `--name` and by its generated id --
    // matching real `podman container exists`'s own identical
    // by-name-or-id resolution.
    let by_name = ociman(storage_dir.path(), &["container", "exists", "exists-test"]);
    assert_eq!(by_name.status.code(), Some(0), "{by_name:?}");
    assert_silent(&by_name);

    let ps = ociman(
        storage_dir.path(),
        &["--json", "ps", "--all", "--filter", "name=exists-test"],
    );
    let containers: serde_json::Value = serde_json::from_slice(&ps.stdout).unwrap();
    let id = containers[0]["id"].as_str().unwrap().to_string();
    let by_id = ociman(storage_dir.path(), &["container", "exists", &id]);
    assert_eq!(by_id.status.code(), Some(0), "{by_id:?}");
}

#[test]
fn container_exists_accepts_the_external_flag_as_a_real_no_op() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    // This project has no "external storage container" concept at
    // all, so `--external` is accepted for CLI compatibility but
    // never changes anything: a nonexistent container is still not
    // found, matching real podman's own documented flag exactly
    // (checked directly).
    let out = ociman(
        storage_dir.path(),
        &["container", "exists", "--external", "nope"],
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_silent(&out);
}

#[test]
fn image_exists_reports_the_real_exit_code_and_prints_nothing() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();

    let missing = ociman(storage_dir.path(), &["image", "exists", "nope"]);
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");
    assert_silent(&missing);

    seed_image(
        &store,
        "ociman-test/image-exists:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let by_tag = ociman(
        storage_dir.path(),
        &["image", "exists", "ociman-test/image-exists:latest"],
    );
    assert_eq!(by_tag.status.code(), Some(0), "{by_tag:?}");
    assert_silent(&by_tag);

    // Resolves by real/short image ID too, matching real `podman
    // image exists` exactly and this project's own established
    // `resolve_by_reference_or_id` convention already shared by
    // `ociman inspect`/`rmi`/`tag`.
    let images = ociman(storage_dir.path(), &["--json", "images"]);
    let list: serde_json::Value = serde_json::from_slice(&images.stdout).unwrap();
    let digest = list[0]["digest"].as_str().unwrap();
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    let short_id = &hex[..12.min(hex.len())];
    let by_id = ociman(storage_dir.path(), &["image", "exists", short_id]);
    assert_eq!(by_id.status.code(), Some(0), "{by_id:?}");
}
