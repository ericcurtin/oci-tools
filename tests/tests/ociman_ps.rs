//! `ociman ps`/`rm`/`run --rm` integration tests: the persistent
//! container tracking `ociman run` (0020) gained on top of its
//! previously ephemeral-only model (`docs/design/0021`). Same fully
//! offline approach as `ociman_run.rs` (a synthetic-but-structurally-
//! real seeded image, no registry access needed).

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

/// Same technique `ociman_stop.rs`'s own identical helper already
/// established, needed here too for the new `rm --force --time`
/// tests, which (unlike every other `rm` test in this file) need a
/// real, still-running, signal-trappable container.
fn ociman_run_detached(
    storage_root: &Path,
    image: &str,
    container_args: &[&str],
) -> std::process::Child {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", image])
        .args(container_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman run")
}

fn only_container_id(storage_root: &Path, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let out = ociman(storage_root, &["ps", "-a", "-q"]);
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !id.is_empty() || Instant::now() >= deadline {
            return id;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_container_status(
    storage_root: &Path,
    id: &str,
    want: &str,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let out = ociman(storage_root, &["ps", "-a", "--json"]);
        if out.status.success()
            && let Ok(views) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
            && let Some(entry) = views
                .as_array()
                .and_then(|a| a.iter().find(|e| e["id"] == id))
        {
            let status = entry["status"].as_str().unwrap_or_default().to_string();
            if status == want || Instant::now() >= deadline {
                return status;
            }
        } else if Instant::now() >= deadline {
            return String::new();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
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

/// `-t`/`--time` (0355) without `--force` is a real, immediate error,
/// matching real `podman rm -t`/`--time`'s own identical, checked-
/// directly restriction exactly (`~/git/podman/cmd/podman/containers/
/// rm.go`).
#[test]
fn rm_time_without_force_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(
        storage_dir.path(),
        &["rm", "--time", "5", "does-not-matter"],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--force option must be specified"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `rm --force --time` (0355) genuinely gives a still-running
/// container a real chance to exit gracefully first, rather than an
/// immediate, unmaskable `KILL` -- matching real `podman rm --force
/// --time` exactly. Same real TERM-trap technique `ociman_stop.rs`'s
/// own `stop_lets_a_signal_handling_container_exit_gracefully` test
/// already established, reused here since this is the exact same
/// underlying `stop_container` escalation, just reached through `rm`
/// instead of `stop`.
#[test]
fn rm_force_time_lets_a_signal_handling_container_exit_gracefully() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rm-force-time-graceful:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/rm-force-time-graceful:latest",
        &[
            "/bin/sh",
            "-c",
            "trap 'exit 0' TERM; while true; do sleep 0.2; done",
        ],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    // Same generous grace window `ociman_stop.rs`'s own identical
    // test uses, for the same reason: what matters is *whether* the
    // trap runs at all, not exactly how many milliseconds it takes
    // under real, possibly-loaded-host scheduling.
    let rm = ociman(storage_dir.path(), &["rm", "--force", "--time", "60", &id]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );

    run.wait().unwrap();
    // `rm`'s own whole point: the container's own record is genuinely
    // gone afterward, not just stopped -- distinct from `ociman
    // stop`'s own identical graceful escalation, which only stops it.
    let ps_after = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(
        String::from_utf8_lossy(&ps_after.stdout).trim().is_empty(),
        "the container should be fully removed, not merely stopped: {ps_after:?}"
    );
}

/// A real, deliberate divergence from real podman's own default,
/// verified directly rather than merely documented: `rm --force`
/// *alone* (no `--time` given at all) still uses this project's own
/// fast, immediate `KILL`, never the graceful-then-kill escalation
/// `--time` opts into. `rm`'s own whole point removes the container's
/// persisted record, so the exit-code-based technique the tests above
/// use (checking a TERM trap's own effect) can't observe anything
/// afterward -- a real, ignores-`TERM`-outright container (the same
/// `docs/design/0017` finding `ociman_kill.rs`'s own tests already
/// rely on) run under a generous, real `--time`-sized deadline is the
/// most direct substitute: if this project's own default `rm --force`
/// genuinely still waited out a grace period the way real podman's
/// own default does, this would time out; completing well within it
/// proves no such wait happens at all.
#[test]
fn rm_force_without_time_completes_fast_with_no_grace_period_at_all() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rm-force-no-time:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/rm-force-no-time:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let start = Instant::now();
    let rm = ociman(storage_dir.path(), &["rm", "--force", &id]);
    let elapsed = start.elapsed();
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "rm --force alone (no --time) must complete fast, with no real grace period at all -- \
         took {elapsed:?}"
    );

    run.wait().unwrap();
    let ps_after = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(
        String::from_utf8_lossy(&ps_after.stdout).trim().is_empty(),
        "{ps_after:?}"
    );
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

    let bad_key = ociman(storage_dir.path(), &["ps", "--filter", "pod=foo"]);
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

/// `ociman ps --filter command=<substring>` (0404), matching real
/// `podman ps --filter command=`'s own checked-directly behavior: a
/// match against just the container's own *first* command element
/// (the executable itself), never its arguments -- proven here by two
/// containers with genuinely different executables, one of which also
/// has an argument (`sleep`) that must *not* itself match, confirming
/// this isn't a naive whole-command-line substring search.
#[test]
fn ps_filter_command_matches_only_the_first_command_element() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-filter-command:latest",
        &busybox,
        &["sh", "true", "sleep"],
        ContainerConfig::default(),
    );
    let true_ctr = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "command-filter-true",
            "ociman-test/ps-filter-command:latest",
            "true",
        ],
    );
    assert!(true_ctr.status.success(), "{true_ctr:?}");
    let sh_ctr = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "command-filter-sh",
            "ociman-test/ps-filter-command:latest",
            "/bin/sh",
            "-c",
            "sleep 300",
        ],
    );
    assert!(sh_ctr.status.success(), "{sh_ctr:?}");

    // Matches the executable of exactly one of the two containers.
    let matched_true = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "command=true", "-q"],
    );
    assert!(matched_true.status.success(), "{matched_true:?}");
    assert_eq!(
        String::from_utf8_lossy(&matched_true.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "{matched_true:?}"
    );
    let matched_sh = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "command=sh", "-q"],
    );
    assert!(matched_sh.status.success(), "{matched_sh:?}");
    assert_eq!(
        String::from_utf8_lossy(&matched_sh.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "{matched_sh:?}"
    );

    // `sleep` is an *argument* of the second container, never its own
    // first command element -- a real match here would prove this is
    // wrongly matching the whole command line rather than just the
    // executable, the same real distinction real podman's own
    // `Command()[0]`-only filter makes.
    let no_match = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "command=sleep", "-q"],
    );
    assert!(no_match.status.success(), "{no_match:?}");
    assert!(
        String::from_utf8_lossy(&no_match.stdout).trim().is_empty(),
        "{no_match:?}"
    );
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

/// `ociman ps --filter label=`/`label!=` (0275): multiple `label=`
/// values are ANDed together, a deliberately *different* combination
/// rule than `ociman prune --filter label=`'s own OR semantics
/// (`0192`) -- matching real podman's own genuinely different,
/// container-specific `MatchLabelFilters`, checked directly.
#[test]
fn ps_filter_label_multiple_values_are_anded_together() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-filter-label:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let create1 = ociman(
        storage_dir.path(),
        &[
            "create",
            "--label",
            "env=prod",
            "--label",
            "team=infra",
            "--name",
            "label-ps1",
            "ociman-test/ps-filter-label:latest",
            "true",
        ],
    );
    assert!(create1.status.success(), "{create1:?}");
    let create2 = ociman(
        storage_dir.path(),
        &[
            "create",
            "--label",
            "env=staging",
            "--name",
            "label-ps2",
            "ociman-test/ps-filter-label:latest",
            "true",
        ],
    );
    assert!(create2.status.success(), "{create2:?}");

    // Single value: only the matching container.
    let single = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "label=env=prod", "-q"],
    );
    assert!(single.status.success(), "{single:?}");
    assert_eq!(
        String::from_utf8_lossy(&single.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "{single:?}"
    );

    // Two jointly satisfiable values (both true for label-ps1): still
    // just that one container.
    let and_match = ociman(
        storage_dir.path(),
        &[
            "ps",
            "-a",
            "--filter",
            "label=env=prod",
            "--filter",
            "label=team=infra",
            "-q",
        ],
    );
    assert!(and_match.status.success(), "{and_match:?}");
    assert_eq!(
        String::from_utf8_lossy(&and_match.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "{and_match:?}"
    );

    // Two jointly unsatisfiable values: nothing (a real AND, not a
    // silent OR that would otherwise still find label-ps1).
    let and_miss = ociman(
        storage_dir.path(),
        &[
            "ps",
            "-a",
            "--filter",
            "label=env=prod",
            "--filter",
            "label=team=wrong",
            "-q",
        ],
    );
    assert!(and_miss.status.success(), "{and_miss:?}");
    assert!(String::from_utf8_lossy(&and_miss.stdout).trim().is_empty());

    // `label!=` negates: everything *except* a container matching.
    let negated = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "label!=env=prod", "-q"],
    );
    assert!(negated.status.success(), "{negated:?}");
    assert_eq!(
        String::from_utf8_lossy(&negated.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "{negated:?}"
    );

    // A bare key (any value) matches both.
    let bare = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "label=env", "-q"],
    );
    assert!(bare.status.success(), "{bare:?}");
    assert_eq!(
        String::from_utf8_lossy(&bare.stdout).trim().lines().count(),
        2,
        "{bare:?}"
    );

    // Unlike `status=`, `label=` alone (no `-a`) does not override the
    // default running-only visibility rule.
    let no_all = ociman(
        storage_dir.path(),
        &["ps", "--filter", "label=env=prod", "-q"],
    );
    assert!(no_all.status.success());
    assert!(String::from_utf8_lossy(&no_all.stdout).trim().is_empty());
}

/// `ociman ps --filter before=`/`since=` (0280), matching real `podman
/// ps --filter before=`/`since=`'s own checked-directly semantics
/// exactly: `before=X` keeps only containers created strictly earlier
/// than `X`, `since=X` strictly later. Also checks the real, somewhat
/// unusual multi-value rule: multiple `before=`/`since=` values use
/// the *earliest* of all the given reference containers' own creation
/// times (checked directly against a real installed `podman ps`).
#[test]
fn ps_filter_before_and_since_use_the_referenced_containers_own_creation_time() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-filter-before-since:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = |name: &str| {
        let out = ociman(
            storage_dir.path(),
            &[
                "create",
                "--name",
                name,
                "ociman-test/ps-filter-before-since:latest",
                "true",
            ],
        );
        assert!(out.status.success(), "{out:?}");
        std::thread::sleep(Duration::from_millis(1200));
    };
    create("ctr1");
    create("ctr2");
    create("ctr3");

    let before = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "before=ctr2", "-q"],
    );
    assert!(before.status.success(), "{before:?}");
    assert_eq!(
        String::from_utf8_lossy(&before.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "expected only ctr1, created strictly before ctr2: {before:?}"
    );

    let since = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "since=ctr2", "-q"],
    );
    assert!(since.status.success(), "{since:?}");
    assert_eq!(
        String::from_utf8_lossy(&since.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "expected only ctr3, created strictly after ctr2: {since:?}"
    );

    // Multiple `before=` values: the *earliest* of ctr2/ctr3's own
    // creation times is ctr2's -- same result as `before=ctr2` alone.
    let before_multi = ociman(
        storage_dir.path(),
        &[
            "ps",
            "-a",
            "--filter",
            "before=ctr2",
            "--filter",
            "before=ctr3",
            "-q",
        ],
    );
    assert!(before_multi.status.success(), "{before_multi:?}");
    assert_eq!(before_multi.stdout, before.stdout, "{before_multi:?}");

    // An unresolvable reference container is a clear error.
    let bad_ref = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "before=does-not-exist"],
    );
    assert!(!bad_ref.status.success());
    assert!(
        String::from_utf8_lossy(&bad_ref.stderr).contains("does not exist"),
        "{bad_ref:?}"
    );

    // Unlike `status=`, `before=`/`since=` alone (no `-a`) don't
    // override the default running-only visibility rule.
    let no_all = ociman(storage_dir.path(), &["ps", "--filter", "before=ctr2", "-q"]);
    assert!(no_all.status.success());
    assert!(String::from_utf8_lossy(&no_all.stdout).trim().is_empty());
}

/// `ociman ps --filter ancestor=` (0281), matching real `podman ps
/// --filter ancestor=`'s own checked-directly name/tag substring
/// matching rule for the common case: a real image reference
/// substring match, and a bare, tagless value also matching a
/// `:latest`-tagged reference.
#[test]
fn ps_filter_ancestor_matches_the_containers_own_image_reference() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-filter-ancestor:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "ancestor-ctr",
            "ociman-test/ps-filter-ancestor:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    // Full reference.
    let full = ociman(
        storage_dir.path(),
        &[
            "ps",
            "-a",
            "--filter",
            "ancestor=docker.io/ociman-test/ps-filter-ancestor:latest",
            "-q",
        ],
    );
    assert!(full.status.success(), "{full:?}");
    assert_eq!(
        String::from_utf8_lossy(&full.stdout).trim().lines().count(),
        1,
        "{full:?}"
    );

    // Bare, tagless value: the real tag is `latest`, so this still
    // matches.
    let bare = ociman(
        storage_dir.path(),
        &[
            "ps",
            "-a",
            "--filter",
            "ancestor=ociman-test/ps-filter-ancestor",
            "-q",
        ],
    );
    assert!(bare.status.success(), "{bare:?}");
    assert_eq!(
        String::from_utf8_lossy(&bare.stdout).trim().lines().count(),
        1,
        "{bare:?}"
    );

    // Wrong tag: no match.
    let wrong_tag = ociman(
        storage_dir.path(),
        &[
            "ps",
            "-a",
            "--filter",
            "ancestor=ociman-test/ps-filter-ancestor:v1",
            "-q",
        ],
    );
    assert!(wrong_tag.status.success(), "{wrong_tag:?}");
    assert!(String::from_utf8_lossy(&wrong_tag.stdout).trim().is_empty());

    // Unlike `status=`, `ancestor=` alone (no `-a`) doesn't override
    // the default running-only visibility rule.
    let no_all = ociman(
        storage_dir.path(),
        &[
            "ps",
            "--filter",
            "ancestor=ociman-test/ps-filter-ancestor",
            "-q",
        ],
    );
    assert!(no_all.status.success());
    assert!(String::from_utf8_lossy(&no_all.stdout).trim().is_empty());
}

/// `ociman ps --filter exited=` (0282), matching real `podman ps
/// --filter exited=` exactly: matches a container with a real,
/// recorded exit code equal to one of the given values, never one
/// that hasn't exited at all. Multiple values are OR'd together
/// (checked directly against a real installed `podman ps`).
#[test]
fn ps_filter_exited_matches_the_containers_own_recorded_exit_code() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-filter-exited:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let run_with_exit = |name: &str, code: &str| {
        let out = ociman(
            storage_dir.path(),
            &[
                "run",
                "--name",
                name,
                "ociman-test/ps-filter-exited:latest",
                "sh",
                "-c",
                &format!("exit {code}"),
            ],
        );
        assert_eq!(out.status.code(), code.parse().ok(), "{out:?}");
    };
    run_with_exit("exit0", "0");
    run_with_exit("exit5", "5");
    run_with_exit("exit7", "7");

    let single = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "exited=5", "-q"],
    );
    assert!(single.status.success(), "{single:?}");
    assert_eq!(
        String::from_utf8_lossy(&single.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "{single:?}"
    );

    let multi = ociman(
        storage_dir.path(),
        &[
            "ps", "-a", "--filter", "exited=5", "--filter", "exited=7", "-q",
        ],
    );
    assert!(multi.status.success(), "{multi:?}");
    assert_eq!(
        String::from_utf8_lossy(&multi.stdout)
            .trim()
            .lines()
            .count(),
        2,
        "{multi:?}"
    );

    let zero = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "exited=0", "-q"],
    );
    assert!(zero.status.success(), "{zero:?}");
    assert_eq!(
        String::from_utf8_lossy(&zero.stdout).trim().lines().count(),
        1,
        "{zero:?}"
    );

    let no_match = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "exited=99", "-q"],
    );
    assert!(no_match.status.success());
    assert!(String::from_utf8_lossy(&no_match.stdout).trim().is_empty());

    let bad_value = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "exited=bogus"],
    );
    assert!(!bad_value.status.success());
    assert!(
        String::from_utf8_lossy(&bad_value.stderr).contains("invalid exit code"),
        "{bad_value:?}"
    );

    // Unlike `status=`, `exited=` alone (no `-a`) doesn't override the
    // default running-only visibility rule.
    let no_all = ociman(storage_dir.path(), &["ps", "--filter", "exited=5", "-q"]);
    assert!(no_all.status.success());
    assert!(String::from_utf8_lossy(&no_all.stdout).trim().is_empty());
}

/// `ociman ps --filter until=` (0289), matching real `podman ps
/// --filter until=`'s own checked-directly semantics exactly: a
/// container matches if its own creation time is *strictly* before
/// the given duration-ago or absolute timestamp (real podman's own
/// `CreatedTime().Before(until)`), reusing the exact same threshold
/// computation `ociman prune --filter until=` (`0198`) already
/// established.
#[test]
fn ps_filter_until_matches_containers_created_strictly_before_the_threshold() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-filter-until:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = |name: &str| {
        let out = ociman(
            storage_dir.path(),
            &[
                "create",
                "--name",
                name,
                "ociman-test/ps-filter-until:latest",
                "true",
            ],
        );
        assert!(out.status.success(), "{out:?}");
        std::thread::sleep(Duration::from_millis(1200));
    };
    create("old1");
    create("old2");

    // A far-future absolute timestamp: every container so far was
    // created strictly before it.
    let all_match = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "until=2999-01-01T00:00:00Z", "-q"],
    );
    assert!(all_match.status.success(), "{all_match:?}");
    assert_eq!(
        String::from_utf8_lossy(&all_match.stdout)
            .trim()
            .lines()
            .count(),
        2,
        "{all_match:?}"
    );

    // A far-past absolute timestamp: nothing created after it
    // matches.
    let none_match = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "until=1970-01-01T00:00:00Z", "-q"],
    );
    assert!(none_match.status.success(), "{none_match:?}");
    assert!(
        String::from_utf8_lossy(&none_match.stdout)
            .trim()
            .is_empty(),
        "{none_match:?}"
    );

    // A relative duration far enough in the past that it also matches
    // nothing (both containers were created well within the last
    // second, not a full day ago).
    let duration_none = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "until=24h", "-q"],
    );
    assert!(duration_none.status.success(), "{duration_none:?}");
    assert!(
        String::from_utf8_lossy(&duration_none.stdout)
            .trim()
            .is_empty(),
        "{duration_none:?}"
    );

    // More than one `until=` value is a clear error, matching real
    // podman's own identical refusal.
    let too_many = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "until=24h", "--filter", "until=48h"],
    );
    assert!(!too_many.status.success());
    assert!(
        String::from_utf8_lossy(&too_many.stderr).contains("more than one until filter"),
        "{too_many:?}"
    );

    // A value that's neither a duration nor an RFC3339 timestamp is a
    // clear error.
    let bad_value = ociman(
        storage_dir.path(),
        &["ps", "-a", "--filter", "until=not-a-real-value"],
    );
    assert!(!bad_value.status.success());
    assert!(
        String::from_utf8_lossy(&bad_value.stderr).contains("invalid value for 'until' filter"),
        "{bad_value:?}"
    );

    // Unlike `status=`, `until=` alone (no `-a`) doesn't override the
    // default running-only visibility rule.
    let no_all = ociman(
        storage_dir.path(),
        &["ps", "--filter", "until=2999-01-01T00:00:00Z", "-q"],
    );
    assert!(no_all.status.success());
    assert!(String::from_utf8_lossy(&no_all.stdout).trim().is_empty());
}

/// `ociman ps -n`/`--last` (0290), matching real `podman ps -n`
/// exactly (checked directly against `~/git/podman/pkg/ps/ps.go`): a
/// positive value overrides the default running-only visibility rule
/// (same as `--all`) *and* keeps only the `n` most-recently-created
/// matching containers, still shown in ascending (oldest-of-the-kept-
/// first) order.
#[test]
fn ps_last_overrides_visibility_and_keeps_only_the_n_most_recently_created() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-last:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = |name: &str| {
        let out = ociman(
            storage_dir.path(),
            &[
                "create",
                "--name",
                name,
                "ociman-test/ps-last:latest",
                "true",
            ],
        );
        assert!(out.status.success(), "{out:?}");
        std::thread::sleep(Duration::from_millis(1200));
    };
    create("last1");
    create("last2");
    create("last3");

    // No `-a`, no `-n`: every container here is merely `created`
    // (never started), so a plain `ps` shows nothing at all.
    let plain = ociman(storage_dir.path(), &["ps", "-q"]);
    assert!(plain.status.success());
    assert!(String::from_utf8_lossy(&plain.stdout).trim().is_empty());

    // `-n 2`, still no `-a`: overrides visibility on its own, and
    // keeps only the 2 most recently created (last2, last3), in
    // ascending order.
    let last2 = ociman(storage_dir.path(), &["ps", "-n", "2", "-q"]);
    assert!(last2.status.success(), "{last2:?}");
    let ids: Vec<String> = String::from_utf8_lossy(&last2.stdout)
        .trim()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(ids.len(), 2, "{ids:?}");

    let names = ociman(storage_dir.path(), &["ps", "-n", "2", "-a", "--noheading"]);
    assert!(names.status.success(), "{names:?}");
    let stdout = String::from_utf8_lossy(&names.stdout);
    assert!(
        stdout.contains("last2") && stdout.contains("last3") && !stdout.contains("last1"),
        "expected only last2/last3 (the two most recent), got: {stdout:?}"
    );
    // Ascending order: last2 appears before last3.
    assert!(
        stdout.find("last2").unwrap() < stdout.find("last3").unwrap(),
        "{stdout:?}"
    );

    // `-n` larger than the real count is a real no-op (every
    // container still shown, nothing dropped).
    let last_big = ociman(storage_dir.path(), &["ps", "-n", "100", "-q"]);
    assert!(last_big.status.success());
    assert_eq!(
        String::from_utf8_lossy(&last_big.stdout)
            .trim()
            .lines()
            .count(),
        3,
        "{last_big:?}"
    );

    // `-n 0` (and the implicit default, `-1`) is a real no-op: no
    // visibility override, nothing hidden that wasn't already hidden.
    let zero = ociman(storage_dir.path(), &["ps", "-n", "0", "-q"]);
    assert!(zero.status.success());
    assert!(String::from_utf8_lossy(&zero.stdout).trim().is_empty());
}

/// `ociman ps --sort` (matching real `podman ps --sort` exactly):
/// `--sort names` orders alphabetically, a real, direct contrast with
/// the default creation-time order.
#[test]
fn ps_sort_names_orders_alphabetically() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-sort-names:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = |name: &str| {
        let out = ociman(
            storage_dir.path(),
            &[
                "create",
                "--name",
                name,
                "ociman-test/ps-sort-names:latest",
                "true",
            ],
        );
        assert!(out.status.success(), "{out:?}");
        std::thread::sleep(Duration::from_millis(1200));
    };
    // Created in this order -- the default view would show zzz-first
    // before aaa-second (creation order).
    create("zzz-first");
    create("aaa-second");

    let default_order = ociman(storage_dir.path(), &["ps", "-a", "--noheading"]);
    assert!(default_order.status.success());
    let stdout = String::from_utf8_lossy(&default_order.stdout);
    assert!(
        stdout.find("zzz-first").unwrap() < stdout.find("aaa-second").unwrap(),
        "default order should be creation order: {stdout:?}"
    );

    let sorted = ociman(
        storage_dir.path(),
        &["ps", "-a", "--noheading", "--sort", "names"],
    );
    assert!(sorted.status.success(), "{sorted:?}");
    let stdout = String::from_utf8_lossy(&sorted.stdout);
    assert!(
        stdout.find("aaa-second").unwrap() < stdout.find("zzz-first").unwrap(),
        "--sort names should order alphabetically: {stdout:?}"
    );
}

/// `--sort runningfor` sorts by real `PersistedState::started_at`, a
/// genuinely *different* timestamp than `created` (matching real
/// podman's own checked-directly `container_ps.go` behavior, not an
/// alias for `--sort created`): a container created *second* but
/// started *first* must sort before one created first but started
/// second.
#[test]
fn ps_sort_runningfor_differs_from_created_order() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-sort-runningfor:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let create = |name: &str| {
        let out = ociman(
            storage_dir.path(),
            &[
                "create",
                "--name",
                name,
                "ociman-test/ps-sort-runningfor:latest",
                "sleep",
                "30",
            ],
        );
        assert!(out.status.success(), "{out:?}");
        std::thread::sleep(Duration::from_millis(1200));
    };
    create("created-first");
    create("created-second");

    // Started in the *opposite* order from creation.
    let start_second = ociman(storage_dir.path(), &["start", "created-second"]);
    assert!(start_second.status.success(), "{start_second:?}");
    std::thread::sleep(Duration::from_millis(1200));
    let start_first = ociman(storage_dir.path(), &["start", "created-first"]);
    assert!(start_first.status.success(), "{start_first:?}");

    let by_created = ociman(storage_dir.path(), &["ps", "-a", "--noheading"]);
    assert!(by_created.status.success());
    let stdout = String::from_utf8_lossy(&by_created.stdout);
    assert!(
        stdout.find("created-first").unwrap() < stdout.find("created-second").unwrap(),
        "default order is still creation order: {stdout:?}"
    );

    let by_runningfor = ociman(
        storage_dir.path(),
        &["ps", "-a", "--noheading", "--sort", "runningfor"],
    );
    assert!(by_runningfor.status.success(), "{by_runningfor:?}");
    let stdout = String::from_utf8_lossy(&by_runningfor.stdout);
    assert!(
        stdout.find("created-second").unwrap() < stdout.find("created-first").unwrap(),
        "--sort runningfor must order by when each was actually started, not created: {stdout:?}"
    );

    ociman(storage_dir.path(), &["kill", "created-first"]);
    ociman(storage_dir.path(), &["kill", "created-second"]);
}

/// `--sort` composes with `--last`/`-n`: the same *set* of containers
/// `--last` would otherwise select (by creation-time recency) is still
/// selected, only their *display* order changes -- matching real
/// podman's own checked-directly behavior exactly (confirmed directly
/// against a real installed `podman ps -a -n 2 --sort names`).
#[test]
fn ps_sort_composes_with_last_without_changing_which_containers_are_selected() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-sort-last:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = |name: &str| {
        let out = ociman(
            storage_dir.path(),
            &[
                "create",
                "--name",
                name,
                "ociman-test/ps-sort-last:latest",
                "true",
            ],
        );
        assert!(out.status.success(), "{out:?}");
        std::thread::sleep(Duration::from_millis(1200));
    };
    // Alphabetically, "zzz-oldest" would sort last -- proving `--sort`
    // doesn't change *which* 2 containers `-n 2` selects.
    create("zzz-oldest");
    create("mmm-middle");
    create("aaa-newest");

    let out = ociman(
        storage_dir.path(),
        &["ps", "-a", "-n", "2", "--noheading", "--sort", "names"],
    );
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("zzz-oldest"),
        "the oldest container must still be excluded by --last, regardless of --sort: {stdout:?}"
    );
    assert!(
        stdout.contains("mmm-middle") && stdout.contains("aaa-newest"),
        "{stdout:?}"
    );
    assert!(
        stdout.find("aaa-newest").unwrap() < stdout.find("mmm-middle").unwrap(),
        "the 2 selected containers must still be displayed alphabetically: {stdout:?}"
    );
}

/// An unrecognized `--sort` value is a real, immediate clap parse
/// error naming the valid choices, matching real podman's own
/// checked-directly cobra-validated-flag rejection exactly.
#[test]
fn ps_sort_rejects_an_invalid_value() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let out = ociman(storage_dir.path(), &["ps", "--sort", "bogus"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("invalid value"), "{stderr}");
    assert!(stderr.contains("runningfor"), "{stderr}");
}

/// `--sort size` without `--size` is a real, semantic no-op (matching
/// real podman's own checked-directly behavior exactly: every size is
/// absent, so every pair compares as "not less", leaving the existing
/// order untouched) rather than an error.
#[test]
fn ps_sort_size_without_size_flag_is_a_no_op() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-sort-size-noop:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = |name: &str| {
        let out = ociman(
            storage_dir.path(),
            &[
                "create",
                "--name",
                name,
                "ociman-test/ps-sort-size-noop:latest",
                "true",
            ],
        );
        assert!(out.status.success(), "{out:?}");
        std::thread::sleep(Duration::from_millis(1200));
    };
    create("zzz-first");
    create("aaa-second");

    let out = ociman(
        storage_dir.path(),
        &["ps", "-a", "--noheading", "--sort", "size"],
    );
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.find("zzz-first").unwrap() < stdout.find("aaa-second").unwrap(),
        "without --size, --sort size must leave the default creation-time order untouched: \
         {stdout:?}"
    );
}

/// `--sort size` combined with `--size`: orders by real
/// `root_fs_size` (image + writable layer), matching real podman's
/// own identical `RootFsSize`-based comparison exactly -- a real,
/// kernel-measured difference (one container writes a real file into
/// its own writable layer, inflating its own `root_fs_size` past the
/// other's, which never writes anything).
#[test]
fn ps_sort_size_with_size_flag_orders_by_real_root_fs_size() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-sort-size:latest",
        &busybox,
        &["sh", "dd", "true"],
        ContainerConfig::default(),
    );

    let empty = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "empty-writable-layer",
            "ociman-test/ps-sort-size:latest",
            "true",
        ],
    );
    assert!(empty.status.success(), "{empty:?}");

    let grown = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "grown-writable-layer",
            "ociman-test/ps-sort-size:latest",
            "/bin/sh",
            "-c",
            "dd if=/dev/zero of=/bigfile bs=1M count=4",
        ],
    );
    assert!(
        grown.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&grown.stderr)
    );

    let out = ociman(
        storage_dir.path(),
        &["ps", "-a", "--noheading", "--size", "--sort", "size"],
    );
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.find("empty-writable-layer").unwrap() < stdout.find("grown-writable-layer").unwrap(),
        "the smaller (empty writable layer) container must sort before the larger one: {stdout:?}"
    );
}

/// `ociman ps --no-trunc`/`--noheading` (0290): the real default
/// 17-character-plus-`...` command truncation (matching real
/// `podman ps`'s own `Command()` formatter exactly) is disabled by
/// `--no-trunc`, and `--noheading` drops the header row entirely.
#[test]
fn ps_no_trunc_and_noheading_control_the_real_table_output() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-no-trunc:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "no-trunc-test",
            "ociman-test/ps-no-trunc:latest",
            "sh",
            "-c",
            "echo this is a genuinely long command line on purpose",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let truncated = ociman(storage_dir.path(), &["ps", "-a"]);
    assert!(truncated.status.success());
    let stdout = String::from_utf8_lossy(&truncated.stdout);
    assert!(
        stdout.contains("CONTAINER ID") && stdout.contains("...") && !stdout.contains("purpose"),
        "the default table must truncate the long command and still show a header: {stdout:?}"
    );

    let full = ociman(storage_dir.path(), &["ps", "-a", "--no-trunc"]);
    assert!(full.status.success());
    let stdout = String::from_utf8_lossy(&full.stdout);
    assert!(
        stdout.contains("purpose") && !stdout.contains("..."),
        "--no-trunc must show the full command: {stdout:?}"
    );

    let no_heading = ociman(storage_dir.path(), &["ps", "-a", "--noheading"]);
    assert!(no_heading.status.success());
    assert!(
        !String::from_utf8_lossy(&no_heading.stdout).contains("CONTAINER ID"),
        "{no_heading:?}"
    );
}

/// `-s`/`--size` shows a real writable-layer size plus a real
/// `(virtual <total>)` figure per container, matching real `docker
/// ps -s`/`podman ps --size` exactly (`~/git/podman/cmd/podman/
/// containers/ps.go`'s own `psReporter.Size()`); a plain `ociman ps`
/// (no `-s`) shows neither a `SIZE` header nor any size figures at
/// all.
#[test]
fn ps_size_flag_shows_a_real_size_and_virtual_total() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-size:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "ps-size-test",
            "ociman-test/ps-size:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let without_size = ociman(storage_dir.path(), &["ps", "-a"]);
    assert!(without_size.status.success());
    let stdout = String::from_utf8_lossy(&without_size.stdout);
    assert!(
        !stdout.contains("SIZE") && !stdout.contains("virtual"),
        "a plain `ps` must show no size information at all: {stdout:?}"
    );

    let with_size = ociman(storage_dir.path(), &["ps", "-a", "--size"]);
    assert!(with_size.status.success(), "{with_size:?}");
    let stdout = String::from_utf8_lossy(&with_size.stdout);
    assert!(
        stdout.contains("SIZE") && stdout.contains("(virtual "),
        "`--size` must show a SIZE column with a virtual total: {stdout:?}"
    );
}

/// `-s` is the short form of `--size`, matching real `docker ps -s`/
/// `podman ps -s` exactly.
#[test]
fn ps_size_short_flag_behaves_identically_to_the_long_form() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-size-short:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/ps-size-short:latest", "true"],
    );
    assert!(create.status.success(), "{create:?}");

    let out = ociman(storage_dir.path(), &["ps", "-a", "-s"]);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("(virtual "));
}

/// `--quiet`/`-q` and `--size`/`-s` together is a clear, immediate
/// error, matching real `podman ps`'s own identical restriction
/// exactly (`~/git/podman/cmd/podman/containers/ps.go`'s own
/// `checkFlags`).
#[test]
fn ps_quiet_and_size_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let _store = Store::open(storage_dir.path()).unwrap();
    let out = ociman(storage_dir.path(), &["ps", "-a", "-q", "-s"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("conflicts"),
        "{out:?}"
    );
}

/// `--json` only ever includes a `size` object per container when
/// `--size` was actually given -- matching real podman's own
/// identical on-demand-only computation (`~/git/podman/pkg/ps/ps.go`:
/// `ListContainer.Size` stays `nil`, omitted from JSON, unless
/// `opts.Size` was set).
#[test]
fn ps_json_only_includes_size_when_the_size_flag_is_given() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-size-json:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/ps-size-json:latest", "true"],
    );
    assert!(create.status.success(), "{create:?}");

    let without_size = ociman(storage_dir.path(), &["--json", "ps", "-a"]);
    assert!(without_size.status.success());
    let json: serde_json::Value = serde_json::from_slice(&without_size.stdout).unwrap();
    assert!(json[0].get("size").is_none(), "{json:?}");

    let with_size = ociman(storage_dir.path(), &["--json", "ps", "-a", "--size"]);
    assert!(with_size.status.success());
    let json: serde_json::Value = serde_json::from_slice(&with_size.stdout).unwrap();
    let size = &json[0]["size"];
    assert!(size["rw_size"].as_u64().is_some(), "{json:?}");
    assert!(
        size["root_fs_size"].as_u64().unwrap() >= size["rw_size"].as_u64().unwrap(),
        "root_fs_size (image + rw) must be at least as large as rw_size alone: {json:?}"
    );
}

/// `ociman rm --cidfile` (0310), matching real `docker rm --cidfile`/
/// `podman rm --cidfile` exactly (checked directly against real
/// podman's own `cmd/podman/containers/rm.go`): reads the container id
/// from a file (repeatable, one per `--cidfile`), merged into the
/// exact same target list an explicit `ID`/`--name` argument already
/// builds. Also proves real podman's own "first line only" semantics
/// (`strings.Cut(content, "\n")`): content after the first newline is
/// simply ignored, not an error.
#[test]
fn rm_cidfile_reads_the_container_id_from_a_file_and_ignores_trailing_content() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rm-cidfile:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    // A plain (blocking, foreground) `run` -- by the time it returns,
    // the container is guaranteed to already be `Stopped` (matching
    // this file's own already-established pattern for the identical
    // reason), so `rm` needs no `--force` afterward. `--name` gives a
    // known identifier to put in the cidfile without needing to parse
    // one back out of `run`'s own output (which, unlike `-d`, never
    // prints anything at all in the foreground case) -- `ociman rm`
    // already resolves a name exactly like a real id, so this is a
    // faithful stand-in for "whatever id a real `--cidfile` would
    // have captured".
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "rm-cidfile-target",
            "ociman-test/rm-cidfile:latest",
            "true",
        ],
    );
    assert!(run.status.success(), "{run:?}");

    let cidfile = storage_dir.path().join("cid.txt");
    // A trailing "garbage" line after the real id: real podman's own
    // `strings.Cut` takes only the first line, ignoring the rest.
    std::fs::write(&cidfile, "rm-cidfile-target\ngarbage second line").unwrap();

    let rm = ociman(
        storage_dir.path(),
        &["rm", "--cidfile", cidfile.to_str().unwrap()],
    );
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&rm.stdout).trim(),
        "rm-cidfile-target"
    );

    let ps_after = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(String::from_utf8_lossy(&ps_after.stdout).trim().is_empty());
}

/// Multiple `--cidfile` flags are merged into the same target list,
/// exactly like multiple explicit `ID`s already are.
#[test]
fn rm_multiple_cidfiles_are_all_merged_into_the_same_target_list() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rm-cidfile-multi:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    // Plain (blocking, foreground) runs -- see the previous test's own
    // doc comment for why `--name` (not a parsed-from-stdout id) is
    // used here, and why no `--force` is needed afterward.
    let run1 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "rm-cidfile-multi-1",
            "ociman-test/rm-cidfile-multi:latest",
            "true",
        ],
    );
    assert!(run1.status.success());
    let run2 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "rm-cidfile-multi-2",
            "ociman-test/rm-cidfile-multi:latest",
            "true",
        ],
    );
    assert!(run2.status.success());

    let cidfile1 = storage_dir.path().join("cid1.txt");
    let cidfile2 = storage_dir.path().join("cid2.txt");
    std::fs::write(&cidfile1, "rm-cidfile-multi-1").unwrap();
    std::fs::write(&cidfile2, "rm-cidfile-multi-2").unwrap();

    let rm = ociman(
        storage_dir.path(),
        &[
            "rm",
            "--cidfile",
            cidfile1.to_str().unwrap(),
            "--cidfile",
            cidfile2.to_str().unwrap(),
        ],
    );
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&rm.stdout).trim().lines().count(),
        2
    );

    let ps_after = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(String::from_utf8_lossy(&ps_after.stdout).trim().is_empty());
}

/// `--all` and `--cidfile` are mutually exclusive, matching real
/// podman's own exact error text ("--all, --latest, and --cidfile
/// cannot be used together" -- this project has no `--latest` concept
/// at all, so its own message correctly omits it).
#[test]
fn rm_all_and_cidfile_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let cidfile = storage_dir.path().join("cid.txt");
    std::fs::write(&cidfile, "whatever").unwrap();

    let out = ociman(
        storage_dir.path(),
        &["rm", "--all", "--cidfile", cidfile.to_str().unwrap()],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--all and --cidfile"),
        "{out:?}"
    );
}

/// A `--cidfile` that can't be read at all (missing file) is a clear,
/// immediate error *without* `--ignore` — see
/// `rm_ignore_tolerates_an_unreadable_cidfile` below for the
/// complementary case: with `--ignore`, matching real podman's own
/// identical `errors.Is(err, os.ErrNotExist)` tolerance, it's a silent
/// skip instead (0318 — corrects `0311`'s own original, incomplete
/// claim that `--ignore` never widened to this case at all, which
/// didn't actually match real podman's own source).
#[test]
fn rm_cidfile_of_a_missing_file_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(
        storage_dir.path(),
        &["rm", "--cidfile", "/no/such/cidfile-path.txt"],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("reading --cidfile"),
        "{out:?}"
    );
}

/// `ociman rm --ignore` (0311), matching real `podman rm --ignore`
/// exactly (checked directly against an installed `podman 4.9.3`,
/// both its own source and live behavior): an id that doesn't resolve
/// to any real container at all is silently skipped instead of
/// erroring the whole call.
#[test]
fn rm_ignore_silently_skips_an_id_that_does_not_resolve_at_all() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let out = ociman(
        storage_dir.path(),
        &["rm", "--ignore", "nonexistent-container-xyz"],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
}

/// `--ignore` merged with a real, resolvable container in the same
/// call: the real one is still removed normally, only the
/// unresolvable one is dropped -- proving `--ignore` doesn't turn the
/// whole call into a no-op, just skips what can't resolve.
#[test]
fn rm_ignore_still_removes_a_real_container_alongside_an_unresolvable_one() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rm-ignore:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "rm-ignore-real",
            "ociman-test/rm-ignore:latest",
            "true",
        ],
    );
    assert!(run.status.success());

    let rm = ociman(
        storage_dir.path(),
        &[
            "rm",
            "--ignore",
            "rm-ignore-real",
            "nonexistent-container-xyz",
        ],
    );
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&rm.stdout).trim(), "rm-ignore-real");

    let ps_after = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(String::from_utf8_lossy(&ps_after.stdout).trim().is_empty());
}

/// `--force` alone (no explicit `--ignore`) also tolerates an
/// unresolvable id -- matching real podman's own checked-directly
/// "forcing implies ignoring too" behavior, the same convention
/// `ociman rmi --force` already established.
#[test]
fn rm_force_alone_also_implies_ignore() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let out = ociman(
        storage_dir.path(),
        &["rm", "--force", "nonexistent-container-xyz"],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `--ignore`'s own real, checked-directly narrow gate: it only ever
/// tolerates "doesn't resolve to anything at all," never a
/// still-running-without-`--force` refusal -- matching real podman's
/// own identical behavior, verified directly against an installed
/// `podman 4.9.3` (an identical hard error, with or without
/// `--ignore`, for a running container without `--force`). Uses the
/// same bare "created" (never-run) record technique
/// `rm_without_force_refuses_to_remove_a_container_still_marked_running`
/// already established -- this only needs a record whose
/// `effective_status` isn't `Stopped` yet, not a real long-lived
/// process.
#[test]
fn rm_ignore_does_not_tolerate_a_non_stopped_container_without_force() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let containers_root = storage_dir.path().join("containers");
    let containers = oci_runtime_core::StateStore::open(&containers_root).unwrap();
    containers
        .create(
            "still-creating-ignore",
            Path::new("/bundle"),
            Path::new("/bundle/rootfs"),
            Default::default(),
        )
        .unwrap();

    let rm = ociman(
        storage_dir.path(),
        &["rm", "--ignore", "still-creating-ignore"],
    );
    assert!(
        !rm.status.success(),
        "--ignore should not tolerate a non-stopped-without-force refusal"
    );

    let forced = ociman(
        storage_dir.path(),
        &["rm", "--force", "still-creating-ignore"],
    );
    assert!(
        forced.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&forced.stderr)
    );
}

/// `--ignore` *does* widen to `--cidfile`'s own separate "the file
/// itself can't be read" case (0318 — corrects `0311`'s own original
/// claim that it never did, which turned out not to match real
/// podman's own source, `~/git/podman/cmd/podman/containers/rm.go`'s
/// own `errors.Is(err, os.ErrNotExist)` check): a missing cidfile,
/// with `--ignore` and nothing else given, is a silent, successful
/// no-op — matching real podman's own identical checked-directly
/// behavior exactly (the CLI-level "you must provide at least one
/// name or id" validation only ever checks whether `--cidfile` was
/// *given*, never whether it later actually resolved to anything).
#[test]
fn rm_ignore_tolerates_an_unreadable_cidfile() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(
        storage_dir.path(),
        &["rm", "--ignore", "--cidfile", "/no/such/cidfile-path.txt"],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
}

/// `ociman ps --format` (0333) renders one line per listed container,
/// reusing the exact same Go-template-*lite* engine `ociman inspect
/// --format` (`0332`) already established.
#[test]
fn ps_format_renders_one_line_per_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-format:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    for name in ["fmt-one", "fmt-two"] {
        let create = ociman(
            storage_dir.path(),
            &[
                "create",
                "--name",
                name,
                "ociman-test/ps-format:latest",
                "true",
            ],
        );
        assert!(create.status.success(), "{create:?}");
    }

    let ps = ociman(
        storage_dir.path(),
        &["ps", "-a", "--format", "{{.name}}={{.status}}"],
    );
    assert!(
        ps.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ps.stderr)
    );
    let stdout = String::from_utf8_lossy(&ps.stdout).into_owned();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.contains(&"fmt-one=created"), "{lines:?}");
    assert!(lines.contains(&"fmt-two=created"), "{lines:?}");
}

/// `--format` can reach into `--size`'s own nested `size.rw_size`/
/// `size.root_fs_size` fields directly, the same way it already
/// reaches into any other nested field -- but only when `--size` was
/// also given (otherwise `size` is absent from the underlying JSON
/// entirely and the path is a real, immediate error, same as any
/// other unresolvable field).
#[test]
fn ps_format_can_reach_the_nested_size_fields_when_size_flag_given() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-format-size:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/ps-format-size:latest", "true"],
    );
    assert!(create.status.success(), "{create:?}");

    let without_size = ociman(
        storage_dir.path(),
        &["ps", "-a", "--format", "{{.size.rw_size}}"],
    );
    assert!(
        !without_size.status.success(),
        "size.rw_size should be unresolvable without --size: {without_size:?}"
    );

    let with_size = ociman(
        storage_dir.path(),
        &[
            "ps",
            "-a",
            "--size",
            "--format",
            "{{.size.rw_size}} {{.size.root_fs_size}}",
        ],
    );
    assert!(
        with_size.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&with_size.stderr)
    );
    let stdout = String::from_utf8_lossy(&with_size.stdout);
    let mut parts = stdout.trim().split(' ');
    let rw_size: u64 = parts.next().unwrap().parse().unwrap();
    let root_fs_size: u64 = parts.next().unwrap().parse().unwrap();
    assert!(
        root_fs_size >= rw_size,
        "root_fs_size (image + rw) must be at least rw_size alone: {stdout:?}"
    );
}

/// `--format`, when given, takes priority over `--quiet`/`--json` and
/// the default table, matching real `podman ps`'s own identical
/// precedence -- and an unresolvable field path is a real, immediate
/// error, same as `inspect --format`.
#[test]
fn ps_format_takes_priority_and_errors_on_an_unknown_field() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-format-priority:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "fmt-priority",
            "ociman-test/ps-format-priority:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let ps = ociman(
        storage_dir.path(),
        &["ps", "-a", "-q", "--format", "{{.id}}"],
    );
    assert!(ps.status.success());
    let id_only = String::from_utf8_lossy(&ps.stdout).trim().to_string();
    assert_eq!(
        id_only.len(),
        12,
        "the format template's own plain id, not -q's own behavior, should have won: {id_only:?}"
    );

    let bad = ociman(
        storage_dir.path(),
        &["ps", "-a", "--format", "{{.nosuchfield}}"],
    );
    assert!(!bad.status.success());
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("no field"),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
}
