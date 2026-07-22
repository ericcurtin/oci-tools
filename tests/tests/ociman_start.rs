//! `ociman start`/`ociman restart` integration tests (0154): re-running
//! an already-`Stopped` container's own already-on-disk bundle exactly
//! as `run` originally left it (`start`), and `stop`-then-`start`
//! (`restart`).
//!
//! Same fully offline seeded-image approach `ociman_run.rs` established.
//!
//! A real, previously-hit race is specifically covered here (not just
//! hypothesized): `stop_container` (shared by `cmd_stop` and
//! `cmd_restart`) can observe a container as "stopped" purely because
//! its own recorded pid is no longer alive, even while its own
//! detached *keeper* process has not yet finished writing the final
//! `Stopped` state to disk. Proceeding to launch a brand new container
//! immediately in that case let the *old* keeper's own delayed
//! terminal write silently clobber the *new* one's fresh `Creating`/
//! `Running` state moments later — `restart_reruns_the_container_a_
//! third_time` below reproduced this quite reliably (well over half of
//! repeated runs) before the fix.

use std::io::Write as _;
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

fn inspect_json(storage_root: &Path, id: &str) -> serde_json::Value {
    let out = ociman(storage_root, &["inspect", id, "--json"]);
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("inspect --json output was not valid JSON: {e}"))
}

fn wait_for_status(storage_root: &Path, id: &str, want: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let status = inspect_json(storage_root, id)["status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == want || Instant::now() >= deadline {
            return status;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A container whose own command appends one line to `/marker.txt`
/// each time it actually runs, then exits immediately — deliberately
/// fast (rather than long-running), both to exercise the exact
/// pid-dies-before-its-keeper-finalizes race the 0154 fix addresses,
/// and to make counting real executions via the marker file's own
/// line count a simple, unambiguous assertion.
fn seed_marker_image(store: &Store, reference: &str, busybox: &Path) {
    seed_image(
        store,
        reference,
        busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hi >> /marker.txt; exit 0".to_string(),
            ]),
            ..Default::default()
        },
    );
}

fn marker_contents(storage_root: &Path, id: &str) -> String {
    let rootfs = inspect_json(storage_root, id)["rootfs"]
        .as_str()
        .expect("inspect --json should report rootfs")
        .to_string();
    std::fs::read_to_string(Path::new(&rootfs).join("marker.txt")).unwrap_or_default()
}

#[test]
fn start_reruns_an_already_stopped_container() {
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
    seed_marker_image(&store, "ociman-test/start-basic:latest", &busybox);

    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/start-basic:latest"],
    );
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );
    assert_eq!(marker_contents(storage_dir.path(), &id), "hi\n");

    let start = ociman(storage_dir.path(), &["start", &id]);
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );
    assert_eq!(
        marker_contents(storage_dir.path(), &id),
        "hi\nhi\n",
        "start should have run the same container's own command a second time"
    );

    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn restart_reruns_an_already_stopped_container_a_third_time() {
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
    seed_marker_image(&store, "ociman-test/restart-basic:latest", &busybox);

    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/restart-basic:latest"],
    );
    assert!(run.status.success());
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );

    let start = ociman(storage_dir.path(), &["start", &id]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );
    assert_eq!(marker_contents(storage_dir.path(), &id), "hi\nhi\n");

    // `restart` on an already-stopped container: matches real
    // podman's own `restartWithTimeout` (stop only if actually
    // running, start regardless).
    let restart = ociman(storage_dir.path(), &["restart", &id]);
    assert!(
        restart.status.success(),
        "{}",
        String::from_utf8_lossy(&restart.stderr)
    );
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );
    assert_eq!(
        marker_contents(storage_dir.path(), &id),
        "hi\nhi\nhi\n",
        "restart should have run the container a third time"
    );

    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn restart_stops_a_running_container_before_starting_it_again() {
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
        "ociman-test/restart-running:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "-d",
            "ociman-test/restart-running:latest",
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
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );
    let first_pid = inspect_json(storage_dir.path(), &id)["pid"]
        .as_i64()
        .expect("running container should report a real pid");

    let restart = ociman(storage_dir.path(), &["restart", "--time", "1", &id]);
    assert!(
        restart.status.success(),
        "{}",
        String::from_utf8_lossy(&restart.stderr)
    );
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running",
        "restart should leave a freshly-restarted long-running container running"
    );
    let second_pid = inspect_json(storage_dir.path(), &id)["pid"]
        .as_i64()
        .expect("running container should report a real pid");
    assert_ne!(
        first_pid, second_pid,
        "restart should have replaced the container's own process with a new one"
    );

    ociman(storage_dir.path(), &["stop", "--time", "0", &id]);
    ociman(storage_dir.path(), &["rm", "-f", &id]);
}

#[test]
fn start_on_an_already_running_container_is_a_clear_error() {
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
        "ociman-test/start-already-running:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "-d",
            "ociman-test/start-already-running:latest",
            "/bin/sh",
            "-c",
            "sleep 30",
        ],
    );
    assert!(run.status.success());
    let id = String::from_utf8_lossy(&run.stdout).trim().to_string();
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let start = ociman(storage_dir.path(), &["start", &id]);
    assert!(!start.status.success());

    ociman(storage_dir.path(), &["stop", "--time", "0", &id]);
    ociman(storage_dir.path(), &["rm", "-f", &id]);
}

#[test]
fn start_of_a_nonexistent_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["start", "does-not-exist"]);
    assert!(!out.status.success());
}

#[test]
fn restart_of_a_nonexistent_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["restart", "does-not-exist"]);
    assert!(!out.status.success());
}

/// `ociman start -a`/`--attach` (0186): streams the container's own
/// live output to stdout and blocks, this command's own exit code
/// then becoming the container's own real, nonzero exit code —
/// matching real `docker start -a`/`podman start -a` exactly (checked
/// directly). Deliberately never prints the container id, unlike the
/// non-attach case.
#[test]
fn start_attach_streams_output_and_propagates_exit_code() {
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
        "ociman-test/start-attach:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hello-from-start-attach; exit 7".to_string(),
            ]),
            ..Default::default()
        },
    );

    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/start-attach:latest"],
    );
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();
    assert!(!id.is_empty());

    let start = ociman(storage_dir.path(), &["start", "--attach", &id]);
    assert_eq!(
        start.status.code(),
        Some(7),
        "start --attach's own exit code should be the container's own real exit code; \
         stderr: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&start.stdout),
        "hello-from-start-attach\n",
        "start --attach should stream the container's own live output, and never print \
         the container id"
    );
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );

    ociman(storage_dir.path(), &["rm", &id]);
}

/// Without `--attach`, `ociman start` keeps its own existing, unchanged
/// behavior: print only the container id, exit `0` regardless of the
/// container's own eventual exit code.
#[test]
fn start_without_attach_still_only_prints_the_container_id() {
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
        "ociman-test/start-no-attach:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo should-not-appear-on-stdout; exit 3".to_string(),
            ]),
            ..Default::default()
        },
    );

    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/start-no-attach:latest"],
    );
    assert!(create.status.success());
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();
    assert!(!id.is_empty());

    let start = ociman(storage_dir.path(), &["start", &id]);
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&start.stdout).trim(),
        id,
        "non-attach start should print only the container id"
    );
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );

    ociman(storage_dir.path(), &["rm", &id]);
}

/// Poll `ociman ps -a -q` until `id` is no longer listed at all —
/// distinct from [`wait_for_status`], which needs the container to
/// still exist (`ociman inspect` would itself fail on a genuinely
/// removed one).
fn wait_until_removed(storage_root: &Path, id: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let out = ociman(storage_root, &["ps", "-a", "-q"]);
        let still_present = String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line == id);
        if !still_present || Instant::now() >= deadline {
            return !still_present;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A real, previously-hit bug (0158, found and fixed before it could
/// ship alongside `ociman create --rm`, which would otherwise have hit
/// it immediately): `restart`'s own internal `stop` is not a real,
/// final stop, but a `--rm` container's own detached keeper process
/// has no way to know that on its own -- left unfixed, it would
/// auto-remove the whole container the moment that internal stop makes
/// its process exit, and `restart`'s own subsequent re-launch attempt
/// would then fail with "container does not exist" (reproduced
/// directly before the fix). A real, final `ociman stop` on the same
/// container afterward should still remove it, exactly like a `--rm`
/// container that was never restarted at all.
#[test]
fn restart_does_not_auto_remove_a_rm_container_but_a_later_real_stop_still_does() {
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
        "ociman-test/restart-rm:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "-d",
            "--rm",
            "ociman-test/restart-rm:latest",
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
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let restart = ociman(storage_dir.path(), &["restart", "--time", "1", &id]);
    assert!(
        restart.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&restart.stderr)
    );
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running",
        "restart's own internal stop must not have auto-removed a --rm container"
    );

    let stop = ociman(storage_dir.path(), &["stop", "--time", "0", &id]);
    assert!(
        stop.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    // A real, tight bound (0159), not just "eventually within a
    // generous window": a real, previously-hit performance bug (found
    // while fixing the auto-removal race above) made this take several
    // real seconds -- `stop_container`'s own `reset_failed_systemd_
    // scope` call left a background D-Bus thread of its own still
    // potentially alive at the exact moment `restart`'s internal
    // `cmd_start` half forked its brand new keeper, corrupting that
    // new keeper's own subsequent systemd scope creation until its own
    // ~10s D-Bus job-wait timeout finally gave up. This bound
    // (comfortably above the real, sub-200ms cost this genuinely
    // takes post-fix, but nowhere near the multi-second stall the bug
    // itself caused) guards against a regression back to that bug,
    // not just the end result.
    assert!(
        wait_until_removed(storage_dir.path(), &id, Duration::from_secs(3)),
        "a real, final stop on a --rm container should still auto-remove it, restarted or not \
         -- and quickly (0159), not after a multi-second stall"
    );
}

/// Spawn `ociman` with `args`, write `stdin_input` to its own real
/// stdin, and return the captured output — used by every `--interactive`
/// test below, which needs a genuinely piped stdin (unlike every other
/// helper in this file, which uses plain `.output()` -- that closes
/// stdin by default, which would silently mask exactly the behavior
/// these tests exist to check).
fn ociman_with_stdin(
    storage_root: &Path,
    args: &[&str],
    stdin_input: &[u8],
) -> std::process::Output {
    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ociman");
    child.stdin.take().unwrap().write_all(stdin_input).unwrap();
    child.wait_with_output().unwrap()
}

/// A container whose own command reads (or fails to read) one line of
/// real stdin within a short timeout, reporting which — used by every
/// `--interactive`/`ANNOTATION_INTERACTIVE` test below.
fn seed_stdin_probe_image(store: &Store, reference: &str, busybox: &Path) {
    seed_image(
        store,
        reference,
        busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "if read -t 3 line; then echo GOT:$line; else echo NOINPUT; fi".to_string(),
            ]),
            ..Default::default()
        },
    );
}

/// `ociman start --attach` on a container `create -i`'d (0188): real
/// stdin must be forwarded, even though `ociman start` has no `-i` of
/// its own at all — matching real `podman start -i -a`'s own checked-
/// directly behavior exactly (confirmed directly against a real
/// `podman`: whether stdin is ever forwarded at all is decided once,
/// at `create` time, never re-decided by a later `start`'s own flags).
#[test]
fn start_attach_forwards_stdin_for_a_container_created_with_interactive() {
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
    seed_stdin_probe_image(&store, "ociman-test/start-interactive:latest", &busybox);

    let create = ociman_with_stdin(
        storage_dir.path(),
        &[
            "create",
            "--interactive",
            "ociman-test/start-interactive:latest",
        ],
        b"",
    );
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();
    assert!(!id.is_empty());

    let start = ociman_with_stdin(
        storage_dir.path(),
        &["start", "--attach", &id],
        b"hello-from-host\n",
    );
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&start.stdout).trim(),
        "GOT:hello-from-host",
        "a container created with --interactive should still have its real stdin forwarded \
         by a later `start --attach`, even though `start` itself has no -i of its own"
    );

    ociman_with_stdin(storage_dir.path(), &["rm", &id], b"");
}

/// The default, un-annotated case: `ociman start --attach` on a
/// container `create`d with no `--interactive` at all must never
/// forward real stdin — matching real `podman start -a`'s own
/// identical default exactly.
#[test]
fn start_attach_never_forwards_stdin_for_a_container_created_without_interactive() {
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
    seed_stdin_probe_image(&store, "ociman-test/start-noninteractive:latest", &busybox);

    let create = ociman_with_stdin(
        storage_dir.path(),
        &["create", "ociman-test/start-noninteractive:latest"],
        b"",
    );
    assert!(create.status.success());
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();
    assert!(!id.is_empty());

    let start = ociman_with_stdin(
        storage_dir.path(),
        &["start", "--attach", &id],
        b"hello-from-host\n",
    );
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&start.stdout).trim(),
        "NOINPUT",
        "a container created without --interactive should never have real stdin forwarded"
    );

    ociman_with_stdin(storage_dir.path(), &["rm", &id], b"");
}

/// A container `run -i`'d in the foreground once, then later re-`start
/// --attach`'d: real stdin must be forwarded again on the *second*
/// launch too, with no `-i` given to `start` at all — matching real
/// `podman run -i` followed by a real `podman start -a`'s own checked-
/// directly behavior exactly (the "interactive" setting survives a
/// restart, it is not re-decided per launch).
#[test]
fn restarting_a_container_originally_run_interactive_still_forwards_stdin() {
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
    seed_stdin_probe_image(
        &store,
        "ociman-test/run-then-start-interactive:latest",
        &busybox,
    );

    let run = ociman_with_stdin(
        storage_dir.path(),
        &[
            "run",
            "--interactive",
            "--name",
            "run-then-start-interactive",
            "ociman-test/run-then-start-interactive:latest",
        ],
        b"first-input\n",
    );
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "GOT:first-input"
    );

    let start = ociman_with_stdin(
        storage_dir.path(),
        &["start", "--attach", "run-then-start-interactive"],
        b"second-input\n",
    );
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    assert!(
        String::from_utf8_lossy(&start.stdout).contains("GOT:second-input"),
        "the second, restarted launch should still have real stdin forwarded, with no -i \
         given to `start` at all: {:?}",
        start.stdout
    );

    ociman_with_stdin(
        storage_dir.path(),
        &["rm", "run-then-start-interactive"],
        b"",
    );
}
