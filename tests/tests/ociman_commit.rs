//! `ociman commit` integration tests (0155): create a new image from a
//! container's own changes relative to the image it was created from.
//!
//! Same fully offline seeded-image approach `ociman_run.rs`/
//! `ociman_diff.rs` established. Every test forces
//! `.rootless-overlay-supported` to `false` first (see
//! `ociman_diff.rs`'s own module doc comment for why), so the
//! container under test deterministically uses the plain
//! `RootfsSetup::Extract` layout `commit` (like `diff`/`cp` before it)
//! actually supports.

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

/// A real, already-stopped container running `shell_command`, forcing
/// plain-`Extract` rootfs setup deterministically first unless
/// `force_extract` is `false` (matching `ociman_diff.rs`'s own
/// identical parameter, for the one test that deliberately needs to
/// exercise whichever rootfs setup this host's own default actually
/// picks).
fn seed_and_run_stopped_container_ex(
    storage_root: &Path,
    image: &str,
    shell_command: &str,
    force_extract: bool,
) -> String {
    if force_extract {
        std::fs::write(storage_root.join(".rootless-overlay-supported"), "false").unwrap();
    }
    let busybox = busybox_path().expect("busybox not found on $PATH");
    let store = Store::open(storage_root).unwrap();
    seed_image(
        &store,
        image,
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                shell_command.to_string(),
            ]),
            ..Default::default()
        },
    );
    let run = ociman(storage_root, &["run", image]);
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let ps = ociman(storage_root, &["ps", "-a", "-q"]);
    let id = String::from_utf8_lossy(&ps.stdout).trim().to_string();
    assert!(!id.is_empty());
    id
}

/// [`seed_and_run_stopped_container_ex`] with `force_extract: true`
/// (the common case: every test but the one rootless-overlay test
/// itself needs a deterministic, plain-`Extract` container).
fn seed_and_run_stopped_container(storage_root: &Path, image: &str, shell_command: &str) -> String {
    seed_and_run_stopped_container_ex(storage_root, image, shell_command, true)
}

#[test]
fn commit_round_trips_an_added_file_and_a_deleted_one_into_a_real_runnable_image() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/commit-base:latest",
        "echo hi > /new-file.txt; rm /bin/sh; exit 0",
    );

    let commit = ociman(
        storage_dir.path(),
        &[
            "commit",
            "--author",
            "Someone <someone@example.com>",
            "--message",
            "my commit message",
            &id,
            "ociman-test/commit-result:latest",
        ],
    );
    assert!(
        commit.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let stdout = String::from_utf8_lossy(&commit.stdout);
    assert!(
        stdout.contains("tagged: docker.io/ociman-test/commit-result:latest"),
        "stdout: {stdout:?}"
    );

    // Real round trip: run a brand new container from the committed
    // image (no shell at all -- `/bin/sh` was deleted above, and this
    // deliberately never relies on it, to prove that deletion actually
    // propagated into the new image rather than merely not erroring).
    // `busybox` dispatches on its own first argument when invoked
    // directly like this, no shell required.
    let run2 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/commit-result:latest",
            "/bin/busybox",
            "cat",
            "/new-file.txt",
        ],
    );
    assert!(
        run2.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run2.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run2.stdout),
        "hi\n",
        "the committed image's own new layer should contain the file the original container added"
    );

    // The deletion (a real whiteout) also really propagated: `/bin/sh`
    // itself is gone from the committed image, not merely absent from
    // this one run's own command.
    let run3 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/commit-result:latest",
            "/bin/busybox",
            "test",
            "-e",
            "/bin/sh",
        ],
    );
    assert!(
        !run3.status.success(),
        "the committed image should no longer have /bin/sh, which the original container deleted"
    );
}

#[test]
fn commit_sets_author_and_message_and_grows_history_by_exactly_one_real_layer() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/commit-meta-base:latest",
        "exit 0",
    );

    let base_history = ociman(
        storage_dir.path(),
        &["history", "ociman-test/commit-meta-base:latest", "--json"],
    );
    let base_views: serde_json::Value = serde_json::from_slice(&base_history.stdout).unwrap();
    let base_len = base_views.as_array().unwrap().len();

    let commit = ociman(
        storage_dir.path(),
        &[
            "commit",
            "--author",
            "Jane Doe <jane@example.com>",
            "--message",
            "a real commit message",
            &id,
            "ociman-test/commit-meta-result:latest",
        ],
    );
    assert!(
        commit.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&commit.stderr)
    );

    let inspect = ociman(
        storage_dir.path(),
        &["inspect", "ociman-test/commit-meta-result:latest", "--json"],
    );
    assert!(inspect.status.success());
    let config: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(config["author"], "Jane Doe <jane@example.com>");

    let history = ociman(
        storage_dir.path(),
        &["history", "ociman-test/commit-meta-result:latest", "--json"],
    );
    assert!(history.status.success());
    let views: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    let views = views.as_array().unwrap();
    assert_eq!(
        views.len(),
        base_len + 1,
        "commit should add exactly one new history entry on top of the base image's own"
    );
    // Newest entry first (matches `ociman history`'s own real
    // `docker history`/`podman history`-compatible ordering).
    assert_eq!(views[0]["comment"], "a real commit message");
    assert!(
        views[0]["created_by"]
            .as_str()
            .unwrap()
            .contains(&id[..12.min(id.len())]),
        "created_by should reference the container id: {:?}",
        views[0]["created_by"]
    );
}

#[test]
fn commit_requires_the_image_argument_to_parse_as_a_reference() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/commit-bad-tag:latest",
        "exit 0",
    );

    let commit = ociman(storage_dir.path(), &["commit", &id, "Not A Valid Tag!!"]);
    assert!(!commit.status.success());
}

#[test]
fn commit_of_an_unknown_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let commit = ociman(
        storage_dir.path(),
        &["commit", "does-not-exist", "some/image:latest"],
    );
    assert!(!commit.status.success());
}

#[test]
fn commit_is_a_clear_error_for_a_rootless_overlay_rootfs_container() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    // Deliberately does *not* force `.rootless-overlay-supported` to
    // `false` first -- see `ociman_diff.rs`'s own identical test and
    // module doc comment for why this still passes either way,
    // depending on whether this host itself supports the
    // optimization.
    let id = seed_and_run_stopped_container_ex(
        storage_dir.path(),
        "ociman-test/commit-overlay:latest",
        "exit 0",
        false,
    );

    let commit = ociman(
        storage_dir.path(),
        &["commit", &id, "ociman-test/commit-overlay-result:latest"],
    );

    let bundle_dir = storage_dir.path().join("containers").join(&id);
    if bundle_dir.join("upper").exists() {
        assert!(!commit.status.success());
        assert!(
            String::from_utf8_lossy(&commit.stderr).contains("rootless-overlay"),
            "stderr: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
    } else {
        assert!(
            commit.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
    }
}
