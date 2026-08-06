//! `ociman port` (`docs/design/0535`): real `podman port`/`podman
//! container port`'s own port-mapping listing command. This project
//! has never implemented any port-publishing concept at all, so a
//! container here can genuinely never have a real mapping to report
//! -- matching real `podman port`'s own checked-directly-confirmed
//! behavior against a real installed `podman 4.9.3` container with no
//! mappings either: a silent, exit-`0` success for *every* case,
//! including an explicit, definitely-unmatched `PORT` argument (real
//! `ContainerPort`'s own "failed to find published port" check lives
//! inside a loop that a permanently-empty report list makes
//! unreachable) -- never assumed, verified directly against the real
//! binary before writing any of this.

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

fn seed(storage_root: &Path, image: &str, busybox: &Path) {
    seed_image(
        &Store::open(storage_root).unwrap(),
        image,
        busybox,
        &["sh", "true", "sleep"],
        ContainerConfig::default(),
    );
}

/// A genuinely `Running` container with no port mappings of its own
/// (this project has no way to ever create one) is a silent success,
/// matching a real installed `podman port` against an identical
/// fixture exactly.
#[test]
fn port_on_a_running_container_with_no_mappings_is_a_silent_success() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(
        storage_dir.path(),
        "ociman-test/port-running:latest",
        &busybox,
    );
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "-d",
            "--name",
            "port-running",
            "ociman-test/port-running:latest",
            "true",
        ],
    );
    assert!(run.status.success(), "{run:?}");

    let port = ociman(storage_dir.path(), &["port", "port-running"]);
    assert!(
        port.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&port.stderr)
    );
    assert!(String::from_utf8_lossy(&port.stdout).trim().is_empty());

    ociman(storage_dir.path(), &["kill", "port-running"]);
}

/// A real, checked-directly upstream quirk: an explicit, definitely-
/// unmatched `PORT` argument is *still* a silent success -- see this
/// file's own module doc comment for exactly why real podman itself
/// never actually reaches its own "failed to find published port"
/// error in this case.
#[test]
fn port_with_an_explicit_nonexistent_port_is_still_a_silent_success() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(
        storage_dir.path(),
        "ociman-test/port-nomatch:latest",
        &busybox,
    );
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "-d",
            "--name",
            "port-nomatch",
            "ociman-test/port-nomatch:latest",
            "true",
        ],
    );
    assert!(run.status.success(), "{run:?}");

    let port = ociman(storage_dir.path(), &["port", "port-nomatch", "80/tcp"]);
    assert!(
        port.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&port.stderr)
    );
    assert!(String::from_utf8_lossy(&port.stdout).trim().is_empty());

    ociman(storage_dir.path(), &["kill", "port-nomatch"]);
}

/// A malformed `PORT` argument is still a real, immediate error --
/// checked *before* ever reaching the always-empty search, matching
/// real podman's own identical `strconv.ParseUint`/slash-count checks
/// exactly.
#[test]
fn port_rejects_a_malformed_port_argument() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(
        storage_dir.path(),
        "ociman-test/port-malformed:latest",
        &busybox,
    );
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "-d",
            "--name",
            "port-malformed",
            "ociman-test/port-malformed:latest",
            "true",
        ],
    );
    assert!(run.status.success(), "{run:?}");

    let non_numeric = ociman(
        storage_dir.path(),
        &["port", "port-malformed", "not-a-number"],
    );
    assert!(!non_numeric.status.success());

    let too_many_slashes = ociman(
        storage_dir.path(),
        &["port", "port-malformed", "80/tcp/udp"],
    );
    assert!(!too_many_slashes.status.success());
    assert!(
        String::from_utf8_lossy(&too_many_slashes.stderr).contains("is invalid"),
        "{too_many_slashes:?}"
    );

    ociman(storage_dir.path(), &["kill", "port-malformed"]);
}

/// A `Created` (never-started) container is also a silent success --
/// matching real podman's own identical `state != ContainerStateRunning
/// { continue }` skip, checked directly against a real installed
/// `podman port` against an identical fixture.
#[test]
fn port_on_a_created_never_started_container_is_a_silent_success() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(
        storage_dir.path(),
        "ociman-test/port-created:latest",
        &busybox,
    );
    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/port-created:latest", "true"],
    );
    assert!(create.status.success());
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let port = ociman(storage_dir.path(), &["port", &id]);
    assert!(
        port.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&port.stderr)
    );
    assert!(String::from_utf8_lossy(&port.stdout).trim().is_empty());
}

/// Matches real podman's own exact wording (checked directly,
/// `port.go`'s own manual body check -- not the shared validator's
/// own differently-worded equivalent, which `port`'s real
/// `ignoreArgLen = true` skips).
#[test]
fn port_with_no_target_at_all_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let port = ociman(storage_dir.path(), &["port"]);
    assert!(!port.status.success());
    assert!(
        String::from_utf8_lossy(&port.stderr)
            .contains("you must supply a running container name or id")
    );
}

/// `--all` and `--latest` together is a real, immediate error --
/// matching real podman's own identical validation exactly (this
/// check runs unconditionally, even under `port`'s own `ignoreArgLen
/// = true`).
#[test]
fn port_all_and_latest_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let port = ociman(storage_dir.path(), &["port", "--all", "--latest"]);
    assert!(!port.status.success());
    assert!(
        String::from_utf8_lossy(&port.stderr)
            .contains("--all and --latest cannot be used together")
    );
}

/// `--all` combined with an explicit container is also a real,
/// immediate error -- matching real podman's own identical
/// validation exactly.
#[test]
fn port_all_with_an_explicit_id_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let port = ociman(storage_dir.path(), &["port", "--all", "some-id"]);
    assert!(!port.status.success());
    assert!(String::from_utf8_lossy(&port.stderr).contains("no arguments are needed with --all"));
}

/// More than two positional arguments is a real, immediate error,
/// matching real podman's own exact `` "`port` accepts at most 2
/// arguments" `` wording.
#[test]
fn port_with_more_than_two_positional_arguments_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let port = ociman(storage_dir.path(), &["port", "ctr", "80/tcp", "extra"]);
    assert!(!port.status.success());
    assert!(
        String::from_utf8_lossy(&port.stderr).contains("`port` accepts at most 2 arguments"),
        "{port:?}"
    );
}

/// An unknown container is a real, immediate error -- matching real
/// podman's own identical hard-error resolution (`getContainers`'s
/// own default explicit-name path, propagated directly, never
/// `ociman container cleanup`'s own separate silent-inversion
/// convention, `0529`).
#[test]
fn port_on_an_unknown_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let port = ociman(storage_dir.path(), &["port", "no-such-container"]);
    assert!(!port.status.success());
}

/// `--latest` on a genuinely empty container store is a real,
/// propagated hard error -- matching real podman's own identical
/// behavior, checked directly against a real installed `podman port
/// --latest` (unlike `ociman container cleanup --latest`'s own
/// separate, checked-directly *different* silent-success convention,
/// `0529`).
#[test]
fn port_latest_on_an_empty_store_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let port = ociman(storage_dir.path(), &["port", "--latest"]);
    assert!(!port.status.success());
}

/// `--all` on an empty container store is a silent success (an empty
/// sweep, matching real podman's own identical behavior).
#[test]
fn port_all_on_an_empty_store_is_a_silent_success() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let port = ociman(storage_dir.path(), &["port", "--all"]);
    assert!(port.status.success(), "{port:?}");
    assert!(String::from_utf8_lossy(&port.stdout).trim().is_empty());
}
