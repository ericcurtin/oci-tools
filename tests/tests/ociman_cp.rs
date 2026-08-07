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
use std::process::{Command, Stdio};

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

/// `--archive`/`-a` (`docs/design/0546`, default `true`): copying a
/// file into a container chowns it to the destination container's own
/// primary uid/gid, matching real `podman cp --archive` exactly. This
/// project's own `busybox` fixture image declares no `USER` at all,
/// so its default resolved user is `0:0` (root) -- copying in as an
/// unprivileged host user can never *observe* that chown actually
/// taking effect (`CAP_CHOWN` is required to chown to any uid other
/// than your own), but it must never fail the copy either: the
/// underlying `set_owner` primitive already tolerates `EPERM`
/// (matching `ociman build`'s own identical, already-tested `--chown`
/// tolerance). This is the "doesn't break the common, unprivileged
/// case" half of the coverage; see
/// `cp_archive_chowns_to_the_destination_containers_own_user_when_
/// privileged_enough` below for the real, observable-difference half,
/// which only runs as real root.
#[test]
fn cp_archive_default_never_fails_the_copy_even_when_the_chown_cannot_apply() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/cp-archive-default:latest",
        true,
    );

    let host_src = storage_dir.path().join("host_src.txt");
    std::fs::write(&host_src, "hello from host").unwrap();

    let cp = ociman(
        storage_dir.path(),
        &[
            "cp",
            host_src.to_str().unwrap(),
            &format!("{id}:/archived.txt"),
        ],
    );
    assert!(
        cp.status.success(),
        "--archive (the default) must never fail the copy just because the chown itself \
         couldn't apply: {}",
        String::from_utf8_lossy(&cp.stderr)
    );
}

/// `--archive=false`: the copied file keeps exactly the source's own
/// original ownership -- deterministic regardless of privilege, since
/// no chown is ever attempted at all. Uses the *calling test
/// process's own* real uid/gid (the same "guaranteed to succeed
/// either way" trick `ociman_build.rs`'s own
/// `copy_chown_is_reflected_in_the_committed_layers_own_tar_header`
/// already established): a freshly-created host file is always owned
/// by the process that created it, so this assertion holds whether
/// the test itself happens to run rootless or as real root.
#[test]
fn cp_archive_false_never_chowns_and_keeps_the_source_files_own_ownership() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/cp-archive-false:latest",
        true,
    );
    let rootfs = container_rootfs(storage_dir.path(), &id);

    let host_src = storage_dir.path().join("host_src.txt");
    std::fs::write(&host_src, "hello from host").unwrap();
    let my_uid = rustix::process::getuid().as_raw();
    let my_gid = rustix::process::getgid().as_raw();

    let cp = ociman(
        storage_dir.path(),
        &[
            "cp",
            "--archive=false",
            host_src.to_str().unwrap(),
            &format!("{id}:/noarchive.txt"),
        ],
    );
    assert!(
        cp.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&cp.stderr)
    );
    let copied = Path::new(&rootfs).join("noarchive.txt");
    let metadata = std::fs::metadata(&copied).unwrap();
    use std::os::unix::fs::MetadataExt as _;
    assert_eq!(metadata.uid(), my_uid, "--archive=false must never chown");
    assert_eq!(metadata.gid(), my_gid, "--archive=false must never chown");
}

/// The real, observable-difference half of `--archive`'s own coverage
/// (see `cp_archive_default_never_fails_the_copy_even_when_the_chown_
/// cannot_apply`'s own doc comment for the unprivileged half): only
/// real root has `CAP_CHOWN` enough to chown a file to a uid other
/// than its own, so this test explicitly chowns the source file to a
/// *different* uid first, then confirms `--archive` (the default)
/// really does override that with the destination container's own
/// resolved primary user (`0:0`, this project's own busybox fixture's
/// real default) -- matching real `podman cp --archive`'s own
/// documented behavior exactly (checked directly, `~/git/podman/cmd/
/// podman/containers/cp.go:60`). Skipped entirely when not running as
/// real root, the same convention `ociman_build.rs`'s own
/// `chown_to_a_different_uid_is_tolerated_not_fatal_when_unprivileged`
/// already established for the identical constraint.
#[test]
fn cp_archive_chowns_to_the_destination_containers_own_user_when_privileged_enough() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if rustix::process::getuid().as_raw() != 0 {
        eprintln!("skipping: not running as real root, --archive's chown cannot be observed");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/cp-archive-privileged:latest",
        true,
    );
    let rootfs = container_rootfs(storage_dir.path(), &id);

    let host_src = storage_dir.path().join("host_src.txt");
    std::fs::write(&host_src, "hello from host").unwrap();
    // A uid genuinely different from both 0 and whatever this test
    // process itself already owns the file as.
    rustix::fs::chown(&host_src, Some(rustix::fs::Uid::from_raw(1)), None).unwrap();

    let cp = ociman(
        storage_dir.path(),
        &[
            "cp",
            host_src.to_str().unwrap(),
            &format!("{id}:/archived.txt"),
        ],
    );
    assert!(
        cp.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&cp.stderr)
    );
    let copied = Path::new(&rootfs).join("archived.txt");
    let metadata = std::fs::metadata(&copied).unwrap();
    use std::os::unix::fs::MetadataExt as _;
    assert_eq!(
        metadata.uid(),
        0,
        "--archive should chown to the destination container's own primary uid (0, root)"
    );
    assert_eq!(
        metadata.gid(),
        0,
        "--archive should chown to the destination container's own primary gid (0, root)"
    );
}

fn tar_entry_names(bytes: &[u8]) -> Vec<String> {
    let mut archive = tar::Archive::new(bytes);
    let mut names: Vec<String> = archive
        .entries()
        .unwrap()
        .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// `-` as `DEST_PATH` (`docs/design/0553`): a single file source
/// streams to stdout as a real tar with exactly one entry named by
/// its own basename -- matching real `podman cp ctr:/etc/passwd -`'s
/// own checked-directly, live-verified shape exactly.
#[test]
fn cp_stdout_streams_a_single_file_tarred_under_its_own_basename() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/cp-stdout-file:latest",
        true,
    );
    let rootfs = container_rootfs(storage_dir.path(), &id);
    std::fs::write(Path::new(&rootfs).join("greeting.txt"), "hello stdout").unwrap();

    let cp = ociman(
        storage_dir.path(),
        &["cp", &format!("{id}:/greeting.txt"), "-"],
    );
    assert!(
        cp.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&cp.stderr)
    );
    assert_eq!(
        tar_entry_names(&cp.stdout),
        vec!["greeting.txt".to_string()]
    );

    let mut archive = tar::Archive::new(cp.stdout.as_slice());
    let mut entry = archive.entries().unwrap().next().unwrap().unwrap();
    let mut content = String::new();
    std::io::Read::read_to_string(&mut entry, &mut content).unwrap();
    assert_eq!(content, "hello stdout");
}

/// A directory source streams with its own basename as the top-level
/// entry, children nested underneath -- matching real `podman cp
/// ctr:/etc -`'s own checked-directly, live-verified shape.
#[test]
fn cp_stdout_streams_a_directory_with_its_own_basename_as_the_top_level_entry() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/cp-stdout-dir:latest",
        true,
    );
    let rootfs = container_rootfs(storage_dir.path(), &id);
    std::fs::create_dir_all(Path::new(&rootfs).join("mydir/sub")).unwrap();
    std::fs::write(Path::new(&rootfs).join("mydir/a.txt"), "a").unwrap();
    std::fs::write(Path::new(&rootfs).join("mydir/sub/b.txt"), "b").unwrap();

    let cp = ociman(storage_dir.path(), &["cp", &format!("{id}:/mydir"), "-"]);
    assert!(
        cp.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&cp.stderr)
    );
    assert_eq!(
        tar_entry_names(&cp.stdout),
        vec![
            "mydir".to_string(),
            "mydir/a.txt".to_string(),
            "mydir/sub".to_string(),
            "mydir/sub/b.txt".to_string(),
        ]
    );
}

/// `-` as `SRC_PATH` (`docs/design/0553`): a real tar piped on stdin
/// extracts correctly into an already-existing container directory --
/// matching real `podman cp - ctr:/existing-dir`'s own identical
/// stdin-streaming behavior.
#[test]
fn cp_stdin_extracts_a_tar_into_an_existing_container_directory() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id =
        seed_and_run_stopped_container(storage_dir.path(), "ociman-test/cp-stdin:latest", true);
    let rootfs = container_rootfs(storage_dir.path(), &id);
    std::fs::create_dir_all(Path::new(&rootfs).join("existing-dir")).unwrap();

    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let content = b"streamed via stdin";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "streamed.txt", &content[..])
            .unwrap();
        builder.finish().unwrap();
    }

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["cp", "-", &format!("{id}:/existing-dir")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ociman");
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), &tar_bytes).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        std::fs::read_to_string(Path::new(&rootfs).join("existing-dir/streamed.txt")).unwrap(),
        "streamed via stdin"
    );
}

/// A destination that doesn't already resolve to a real directory is
/// a clear, immediate error when streaming from stdin -- matching
/// real `podman cp - ctr:/dest`'s own exact, checked-directly wording
/// (`~/git/podman/cmd/podman/containers/cp.go:375-377`).
#[test]
fn cp_stdin_requires_the_destination_to_already_be_a_real_directory() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/cp-stdin-not-a-dir:latest",
        true,
    );

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["cp", "-", &format!("{id}:/does-not-exist-at-all")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ociman");
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), b"irrelevant").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("destination must be a directory"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Genuinely non-tar stdin input is a real, immediate error, not a
/// silent no-op or a confusing low-level panic.
#[test]
fn cp_stdin_of_non_tar_input_is_a_clear_error() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/cp-stdin-garbage:latest",
        true,
    );
    let rootfs = container_rootfs(storage_dir.path(), &id);
    std::fs::create_dir_all(Path::new(&rootfs).join("existing-dir2")).unwrap();

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["cp", "-", &format!("{id}:/existing-dir2")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ociman");
    std::io::Write::write_all(
        child.stdin.as_mut().unwrap(),
        b"this is definitely not a tar file, just plain garbage bytes padded out",
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
}
