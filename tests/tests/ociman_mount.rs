//! `ociman mount`/`ociman unmount` integration tests (`docs/design/
//! 0362`): a container's own real, already-directly-accessible root
//! filesystem path, and a real no-op respectively — for a running
//! container, a stopped one, an unknown one, and a rootless-overlay-
//! rootfs container being a clear error for `mount` (but never for
//! `unmount`, which has no such gap at all).
//!
//! Every test that needs a *plain*-rootfs container forces
//! `.rootless-overlay-supported` to `false` first (see
//! `ociman_diff.rs`'s own identical, already-established convention
//! and doc comment) — `mount_is_a_clear_error_for_a_rootless_overlay_
//! rootfs_container` below is the one test that deliberately leaves
//! it unset, written so it passes either way this host happens to
//! land.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

/// A real, already-stopped container running `shell_command`, the
/// same technique `ociman_diff.rs`'s own `seed_and_run_stopped_
/// container` already established (forces plain-`Extract` rootfs
/// setup deterministically unless `force_extract` is `false`).
fn seed_and_run_stopped_container(
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

/// Like [`seed_and_run_stopped_container`], but resolves the new
/// container's own id via an explicit `--name` + `ps --filter
/// name=...` instead of a bare `ps -a -q` — required the moment more
/// than one container ever shares the same store at once, since a
/// bare `ps -a -q` then prints one line *per* container, silently
/// corrupting a plain `.trim()`'s "exactly one clean id" assumption
/// (a real bug this module's own first multi-container test hit and
/// fixed before landing, see `docs/design/0470`/`0471`).
fn seed_and_run_stopped_container_named(
    storage_root: &Path,
    image: &str,
    name: &str,
    force_extract: bool,
) -> String {
    if force_extract {
        std::fs::write(storage_root.join(".rootless-overlay-supported"), "false").unwrap();
    }
    let busybox = busybox_path().expect("busybox not found on $PATH");
    let store = Store::open(storage_root).unwrap();
    seed_image(&store, image, &busybox, &["sh"], ContainerConfig::default());
    let run = ociman(
        storage_root,
        &["run", "--name", name, image, "sh", "-c", "exit 0"],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let ps = ociman(
        storage_root,
        &["ps", "-a", "-q", "--filter", &format!("name={name}")],
    );
    let id = String::from_utf8_lossy(&ps.stdout).trim().to_string();
    assert!(!id.is_empty(), "{ps:?}");
    id
}

#[test]
fn mount_prints_the_real_rootfs_path_of_a_stopped_container() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/mount-stopped:latest",
        "exit 0",
        true,
    );

    let mount = ociman(storage_dir.path(), &["mount", &id]);
    assert!(
        mount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&mount.stderr)
    );
    let printed = String::from_utf8_lossy(&mount.stdout).trim().to_string();
    let expected = storage_dir
        .path()
        .join("containers")
        .join(&id)
        .join("rootfs");
    assert_eq!(std::path::PathBuf::from(&printed), expected, "{mount:?}");
    assert!(
        Path::new(&printed).is_dir(),
        "the printed path should be a real, already-existing directory"
    );
}

#[test]
fn mount_works_on_a_genuinely_running_container_too() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        storage_dir.path().join(".rootless-overlay-supported"),
        "false",
    )
    .unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/mount-running:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let run = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "-d",
            "ociman-test/mount-running:latest",
            "sleep",
            "30",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn ociman run -d");
    assert!(run.status.success(), "{run:?}");
    let id = String::from_utf8_lossy(&ociman(storage_dir.path(), &["ps", "-a", "-q"]).stdout)
        .trim()
        .to_string();
    assert!(!id.is_empty());

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let inspect = ociman(storage_dir.path(), &["inspect", &id, "--json"]);
        let json: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
        if json["status"] == "running" || Instant::now() >= deadline {
            assert_eq!(json["status"], "running", "{json:?}");
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let mount = ociman(storage_dir.path(), &["mount", &id]);
    assert!(mount.status.success(), "{mount:?}");
    let printed = String::from_utf8_lossy(&mount.stdout).trim().to_string();
    assert!(Path::new(&printed).is_dir());

    let _ = ociman(storage_dir.path(), &["kill", &id]);
}

/// A real no-op: the container's own rootfs is fully intact
/// afterward, and `unmount` prints the container's own id, matching a
/// real installed `podman unmount`'s own checked-directly output.
#[test]
fn unmount_is_a_real_no_op_that_prints_the_container_id() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/unmount-stopped:latest",
        "exit 0",
        true,
    );
    let rootfs = storage_dir
        .path()
        .join("containers")
        .join(&id)
        .join("rootfs");

    let unmount = ociman(storage_dir.path(), &["unmount", &id]);
    assert!(
        unmount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unmount.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&unmount.stdout).trim(), id);
    assert!(
        rootfs.is_dir(),
        "the container's own rootfs must survive unmount untouched"
    );
}

#[test]
fn mount_and_unmount_against_an_unknown_container_are_clear_errors() {
    let storage_dir = tempfile::tempdir().unwrap();

    let mount = ociman(storage_dir.path(), &["mount", "does-not-exist"]);
    assert!(!mount.status.success());

    let unmount = ociman(storage_dir.path(), &["unmount", "does-not-exist"]);
    assert!(!unmount.status.success());
}

/// Unlike `unmount` (never affected at all), `mount` shares `cp`/
/// `diff`/`export`/`commit`'s own real, checked-directly rootless-
/// overlay-rootfs gap (`docs/design/0146`) — a clear error, not a
/// silently wrong path.
#[test]
fn mount_is_a_clear_error_for_a_rootless_overlay_rootfs_container_but_unmount_still_succeeds() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    // Deliberately does *not* force the marker -- see the module's
    // own doc comment for why this test still passes either way.
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/mount-overlay:latest",
        "exit 0",
        false,
    );

    let mount = ociman(storage_dir.path(), &["mount", &id]);
    let unmount = ociman(storage_dir.path(), &["unmount", &id]);

    let bundle_dir = storage_dir.path().join("containers").join(&id);
    if bundle_dir.join("upper").exists() {
        // This host really does support the rootless-overlay
        // optimization -- `mount` must refuse it clearly.
        assert!(!mount.status.success());
        assert!(
            String::from_utf8_lossy(&mount.stderr).contains("rootless-overlay"),
            "stderr: {}",
            String::from_utf8_lossy(&mount.stderr)
        );
    } else {
        // This host doesn't support it either -- plain `Extract` was
        // used, so `mount` succeeds normally.
        assert!(
            mount.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&mount.stderr)
        );
    }
    // `unmount` never has this gap at all, regardless of which branch
    // above actually ran on this host.
    assert!(
        unmount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unmount.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&unmount.stdout).trim(), id);
}

/// A real, previously-deferred gap now closed (see `Command::Mount`'s
/// own doc comment): `ociman mount` with no `CONTAINER` at all lists
/// every currently-mounted container, matching real `podman mount`'s
/// own identical bare-invocation mode exactly -- `<12-char-id>\t
/// <rootfs-path>` per line, no header, sorted by creation time
/// ascending.
#[test]
fn mount_with_no_container_lists_every_container_sorted_by_creation_time() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        storage_dir.path().join(".rootless-overlay-supported"),
        "false",
    )
    .unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/mount-list:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    // Named explicitly (rather than reusing `seed_and_run_stopped_
    // container`'s own bare `ps -a -q`, which only ever disambiguates
    // correctly when exactly one container exists in the store at a
    // time) so each one's own id can be resolved unambiguously with
    // both already present in the same store at once.
    let run_named = |name: &str| {
        let run = ociman(
            storage_dir.path(),
            &[
                "run",
                "--name",
                name,
                "ociman-test/mount-list:latest",
                "sh",
                "-c",
                "exit 0",
            ],
        );
        assert!(run.status.success(), "{run:?}");
        let ps = ociman(
            storage_dir.path(),
            &["ps", "-a", "-q", "--filter", &format!("name={name}")],
        );
        let id = String::from_utf8_lossy(&ps.stdout).trim().to_string();
        assert!(!id.is_empty(), "{ps:?}");
        id
    };

    let first = run_named("mount-list-first");
    // A real, wall-clock timestamp gap so the two containers' own
    // `created` values are unambiguously ordered -- `created`'s own
    // whole-second-only precision (`format_rfc3339_utc`) needs more
    // than the usual 20ms poll interval, matching `ociman_ps.rs`'s
    // own identical, already-established 1200ms convention for this
    // exact reason.
    std::thread::sleep(Duration::from_millis(1200));
    let second = run_named("mount-list-second");

    let mount = ociman(storage_dir.path(), &["mount"]);
    assert!(
        mount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&mount.stderr)
    );
    let stdout = String::from_utf8_lossy(&mount.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "stdout: {stdout:?}");

    let first_rootfs = storage_dir
        .path()
        .join("containers")
        .join(&first)
        .join("rootfs");
    let second_rootfs = storage_dir
        .path()
        .join("containers")
        .join(&second)
        .join("rootfs");
    assert_eq!(
        lines[0],
        format!("{}\t{}", &first[..12], first_rootfs.display())
    );
    assert_eq!(
        lines[1],
        format!("{}\t{}", &second[..12], second_rootfs.display())
    );
}

/// A real, honest empty listing, not an error -- matching real
/// `podman mount`'s own identical behavior when nothing is mounted.
#[test]
fn mount_with_no_container_and_no_containers_at_all_prints_nothing() {
    let storage_dir = tempfile::tempdir().unwrap();
    let mount = ociman(storage_dir.path(), &["mount"]);
    assert!(
        mount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&mount.stderr)
    );
    assert!(mount.stdout.is_empty(), "stdout: {mount:?}");
}

/// The one real, rootless-overlay-rootfs container this project can't
/// resolve a plain root path for is silently skipped from the bare
/// listing rather than aborting it outright -- see `Command::Mount`'s
/// own doc comment for why. Written to pass either way this host
/// happens to support that optimization.
#[test]
fn mount_with_no_container_silently_skips_a_rootless_overlay_rootfs_container() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    // Deliberately does *not* force the marker -- see the module's
    // own doc comment for why this test still passes either way.
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/mount-list-overlay:latest",
        "exit 0",
        false,
    );

    let mount = ociman(storage_dir.path(), &["mount"]);
    assert!(
        mount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&mount.stderr)
    );
    let stdout = String::from_utf8_lossy(&mount.stdout);
    let bundle_dir = storage_dir.path().join("containers").join(&id);
    if bundle_dir.join("upper").exists() {
        // This host really does support the rootless-overlay
        // optimization -- the container is silently absent from the
        // listing, not an error.
        assert!(stdout.trim().is_empty(), "stdout: {stdout:?}");
    } else {
        // This host doesn't support it either -- plain `Extract` was
        // used, so the container appears normally.
        assert!(
            stdout.starts_with(&id[..12]),
            "stdout: {stdout:?}, id: {id}"
        );
    }
}

/// `ociman unmount CONTAINER CONTAINER...` (multiple explicit
/// targets) — matching real `podman unmount`'s own multi-id support
/// exactly. Each container's own id is printed, in the same order
/// given.
#[test]
fn unmount_accepts_multiple_explicit_containers() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let first = seed_and_run_stopped_container_named(
        storage_dir.path(),
        "ociman-test/unmount-multi-first:latest",
        "unmount-multi-first",
        true,
    );
    let second = seed_and_run_stopped_container_named(
        storage_dir.path(),
        "ociman-test/unmount-multi-second:latest",
        "unmount-multi-second",
        true,
    );

    let unmount = ociman(storage_dir.path(), &["unmount", &first, &second]);
    assert!(
        unmount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unmount.stderr)
    );
    let stdout = String::from_utf8_lossy(&unmount.stdout);
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        vec![first.as_str(), second.as_str()]
    );
}

/// A real, two-phase resolution: every explicit target is resolved
/// before anything is printed, so one unknown container among several
/// aborts the whole call rather than partially succeeding — matching
/// `cmd_kill`'s own already-established multi-id convention.
#[test]
fn unmount_with_one_unknown_container_among_several_aborts_before_printing_any() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let first = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/unmount-abort:latest",
        "exit 0",
        true,
    );

    let unmount = ociman(storage_dir.path(), &["unmount", &first, "does-not-exist"]);
    assert!(!unmount.status.success());
    assert!(
        unmount.stdout.is_empty(),
        "stdout: {}",
        String::from_utf8_lossy(&unmount.stdout)
    );
}

/// `ociman unmount --all`/`-a` — matching real `podman unmount --all`
/// exactly for this project's own honest "every container is always
/// already mounted" model: an unconditional sweep, every existing
/// container's own id printed.
#[test]
fn unmount_all_prints_every_containers_own_id() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let first = seed_and_run_stopped_container_named(
        storage_dir.path(),
        "ociman-test/unmount-all-first:latest",
        "unmount-all-first",
        true,
    );
    let second = seed_and_run_stopped_container_named(
        storage_dir.path(),
        "ociman-test/unmount-all-second:latest",
        "unmount-all-second",
        true,
    );

    let unmount = ociman(storage_dir.path(), &["unmount", "--all"]);
    assert!(
        unmount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unmount.stderr)
    );
    let stdout = String::from_utf8_lossy(&unmount.stdout);
    let mut lines: Vec<&str> = stdout.lines().collect();
    lines.sort();
    let mut expected = vec![first.as_str(), second.as_str()];
    expected.sort();
    assert_eq!(lines, expected);
}

/// `ociman unmount --latest`/`-l` — matching real `podman unmount
/// --latest` exactly.
#[test]
fn unmount_latest_targets_the_most_recently_created_container() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let _first = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/unmount-latest-first:latest",
        "exit 0",
        true,
    );
    // A real, wall-clock timestamp gap so the two containers' own
    // `created` values are unambiguously ordered -- see `ociman_
    // mount.rs`'s own `mount_with_no_container_lists_every_
    // container_sorted_by_creation_time`'s identical doc comment for
    // why 1200ms specifically.
    std::thread::sleep(Duration::from_millis(1200));
    let second = seed_and_run_stopped_container_named(
        storage_dir.path(),
        "ociman-test/unmount-latest-second:latest",
        "unmount-latest-second",
        true,
    );

    let unmount = ociman(storage_dir.path(), &["unmount", "--latest"]);
    assert!(
        unmount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unmount.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&unmount.stdout).trim(), second);
}

/// Matches real podman's own exact wording and check order, checked
/// directly (`~/git/podman/cmd/podman/validate/args.go`'s own
/// `CheckAllLatestAndIDFile`).
#[test]
fn unmount_all_and_latest_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let unmount = ociman(storage_dir.path(), &["unmount", "--all", "--latest"]);
    assert!(!unmount.status.success());
    assert!(
        String::from_utf8_lossy(&unmount.stderr)
            .contains("--all and --latest cannot be used together"),
        "{}",
        String::from_utf8_lossy(&unmount.stderr)
    );
}

#[test]
fn unmount_all_with_an_explicit_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let unmount = ociman(storage_dir.path(), &["unmount", "--all", "some-container"]);
    assert!(!unmount.status.success());
    assert!(
        String::from_utf8_lossy(&unmount.stderr).contains("no arguments are needed with --all"),
        "{}",
        String::from_utf8_lossy(&unmount.stderr)
    );
}

#[test]
fn unmount_latest_with_an_explicit_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let unmount = ociman(
        storage_dir.path(),
        &["unmount", "--latest", "some-container"],
    );
    assert!(!unmount.status.success());
    assert!(
        String::from_utf8_lossy(&unmount.stderr)
            .contains("--latest and containers cannot be used together"),
        "{}",
        String::from_utf8_lossy(&unmount.stderr)
    );
}

#[test]
fn unmount_with_nothing_given_at_all_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let unmount = ociman(storage_dir.path(), &["unmount"]);
    assert!(!unmount.status.success());
    assert!(
        String::from_utf8_lossy(&unmount.stderr)
            .contains("you must provide at least one name or id"),
        "{}",
        String::from_utf8_lossy(&unmount.stderr)
    );
}

/// `ociman mount CONTAINER CONTAINER...` (multiple explicit targets)
/// — matching real `podman mount`'s own multi-id support exactly.
/// Each container's own real root path is printed (never the
/// `<id>\t<path>` bare-mode table), in the same order given.
#[test]
fn mount_accepts_multiple_explicit_containers() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let first = seed_and_run_stopped_container_named(
        storage_dir.path(),
        "ociman-test/mount-multi-first:latest",
        "mount-multi-first",
        true,
    );
    let second = seed_and_run_stopped_container_named(
        storage_dir.path(),
        "ociman-test/mount-multi-second:latest",
        "mount-multi-second",
        true,
    );

    let mount = ociman(storage_dir.path(), &["mount", &first, &second]);
    assert!(
        mount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&mount.stderr)
    );
    let stdout = String::from_utf8_lossy(&mount.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    let first_rootfs = storage_dir
        .path()
        .join("containers")
        .join(&first)
        .join("rootfs");
    let second_rootfs = storage_dir
        .path()
        .join("containers")
        .join(&second)
        .join("rootfs");
    assert_eq!(
        lines,
        vec![
            first_rootfs.display().to_string(),
            second_rootfs.display().to_string()
        ]
    );
}

/// A real, two-phase resolution: every explicit target is resolved
/// before anything is printed, so one unknown container among several
/// aborts the whole call rather than partially succeeding — matching
/// `cmd_unmount`'s own identical multi-id convention (`0471`).
#[test]
fn mount_with_one_unknown_container_among_several_aborts_before_printing_any() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let first = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/mount-abort:latest",
        "exit 0",
        true,
    );

    let mount = ociman(storage_dir.path(), &["mount", &first, "does-not-exist"]);
    assert!(!mount.status.success());
    assert!(
        mount.stdout.is_empty(),
        "stdout: {}",
        String::from_utf8_lossy(&mount.stdout)
    );
}

/// `ociman mount --all`/`-a` — matching real `podman mount --all`
/// exactly: each container's own real root path is printed (never the
/// bare-mode table), continuing past the one real rootless-overlay-
/// rootfs gap container instead of aborting the whole sweep.
#[test]
fn mount_all_prints_every_containers_own_root_path() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let first = seed_and_run_stopped_container_named(
        storage_dir.path(),
        "ociman-test/mount-all-first:latest",
        "mount-all-first",
        true,
    );
    let second = seed_and_run_stopped_container_named(
        storage_dir.path(),
        "ociman-test/mount-all-second:latest",
        "mount-all-second",
        true,
    );

    let mount = ociman(storage_dir.path(), &["mount", "--all"]);
    assert!(
        mount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&mount.stderr)
    );
    let stdout = String::from_utf8_lossy(&mount.stdout);
    let mut lines: Vec<&str> = stdout.lines().collect();
    lines.sort();
    let first_rootfs = storage_dir
        .path()
        .join("containers")
        .join(&first)
        .join("rootfs")
        .display()
        .to_string();
    let second_rootfs = storage_dir
        .path()
        .join("containers")
        .join(&second)
        .join("rootfs")
        .display()
        .to_string();
    let mut expected = vec![first_rootfs.as_str(), second_rootfs.as_str()];
    expected.sort();
    assert_eq!(lines, expected);
}

/// `ociman mount --latest`/`-l` — matching real `podman mount
/// --latest` exactly.
#[test]
fn mount_latest_targets_the_most_recently_created_container() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let _first = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/mount-latest-first:latest",
        "exit 0",
        true,
    );
    std::thread::sleep(Duration::from_millis(1200));
    let second = seed_and_run_stopped_container_named(
        storage_dir.path(),
        "ociman-test/mount-latest-second:latest",
        "mount-latest-second",
        true,
    );

    let mount = ociman(storage_dir.path(), &["mount", "--latest"]);
    assert!(
        mount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&mount.stderr)
    );
    let printed = String::from_utf8_lossy(&mount.stdout).trim().to_string();
    let expected = storage_dir
        .path()
        .join("containers")
        .join(&second)
        .join("rootfs");
    assert_eq!(std::path::PathBuf::from(&printed), expected);
}

#[test]
fn mount_all_and_latest_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let mount = ociman(storage_dir.path(), &["mount", "--all", "--latest"]);
    assert!(!mount.status.success());
    assert!(
        String::from_utf8_lossy(&mount.stderr)
            .contains("--all and --latest cannot be used together"),
        "{}",
        String::from_utf8_lossy(&mount.stderr)
    );
}

#[test]
fn mount_all_with_an_explicit_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let mount = ociman(storage_dir.path(), &["mount", "--all", "some-container"]);
    assert!(!mount.status.success());
    assert!(
        String::from_utf8_lossy(&mount.stderr).contains("no arguments are needed with --all"),
        "{}",
        String::from_utf8_lossy(&mount.stderr)
    );
}

#[test]
fn mount_latest_with_an_explicit_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let mount = ociman(storage_dir.path(), &["mount", "--latest", "some-container"]);
    assert!(!mount.status.success());
    assert!(
        String::from_utf8_lossy(&mount.stderr)
            .contains("--latest and containers cannot be used together"),
        "{}",
        String::from_utf8_lossy(&mount.stderr)
    );
}
