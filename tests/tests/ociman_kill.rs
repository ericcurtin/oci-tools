//! `ociman kill` integration tests: a single, immediate signal send
//! with no wait/escalation policy at all — distinct from `ociman stop`
//! (see `ociman_stop.rs`'s own doc comment), matching real `docker
//! kill`/`podman kill` exactly (default signal `KILL`, one
//! `Kill(sig)` call, no waiting — checked directly against
//! `~/git/podman/cmd/podman/containers/kill.go`).
//!
//! Same fully offline seeded-image approach `ociman_run.rs`
//! established, and the same `spawn()`+detached-stdio+poll concurrency
//! pattern `ociman_stop.rs`/`ociman_exec.rs` use for a container that
//! needs to still be running while a separate invocation acts on it.

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

/// Same as [`wait_for_container_status`], but matches on a
/// container's own `--name` rather than its generated id (`ps
/// --json`'s own `"name"` field, not `"id"`) -- for the `--all` tests
/// below, which need several containers running at once and so name
/// each rather than relying on `only_container_id`'s own "exactly one
/// container" assumption.
fn wait_for_container_status_by_name(
    storage_root: &Path,
    name: &str,
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
                .and_then(|a| a.iter().find(|e| e["name"] == name))
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

/// Same as [`ociman_run_detached`], but with `--name` given *before*
/// the image -- `RunArgs::args`'s own `trailing_var_arg = true`
/// captures everything positional after the image into the
/// container's own command, so `--name` (a real `ociman run` flag,
/// not part of the container's command) must come first.
fn ociman_run_detached_named(
    storage_root: &Path,
    name: &str,
    image: &str,
    container_args: &[&str],
) -> std::process::Child {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "--name", name, image])
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
fn kill_sends_a_real_sigkill_by_default_and_stops_the_container_immediately() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/kill-default:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/kill-default:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    // No `--signal` given at all -- real `docker`/`podman kill`'s own
    // default is `KILL`, not `TERM` (unlike `stop`'s own default).
    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(
        kill.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&kill.stderr)
    );

    run.wait().unwrap();
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );

    ociman(storage_dir.path(), &["rm", &id]);
}

/// A `sleep 30` run as a pid-namespace's own init ignores an
/// unhandled-default-action `TERM` outright (the same real,
/// already-established kernel finding `docs/design/0017` and
/// `ociman_stop.rs`'s own escalation test rely on) — `kill --signal
/// TERM`, unlike `stop`, never escalates at all, so the container
/// should genuinely still be running afterward. This is the expected,
/// correct behavior for a single-signal-send primitive, not a bug.
#[test]
fn kill_with_a_custom_signal_sends_exactly_that_signal_and_never_escalates() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/kill-term:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/kill-term:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let kill = ociman(storage_dir.path(), &["kill", "--signal", "TERM", &id]);
    assert!(
        kill.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&kill.stderr)
    );

    // Give the (never-escalating) `TERM` every chance to have taken
    // effect if it somehow were going to -- it shouldn't, and the
    // container should still be running.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        wait_for_container_status(
            storage_dir.path(),
            &id,
            "running",
            Duration::from_millis(200)
        ),
        "running",
        "an unhandled TERM should be silently ignored by the pid-namespace init, and `kill` \
         itself never escalates"
    );

    // Real `KILL` cannot be ignored -- clean the container up for
    // real.
    let real_kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(real_kill.status.success());
    run.wait().unwrap();
    wait_for_container_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20));
    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn kill_on_an_already_stopped_container_is_a_real_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/kill-already-stopped:latest",
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

    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/kill-already-stopped:latest"],
    );
    assert!(run.status.success());
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());

    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(
        !kill.status.success(),
        "kill on an already-stopped container should be a real error, unlike `stop`'s own \
         no-op, matching real podman's own ErrCtrStateInvalid"
    );
}

#[test]
fn kill_of_a_nonexistent_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["kill", "does-not-exist"]);
    assert!(!out.status.success());
}

/// `--all` (0312) matches real `podman kill --all` exactly: every
/// running container gets signaled, and a container that was `create`d
/// but never `start`ed (so has no live process to signal at all) is
/// silently skipped -- no error, nothing printed for it -- rather than
/// aborting the whole call, checked directly against a real installed
/// `podman kill --all` in the same situation.
#[test]
fn kill_all_kills_every_running_container_and_silently_skips_a_never_started_one() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/kill-all:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run1 = ociman_run_detached_named(
        storage_dir.path(),
        "kill-all-run1",
        "ociman-test/kill-all:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let mut run2 = ociman_run_detached_named(
        storage_dir.path(),
        "kill-all-run2",
        "ociman-test/kill-all:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "kill-all-run1",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "kill-all-run2",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "kill-all-created",
            "ociman-test/kill-all:latest",
            "/bin/sh",
            "-c",
            "sleep 30",
        ],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let kill_all = ociman(storage_dir.path(), &["kill", "--all"]);
    assert!(
        kill_all.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&kill_all.stderr)
    );

    run1.wait().unwrap();
    run2.wait().unwrap();
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "kill-all-run1",
            "stopped",
            Duration::from_secs(20)
        ),
        "stopped"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "kill-all-run2",
            "stopped",
            Duration::from_secs(20)
        ),
        "stopped"
    );
    // The never-started container should be completely untouched --
    // still `created`, not silently transitioned or errored on.
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "kill-all-created",
            "created",
            Duration::from_millis(200)
        ),
        "created"
    );

    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// Real `podman`'s own `--all` and an explicit container ID/name are
/// mutually exclusive; giving both is a clear, immediate error rather
/// than silently picking one.
#[test]
fn kill_all_with_an_explicit_id_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["kill", "--all", "some-id"]);
    assert!(!out.status.success());
}

/// `--all` with nothing running at all is a legitimate, successful
/// no-op (matches real `podman kill --all` on an empty/all-stopped
/// container list: exit 0, nothing printed), not an error.
#[test]
fn kill_all_with_no_containers_at_all_succeeds_as_a_no_op() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["kill", "--all"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
}

/// Multiple explicit ids (0317, a real, previously-unsupported gap:
/// `ociman kill` only ever accepted exactly one target before this)
/// each get signaled, and each one's own *raw* given name (not the
/// resolved canonical id) is printed on success -- matching this
/// command's own existing single-target convention, and real podman's
/// own identical `RawInput`-over-`Id` printing rule.
#[test]
fn kill_with_multiple_explicit_ids_signals_each_and_prints_the_raw_name_given() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/kill-multi:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run1 = ociman_run_detached_named(
        storage_dir.path(),
        "kill-multi-run1",
        "ociman-test/kill-multi:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let mut run2 = ociman_run_detached_named(
        storage_dir.path(),
        "kill-multi-run2",
        "ociman-test/kill-multi:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "kill-multi-run1",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "kill-multi-run2",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let kill = ociman(
        storage_dir.path(),
        &["kill", "kill-multi-run1", "kill-multi-run2"],
    );
    assert!(
        kill.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&kill.stderr)
    );
    let mut lines: Vec<String> = String::from_utf8_lossy(&kill.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    assert_eq!(
        lines,
        vec!["kill-multi-run1", "kill-multi-run2"],
        "kill should print each raw name given, not a resolved id"
    );

    run1.wait().unwrap();
    run2.wait().unwrap();
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "kill-multi-run1",
            "stopped",
            Duration::from_secs(20)
        ),
        "stopped"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "kill-multi-run2",
            "stopped",
            Duration::from_secs(20)
        ),
        "stopped"
    );

    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// An unresolvable id among several explicit targets aborts the whole
/// call before signaling *any* of them, matching real podman's own
/// identical two-phase behavior (checked directly, `getContainers`'s
/// own `default` case: a `LookupContainer` failure on any one name
/// aborts immediately).
#[test]
fn kill_with_one_nonexistent_id_among_several_aborts_before_signaling_any() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/kill-multi-bad:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run1 = ociman_run_detached_named(
        storage_dir.path(),
        "kill-multi-bad-run1",
        "ociman-test/kill-multi-bad:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "kill-multi-bad-run1",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let kill = ociman(
        storage_dir.path(),
        &[
            "kill",
            "kill-multi-bad-run1",
            "kill-multi-bad-does-not-exist",
        ],
    );
    assert!(
        !kill.status.success(),
        "an unresolvable id among several should abort the whole call"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "kill-multi-bad-run1",
            "running",
            Duration::from_millis(200)
        ),
        "running",
        "the real container must be completely untouched by the aborted call"
    );

    let real_kill = ociman(storage_dir.path(), &["kill", "kill-multi-bad-run1"]);
    assert!(real_kill.status.success());
    run1.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// Same real, reachable-`systemd --user`-session probe
/// `ociman_pause.rs`'s own tests use.
fn systemd_user_session_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
        .is_ok_and(|out| !out.stdout.is_empty())
}

/// `ociman kill` against a *still-paused* (not first unpaused) real
/// container (0319, closing `0312`'s own discovered gap): a genuinely
/// frozen cgroup *queues* a sent signal rather than delivering it at
/// all until thawed, so a plain signal send alone would report
/// success while the container silently stays alive and paused
/// forever. `ociman kill` must thaw it as part of delivering the
/// signal, not require a separate `unpause` first -- checked directly
/// against a real installed podman, which genuinely does this too
/// (`podman kill` on a paused container reports `Exited (137)`
/// afterward, not a silent no-op).
#[test]
fn kill_on_a_still_paused_container_actually_terminates_it() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/kill-pause:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached_named(
        storage_dir.path(),
        "kill-pause-target",
        "ociman-test/kill-pause:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "kill-pause-target",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let pause = ociman(storage_dir.path(), &["pause", "kill-pause-target"]);
    assert!(
        pause.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pause.stderr)
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "kill-pause-target",
            "paused",
            Duration::from_secs(5)
        ),
        "paused",
        "must genuinely be paused before killing it -- otherwise this test proves nothing"
    );

    // No `unpause` here at all -- the whole point is that `kill`
    // itself must make the signal actually take effect on a
    // still-frozen container.
    let kill = ociman(storage_dir.path(), &["kill", "kill-pause-target"]);
    assert!(
        kill.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&kill.stderr)
    );

    run.wait().unwrap();
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "kill-pause-target",
            "stopped",
            Duration::from_secs(20)
        ),
        "stopped",
        "kill on a still-paused container must actually terminate it, not silently leave it \
         alive and frozen forever"
    );

    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}
