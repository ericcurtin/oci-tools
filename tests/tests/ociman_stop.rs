//! `ociman stop` integration tests: a graceful-signal-then-`KILL`
//! policy on top of the same `oci_runtime_core::process::{kill,alive}`
//! primitives `rm --force` already uses (0021) — distinct from that
//! immediate `SIGKILL`, and distinct from `ocirun kill` (a single raw
//! signal with no wait/escalation policy at all, matching real
//! low-level runtimes' own minimal `kill` primitive).
//!
//! Same fully offline seeded-image approach `ociman_run.rs` established,
//! and the same `spawn()`+detached-stdio+poll concurrency pattern
//! `ociman_exec.rs`/`ociman_logs.rs` use for a container that needs to
//! still be running while a separate invocation acts on it.

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

/// A generous timeout: `ociman run` now attempts a real systemd cgroup
/// driver D-Bus round trip per container (`docs/design/0034`), which
/// can occasionally take noticeably longer under heavy *concurrent*
/// test-suite load — the ordinary case still resolves in milliseconds.
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

/// Same as [`wait_for_container_status`], but matches on a
/// container's own `--name` rather than its generated id -- for the
/// `--all` tests below, which need several containers running at once
/// and so name each rather than relying on `only_container_id`'s own
/// "exactly one container" assumption.
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

#[test]
fn stop_lets_a_signal_handling_container_exit_gracefully() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stop-graceful:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stop-graceful:latest",
        // A *single*, long-running foreground `sleep 30` here would be
        // a real footgun, not a flaky test: shells commonly defer
        // running a trap until the current foreground child actually
        // exits on its own (verified against a real kernel/busybox —
        // a `sleep 30` variant of this exact test took the entire
        // grace window rather than reacting to `TERM` promptly, even
        // though the trap itself was installed correctly). Looping
        // over short sleeps instead bounds how long a pending trap can
        // possibly be deferred to a fraction of a second, regardless.
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

    // A generous grace window: what actually matters here is *whether*
    // the trap gets to run at all before any `KILL` escalation, not
    // exactly how many milliseconds that takes (real OS scheduling
    // jitter, especially under a loaded host, makes any assertion on
    // elapsed wall-clock time flaky by nature -- an earlier version of
    // this test asserted `stop` returned quickly and intermittently
    // failed under host load for exactly that reason; the *exit code*
    // check below is the deterministic, meaningful assertion: a `KILL`
    // escalation would produce 137, not the trap's own `exit 0`). 60s
    // (not the original 20s) after this test was *still* observed to
    // occasionally take the entire window and escalate to `KILL` on
    // this project's own shared dev host under heavy, unrelated
    // concurrent load (a separate session's own `cargo build --release
    // -C lto=fat -C codegen-units=1`, confirmed directly via `ps` at
    // the exact time of the failure) — the normal, uncontended case
    // still finishes in milliseconds regardless of how generous this
    // ceiling is, so raising it only helps, never slows down the
    // common case.
    let stop = ociman(storage_dir.path(), &["stop", "--time", "60", &id]);
    assert!(
        stop.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop.stderr)
    );

    run.wait().unwrap();
    let ps = ociman(storage_dir.path(), &["ps", "-a", "--json"]);
    let views: serde_json::Value = serde_json::from_slice(&ps.stdout).unwrap();
    let entry = views.as_array().unwrap().iter().find(|e| e["id"] == id);
    let entry = entry.expect("container should still be listed");
    assert_eq!(entry["status"], "stopped");
    assert_eq!(
        entry["exit_code"], 0,
        "a graceful exit(0) from the TERM trap, not a KILL exit code: {entry:?}"
    );

    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn stop_escalates_to_kill_when_the_container_ignores_the_signal() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stop-escalate:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stop-escalate:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    // A plain `sleep 30` run as a pid-namespace's own init ignores an
    // unhandled-default-action `TERM` outright (0017's own finding) --
    // a real, deliberately short grace window here so this test
    // doesn't have to wait long to observe the escalation to `KILL`.
    let stop = ociman(storage_dir.path(), &["stop", "--time", "1", &id]);
    assert!(
        stop.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop.stderr)
    );

    run.wait().unwrap();
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );

    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn stop_is_a_noop_on_an_already_stopped_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stop-already-stopped:latest",
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
        &["run", "ociman-test/stop-already-stopped:latest"],
    );
    assert!(run.status.success());
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());

    let stop = ociman(storage_dir.path(), &["stop", &id]);
    assert!(
        stop.status.success(),
        "stop on an already-stopped container should be a no-op, not an error: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
}

#[test]
fn stop_of_a_nonexistent_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["stop", "does-not-exist"]);
    assert!(!out.status.success());
}

/// `ociman stop` with no explicit `--signal` honors the image's own
/// declared `STOPSIGNAL` (`docs/design/0244`) — matching real
/// `docker stop`/`podman stop`: the container's USR1 trap (its
/// distinctive `exit 43`) proves the image-declared signal, not
/// `TERM`, was what actually arrived. An explicit `--signal TERM`
/// still overrides it (the TERM trap's own `exit 21` proves that
/// side).
#[test]
fn stop_honors_the_images_declared_stopsignal() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stopsignal:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig {
            stop_signal: Some("SIGUSR1".to_string()),
            ..Default::default()
        },
    );

    // Distinct exit codes per signal: whichever trap fires tells us
    // exactly which signal was delivered.
    let script = "trap 'exit 43' USR1; trap 'exit 21' TERM; while true; do sleep 0.2; done";
    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stopsignal:latest",
        &["/bin/sh", "-c", script],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    // No --signal: the image's own STOPSIGNAL (USR1) is what lands.
    let stop = ociman(storage_dir.path(), &["stop", "--time", "60", &id]);
    assert!(
        stop.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    run.wait().unwrap();
    let ps = ociman(storage_dir.path(), &["ps", "-a", "--json"]);
    let views: serde_json::Value = serde_json::from_slice(&ps.stdout).unwrap();
    let entry = views
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == id)
        .expect("container should be listed")
        .clone();
    assert_eq!(entry["status"], "stopped");
    assert_eq!(
        entry["exit_code"], 43,
        "the USR1 trap's own code proves STOPSIGNAL was honored: {entry:?}"
    );
    ociman(storage_dir.path(), &["rm", &id]);

    // Explicit --signal TERM: overrides the image's STOPSIGNAL.
    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stopsignal:latest",
        &["/bin/sh", "-c", script],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );
    let stop = ociman(
        storage_dir.path(),
        &["stop", "--time", "60", "--signal", "TERM", &id],
    );
    assert!(
        stop.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    run.wait().unwrap();
    let ps = ociman(storage_dir.path(), &["ps", "-a", "--json"]);
    let views: serde_json::Value = serde_json::from_slice(&ps.stdout).unwrap();
    let entry = views
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == id)
        .expect("container should be listed")
        .clone();
    assert_eq!(
        entry["exit_code"], 21,
        "an explicit --signal overrides STOPSIGNAL: {entry:?}"
    );
    ociman(storage_dir.path(), &["rm", &id]);
}

/// `ociman run --stop-signal` (0300) overrides *both* the default
/// `TERM` and the image's own declared `STOPSIGNAL` -- checked
/// directly against real `podman run --stop-signal`/`docker run
/// --stop-signal`: a `run`/`create`-time override persists on the
/// container record itself, taking precedence over the image's own
/// config for every later `stop` given no `--signal` of its own,
/// exactly matching the same precedence order
/// `stop_honors_the_images_declared_stopsignal` already exercises one
/// level up.
#[test]
fn run_stop_signal_overrides_the_images_declared_stopsignal() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/run-stop-signal:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig {
            stop_signal: Some("SIGUSR1".to_string()),
            ..Default::default()
        },
    );

    // Distinct exit codes per signal: whichever trap fires tells us
    // exactly which signal was delivered. The image declares USR1 as
    // its own STOPSIGNAL, but --stop-signal USR2 should win instead.
    let script = "trap 'exit 43' USR1; trap 'exit 62' USR2; while true; do sleep 0.2; done";
    let mut run = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "-d",
            "--stop-signal",
            "SIGUSR2",
            "ociman-test/run-stop-signal:latest",
            "/bin/sh",
            "-c",
            script,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman run");
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    // Inspect reports the effective, persisted-override signal, not
    // the image's own STOPSIGNAL.
    let inspect = ociman(storage_dir.path(), &["inspect", &id]);
    assert!(inspect.status.success());
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(view["stop_signal"], "SIGUSR2");

    // No --signal at stop time: the persisted --stop-signal override
    // (USR2) wins over the image's own declared STOPSIGNAL (USR1).
    let stop = ociman(storage_dir.path(), &["stop", "--time", "60", &id]);
    assert!(
        stop.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    run.wait().unwrap();
    let ps = ociman(storage_dir.path(), &["ps", "-a", "--json"]);
    let views: serde_json::Value = serde_json::from_slice(&ps.stdout).unwrap();
    let entry = views
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == id)
        .expect("container should be listed")
        .clone();
    assert_eq!(entry["status"], "stopped");
    assert_eq!(
        entry["exit_code"], 62,
        "the USR2 trap's own code proves --stop-signal overrode the image's STOPSIGNAL: {entry:?}"
    );
    ociman(storage_dir.path(), &["rm", &id]);
}

/// An invalid `--stop-signal` at `run`/`create` time is a real, clear,
/// upfront error -- matching real podman's own checked-directly
/// spec-generation-time validation (see `ANNOTATION_STOP_SIGNAL`'s own
/// doc comment) -- rather than only surfacing much later at the first
/// real `stop`. No container is created at all: `ps -a` afterward is
/// empty.
#[test]
fn run_with_an_unparsable_stop_signal_fails_fast_and_creates_nothing() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/bad-stop-signal:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--stop-signal",
            "NOTASIGNAL",
            "ociman-test/bad-stop-signal:latest",
            "/bin/sh",
        ],
    );
    assert!(!create.status.success());
    assert!(
        String::from_utf8_lossy(&create.stderr).contains("NOTASIGNAL"),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(
        String::from_utf8_lossy(&ps.stdout).trim().is_empty(),
        "no container should have been created at all"
    );
}

/// `ociman run --stop-timeout` (0301) is honored when a later `stop`
/// gives no `--time` of its own -- checked directly against real
/// `podman run --stop-timeout`/`docker run --stop-timeout` and their
/// own real CLI-level precedence (`~/git/podman/cmd/podman/
/// containers/stop.go`: an explicit `--time` always wins, but with
/// none given, the persisted per-container value is used instead of
/// the plain `10`-second default). A container that ignores `TERM`
/// entirely, given a short persisted `--stop-timeout`, must have been
/// force-killed well before the plain default `10`-second window
/// would have elapsed -- proving the persisted value, not the
/// default, actually governed the wait.
#[test]
fn run_stop_timeout_is_honored_when_stop_gives_no_explicit_time() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/run-stop-timeout:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "-d",
            "--stop-timeout",
            "1",
            "ociman-test/run-stop-timeout:latest",
            "/bin/sh",
            "-c",
            "trap '' TERM; sleep 300",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman run");
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let inspect = ociman(storage_dir.path(), &["inspect", &id]);
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(view["stop_timeout"], 1);

    let before = Instant::now();
    let stop = ociman(storage_dir.path(), &["stop", &id]);
    let elapsed = before.elapsed();
    assert!(
        stop.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    run.wait().unwrap();
    assert!(
        elapsed < Duration::from_secs(8),
        "should have escalated to KILL around the persisted 1s timeout, not the plain 10s \
         default: took {elapsed:?}"
    );

    let ps = ociman(storage_dir.path(), &["ps", "-a", "--json"]);
    let views: serde_json::Value = serde_json::from_slice(&ps.stdout).unwrap();
    let entry = views
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == id)
        .expect("container should be listed")
        .clone();
    assert_eq!(entry["status"], "stopped");
    assert_eq!(
        entry["exit_code"], 137,
        "a real SIGKILL exit code: {entry:?}"
    );
    ociman(storage_dir.path(), &["rm", &id]);
}

/// An explicit `ociman stop --time` still overrides a persisted
/// `run --stop-timeout`, exactly the same "explicit call-time value
/// always wins" precedence `run_stop_signal_overrides_the_images_
/// declared_stopsignal` already exercises for `--signal` -- checked
/// directly against real podman's own identical rule.
#[test]
fn stop_explicit_time_overrides_the_persisted_stop_timeout() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stop-timeout-override:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "-d",
            "--stop-timeout",
            "60",
            "ociman-test/stop-timeout-override:latest",
            "/bin/sh",
            "-c",
            "trap '' TERM; sleep 300",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman run");
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let before = Instant::now();
    let stop = ociman(storage_dir.path(), &["stop", "--time", "1", &id]);
    let elapsed = before.elapsed();
    assert!(
        stop.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    run.wait().unwrap();
    assert!(
        elapsed < Duration::from_secs(8),
        "an explicit --time 1 should override the persisted 60s --stop-timeout: took {elapsed:?}"
    );
    ociman(storage_dir.path(), &["rm", &id]);
}

/// `--all` (0313) matches real `podman stop --all` exactly: every
/// running container is stopped, and a container that was `create`d
/// but never `start`ed (so has no live process to signal at all) is
/// silently tolerated -- still printed, no error -- rather than
/// erroring, checked directly against a real installed `podman stop
/// --all` in the same situation.
#[test]
fn stop_all_stops_every_running_container_and_tolerates_a_never_started_one() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stop-all:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run1 = ociman_run_detached_named(
        storage_dir.path(),
        "stop-all-run1",
        "ociman-test/stop-all:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let mut run2 = ociman_run_detached_named(
        storage_dir.path(),
        "stop-all-run2",
        "ociman-test/stop-all:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stop-all-run1",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stop-all-run2",
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
            "stop-all-created",
            "ociman-test/stop-all:latest",
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

    let stop_all = ociman(storage_dir.path(), &["stop", "--time", "1", "--all"]);
    assert!(
        stop_all.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop_all.stderr)
    );

    run1.wait().unwrap();
    run2.wait().unwrap();
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stop-all-run1",
            "stopped",
            Duration::from_secs(20)
        ),
        "stopped"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stop-all-run2",
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
            "stop-all-created",
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
fn stop_all_with_an_explicit_id_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["stop", "--all", "some-id"]);
    assert!(!out.status.success());
}

/// `--all` with nothing to stop at all is a legitimate, successful
/// no-op (matches real `podman stop --all` on an empty/all-stopped
/// container list: exit 0, nothing printed), not an error.
#[test]
fn stop_all_with_no_containers_at_all_succeeds_as_a_no_op() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["stop", "--all"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
}

/// `stop` on a genuinely paused container now really works (0324,
/// closing the single most-repeated "still ahead" item across six
/// consecutive design notes, 0313/0315-0320): unlike real podman's
/// own `stop`/`restart`, which both deliberately refuse a paused
/// container outright (`libpod/container_internal.go`'s own
/// `stopInternal` excludes `ContainerStatePaused` from its own
/// allowed-to-attempt state set entirely — checked directly, and
/// confirmed empirically: both real `podman stop`/`podman restart` on
/// a paused container are a real, immediate error), this project's
/// own `stop` genuinely thaws the container as part of delivering its
/// first signal (the same real primitive `kill` itself already uses,
/// 0319) and then proceeds exactly as it would for any other running
/// container — no `unpause` step required first.
#[test]
fn stop_on_a_paused_container_genuinely_thaws_and_stops_it() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stop-paused:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stop-paused:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let pause = ociman(storage_dir.path(), &["pause", &id]);
    assert!(
        pause.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pause.stderr)
    );
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "paused", Duration::from_secs(5)),
        "paused"
    );

    // No `unpause` here at all -- `stop` itself must make the signal
    // actually take effect on a still-frozen container.
    let stop = ociman(storage_dir.path(), &["stop", "--time", "1", &id]);
    assert!(
        stop.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop.stderr)
    );

    run.wait().unwrap();
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped",
        "stop on a paused container must actually terminate it, not silently leave it alive \
         and frozen forever"
    );

    ociman(storage_dir.path(), &["rm", &id]);
}

/// `restart` on a genuinely paused container also now really works
/// (0324): `cmd_restart` shares `stop_container` unchanged, so this
/// fix applies to both with no `restart`-specific code at all.
#[test]
fn restart_on_a_paused_container_genuinely_thaws_and_restarts_it() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/restart-paused:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "-d",
            "ociman-test/restart-paused:latest",
            "/bin/sh",
            "-c",
            "sleep 30",
        ],
    );
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let id = String::from_utf8_lossy(&run.stdout).trim().to_string();
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );
    let first_pid = ociman(storage_dir.path(), &["inspect", &id, "--json"]);
    let first_pid: serde_json::Value = serde_json::from_slice(&first_pid.stdout).unwrap();
    let first_pid = first_pid["pid"].as_i64().expect("a real pid");

    let pause = ociman(storage_dir.path(), &["pause", &id]);
    assert!(pause.status.success());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "paused", Duration::from_secs(5)),
        "paused"
    );

    // No `unpause` here at all either.
    let restart = ociman(storage_dir.path(), &["restart", "--time", "1", &id]);
    assert!(
        restart.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&restart.stderr)
    );
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running",
        "restart on a paused container must leave it genuinely running again"
    );
    let second_pid = ociman(storage_dir.path(), &["inspect", &id, "--json"]);
    let second_pid: serde_json::Value = serde_json::from_slice(&second_pid.stdout).unwrap();
    let second_pid = second_pid["pid"].as_i64().expect("a real pid");
    assert_ne!(
        first_pid, second_pid,
        "restart should have replaced the container's own process with a new one"
    );

    ociman(storage_dir.path(), &["stop", "--time", "0", &id]);
    ociman(storage_dir.path(), &["rm", "-f", &id]);
}

/// Multiple explicit ids (0318, a real, previously-unsupported gap:
/// `ociman stop` only ever accepted exactly one target before this,
/// unlike real podman's own `stop [options] CONTAINER
/// [CONTAINER...]`) each get stopped, printing each one's own raw
/// given name.
#[test]
fn stop_with_multiple_explicit_ids_stops_each() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stop-multi:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run1 = ociman_run_detached_named(
        storage_dir.path(),
        "stop-multi-run1",
        "ociman-test/stop-multi:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let mut run2 = ociman_run_detached_named(
        storage_dir.path(),
        "stop-multi-run2",
        "ociman-test/stop-multi:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stop-multi-run1",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stop-multi-run2",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let stop = ociman(
        storage_dir.path(),
        &["stop", "--time", "1", "stop-multi-run1", "stop-multi-run2"],
    );
    assert!(
        stop.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let mut lines: Vec<String> = String::from_utf8_lossy(&stop.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    assert_eq!(lines, vec!["stop-multi-run1", "stop-multi-run2"]);

    run1.wait().unwrap();
    run2.wait().unwrap();
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stop-multi-run1",
            "stopped",
            Duration::from_secs(20)
        ),
        "stopped"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stop-multi-run2",
            "stopped",
            Duration::from_secs(20)
        ),
        "stopped"
    );

    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// An unresolvable id among several explicit targets aborts the whole
/// call before stopping *any* of them, matching real podman's own
/// identical two-phase behavior for a plain multi-id `stop` (checked
/// directly, `getContainers`'s own `default` case).
#[test]
fn stop_with_one_nonexistent_id_among_several_aborts_before_stopping_any() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stop-multi-bad:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run1 = ociman_run_detached_named(
        storage_dir.path(),
        "stop-multi-bad-run1",
        "ociman-test/stop-multi-bad:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stop-multi-bad-run1",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let stop = ociman(
        storage_dir.path(),
        &[
            "stop",
            "--time",
            "1",
            "stop-multi-bad-run1",
            "stop-multi-bad-does-not-exist",
        ],
    );
    assert!(
        !stop.status.success(),
        "an unresolvable id among several should abort the whole call"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stop-multi-bad-run1",
            "running",
            Duration::from_millis(200)
        ),
        "running",
        "the real container must be completely untouched by the aborted call"
    );

    // `--ignore` tolerates the unresolvable one instead.
    let stop_ignore = ociman(
        storage_dir.path(),
        &[
            "stop",
            "--time",
            "1",
            "--ignore",
            "stop-multi-bad-run1",
            "stop-multi-bad-does-not-exist",
        ],
    );
    assert!(
        stop_ignore.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop_ignore.stderr)
    );
    run1.wait().unwrap();
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stop-multi-bad-run1",
            "stopped",
            Duration::from_secs(20)
        ),
        "stopped"
    );

    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// `--cidfile` (0318) matches real `podman stop --cidfile` exactly:
/// the file's own first line only, trailing content ignored, merged
/// into the same target list an explicit `ID`/`--name` argument
/// already builds -- same technique `ociman_ps.rs`'s own
/// `rm_cidfile_reads_the_container_id_from_a_file_and_ignores_
/// trailing_content` already established.
#[test]
fn stop_cidfile_reads_the_container_id_from_a_file_and_ignores_trailing_content() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stop-cidfile:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached_named(
        storage_dir.path(),
        "stop-cidfile-target",
        "ociman-test/stop-cidfile:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stop-cidfile-target",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let cidfile = storage_dir.path().join("cid.txt");
    std::fs::write(&cidfile, "stop-cidfile-target\ngarbage second line").unwrap();

    let stop = ociman(
        storage_dir.path(),
        &[
            "stop",
            "--time",
            "1",
            "--cidfile",
            cidfile.to_str().unwrap(),
        ],
    );
    assert!(
        stop.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&stop.stdout).trim(),
        "stop-cidfile-target"
    );

    run.wait().unwrap();
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "stop-cidfile-target",
            "stopped",
            Duration::from_secs(20)
        ),
        "stopped"
    );

    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// Real `podman stop`'s own `--cidfile` and `--all` are mutually
/// exclusive, matching `rm`/`restart`'s own identical rule.
#[test]
fn stop_all_and_cidfile_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let cidfile = storage_dir.path().join("cid.txt");
    std::fs::write(&cidfile, "some-id").unwrap();
    let out = ociman(
        storage_dir.path(),
        &["stop", "--all", "--cidfile", cidfile.to_str().unwrap()],
    );
    assert!(!out.status.success());
}

/// A cidfile that can't be read at all is a hard error without
/// `--ignore`, but a silent, successful no-op *with* `--ignore` and
/// nothing else given -- matching real podman's own identical
/// checked-directly behavior exactly (0318).
#[test]
fn stop_cidfile_that_cannot_be_read_is_a_hard_error_unless_ignored() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(
        storage_dir.path(),
        &["stop", "--cidfile", "/does/not/exist/at/all"],
    );
    assert!(!out.status.success());

    let out_ignored = ociman(
        storage_dir.path(),
        &["stop", "--ignore", "--cidfile", "/does/not/exist/at/all"],
    );
    assert!(
        out_ignored.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out_ignored.stderr)
    );
    assert!(out_ignored.stdout.is_empty());
}

/// Giving nothing at all (no ids, no `--cidfile`, no `--all`) is
/// always a clear error, `--ignore` or not -- `--ignore` alone doesn't
/// satisfy the "you must give something" rule.
#[test]
fn stop_with_nothing_given_at_all_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["stop"]);
    assert!(!out.status.success());
    let out_ignore = ociman(storage_dir.path(), &["stop", "--ignore"]);
    assert!(!out_ignore.status.success());
}
