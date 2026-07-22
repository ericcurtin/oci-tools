//! `ociman commit` integration tests (0155): create a new image from a
//! container's own changes relative to the image it was created from.
//!
//! Same fully offline seeded-image approach `ociman_run.rs`/
//! `ociman_diff.rs` established. Every test forces
//! `.rootless-overlay-supported` to `false` first (see
//! `ociman_diff.rs`'s own module doc comment for why), so the
//! container under test deterministically uses the plain
//! `RootfsSetup::Extract` layout `commit` (like `diff`/`cp` before it)
//! actually supports.

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

/// A real, already-stopped container running `shell_command`, forcing
/// plain-`Extract` rootfs setup deterministically first unless
/// `force_extract` is `false` (matching `ociman_diff.rs`'s own
/// identical parameter, for the one test that deliberately needs to
/// exercise whichever rootfs setup this host's own default actually
/// picks).
fn seed_and_run_stopped_container_ex(
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

/// [`seed_and_run_stopped_container_ex`] with `force_extract: true`
/// (the common case: every test but the one rootless-overlay test
/// itself needs a deterministic, plain-`Extract` container).
fn seed_and_run_stopped_container(storage_root: &Path, image: &str, shell_command: &str) -> String {
    seed_and_run_stopped_container_ex(storage_root, image, shell_command, true)
}

#[test]
fn commit_round_trips_an_added_file_and_a_deleted_one_into_a_real_runnable_image() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/commit-base:latest",
        "echo hi > /new-file.txt; rm /bin/sh; exit 0",
    );

    let commit = ociman(
        storage_dir.path(),
        &[
            "commit",
            "--author",
            "Someone <someone@example.com>",
            "--message",
            "my commit message",
            &id,
            "ociman-test/commit-result:latest",
        ],
    );
    assert!(
        commit.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let stdout = String::from_utf8_lossy(&commit.stdout);
    assert!(
        stdout.contains("tagged: docker.io/ociman-test/commit-result:latest"),
        "stdout: {stdout:?}"
    );

    // Real round trip: run a brand new container from the committed
    // image (no shell at all -- `/bin/sh` was deleted above, and this
    // deliberately never relies on it, to prove that deletion actually
    // propagated into the new image rather than merely not erroring).
    // `busybox` dispatches on its own first argument when invoked
    // directly like this, no shell required.
    let run2 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/commit-result:latest",
            "/bin/busybox",
            "cat",
            "/new-file.txt",
        ],
    );
    assert!(
        run2.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run2.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run2.stdout),
        "hi\n",
        "the committed image's own new layer should contain the file the original container added"
    );

    // The deletion (a real whiteout) also really propagated: `/bin/sh`
    // itself is gone from the committed image, not merely absent from
    // this one run's own command.
    let run3 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/commit-result:latest",
            "/bin/busybox",
            "test",
            "-e",
            "/bin/sh",
        ],
    );
    assert!(
        !run3.status.success(),
        "the committed image should no longer have /bin/sh, which the original container deleted"
    );
}

#[test]
fn commit_sets_author_and_message_and_grows_history_by_exactly_one_real_layer() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/commit-meta-base:latest",
        "exit 0",
    );

    let base_history = ociman(
        storage_dir.path(),
        &["history", "ociman-test/commit-meta-base:latest", "--json"],
    );
    let base_views: serde_json::Value = serde_json::from_slice(&base_history.stdout).unwrap();
    let base_len = base_views.as_array().unwrap().len();

    let commit = ociman(
        storage_dir.path(),
        &[
            "commit",
            "--author",
            "Jane Doe <jane@example.com>",
            "--message",
            "a real commit message",
            &id,
            "ociman-test/commit-meta-result:latest",
        ],
    );
    assert!(
        commit.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&commit.stderr)
    );

    let inspect = ociman(
        storage_dir.path(),
        &["inspect", "ociman-test/commit-meta-result:latest", "--json"],
    );
    assert!(inspect.status.success());
    let config: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(config["author"], "Jane Doe <jane@example.com>");

    let history = ociman(
        storage_dir.path(),
        &["history", "ociman-test/commit-meta-result:latest", "--json"],
    );
    assert!(history.status.success());
    let views: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    let views = views.as_array().unwrap();
    assert_eq!(
        views.len(),
        base_len + 1,
        "commit should add exactly one new history entry on top of the base image's own"
    );
    // Newest entry first (matches `ociman history`'s own real
    // `docker history`/`podman history`-compatible ordering).
    assert_eq!(views[0]["comment"], "a real commit message");
    assert!(
        views[0]["created_by"]
            .as_str()
            .unwrap()
            .contains(&id[..12.min(id.len())]),
        "created_by should reference the container id: {:?}",
        views[0]["created_by"]
    );
}

#[test]
fn commit_requires_the_image_argument_to_parse_as_a_reference() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/commit-bad-tag:latest",
        "exit 0",
    );

    let commit = ociman(storage_dir.path(), &["commit", &id, "Not A Valid Tag!!"]);
    assert!(!commit.status.success());
}

#[test]
fn commit_of_an_unknown_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let commit = ociman(
        storage_dir.path(),
        &["commit", "does-not-exist", "some/image:latest"],
    );
    assert!(!commit.status.success());
}

#[test]
fn commit_is_a_clear_error_for_a_rootless_overlay_rootfs_container() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    // Deliberately does *not* force `.rootless-overlay-supported` to
    // `false` first -- see `ociman_diff.rs`'s own identical test and
    // module doc comment for why this still passes either way,
    // depending on whether this host itself supports the
    // optimization.
    let id = seed_and_run_stopped_container_ex(
        storage_dir.path(),
        "ociman-test/commit-overlay:latest",
        "exit 0",
        false,
    );

    let commit = ociman(
        storage_dir.path(),
        &["commit", &id, "ociman-test/commit-overlay-result:latest"],
    );

    let bundle_dir = storage_dir.path().join("containers").join(&id);
    if bundle_dir.join("upper").exists() {
        assert!(!commit.status.success());
        assert!(
            String::from_utf8_lossy(&commit.stderr).contains("rootless-overlay"),
            "stderr: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
    } else {
        assert!(
            commit.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
    }
}

/// Same real, reachable-`systemd --user`-session probe
/// `ociman_pause.rs`/`ociman_top.rs` already use: `--pause`'s own real
/// freeze effect only ever exists through the systemd cgroup driver.
fn systemd_user_session_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
        .is_ok_and(|out| !out.stdout.is_empty())
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
        let out = ociman(storage_root, &["inspect", id, "--json"]);
        if out.status.success()
            && let Ok(view) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
        {
            let status = view["status"].as_str().unwrap_or_default().to_string();
            if status == want || Instant::now() >= deadline {
                return status;
            }
        } else if Instant::now() >= deadline {
            return String::new();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The real pid `ociman top`'s own table shows for the container's
/// actual init process, same as `ociman_pause.rs`'s own identical
/// helper.
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
/// actually in right now, same as `ociman_pause.rs`'s own identical
/// helper.
fn real_cgroup_dir(pid: i32) -> std::path::PathBuf {
    let contents = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
    let relative = contents
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .expect("a real cgroup v2 (\"0::\") entry");
    Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/'))
}

fn cgroup_is_frozen(cgroup_dir: &Path) -> bool {
    std::fs::read_to_string(cgroup_dir.join("cgroup.freeze"))
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

/// `--pause` (real podman's own default): the container's own real
/// cgroup v2 freezer must actually engage for the real duration of the
/// commit, and be lifted again afterward -- not just "the CLI call
/// succeeded and the container still looks fine from the outside
/// afterward". Verified the same real, direct way `ociman_pause.rs`'s
/// own tests already verify the freezer itself: reading
/// `cgroup.freeze` straight from `/sys/fs/cgroup`, independently of
/// `ociman`'s own implementation. `ociman commit` runs as a spawned
/// (not `.output()`-blocked) child specifically so this test can
/// busy-poll `cgroup.freeze` concurrently while it's still running,
/// rather than only being able to check before/after.
#[test]
fn commit_pauses_a_running_container_and_unpauses_it_afterward() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        storage_dir.path().join(".rootless-overlay-supported"),
        "false",
    )
    .unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/commit-pause:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/commit-pause:latest",
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
    assert!(
        !cgroup_is_frozen(&cgroup_dir),
        "must not already be frozen before commit even starts"
    );

    let mut commit_child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["commit", &id, "ociman-test/commit-pause-result:latest"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman commit");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut observed_frozen = false;
    loop {
        if cgroup_is_frozen(&cgroup_dir) {
            observed_frozen = true;
            break;
        }
        if let Ok(Some(_)) = commit_child.try_wait() {
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    let status = commit_child
        .wait()
        .expect("waiting for ociman commit to finish");
    assert!(status.success(), "ociman commit exited with {status:?}");
    assert!(
        observed_frozen,
        "commit --pause (the real podman default) should have frozen the container's own \
         cgroup at some point while committing"
    );
    assert!(
        !cgroup_is_frozen(&cgroup_dir),
        "commit should unpause the container again once it's done"
    );
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running",
        "the container should be genuinely running again afterward, not stuck paused"
    );

    ociman(storage_dir.path(), &["stop", "--time", "0", &id]);
    ociman(storage_dir.path(), &["rm", "-f", &id]);
    let _ = run.kill();
    let _ = run.wait();
}

/// `--pause=false`: the container must never be frozen at all,
/// matching real `podman commit --pause=false` exactly.
#[test]
fn commit_with_pause_false_never_freezes_a_running_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        storage_dir.path().join(".rootless-overlay-supported"),
        "false",
    )
    .unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/commit-nopause:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/commit-nopause:latest",
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

    let mut commit_child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "commit",
            "--pause=false",
            &id,
            "ociman-test/commit-nopause-result:latest",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman commit");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut observed_frozen = false;
    loop {
        if cgroup_is_frozen(&cgroup_dir) {
            observed_frozen = true;
            break;
        }
        if let Ok(Some(_)) = commit_child.try_wait() {
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    let status = commit_child
        .wait()
        .expect("waiting for ociman commit to finish");
    assert!(status.success(), "ociman commit exited with {status:?}");
    assert!(
        !observed_frozen,
        "commit --pause=false should never freeze the container at all"
    );
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    ociman(storage_dir.path(), &["stop", "--time", "0", &id]);
    ociman(storage_dir.path(), &["rm", "-f", &id]);
    let _ = run.kill();
    let _ = run.wait();
}
