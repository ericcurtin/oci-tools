//! `ociman ps`/`rm`/`run --rm` integration tests: the persistent
//! container tracking `ociman run` (0020) gained on top of its
//! previously ephemeral-only model (`docs/design/0021`). Same fully
//! offline approach as `ociman_run.rs` (a synthetic-but-structurally-
//! real seeded image, no registry access needed).

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
fn run_persists_a_container_ps_and_rm_can_see_and_remove() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-basic:latest",
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

    // No containers at all before `run`.
    let ps_before = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(ps_before.status.success());
    assert!(String::from_utf8_lossy(&ps_before.stdout).trim().is_empty());

    let run = ociman(storage_dir.path(), &["run", "ociman-test/ps-basic:latest"]);
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    // `ps` (running only) shows nothing: the container already exited
    // by the time the foreground `run` above returned.
    let ps_running_only = ociman(storage_dir.path(), &["ps", "-q"]);
    assert!(ps_running_only.status.success());
    assert!(
        String::from_utf8_lossy(&ps_running_only.stdout)
            .trim()
            .is_empty()
    );

    // `ps -a` shows the stopped container.
    let ps_all = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(ps_all.status.success());
    let id = String::from_utf8_lossy(&ps_all.stdout).trim().to_string();
    assert!(!id.is_empty(), "expected exactly one container id");

    let ps_json = ociman(storage_dir.path(), &["ps", "-a", "--json"]);
    assert!(ps_json.status.success());
    let views: serde_json::Value = serde_json::from_slice(&ps_json.stdout).unwrap();
    let entry = &views[0];
    assert_eq!(entry["id"], id);
    assert_eq!(entry["image"], "docker.io/ociman-test/ps-basic:latest");
    assert_eq!(entry["status"], "stopped");
    assert_eq!(entry["exit_code"], 0);

    // `rm` removes it; `ps -a` is empty again afterward.
    let rm = ociman(storage_dir.path(), &["rm", &id]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    let ps_after_rm = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(
        String::from_utf8_lossy(&ps_after_rm.stdout)
            .trim()
            .is_empty()
    );
}

#[test]
fn run_rm_flag_removes_the_container_automatically() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/auto-rm:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 3".to_string(),
            ]),
            ..Default::default()
        },
    );

    let run = ociman(
        storage_dir.path(),
        &["run", "--rm", "ociman-test/auto-rm:latest"],
    );
    assert_eq!(run.status.code(), Some(3));

    let ps_all = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(
        String::from_utf8_lossy(&ps_all.stdout).trim().is_empty(),
        "expected --rm to remove the container's record"
    );
}

#[test]
fn rm_without_force_refuses_to_remove_a_container_still_marked_running() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/refuse-rm:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    // Seed a bare "created" (never-run) record directly via the same
    // state store `ociman` itself would open, rather than running a
    // real long-lived container — this test only needs a record whose
    // `effective_status` isn't `Stopped` yet, and a `create`d-but-
    // never-`run` one is the simplest way to get exactly that.
    let containers_root = storage_dir.path().join("containers");
    let containers = oci_runtime_core::StateStore::open(&containers_root).unwrap();
    containers
        .create(
            "still-creating",
            Path::new("/bundle"),
            Path::new("/bundle/rootfs"),
            Default::default(),
        )
        .unwrap();

    let refused = ociman(storage_dir.path(), &["rm", "still-creating"]);
    assert!(
        !refused.status.success(),
        "rm without --force should refuse a non-stopped container"
    );

    let forced = ociman(storage_dir.path(), &["rm", "--force", "still-creating"]);
    assert!(
        forced.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&forced.stderr)
    );
}

#[test]
fn rm_of_a_nonexistent_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["rm", "does-not-exist"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not exist"));
}

/// `ociman rm --all` (`docs/design/0266`): removes every real, stopped
/// container in one call, matching real `podman rm --all` exactly.
#[test]
fn rm_all_removes_every_stopped_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rm-all:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    for _ in 0..2 {
        let run = ociman(
            storage_dir.path(),
            &["run", "ociman-test/rm-all:latest", "true"],
        );
        assert!(run.status.success(), "{run:?}");
    }
    let ps_all = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert_eq!(
        String::from_utf8_lossy(&ps_all.stdout)
            .trim()
            .lines()
            .count(),
        2,
        "expected exactly two real stopped containers before --all"
    );

    let rm_all = ociman(storage_dir.path(), &["rm", "--all"]);
    assert!(
        rm_all.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm_all.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&rm_all.stdout)
            .trim()
            .lines()
            .count(),
        2,
        "each removed container's own id should be printed: {rm_all:?}"
    );

    let ps_after = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(
        String::from_utf8_lossy(&ps_after.stdout).trim().is_empty(),
        "every container should be gone after --all"
    );

    // A real, silent no-op on an already-empty store, matching this
    // project's own established "empty is a valid, unremarkable
    // state" convention (`ocibox rm --all`'s own identical rule).
    let rm_all_again = ociman(storage_dir.path(), &["rm", "--all"]);
    assert!(rm_all_again.status.success());
    assert!(
        String::from_utf8_lossy(&rm_all_again.stdout)
            .trim()
            .is_empty()
    );
}

/// `ociman rm id1 id2` removes multiple explicit containers in one
/// call, matching real `podman rm id1 id2` exactly.
#[test]
fn rm_accepts_multiple_explicit_ids_and_removes_them_all() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rm-multi:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    let run1 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "multi-1",
            "ociman-test/rm-multi:latest",
            "true",
        ],
    );
    assert!(run1.status.success(), "{run1:?}");
    let run2 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "multi-2",
            "ociman-test/rm-multi:latest",
            "true",
        ],
    );
    assert!(run2.status.success(), "{run2:?}");

    let rm = ociman(storage_dir.path(), &["rm", "multi-1", "multi-2"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&rm.stdout).trim().lines().count(),
        2,
        "each removed container's own id should be printed: {rm:?}"
    );

    let ps_after = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(String::from_utf8_lossy(&ps_after.stdout).trim().is_empty());
}

/// A single unresolvable name among otherwise-valid ones aborts the
/// *whole* call before anything is removed — checked directly against
/// real `podman rm id1 nonexistent id2`: neither `id1` nor `id2` gets
/// removed either, unlike `--all`'s own continue-past-failure policy.
#[test]
fn rm_with_one_unresolvable_id_among_valid_ones_removes_nothing() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rm-multi-bogus:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    let run1 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "valid-1",
            "ociman-test/rm-multi-bogus:latest",
            "true",
        ],
    );
    assert!(run1.status.success(), "{run1:?}");
    let run2 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "valid-2",
            "ociman-test/rm-multi-bogus:latest",
            "true",
        ],
    );
    assert!(run2.status.success(), "{run2:?}");

    let rm = ociman(
        storage_dir.path(),
        &["rm", "valid-1", "does-not-exist-xyz", "valid-2"],
    );
    assert!(
        !rm.status.success(),
        "an unresolvable name in the list should fail the whole call"
    );
    assert!(String::from_utf8_lossy(&rm.stderr).contains("does not exist"));

    // Neither valid container was removed.
    let ps_after = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert_eq!(
        String::from_utf8_lossy(&ps_after.stdout)
            .trim()
            .lines()
            .count(),
        2,
        "both valid containers should still be present: {ps_after:?}"
    );
}

/// Once every name has resolved, a *different* per-container failure
/// (still running, no `--force`) does NOT block removing the other
/// already-resolved targets — checked directly against real `podman
/// rm a b c` where `b` is running without `--force`: `a` and `c` are
/// still removed, only `b` is refused. A different policy than the
/// unresolvable-name case above, matching `--all`'s own behavior.
#[test]
fn rm_with_one_non_stopped_id_among_valid_ones_still_removes_the_rest() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rm-multi-running:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    let run1 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "stopped-a",
            "ociman-test/rm-multi-running:latest",
            "true",
        ],
    );
    assert!(run1.status.success(), "{run1:?}");
    let run2 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "stopped-c",
            "ociman-test/rm-multi-running:latest",
            "true",
        ],
    );
    assert!(run2.status.success(), "{run2:?}");

    // A bare "created" (never-run) record standing in for a running
    // container, the same technique used by the `--all` tests above.
    let containers_root = storage_dir.path().join("containers");
    let containers = oci_runtime_core::StateStore::open(&containers_root).unwrap();
    containers
        .create(
            "running-b",
            Path::new("/bundle"),
            Path::new("/bundle/rootfs"),
            Default::default(),
        )
        .unwrap();

    let rm = ociman(
        storage_dir.path(),
        &["rm", "stopped-a", "running-b", "stopped-c"],
    );
    assert!(
        !rm.status.success(),
        "running-b's own failure should still surface"
    );

    let ps_after = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    let remaining = String::from_utf8_lossy(&ps_after.stdout);
    assert_eq!(
        remaining.trim().lines().count(),
        1,
        "only running-b should remain: {remaining:?}"
    );

    let forced = ociman(storage_dir.path(), &["rm", "--force", "running-b"]);
    assert!(forced.status.success(), "{forced:?}");
}

/// `--all` and an explicit ID together is a clear error, never an
/// ambiguous silent choice between the two (matching this project's
/// own `ocibox rm --all`'s own identical rule).
#[test]
fn rm_all_and_an_explicit_id_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(storage_dir.path(), &["rm", "--all", "some-id"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot give both"),
        "{out:?}"
    );
}

/// `rm --all` without `--force` still refuses a non-stopped container
/// (real `podman rm --all` alone, without `--force`, leaves a running
/// container untouched too) — but every *other* container is still
/// attempted, matching real `podman rm`'s own multi-target behavior
/// and this project's own `ocibox rm --all`'s identical policy.
#[test]
fn rm_all_without_force_skips_a_non_stopped_container_but_still_removes_the_rest() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rm-all-mixed:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/rm-all-mixed:latest", "true"],
    );
    assert!(run.status.success(), "{run:?}");

    // A bare "created" (never-run) record, the same
    // `rm_without_force_refuses_to_remove_a_container_still_marked_running`
    // technique above: `effective_status` isn't `Stopped`, so `--all`
    // without `--force` must skip it rather than fail outright.
    let containers_root = storage_dir.path().join("containers");
    let containers = oci_runtime_core::StateStore::open(&containers_root).unwrap();
    containers
        .create(
            "still-creating-2",
            Path::new("/bundle"),
            Path::new("/bundle/rootfs"),
            Default::default(),
        )
        .unwrap();

    let rm_all = ociman(storage_dir.path(), &["rm", "--all"]);
    assert!(
        !rm_all.status.success(),
        "the one non-stopped container's own failure should still surface"
    );

    // The real, stopped container is gone; the non-stopped one
    // survives untouched.
    let ps_after = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    let remaining = String::from_utf8_lossy(&ps_after.stdout);
    assert_eq!(
        remaining.trim().lines().count(),
        1,
        "the stopped container should be gone, the non-stopped one left: {remaining:?}"
    );

    let forced = ociman(storage_dir.path(), &["rm", "--all", "--force"]);
    assert!(forced.status.success(), "{forced:?}");
    let ps_final = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(String::from_utf8_lossy(&ps_final.stdout).trim().is_empty());
}

/// `ociman ps --filter status=created` (0272), given *without* `-a`,
/// still shows a `created` (never-started) container — real `podman
/// ps --filter status=` (checked directly) overrides the default
/// running-only filter entirely, exactly like this.
#[test]
fn ps_filter_status_created_shows_a_never_started_container_without_all() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-filter-status:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "created-only",
            "ociman-test/ps-filter-status:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    // A plain `ps` (no `-a`, no filter) hides it, matching the
    // existing established default.
    let plain = ociman(storage_dir.path(), &["ps", "-q"]);
    assert!(String::from_utf8_lossy(&plain.stdout).trim().is_empty());

    // `--filter status=created` alone (still no `-a`) shows it.
    let filtered = ociman(
        storage_dir.path(),
        &["ps", "--filter", "status=created", "-q"],
    );
    assert!(filtered.status.success(), "{filtered:?}");
    assert_eq!(
        String::from_utf8_lossy(&filtered.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "{filtered:?}"
    );

    // `--filter status=running` finds nothing (it never started).
    let no_match = ociman(
        storage_dir.path(),
        &["ps", "--filter", "status=running", "-q"],
    );
    assert!(no_match.status.success());
    assert!(String::from_utf8_lossy(&no_match.stdout).trim().is_empty());
}

/// Multiple `--filter status=` values are OR'd together, matching
/// real `podman ps --filter status=` exactly (checked directly).
#[test]
fn ps_filter_status_multiple_values_are_ored_together() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-filter-or:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "created-c",
            "ociman-test/ps-filter-or:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "stopped-c",
            "ociman-test/ps-filter-or:latest",
            "true",
        ],
    );
    assert!(run.status.success(), "{run:?}");

    let both = ociman(
        storage_dir.path(),
        &[
            "ps",
            "--filter",
            "status=created",
            "--filter",
            "status=stopped",
            "-q",
        ],
    );
    assert!(both.status.success(), "{both:?}");
    assert_eq!(
        String::from_utf8_lossy(&both.stdout).trim().lines().count(),
        2,
        "{both:?}"
    );
}

/// An unrecognized `--filter` key, or an unrecognized `status=` value,
/// is a clear, immediate error rather than a silently-ignored no-op.
#[test]
fn ps_filter_with_an_unrecognized_key_or_value_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let bad_key = ociman(storage_dir.path(), &["ps", "--filter", "label=foo"]);
    assert!(!bad_key.status.success());
    assert!(
        String::from_utf8_lossy(&bad_key.stderr).contains("not yet supported"),
        "{bad_key:?}"
    );

    let bad_value = ociman(storage_dir.path(), &["ps", "--filter", "status=bogus"]);
    assert!(!bad_value.status.success());
    assert!(
        String::from_utf8_lossy(&bad_value.stderr).contains("invalid value"),
        "{bad_value:?}"
    );
}

/// `ociman ps --filter name=<substring>` (0273), matching real
/// `docker`/`podman ps --filter name=`'s own checked-directly plain-
/// text behavior (a substring match) — but, unlike `status=`, does
/// *not* override the default running-only visibility rule on its
/// own.
#[test]
fn ps_filter_name_matches_a_substring_and_still_respects_default_visibility() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-filter-name:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "mycontainer123",
            "ociman-test/ps-filter-name:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    // A substring, not the full name, still matches.
    let matched = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "name=contain", "-q"],
    );
    assert!(matched.status.success(), "{matched:?}");
    assert_eq!(
        String::from_utf8_lossy(&matched.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "{matched:?}"
    );

    // A non-matching substring finds nothing.
    let no_match = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "name=zzz", "-q"],
    );
    assert!(no_match.status.success());
    assert!(String::from_utf8_lossy(&no_match.stdout).trim().is_empty());

    // Unlike `status=`, `name=` alone (no `-a`) does *not* override
    // the default running-only visibility rule -- the never-started
    // container stays hidden.
    let no_all = ociman(
        storage_dir.path(),
        &["ps", "--filter", "name=contain", "-q"],
    );
    assert!(no_all.status.success());
    assert!(String::from_utf8_lossy(&no_all.stdout).trim().is_empty());
}

/// `ociman ps --filter id=<prefix>` (0273), matching real `podman ps
/// --filter id=`'s own checked-directly prefix-match semantics for a
/// plain hex value.
#[test]
fn ps_filter_id_matches_by_prefix() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-filter-id:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/ps-filter-id:latest", "true"],
    );
    assert!(create.status.success(), "{create:?}");

    let full_id = String::from_utf8_lossy(&ociman(storage_dir.path(), &["ps", "-a", "-q"]).stdout)
        .trim()
        .to_string();
    let prefix = &full_id[..6];

    let matched = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", &format!("id={prefix}"), "-q"],
    );
    assert!(matched.status.success(), "{matched:?}");
    assert_eq!(
        String::from_utf8_lossy(&matched.stdout).trim(),
        full_id,
        "{matched:?}"
    );

    let no_match = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "id=zzzzzz", "-q"],
    );
    assert!(no_match.status.success());
    assert!(String::from_utf8_lossy(&no_match.stdout).trim().is_empty());
}

/// Different filter *keys* are ANDed together, matching real `podman
/// ps` exactly (checked directly): `status=running --filter
/// name=<name-of-a-non-running-container>` finds nothing, even though
/// each condition alone would match a *different* real container.
#[test]
fn ps_filter_different_keys_are_anded_together() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-filter-and:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "created-and",
            "ociman-test/ps-filter-and:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let neither = ociman(
        storage_dir.path(),
        &[
            "ps",
            "-a",
            "--filter",
            "status=running",
            "--filter",
            "name=created-and",
            "-q",
        ],
    );
    assert!(neither.status.success(), "{neither:?}");
    assert!(
        String::from_utf8_lossy(&neither.stdout).trim().is_empty(),
        "a stopped-state container named created-and should match neither an AND of \
         status=running and name=created-and: {neither:?}"
    );

    // But the same `name=` filter alone (with a matching status) does
    // find it, confirming the AND is real and not just a bug hiding
    // everything.
    let matches_alone = ociman(
        storage_dir.path(),
        &[
            "ps",
            "-a",
            "--filter",
            "status=created",
            "--filter",
            "name=created-and",
            "-q",
        ],
    );
    assert!(matches_alone.status.success(), "{matches_alone:?}");
    assert_eq!(
        String::from_utf8_lossy(&matches_alone.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "{matches_alone:?}"
    );
}
