//! `ociman cp` integration tests (see `docs/design/0146`): real file
//! copies between the host and a container's own on-disk storage,
//! for both directions, both file and directory sources, the `..`
//! path-traversal guard, `--overwrite`, container-to-container being
//! a clear error, and a rootless-overlay-rootfs container being a
//! clear error too.
//!
//! Every test here forces `.rootless-overlay-supported` to `false`
//! (see `rootfs_setup::rootless_overlay_supported_cached`'s own doc
//! comment) *before* the container's first `run`, so the container
//! under test deterministically uses the plain `RootfsSetup::Extract`
//! layout `cp` actually supports, regardless of whether this
//! particular host happens to support the rootless-overlay
//! optimization or not — `cp_is_a_clear_error_for_a_rootless_overlay_
//! rootfs_container` below is the one test that deliberately leaves
//! it unset, to exercise the *other* branch for real (and is written
//! so it still passes either way: if this host doesn't support the
//! optimization either, `cp` just succeeds instead, which is also a
//! correct, passing outcome for that one test).

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

/// A real container that has already run to completion (`exit 0`) --
/// `cp` must work against a stopped container exactly as well as a
/// running one, matching real `podman cp`. Forces plain-`Extract`
/// rootfs setup deterministically first (see the module's own doc
/// comment) unless `force_extract` is `false`.
fn seed_and_run_stopped_container(storage_root: &Path, image: &str, force_extract: bool) -> String {
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
                "exit 0".to_string(),
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

fn container_rootfs(storage_root: &Path, id: &str) -> String {
    let inspect = ociman(storage_root, &["inspect", id, "--json"]);
    assert!(inspect.status.success());
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    view["rootfs"].as_str().unwrap().to_string()
}

#[test]
fn cp_copies_a_single_file_both_directions() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(storage_dir.path(), "ociman-test/cp-file:latest", true);
    let rootfs = container_rootfs(storage_dir.path(), &id);

    let host_src = storage_dir.path().join("host_src.txt");
    std::fs::write(&host_src, "hello from host").unwrap();

    let to_container = ociman(
        storage_dir.path(),
        &[
            "cp",
            host_src.to_str().unwrap(),
            &format!("{id}:/copied.txt"),
        ],
    );
    assert!(
        to_container.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&to_container.stderr)
    );
    let in_container = Path::new(&rootfs).join("copied.txt");
    assert_eq!(
        std::fs::read_to_string(&in_container).unwrap(),
        "hello from host"
    );

    let host_dest = storage_dir.path().join("host_dest.txt");
    let from_container = ociman(
        storage_dir.path(),
        &[
            "cp",
            &format!("{id}:/copied.txt"),
            host_dest.to_str().unwrap(),
        ],
    );
    assert!(
        from_container.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&from_container.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&host_dest).unwrap(),
        "hello from host"
    );
}

#[test]
fn cp_copying_a_file_onto_an_existing_directory_lands_inside_it_under_its_own_basename() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/cp-file-into-dir:latest",
        true,
    );
    let rootfs = container_rootfs(storage_dir.path(), &id);

    // Create a real destination directory in the container first (the
    // seeded busybox image's own top-level entries are mostly
    // symlinks, e.g. `/lib` -> `usr/lib`, not always real
    // directories).
    let host_src_dir = storage_dir.path().join("existing_dir_source");
    std::fs::create_dir_all(&host_src_dir).unwrap();
    let mkdir = ociman(
        storage_dir.path(),
        &[
            "cp",
            host_src_dir.to_str().unwrap(),
            &format!("{id}:/existing_dir"),
        ],
    );
    assert!(mkdir.status.success());

    let host_src = storage_dir.path().join("host_src.txt");
    std::fs::write(&host_src, "into a directory").unwrap();
    let cp = ociman(
        storage_dir.path(),
        &[
            "cp",
            host_src.to_str().unwrap(),
            &format!("{id}:/existing_dir"),
        ],
    );
    assert!(
        cp.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&cp.stderr)
    );
    let landed = Path::new(&rootfs).join("existing_dir/host_src.txt");
    assert_eq!(
        std::fs::read_to_string(&landed).unwrap(),
        "into a directory"
    );
}

#[test]
fn cp_copies_a_directory_recursively_and_merges_into_an_existing_destination() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(storage_dir.path(), "ociman-test/cp-dir:latest", true);
    let rootfs = container_rootfs(storage_dir.path(), &id);

    let host_src_dir = storage_dir.path().join("host_src_dir");
    std::fs::create_dir_all(host_src_dir.join("nested")).unwrap();
    std::fs::write(host_src_dir.join("a.txt"), "a").unwrap();
    std::fs::write(host_src_dir.join("nested/b.txt"), "b").unwrap();

    let to_container = ociman(
        storage_dir.path(),
        &[
            "cp",
            host_src_dir.to_str().unwrap(),
            &format!("{id}:/dir_in_container"),
        ],
    );
    assert!(
        to_container.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&to_container.stderr)
    );
    let container_dir = Path::new(&rootfs).join("dir_in_container");
    assert_eq!(
        std::fs::read_to_string(container_dir.join("a.txt")).unwrap(),
        "a"
    );
    assert_eq!(
        std::fs::read_to_string(container_dir.join("nested/b.txt")).unwrap(),
        "b"
    );

    // Copying again (dest already exists as a directory) merges
    // rather than erroring or nesting an extra level.
    std::fs::write(host_src_dir.join("c.txt"), "c").unwrap();
    let again = ociman(
        storage_dir.path(),
        &[
            "cp",
            host_src_dir.to_str().unwrap(),
            &format!("{id}:/dir_in_container"),
        ],
    );
    assert!(again.status.success());
    assert_eq!(
        std::fs::read_to_string(container_dir.join("c.txt")).unwrap(),
        "c"
    );
    assert_eq!(
        std::fs::read_to_string(container_dir.join("a.txt")).unwrap(),
        "a"
    );

    // And the reverse direction: container directory -> a fresh host directory.
    let host_dest_dir = storage_dir.path().join("host_dest_dir");
    let from_container = ociman(
        storage_dir.path(),
        &[
            "cp",
            &format!("{id}:/dir_in_container"),
            host_dest_dir.to_str().unwrap(),
        ],
    );
    assert!(
        from_container.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&from_container.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(host_dest_dir.join("a.txt")).unwrap(),
        "a"
    );
    assert_eq!(
        std::fs::read_to_string(host_dest_dir.join("c.txt")).unwrap(),
        "c"
    );
}

#[test]
fn cp_a_dotdot_component_in_the_container_path_is_a_clear_error() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id =
        seed_and_run_stopped_container(storage_dir.path(), "ociman-test/cp-dotdot:latest", true);

    let host_src = storage_dir.path().join("host_src.txt");
    std::fs::write(&host_src, "should never land anywhere").unwrap();
    let cp = ociman(
        storage_dir.path(),
        &["cp", host_src.to_str().unwrap(), &format!("{id}:../evil")],
    );
    assert!(!cp.status.success());
    assert!(String::from_utf8_lossy(&cp.stderr).contains(".."));
}

/// [`seed_and_run_stopped_container`] resolves the container it just
/// created via `ps -a -q`, which lists *every* container in
/// `storage_root` -- fine for every other test here (one container
/// per storage root at a time), but ambiguous the moment a *second*
/// container needs to coexist in the same store, as a real
/// container-to-container `cp` needs. This variant sidesteps that
/// entirely: `--name` gives each container its own real, stable
/// identifier up front, so no `ps` lookup (or its own inherent
/// "which one do you mean" ambiguity once more than one container
/// exists) is needed at all.
fn seed_and_run_named_stopped_container(storage_root: &Path, image: &str, name: &str) -> String {
    std::fs::write(storage_root.join(".rootless-overlay-supported"), "false").unwrap();
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
                "exit 0".to_string(),
            ]),
            ..Default::default()
        },
    );
    let run = ociman(storage_root, &["run", "--name", name, image]);
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    name.to_string()
}

/// Container-to-container `cp` (real `podman cp` supports this too;
/// see `docs/design/0151`) copies a real file directly from one
/// container's own storage into another's, with no host-side
/// intermediate step at all.
#[test]
fn cp_between_two_containers_copies_a_real_file() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id_a = seed_and_run_named_stopped_container(
        storage_dir.path(),
        "ociman-test/cp-c2c-a:latest",
        "cp-c2c-a",
    );
    let id_b = seed_and_run_named_stopped_container(
        storage_dir.path(),
        "ociman-test/cp-c2c-b:latest",
        "cp-c2c-b",
    );
    let rootfs_a = container_rootfs(storage_dir.path(), &id_a);
    let rootfs_b = container_rootfs(storage_dir.path(), &id_b);

    std::fs::write(Path::new(&rootfs_a).join("from-a.txt"), "hello from a").unwrap();

    let cp = ociman(
        storage_dir.path(),
        &[
            "cp",
            &format!("{id_a}:/from-a.txt"),
            &format!("{id_b}:/copied.txt"),
        ],
    );
    assert!(
        cp.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&cp.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(Path::new(&rootfs_b).join("copied.txt")).unwrap(),
        "hello from a"
    );
    // The source container's own copy is untouched.
    assert_eq!(
        std::fs::read_to_string(Path::new(&rootfs_a).join("from-a.txt")).unwrap(),
        "hello from a"
    );
}

/// Container-to-container `cp` against an unknown destination
/// container is a clear, real error (the source side resolves fine).
#[test]
fn cp_between_two_containers_an_unknown_destination_is_a_clear_error() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id =
        seed_and_run_stopped_container(storage_dir.path(), "ociman-test/cp-c2c-src:latest", true);

    let cp = ociman(
        storage_dir.path(),
        &["cp", &format!("{id}:/lib"), "does-not-exist:/lib2"],
    );
    assert!(!cp.status.success());
}

#[test]
fn cp_neither_side_naming_a_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let cp = ociman(
        storage_dir.path(),
        &["cp", "/etc/hostname", "/tmp/somewhere"],
    );
    assert!(!cp.status.success());
}

#[test]
fn cp_overwrite_flag_governs_a_real_directory_vs_non_directory_conflict() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id =
        seed_and_run_stopped_container(storage_dir.path(), "ociman-test/cp-overwrite:latest", true);
    let rootfs = container_rootfs(storage_dir.path(), &id);

    let conflict_path = Path::new(&rootfs).join("conflict");
    std::fs::write(&conflict_path, "i am a plain file").unwrap();

    let host_src_dir = storage_dir.path().join("host_src_dir");
    std::fs::create_dir_all(&host_src_dir).unwrap();
    std::fs::write(host_src_dir.join("a.txt"), "a").unwrap();

    let without_overwrite = ociman(
        storage_dir.path(),
        &[
            "cp",
            host_src_dir.to_str().unwrap(),
            &format!("{id}:/conflict"),
        ],
    );
    assert!(!without_overwrite.status.success());
    assert!(
        String::from_utf8_lossy(&without_overwrite.stderr).contains("--overwrite"),
        "stderr: {}",
        String::from_utf8_lossy(&without_overwrite.stderr)
    );
    // Untouched: still a plain file.
    assert!(std::fs::symlink_metadata(&conflict_path).unwrap().is_file());

    let with_overwrite = ociman(
        storage_dir.path(),
        &[
            "cp",
            host_src_dir.to_str().unwrap(),
            &format!("{id}:/conflict"),
            "--overwrite",
        ],
    );
    assert!(
        with_overwrite.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&with_overwrite.stderr)
    );
    assert!(conflict_path.is_dir());
    assert_eq!(
        std::fs::read_to_string(conflict_path.join("a.txt")).unwrap(),
        "a"
    );
}

#[test]
fn cp_is_a_clear_error_for_a_rootless_overlay_rootfs_container() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    // Deliberately does *not* force the marker -- see the module's own
    // doc comment for why this test still passes either way.
    let id =
        seed_and_run_stopped_container(storage_dir.path(), "ociman-test/cp-overlay:latest", false);

    let host_src = storage_dir.path().join("host_src.txt");
    std::fs::write(&host_src, "hello").unwrap();
    let cp = ociman(
        storage_dir.path(),
        &["cp", host_src.to_str().unwrap(), &format!("{id}:/x.txt")],
    );

    let bundle_dir = storage_dir.path().join("containers").join(&id);
    if bundle_dir.join("upper").exists() {
        // This host really does support the rootless-overlay
        // optimization -- `cp` must refuse it clearly.
        assert!(!cp.status.success());
        assert!(
            String::from_utf8_lossy(&cp.stderr).contains("rootless-overlay"),
            "stderr: {}",
            String::from_utf8_lossy(&cp.stderr)
        );
    } else {
        // This host doesn't support it either -- plain `Extract` was
        // used, so `cp` succeeds normally.
        assert!(
            cp.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&cp.stderr)
        );
    }
}

#[test]
fn cp_against_an_unknown_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let cp = ociman(
        storage_dir.path(),
        &["cp", "/etc/hostname", "does-not-exist:/x"],
    );
    assert!(!cp.status.success());
}
