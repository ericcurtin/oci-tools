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

/// `ociman system df -v`/`--verbose` (`docs/design/0285`): a real,
/// per-item breakdown instead of just the aggregate summary above --
/// see `cmd_system_df_verbose`'s own doc comment in
/// `bin/ociman/src/main.rs` for exactly how the cross-image shared/
/// unique size split is computed.
fn df_verbose_json(storage_root: &Path) -> serde_json::Value {
    let out = ociman(storage_root, &["--json", "system", "df", "--verbose"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn df_verbose_json_reports_a_single_image_row_with_its_own_repository_and_tag() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/df-verbose-image:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let view = df_verbose_json(storage_dir.path());
    let images = view["images"].as_array().unwrap();
    assert_eq!(images.len(), 1, "{view:?}");
    assert_eq!(images[0]["repository"], "ociman-test/df-verbose-image");
    assert_eq!(images[0]["tag"], "latest");
    assert!(images[0]["size_bytes"].as_u64().unwrap() > 0, "{view:?}");
    assert_eq!(images[0]["containers"], 0, "{view:?}");
    // Not referenced by any other stored image, so the entire size is
    // this one image's own "unique" share.
    let size = images[0]["size_bytes"].as_u64().unwrap();
    assert_eq!(images[0]["shared_size_bytes"], 0, "{view:?}");
    assert_eq!(images[0]["unique_size_bytes"], size, "{view:?}");
}

#[test]
fn df_verbose_reports_a_real_shared_layer_across_two_distinct_images() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    // Same applets (so the same real layer blob, byte for byte) but a
    // different `Cmd`, so the config blob -- and therefore the
    // manifest digest -- genuinely differs: two real, distinct images
    // that happen to share one real layer.
    seed_image(
        &store,
        "ociman-test/df-verbose-shared-a:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    seed_image(
        &store,
        "ociman-test/df-verbose-shared-b:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec!["sh".to_string()]),
            ..Default::default()
        },
    );

    let view = df_verbose_json(storage_dir.path());
    let images = view["images"].as_array().unwrap();
    assert_eq!(images.len(), 2, "{view:?}");
    for image in images {
        assert!(
            image["shared_size_bytes"].as_u64().unwrap() > 0,
            "the shared busybox layer must count toward shared_size_bytes for both images: {view:?}"
        );
    }
}

#[test]
fn df_verbose_container_row_reports_local_volumes_count() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/df-verbose-container:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "df-verbose-container-test",
            "-v",
            "df-verbose-vol:/data",
            "ociman-test/df-verbose-container:latest",
            "sh",
            "-c",
            "true",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let view = df_verbose_json(storage_dir.path());
    let containers = view["containers"].as_array().unwrap();
    assert_eq!(containers.len(), 1, "{view:?}");
    assert_eq!(containers[0]["name"], "df-verbose-container-test");
    assert_eq!(
        containers[0]["local_volumes"], 1,
        "the one -v mount must be counted: {view:?}"
    );

    ociman(
        storage_dir.path(),
        &["rm", "--force", "df-verbose-container-test"],
    );
}

#[test]
fn df_verbose_volume_row_reports_real_links_count() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/df-verbose-links:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "df-verbose-links-test",
            "-v",
            "df-verbose-links-vol:/data",
            "ociman-test/df-verbose-links:latest",
            "sh",
            "-c",
            "true",
        ],
    );
    assert!(run.status.success(), "{run:?}");

    let view = df_verbose_json(storage_dir.path());
    let volumes = view["local_volumes"].as_array().unwrap();
    let volume = volumes
        .iter()
        .find(|v| v["name"] == "df-verbose-links-vol")
        .unwrap_or_else(|| panic!("{view:?}"));
    assert_eq!(
        volume["links"], 1,
        "the one container mounting it must be counted: {view:?}"
    );

    ociman(
        storage_dir.path(),
        &["rm", "--force", "df-verbose-links-test"],
    );
}

#[test]
fn df_verbose_text_output_shows_three_headed_sections() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let out = ociman(storage_dir.path(), &["system", "df", "--verbose"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Images space usage:")
            && stdout.contains("Containers space usage:")
            && stdout.contains("Local Volumes space usage:"),
        "{stdout:?}"
    );
    assert!(
        stdout.contains("REPOSITORY")
            && stdout.contains("SHARED SIZE")
            && stdout.contains("UNIQUE SIZE"),
        "{stdout:?}"
    );
    assert!(
        stdout.contains("CONTAINER ID") && stdout.contains("LOCAL VOLUMES"),
        "{stdout:?}"
    );
    assert!(
        stdout.contains("VOLUME NAME") && stdout.contains("LINKS"),
        "{stdout:?}"
    );
}
