//! `ociman wait` integration tests: block until a container stops,
//! then print its exit code — matching real `docker wait`/`podman
//! wait` exactly (`~/git/podman/cmd/podman/containers/wait.go`: block,
//! then print a bare exit-code integer, nothing else). The exit code
//! itself is whatever `cmd_run`'s own foreground wait already
//! recorded (`ANNOTATION_EXIT_CODE`) — `wait` adds no new state of
//! its own.
//!
//! Same fully offline seeded-image approach `ociman_run.rs`
//! established, and the same `spawn()`+detached-stdio+poll concurrency
//! pattern `ociman_stop.rs`/`ociman_kill.rs` use for a container that
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
fn wait_blocks_until_the_container_exits_then_prints_its_real_exit_code() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/wait-basic:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/wait-basic:latest",
        &["/bin/sh", "-c", "sleep 1; exit 7"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let started = Instant::now();
    let wait = ociman(storage_dir.path(), &["wait", &id]);
    let elapsed = started.elapsed();
    assert!(
        wait.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wait.stderr)
    );
    assert!(
        elapsed >= Duration::from_millis(500),
        "wait should have genuinely blocked until the container exited: {elapsed:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&wait.stdout).trim(),
        "7",
        "wait should print the real exit code"
    );

    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn wait_on_an_already_stopped_container_returns_immediately() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/wait-already-stopped:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 42".to_string(),
            ]),
            ..Default::default()
        },
    );

    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/wait-already-stopped:latest"],
    );
    // `ociman run`'s own exit code mirrors the container's real exit
    // code (42 here), not a plain success/failure boolean -- matching
    // real `podman run`/`ocirun run`.
    assert_eq!(run.status.code(), Some(42));
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());

    let started = Instant::now();
    let wait = ociman(storage_dir.path(), &["wait", &id]);
    let elapsed = started.elapsed();
    assert!(
        wait.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wait.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "wait on an already-stopped container should return immediately: {elapsed:?}"
    );
    assert_eq!(String::from_utf8_lossy(&wait.stdout).trim(), "42");
}

#[test]
fn wait_on_a_nonexistent_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["wait", "does-not-exist"]);
    assert!(!out.status.success());
}

/// `ociman wait` with multiple containers (0190): each one's own real
/// exit code is printed on its own line, in the exact order given —
/// matching real `docker wait`/`podman wait` exactly.
#[test]
fn wait_on_multiple_containers_prints_each_exit_code_in_order() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/wait-multi:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let run1 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "wait-multi-1",
            "ociman-test/wait-multi:latest",
            "/bin/sh",
            "-c",
            "exit 3",
        ],
    );
    assert_eq!(run1.status.code(), Some(3));
    let run2 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "wait-multi-2",
            "ociman-test/wait-multi:latest",
            "/bin/sh",
            "-c",
            "exit 5",
        ],
    );
    assert_eq!(run2.status.code(), Some(5));

    let wait = ociman(
        storage_dir.path(),
        &["wait", "wait-multi-1", "wait-multi-2"],
    );
    assert!(
        wait.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wait.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&wait.stdout).trim(),
        "3\n5",
        "each container's own exit code should print on its own line, in the order given"
    );

    ociman(storage_dir.path(), &["rm", "wait-multi-1"]);
    ociman(storage_dir.path(), &["rm", "wait-multi-2"]);
}

/// `ociman wait --ignore` (0190): a nonexistent container prints `-1`
/// instead of erroring — matching real `docker wait --ignore`/`podman
/// wait --ignore` exactly.
#[test]
fn wait_ignore_prints_negative_one_for_a_nonexistent_container() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(
        storage_dir.path(),
        &["wait", "--ignore", "does-not-exist-at-all"],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "-1");
}

/// `ociman wait` without `--ignore`, given a mix of a real container
/// and a nonexistent one: the whole command fails immediately, with
/// nothing printed for *any* container, even the one that does exist
/// — matching real podman's own checked-directly fail-fast behavior
/// exactly (every name is resolved up front, before any waiting
/// begins at all).
#[test]
fn wait_without_ignore_fails_fast_before_printing_anything_for_a_valid_container_too() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/wait-failfast:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "wait-failfast-valid",
            "ociman-test/wait-failfast:latest",
            "/bin/sh",
            "-c",
            "exit 0",
        ],
    );
    assert!(run.status.success());

    let wait = ociman(
        storage_dir.path(),
        &["wait", "wait-failfast-valid", "does-not-exist-either"],
    );
    assert!(!wait.status.success());
    assert_eq!(
        String::from_utf8_lossy(&wait.stdout),
        "",
        "nothing should be printed for any container once one name fails to resolve"
    );

    ociman(storage_dir.path(), &["rm", "wait-failfast-valid"]);
}

/// `ociman wait --condition running` (0190): matches real `docker wait
/// --condition`/`podman wait --condition` exactly, including always
/// printing `-1` for a condition other than `stopped`/`exited` (never
/// a real exit code, checked directly against real podman).
#[test]
fn wait_condition_running_returns_immediately_with_negative_one() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/wait-condition-running:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/wait-condition-running:latest",
        &["/bin/sh", "-c", "sleep 5"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let started = Instant::now();
    let wait = ociman(storage_dir.path(), &["wait", "--condition", "running", &id]);
    let elapsed = started.elapsed();
    assert!(
        wait.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wait.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "already-running should satisfy --condition running immediately: {elapsed:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&wait.stdout).trim(),
        "-1",
        "a non-stopped condition should never print a real exit code"
    );

    ociman(storage_dir.path(), &["kill", &id]);
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

/// An unsupported `--condition` value is a clear, immediate error —
/// this project's own simpler container lifecycle has no equivalent
/// of real podman's own `configured`/`removing`/`stopping`/`unknown`
/// states or `healthy`/`unhealthy` healthcheck conditions.
#[test]
fn wait_condition_unsupported_value_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(
        storage_dir.path(),
        &["wait", "--condition", "healthy", "does-not-matter"],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unsupported wait condition"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `ociman wait --latest`/`-l` (matching real `podman wait --latest`
/// exactly) waits on the single, real most-recently-*created*
/// container -- an earlier container's own, genuinely different exit
/// code must never be printed.
#[test]
fn wait_latest_prints_the_most_recently_created_containers_own_exit_code() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/wait-latest:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    // `ociman run`'s own exit code mirrors the container's real exit
    // code (matching real podman/docker exactly), so a deliberately
    // nonzero one here is not itself a test failure -- only the
    // exact code value is checked, via `wait` below.
    let older = ociman(
        storage_dir.path(),
        &[
            "run",
            "ociman-test/wait-latest:latest",
            "/bin/sh",
            "-c",
            "exit 5",
        ],
    );
    assert_eq!(older.status.code(), Some(5), "{older:?}");

    // A real, distinguishable creation-time gap.
    std::thread::sleep(Duration::from_secs(2));

    let newer = ociman(
        storage_dir.path(),
        &[
            "run",
            "ociman-test/wait-latest:latest",
            "/bin/sh",
            "-c",
            "exit 9",
        ],
    );
    assert_eq!(newer.status.code(), Some(9), "{newer:?}");

    let wait = ociman(storage_dir.path(), &["wait", "--latest"]);
    assert!(
        wait.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wait.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&wait.stdout).trim(),
        "9",
        "--latest must print the most recently created container's own exit code"
    );
}

/// `--latest` and an explicit container together is a real, immediate
/// error, matching real podman's own exact wording.
#[test]
fn wait_latest_and_explicit_id_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["wait", "--latest", "some-id"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("--latest and containers cannot be used together"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Neither `--latest` nor an explicit container at all is a real,
/// immediate error.
#[test]
fn wait_with_no_container_and_no_latest_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["wait"]);
    assert!(!out.status.success());
}

/// `wait --latest` on a genuinely empty store is a real, clear error,
/// matching real `podman wait --latest`'s own `ErrNoSuchCtr`.
#[test]
fn wait_latest_on_an_empty_store_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(storage_dir.path(), &["wait", "--latest"]);
    assert!(!out.status.success());
}

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

/// `wait --exit-first-match` (`docs/design/0538`), matching real
/// `podman wait --exit-first-match` exactly: given two genuinely
/// still-running containers with very different completion times,
/// only the *first* one's own real exit code is ever printed, well
/// before the slower one would otherwise finish -- proven here by a
/// real wall-clock bound tighter than the slow container's own sleep
/// duration, and exactly one line of output regardless of how many
/// containers were given.
///
/// The slow container's own sleep is deliberately *very* long (not
/// just "a few seconds longer" than the fast one): each container's
/// own transition to `Stopped` is only recorded once its own
/// detached *keeper* process (see `docs/design/0154`'s own doc
/// comment) gets scheduled to actually finalize that write, a real,
/// already-documented source of variable latency under heavy *host*
/// contention (not a bug in `--exit-first-match` itself) -- a merely
/// few-seconds gap between the two containers' own sleep durations
/// was observed to occasionally flip under genuinely heavy concurrent
/// load on this project's own shared dev host (several independent
/// `opencode` sessions each running their own full test suites at
/// once), even though the race mechanism itself was working
/// correctly. A large enough gap makes that real scheduling variance
/// irrelevant to this test's own result.
#[test]
fn wait_exit_first_match_prints_only_the_first_containers_own_exit_code() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/wait-exit-first:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut slow = ociman_run_detached_named(
        storage_dir.path(),
        "wait-exit-first-slow",
        "ociman-test/wait-exit-first:latest",
        &["/bin/sh", "-c", "sleep 60; exit 3"],
    );
    let mut fast = ociman_run_detached_named(
        storage_dir.path(),
        "wait-exit-first-fast",
        "ociman-test/wait-exit-first:latest",
        &["/bin/sh", "-c", "sleep 1; exit 7"],
    );
    // Only the *slow* container's own "running" status is actually
    // load-bearing for this test's own premise (a real race against
    // something genuinely still running for a full 60s) -- the fast
    // one is deliberately *not* asserted to still be "running" at
    // this exact instant: under enough host contention, its own 1s
    // sleep could already be done (and its own state fully finalized)
    // by the time this check runs, which is still a perfectly valid
    // -- if less interesting -- exercise of the exact same race
    // (`--exit-first-match` still needs to detect an already-matched
    // container and win immediately either way).
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "wait-exit-first-slow",
            "running",
            Duration::from_secs(20),
        ),
        "running"
    );

    let started = Instant::now();
    let wait = ociman(
        storage_dir.path(),
        &[
            "wait",
            "--exit-first-match",
            "wait-exit-first-slow",
            "wait-exit-first-fast",
        ],
    );
    let elapsed = started.elapsed();
    assert!(
        wait.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wait.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&wait.stdout).trim(),
        "7",
        "the fast container's own exit code should win the race"
    );
    assert!(
        elapsed < Duration::from_secs(30),
        "--exit-first-match should return well before the slow container's own 60s sleep: \
         {elapsed:?}"
    );

    let _ = ociman(storage_dir.path(), &["kill", "wait-exit-first-slow"]);
    slow.wait().ok();
    fast.wait().ok();
}

/// Real `ContainerWait`'s own identical "a container already resolved
/// to nonexistent under `--ignore` never enters the race at all"
/// behavior -- but if *every* given target is skipped that way, this
/// project deliberately does not reproduce real podman's own checked-
/// directly hang in that exact case (see `Command::Wait::
/// exit_first_match`'s own doc comment); this must return almost
/// immediately with `-1`, not block at all.
#[test]
fn wait_exit_first_match_with_only_ignored_nonexistent_containers_prints_negative_one() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let started = Instant::now();
    let wait = ociman(
        storage_dir.path(),
        &[
            "wait",
            "--exit-first-match",
            "--ignore",
            "no-such-container-1",
            "no-such-container-2",
        ],
    );
    let elapsed = started.elapsed();
    assert!(
        wait.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wait.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&wait.stdout).trim(), "-1");
    assert!(
        elapsed < Duration::from_secs(2),
        "must never hang, matching the non-racing path's own identical immediate '-1': \
         {elapsed:?}"
    );
}

/// A real target combined with an `--ignore`d nonexistent one: the
/// nonexistent one never enters the race at all, but the real one
/// still does, and its own exit code is what gets printed.
#[test]
fn wait_exit_first_match_ignores_a_nonexistent_container_and_waits_for_the_real_one() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/wait-exit-first-mixed:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );
    let mut run = ociman_run_detached_named(
        storage_dir.path(),
        "wait-exit-first-mixed-real",
        "ociman-test/wait-exit-first-mixed:latest",
        &["/bin/sh", "-c", "sleep 1; exit 9"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "wait-exit-first-mixed-real",
            "running",
            Duration::from_secs(20),
        ),
        "running"
    );

    let wait = ociman(
        storage_dir.path(),
        &[
            "wait",
            "--exit-first-match",
            "--ignore",
            "no-such-container",
            "wait-exit-first-mixed-real",
        ],
    );
    assert!(
        wait.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wait.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&wait.stdout).trim(), "9");

    run.wait().ok();
}

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
