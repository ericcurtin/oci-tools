//! `ociman pause`/`ociman unpause` integration tests: real cgroup v2
//! freezer support (see `docs/design/0143`) against a real, running,
//! systemd-cgroup-driver-managed container — `ociman run` always
//! attempts the systemd cgroup driver itself (no `systemd-run --user
//! --scope` carrier needed, matching `ociman_top.rs`'s own identical
//! reasoning), so this only needs a reachable `systemd --user`
//! session to skip cleanly where unavailable.

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

/// Same as [`wait_for_container_status`], but matches on a
/// container's own `--name` rather than its generated id -- for the
/// `--all`/multi-target tests below, which need several containers
/// running at once.
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
/// container's own command, so `--name` must come first.
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

/// The `status` field from `ociman inspect <id> --json`, asserting
/// the command itself succeeded.
fn inspect_status(storage_root: &Path, id: &str) -> String {
    let out = ociman(storage_root, &["inspect", id, "--json"]);
    assert!(
        out.status.success(),
        "ociman inspect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    json["status"].as_str().unwrap().to_string()
}

/// Same real, reachable-`systemd --user`-session probe
/// `ociman_top.rs`'s own tests use.
fn systemd_user_session_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
        .is_ok_and(|out| !out.stdout.is_empty())
}

/// The real pid `ociman top`'s own table shows for the container's
/// actual init process (the second, higher-numbered pid — the first
/// is always this test's own `ociman run` process itself).
fn container_init_pid(storage_root: &Path, id: &str) -> i32 {
    let top = ociman(storage_root, &["top", id]);
    assert!(top.status.success());
    let stdout = String::from_utf8_lossy(&top.stdout);
    let last_line = stdout.lines().next_back().expect("at least one pid line");
    last_line
        .split_whitespace()
        .nth(1)
        .expect("a PID column")
        .parse()
        .expect("a real numeric pid")
}

/// The real cgroup directory a running container's own init process is
/// actually in right now, read directly from `/proc/<pid>/cgroup` —
/// the exact same real resolution `ociman`'s own `resolve_running_
/// container_cgroup` uses internally, reused here so this test can
/// observe the real `cpu.stat`/`cgroup.freeze` files independently of
/// `ociman`'s own implementation.
fn real_cgroup_dir(pid: i32) -> std::path::PathBuf {
    let contents = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
    let relative = contents
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .expect("a real cgroup v2 (\"0::\") entry");
    Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/'))
}

fn cpu_usage_usec(cgroup_dir: &Path) -> u64 {
    std::fs::read_to_string(cgroup_dir.join("cpu.stat"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("usage_usec "))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// `ociman pause`/`ociman unpause` against a real, running, CPU-
/// burning container: pausing must make the cgroup's own real
/// `cpu.stat`'s `usage_usec` counter stop moving *entirely* for a
/// real, measured wall-clock interval, and unpausing must make it
/// start moving again — the actual, real kernel-level effect these
/// commands exist for, not just that the CLI calls themselves exit
/// successfully. Same real end-to-end verification technique
/// `ocirun_lifecycle.rs`'s own `pause_freezes_and_resume_thaws_a_
/// real_running_containers_own_cpu_usage` test already established.
#[test]
fn pause_freezes_and_unpause_thaws_a_real_running_containers_own_cpu_usage() {
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
        "ociman-test/pause-basic:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/pause-basic:latest",
        &["/bin/sh", "-c", "i=0; while true; do i=$((i+1)); done"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let pid = container_init_pid(storage_dir.path(), &id);
    let cgroup_dir = real_cgroup_dir(pid);

    // Let it genuinely burn some real CPU before pausing.
    std::thread::sleep(Duration::from_millis(300));

    let pause = ociman(storage_dir.path(), &["pause", &id]);
    assert!(
        pause.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pause.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&pause.stdout).trim(),
        id,
        "pause should print the container id back, matching real podman"
    );
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("cgroup.freeze"))
            .unwrap()
            .trim(),
        "1"
    );
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "paused", Duration::from_secs(5)),
        "paused",
        "ociman ps should report the real, computed \"paused\" status once frozen"
    );
    assert_eq!(
        inspect_status(storage_dir.path(), &id),
        "paused",
        "ociman inspect should also report the real, computed \"paused\" status once frozen"
    );

    let usage_just_after_pause = cpu_usage_usec(&cgroup_dir);
    std::thread::sleep(Duration::from_millis(500));
    let usage_after_waiting_while_frozen = cpu_usage_usec(&cgroup_dir);
    assert_eq!(
        usage_just_after_pause, usage_after_waiting_while_frozen,
        "a real frozen container must not consume any more CPU at all while paused"
    );

    let unpause = ociman(storage_dir.path(), &["unpause", &id]);
    assert!(
        unpause.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unpause.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("cgroup.freeze"))
            .unwrap()
            .trim(),
        "0"
    );
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(5)),
        "running",
        "ociman ps should report \"running\" again once genuinely thawed"
    );
    assert_eq!(
        inspect_status(storage_dir.path(), &id),
        "running",
        "ociman inspect should also report \"running\" again once genuinely thawed"
    );

    std::thread::sleep(Duration::from_millis(300));
    let usage_after_unpause = cpu_usage_usec(&cgroup_dir);
    assert!(
        usage_after_unpause > usage_after_waiting_while_frozen,
        "a real unpaused container must start consuming CPU again \
         (frozen: {usage_after_waiting_while_frozen}, after unpause: {usage_after_unpause})"
    );

    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(kill.status.success());
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

/// `pause`/`unpause` against a container that has already stopped is
/// a clear, real error, not a silent no-op.
#[test]
fn pause_and_unpause_on_a_stopped_container_are_clear_errors() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/pause-stopped:latest",
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
        &["run", "ociman-test/pause-stopped:latest"],
    );
    assert!(run.status.success());
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());

    let pause = ociman(storage_dir.path(), &["pause", &id]);
    assert!(!pause.status.success());

    let unpause = ociman(storage_dir.path(), &["unpause", &id]);
    assert!(!unpause.status.success());
}

#[test]
fn pause_and_unpause_on_an_unknown_container_are_clear_errors() {
    let storage_dir = tempfile::tempdir().unwrap();
    let pause = ociman(storage_dir.path(), &["pause", "does-not-exist"]);
    assert!(!pause.status.success());
    let unpause = ociman(storage_dir.path(), &["unpause", "does-not-exist"]);
    assert!(!unpause.status.success());
}

/// A second `pause` on an already-paused container, or a second
/// `unpause` on an already-running (never/no-longer paused) one, are
/// both real, immediate errors (0320) — matching real `podman pause`/
/// `unpause` exactly (checked directly against a real installed
/// binary): both are a real, reported `ErrCtrStateInvalid`-equivalent
/// error, not a silent success the way this project's own
/// implementation used to give it before this note (freezing an
/// already-frozen, or thawing an already-thawed, cgroup being a
/// harmless no-op at the kernel level is not the same thing as this
/// command's own contract, which real podman's own error makes clear
/// it should not silently tolerate).
#[test]
fn double_pause_and_double_unpause_are_real_errors() {
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
        "ociman-test/double-pause:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/double-pause:latest",
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
        "{}",
        String::from_utf8_lossy(&pause.stderr)
    );
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "paused", Duration::from_secs(5)),
        "paused"
    );

    let double_pause = ociman(storage_dir.path(), &["pause", &id]);
    assert!(
        !double_pause.status.success(),
        "pausing an already-paused container must be a real error"
    );

    let unpause = ociman(storage_dir.path(), &["unpause", &id]);
    assert!(
        unpause.status.success(),
        "{}",
        String::from_utf8_lossy(&unpause.stderr)
    );
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(5)),
        "running"
    );

    let double_unpause = ociman(storage_dir.path(), &["unpause", &id]);
    assert!(
        !double_unpause.status.success(),
        "unpausing an already-running container must be a real error"
    );

    ociman(storage_dir.path(), &["stop", "--time", "0", &id]);
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

/// `--all` (0320) matches real `podman pause --all`/`podman unpause
/// --all` exactly (checked directly, and empirically against a real
/// installed binary given a real mix of running/paused/never-started
/// containers): `pause --all` pauses every genuinely running
/// container and silently skips both an already-paused one and a
/// never-started one; `unpause --all` unpauses every genuinely paused
/// one and silently skips everything else.
#[test]
fn pause_all_and_unpause_all_skip_containers_in_the_wrong_state() {
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
        "ociman-test/pause-all:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run1 = ociman_run_detached_named(
        storage_dir.path(),
        "pause-all-run1",
        "ociman-test/pause-all:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let mut run2 = ociman_run_detached_named(
        storage_dir.path(),
        "pause-all-run2",
        "ociman-test/pause-all:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-all-run1",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-all-run2",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    // Already paused before the `--all` call.
    let pause_run2 = ociman(storage_dir.path(), &["pause", "pause-all-run2"]);
    assert!(pause_run2.status.success());
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-all-run2",
            "paused",
            Duration::from_secs(5)
        ),
        "paused"
    );

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "pause-all-created",
            "ociman-test/pause-all:latest",
            "true",
        ],
    );
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );

    let pause_all = ociman(storage_dir.path(), &["pause", "--all"]);
    assert!(
        pause_all.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pause_all.stderr)
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-all-run1",
            "paused",
            Duration::from_secs(5)
        ),
        "paused",
        "the genuinely running container should have been paused by --all"
    );
    // Still just paused, untouched by this second call.
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-all-run2",
            "paused",
            Duration::from_millis(200)
        ),
        "paused"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-all-created",
            "created",
            Duration::from_millis(200)
        ),
        "created",
        "a never-started container must be left completely untouched by pause --all"
    );

    let unpause_all = ociman(storage_dir.path(), &["unpause", "--all"]);
    assert!(
        unpause_all.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unpause_all.stderr)
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-all-run1",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-all-run2",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-all-created",
            "created",
            Duration::from_millis(200)
        ),
        "created"
    );

    ociman(storage_dir.path(), &["stop", "--time", "0", "-a"]);
    run1.wait().unwrap();
    run2.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// Real `podman pause`/`unpause`'s own `--cidfile` and `--all` are
/// mutually exclusive, matching `rm`/`stop`/`restart`'s own identical
/// rule.
#[test]
fn pause_all_and_cidfile_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let cidfile = storage_dir.path().join("cid.txt");
    std::fs::write(&cidfile, "some-id").unwrap();
    let pause = ociman(
        storage_dir.path(),
        &["pause", "--all", "--cidfile", cidfile.to_str().unwrap()],
    );
    assert!(!pause.status.success());
    let unpause = ociman(
        storage_dir.path(),
        &["unpause", "--all", "--cidfile", cidfile.to_str().unwrap()],
    );
    assert!(!unpause.status.success());
}

/// `pause --filter label=`/`unpause --filter label=` only act on a
/// container also matching (OR'd across multiple values, the same
/// `ociman rm`/`stop`/`restart --filter label=` convention), leaving
/// a non-matching one completely untouched.
#[test]
fn pause_and_unpause_filter_label_only_act_on_a_matching_container() {
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
        "ociman-test/pause-filter-label:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run_match = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "--name",
            "pause-filter-match",
            "--label",
            "env=prod",
            "ociman-test/pause-filter-label:latest",
            "/bin/sh",
            "-c",
            "sleep 30",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut run_other = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "--name",
            "pause-filter-other",
            "--label",
            "env=staging",
            "ociman-test/pause-filter-label:latest",
            "/bin/sh",
            "-c",
            "sleep 30",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-filter-match",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-filter-other",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let pause = ociman(storage_dir.path(), &["pause", "--filter", "label=env=prod"]);
    assert!(
        pause.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pause.stderr)
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-filter-match",
            "paused",
            Duration::from_secs(5)
        ),
        "paused"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-filter-other",
            "running",
            Duration::from_millis(200)
        ),
        "running",
        "the non-matching container must be left completely untouched"
    );

    let unpause = ociman(
        storage_dir.path(),
        &["unpause", "--filter", "label=env=prod"],
    );
    assert!(
        unpause.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unpause.stderr)
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-filter-match",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    ociman(storage_dir.path(), &["stop", "--time", "0", "-a"]);
    run_match.wait().unwrap();
    run_other.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// A real, deliberate divergence from `--all`'s own tolerant skip
/// (see `Command::Pause::filter`'s own doc comment): a `--filter`
/// match that isn't actually running is a real, reported error here
/// too, exactly like an explicit multi-id call already is -- `--all`
/// alone is what silently tolerates it, not `--filter`.
#[test]
fn pause_filter_on_a_non_running_match_is_a_real_error_unlike_all() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/pause-filter-error:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--label",
            "env=prod",
            "ociman-test/pause-filter-error:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let pause = ociman(storage_dir.path(), &["pause", "--filter", "label=env=prod"]);
    assert!(
        !pause.status.success(),
        "a never-started container matched by --filter must be a real error, not a silent skip"
    );
}

/// `--filter` cannot be combined with an explicit id, `--cidfile`, or
/// `--all` -- the same deliberate scope narrowing `ociman rm`/`stop`/
/// `restart --filter` already established.
#[test]
fn pause_and_unpause_filter_combined_with_all_or_an_explicit_id_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let pause_with_all = ociman(
        storage_dir.path(),
        &["pause", "--filter", "label=env=prod", "--all"],
    );
    assert!(!pause_with_all.status.success());
    let pause_with_id = ociman(
        storage_dir.path(),
        &["pause", "--filter", "label=env=prod", "some-id"],
    );
    assert!(!pause_with_id.status.success());

    let unpause_with_all = ociman(
        storage_dir.path(),
        &["unpause", "--filter", "label=env=prod", "--all"],
    );
    assert!(!unpause_with_all.status.success());
    let unpause_with_id = ociman(
        storage_dir.path(),
        &["unpause", "--filter", "label=env=prod", "some-id"],
    );
    assert!(!unpause_with_id.status.success());
}

/// `--cidfile` (0320) matches real `podman pause --cidfile`/`podman
/// unpause --cidfile` exactly: the file's own first line only,
/// trailing content ignored, merged into the same target list an
/// explicit `ID`/`--name` argument already builds -- same technique
/// `ociman_ps.rs`'s own established cidfile tests use.
#[test]
fn pause_and_unpause_cidfile_read_the_container_id_from_a_file() {
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
        "ociman-test/pause-cidfile:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached_named(
        storage_dir.path(),
        "pause-cidfile-target",
        "ociman-test/pause-cidfile:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-cidfile-target",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let cidfile = storage_dir.path().join("cid.txt");
    std::fs::write(&cidfile, "pause-cidfile-target\ngarbage second line").unwrap();

    let pause = ociman(
        storage_dir.path(),
        &["pause", "--cidfile", cidfile.to_str().unwrap()],
    );
    assert!(
        pause.status.success(),
        "{}",
        String::from_utf8_lossy(&pause.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&pause.stdout).trim(),
        "pause-cidfile-target"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-cidfile-target",
            "paused",
            Duration::from_secs(5)
        ),
        "paused"
    );

    let unpause = ociman(
        storage_dir.path(),
        &["unpause", "--cidfile", cidfile.to_str().unwrap()],
    );
    assert!(
        unpause.status.success(),
        "{}",
        String::from_utf8_lossy(&unpause.stderr)
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-cidfile-target",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    ociman(storage_dir.path(), &["stop", "--time", "0", "-a"]);
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// Multiple explicit ids (0320, a real, previously-unsupported gap:
/// `ociman pause`/`unpause` only ever accepted exactly one target
/// before this) each get paused/unpaused; an unresolvable id among
/// several aborts the whole call before touching any of them,
/// matching real podman's own identical two-phase behavior (checked
/// directly, `getContainers`'s own `default` case).
#[test]
fn pause_and_unpause_with_multiple_explicit_ids() {
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
        "ociman-test/pause-multi:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run1 = ociman_run_detached_named(
        storage_dir.path(),
        "pause-multi-run1",
        "ociman-test/pause-multi:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let mut run2 = ociman_run_detached_named(
        storage_dir.path(),
        "pause-multi-run2",
        "ociman-test/pause-multi:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-multi-run1",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-multi-run2",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    // Unresolvable third target: the whole call should abort before
    // touching either real container at all.
    let bad = ociman(
        storage_dir.path(),
        &[
            "pause",
            "pause-multi-run1",
            "pause-multi-run2",
            "pause-multi-nope",
        ],
    );
    assert!(!bad.status.success());
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-multi-run1",
            "running",
            Duration::from_millis(200)
        ),
        "running",
        "must be completely untouched by the aborted call"
    );

    let pause = ociman(
        storage_dir.path(),
        &["pause", "pause-multi-run1", "pause-multi-run2"],
    );
    assert!(
        pause.status.success(),
        "{}",
        String::from_utf8_lossy(&pause.stderr)
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-multi-run1",
            "paused",
            Duration::from_secs(5)
        ),
        "paused"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-multi-run2",
            "paused",
            Duration::from_secs(5)
        ),
        "paused"
    );

    let unpause = ociman(
        storage_dir.path(),
        &["unpause", "pause-multi-run1", "pause-multi-run2"],
    );
    assert!(
        unpause.status.success(),
        "{}",
        String::from_utf8_lossy(&unpause.stderr)
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-multi-run1",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-multi-run2",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    ociman(storage_dir.path(), &["stop", "--time", "0", "-a"]);
    run1.wait().unwrap();
    run2.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// `--latest`/`-l` (0437) acts only on the single, real
/// most-recently-*created* container, exactly like `ociman rm`/
/// `stop`/`restart --latest` already established -- an earlier
/// container, even one in the exact same eligible state, must be left
/// completely untouched.
#[test]
fn pause_and_unpause_latest_act_only_on_the_most_recently_created_container() {
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
        "ociman-test/pause-latest:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut older = ociman_run_detached_named(
        storage_dir.path(),
        "pause-latest-older",
        "ociman-test/pause-latest:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-latest-older",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    // A real, distinguishable creation-time gap -- this project's own
    // `created` timestamp has one-second resolution (RFC3339).
    std::thread::sleep(Duration::from_secs(2));

    let mut newer = ociman_run_detached_named(
        storage_dir.path(),
        "pause-latest-newer",
        "ociman-test/pause-latest:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-latest-newer",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let pause = ociman(storage_dir.path(), &["pause", "--latest"]);
    assert!(
        pause.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pause.stderr)
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-latest-newer",
            "paused",
            Duration::from_secs(5)
        ),
        "paused",
        "the most recently created container should have been paused by --latest"
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-latest-older",
            "running",
            Duration::from_millis(200)
        ),
        "running",
        "an earlier container must be left completely untouched by --latest"
    );

    let unpause = ociman(storage_dir.path(), &["unpause", "--latest"]);
    assert!(
        unpause.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unpause.stderr)
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "pause-latest-newer",
            "running",
            Duration::from_secs(5)
        ),
        "running",
        "the most recently created container should have been unpaused by --latest"
    );

    ociman(storage_dir.path(), &["stop", "--time", "0", "-a"]);
    older.wait().unwrap();
    newer.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// The same real, deliberate divergence from `--all`'s own tolerant
/// skip [`pause_filter_on_a_non_running_match_is_a_real_error_unlike_
/// all`] already covers for `--filter` applies identically to
/// `--latest` (see `Command::Pause::latest`'s own doc comment):
/// checked directly, real podman's own tolerant skip is gated on
/// `options.All` specifically, never on `options.Latest` either.
#[test]
fn pause_latest_on_a_non_running_match_is_a_real_error_unlike_all() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/pause-latest-error:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/pause-latest-error:latest", "true"],
    );
    assert!(create.status.success(), "{create:?}");

    let pause = ociman(storage_dir.path(), &["pause", "--latest"]);
    assert!(
        !pause.status.success(),
        "a never-started latest container must be a real error, not a silent skip"
    );
}

/// `pause`/`unpause --latest` on a genuinely empty store is a real,
/// clear error, matching real `podman pause`/`unpause --latest`'s own
/// `ErrNoSuchCtr`.
#[test]
fn pause_and_unpause_latest_on_an_empty_store_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let pause = ociman(storage_dir.path(), &["pause", "--latest"]);
    assert!(!pause.status.success());
    let unpause = ociman(storage_dir.path(), &["unpause", "--latest"]);
    assert!(!unpause.status.success());
}

/// `--latest` cannot be combined with an explicit id, `--cidfile`,
/// `--all`, or `--filter` -- matching real podman's own checked-
/// directly `validate.AddLatestFlag`/`validate.CheckAllLatestAndIDFile`
/// restriction exactly, the same rule `ociman rm`/`stop`/`restart
/// --latest` already established.
#[test]
fn pause_and_unpause_latest_combined_with_anything_else_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();

    let pause_with_all = ociman(storage_dir.path(), &["pause", "--latest", "--all"]);
    assert!(!pause_with_all.status.success());
    let pause_with_id = ociman(storage_dir.path(), &["pause", "--latest", "some-id"]);
    assert!(!pause_with_id.status.success());
    let pause_with_filter = ociman(
        storage_dir.path(),
        &["pause", "--latest", "--filter", "label=env=prod"],
    );
    assert!(!pause_with_filter.status.success());

    let unpause_with_all = ociman(storage_dir.path(), &["unpause", "--latest", "--all"]);
    assert!(!unpause_with_all.status.success());
    let unpause_with_id = ociman(storage_dir.path(), &["unpause", "--latest", "some-id"]);
    assert!(!unpause_with_id.status.success());
    let unpause_with_filter = ociman(
        storage_dir.path(),
        &["unpause", "--latest", "--filter", "label=env=prod"],
    );
    assert!(!unpause_with_filter.status.success());
}
