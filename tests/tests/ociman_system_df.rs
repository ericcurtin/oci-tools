//! `ociman system df` integration tests (`docs/design/0263`): real
//! disk usage across images, containers, and local volumes, matching
//! real `podman system df`'s own default summary table — see
//! `cmd_system_df`'s own doc comment in `bin/ociman/src/main.rs` for
//! exactly how each column is computed and the one deliberate
//! simplification from real podman's own precise per-image
//! cross-sharing calculation.

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

fn df_json(storage_root: &Path) -> serde_json::Value {
    let out = ociman(storage_root, &["--json", "system", "df"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn df_on_an_empty_store_reports_all_zero() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let view = df_json(storage_dir.path());
    for row in ["images", "containers", "local_volumes"] {
        assert_eq!(view[row]["total"], 0, "{row}: {view:?}");
        assert_eq!(view[row]["active"], 0, "{row}: {view:?}");
        assert_eq!(view[row]["size_bytes"], 0, "{row}: {view:?}");
        assert_eq!(view[row]["reclaimable_bytes"], 0, "{row}: {view:?}");
    }

    // The plain-text table still succeeds and prints the real header.
    let out = ociman(storage_dir.path(), &["system", "df"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("TYPE") && stdout.contains("RECLAIMABLE"),
        "{stdout:?}"
    );
    assert!(
        stdout.contains("Images") && stdout.contains("Local Volumes"),
        "{stdout:?}"
    );
}

#[test]
fn df_reports_a_real_unused_image_as_fully_reclaimable() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/df-unused:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let view = df_json(storage_dir.path());
    assert_eq!(view["images"]["total"], 1, "{view:?}");
    assert_eq!(view["images"]["active"], 0, "{view:?}");
    let size = view["images"]["size_bytes"].as_u64().unwrap();
    assert!(size > 0, "{view:?}");
    assert_eq!(
        view["images"]["reclaimable_bytes"], size,
        "an entirely unused image is 100% reclaimable: {view:?}"
    );
}

#[test]
fn df_reports_two_tags_of_the_same_image_only_once() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/df-dedup:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let tag = ociman(
        storage_dir.path(),
        &[
            "tag",
            "ociman-test/df-dedup:latest",
            "ociman-test/df-dedup:second-tag",
        ],
    );
    assert!(tag.status.success(), "{tag:?}");

    let view = df_json(storage_dir.path());
    assert_eq!(
        view["images"]["total"], 1,
        "two tags of the same real image must be deduplicated by manifest digest: {view:?}"
    );
}

#[test]
fn df_reports_real_container_and_active_image_state_after_a_real_run() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/df-container:latest",
        &busybox,
        &["sh", "dd"],
        ContainerConfig::default(),
    );

    // A real, foreground run that writes a known-size file, then
    // exits -- leaving a real, stopped container record with a real
    // writable-layer directory (whichever shape this project's own
    // rootless-overlay optimization picked, 0108-0110 -- `system df`
    // must handle either).
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "df-container-test",
            "ociman-test/df-container:latest",
            "dd",
            "if=/dev/zero",
            "of=/bigfile",
            "bs=1024",
            "count=64",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let view = df_json(storage_dir.path());
    assert_eq!(view["images"]["active"], 1, "{view:?}");
    assert_eq!(
        view["images"]["reclaimable_bytes"], 0,
        "the image is now in use, so nothing about it is reclaimable: {view:?}"
    );

    assert_eq!(view["containers"]["total"], 1, "{view:?}");
    assert_eq!(
        view["containers"]["active"], 0,
        "the container already exited: {view:?}"
    );
    let container_size = view["containers"]["size_bytes"].as_u64().unwrap();
    assert!(
        container_size >= 64 * 1024,
        "the real 64KiB file written must be reflected in the real writable-layer size: {view:?}"
    );
    assert_eq!(
        view["containers"]["reclaimable_bytes"], container_size,
        "a non-running container's own writable layer is fully reclaimable: {view:?}"
    );

    ociman(storage_dir.path(), &["rm", "--force", "df-container-test"]);
}

#[test]
fn df_reports_real_volume_size_and_active_state() {
    let create = |storage_dir: &Path| {
        let create = ociman(storage_dir, &["volume", "create", "df-volume-test"]);
        assert!(create.status.success(), "{create:?}");
    };
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    create(storage_dir.path());

    let view = df_json(storage_dir.path());
    assert_eq!(view["local_volumes"]["total"], 1, "{view:?}");
    assert_eq!(
        view["local_volumes"]["active"], 0,
        "not referenced by any container yet: {view:?}"
    );
    assert_eq!(view["local_volumes"]["size_bytes"], 0, "{view:?}");
    assert_eq!(view["local_volumes"]["reclaimable_bytes"], 0, "{view:?}");
}
