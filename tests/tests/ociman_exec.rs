//! `ociman exec` integration tests: running an additional process
//! inside an already-running `ociman run` container, exercised end to
//! end with the same fully offline seeded-image approach `ociman_run.rs`
//! established (no registry access needed).
//!
//! Unlike `ociman run` itself (which blocks in the foreground until the
//! container exits), these tests need a container that's still
//! *running* while a separate `ociman exec` invocation acts on it — so
//! `run` is `spawn()`ed (not `.output()`ed) with its own stdio detached
//! (same reasoning `oci_tools_tests::ocirun_create` already documents:
//! a real pipe would never see EOF until the backgrounded process
//! itself exits) and polled via `ociman ps` until its status is
//! `running` before `exec` is attempted.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image, seed_image_with_files};

fn ociman(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociman")
}

/// Start `ociman run <image> <container args>` in the background
/// (detached stdio — see this file's own doc comment), returning the
/// child handle so the caller can eventually reap it.
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

/// Find the (only) container's id via `ps -a -q`, polling briefly
/// since it may not have been persisted yet the instant `run` was
/// spawned. A generous timeout: `ociman run` now attempts a real
/// systemd cgroup driver D-Bus round trip per container
/// (`docs/design/0034`), which can occasionally take noticeably
/// longer than usual under heavy *concurrent* test-suite load (many
/// simultaneous `StartTransientUnit` calls contending for the same
/// user systemd instance) -- the ordinary case still resolves in
/// milliseconds regardless of how generous this ceiling is.
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

/// Poll `ociman ps -a --json`'s single-container status field until it
/// equals `want` or `timeout` elapses.
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

/// Same as [`wait_for_container_status`], but matched by `--name`
/// instead of id -- needed for `--latest`/leading-slash tests, which
/// exist to prove the *name-given* target is the one actually acted
/// on, distinct from this file's own existing id-based tests.
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

#[test]
fn exec_joins_a_still_running_ociman_run_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-basic:latest",
        &busybox,
        &["sh", "ps"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-basic:latest",
        &["/bin/sh", "-c", "sleep 5"],
    );

    // Find the container's id via `ps -a` (only one exists).
    let id = {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let out = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
            let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !id.is_empty() || Instant::now() >= deadline {
                break id;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    assert!(!id.is_empty(), "expected a container id to appear in ps -a");
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running",
        "container never reached 'running' before exec was attempted"
    );

    let exec = ociman(
        storage_dir.path(),
        &[
            "exec",
            &id,
            "/bin/sh",
            "-c",
            "echo exec-worked-in-ociman; ps aux",
        ],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    let stdout = String::from_utf8_lossy(&exec.stdout).into_owned();
    assert!(stdout.contains("exec-worked-in-ociman"), "got: {stdout:?}");
    assert!(
        stdout.contains("sleep 5"),
        "exec'd process should see the container's own init in `ps`: {stdout:?}"
    );

    // The container itself must still be running after `exec` returns.
    assert_eq!(
        wait_for_container_status(
            storage_dir.path(),
            &id,
            "running",
            Duration::from_millis(200)
        ),
        "running"
    );

    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "--force", &id]);
}

#[test]
fn exec_refuses_a_container_that_has_already_stopped() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-stopped:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "ociman-test/exec-stopped:latest",
            "/bin/sh",
            "-c",
            "true",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let id = {
        let out = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert!(!id.is_empty());

    let exec = ociman(storage_dir.path(), &["exec", &id, "/bin/sh", "-c", "true"]);
    assert!(
        !exec.status.success(),
        "exec should refuse an already-stopped container"
    );
}

#[test]
fn exec_workdir_and_env_flags_override_the_defaults() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-cwd-env:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            env: vec!["PATH=/bin".to_string()],
            ..Default::default()
        },
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-cwd-env:latest",
        &["/bin/sh", "-c", "sleep 5"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let exec = ociman(
        storage_dir.path(),
        &[
            "exec",
            "--workdir",
            "/bin",
            "--env",
            "EXEC_TEST_VAR=exec-test-value",
            &id,
            "/bin/sh",
            "-c",
            "pwd; echo \"$EXEC_TEST_VAR\"; echo \"got:$PATH\"",
        ],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    let stdout = String::from_utf8_lossy(&exec.stdout).into_owned();
    assert_eq!(
        stdout.lines().next(),
        Some("/bin"),
        "--workdir should override the default cwd (\"/\"): got {stdout:?}"
    );
    assert!(stdout.contains("exec-test-value"), "got: {stdout:?}");
    assert!(
        stdout.contains("got:/bin"),
        "the container's own base PATH should still be set (appended to, not replaced): {stdout:?}"
    );

    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "--force", &id]);
}

/// The short `-w` form (matching real `podman exec -w` exactly, not
/// just its long `--workdir` spelling) works identically.
#[test]
fn exec_workdir_short_flag_overrides_the_default() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-workdir-short:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-workdir-short:latest",
        &["/bin/sh", "-c", "sleep 5"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let exec = ociman(
        storage_dir.path(),
        &["exec", "-w", "/bin", &id, "/bin/sh", "-c", "pwd"],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&exec.stdout).trim(),
        "/bin",
        "the short -w flag should behave identically to --workdir"
    );

    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "--force", &id]);
}

/// `exec --env` overrides a variable name the container's own process
/// environment *already* has, in place — not as a second, shadowed
/// entry a real `getenv(3)`-style lookup would never actually see
/// (see `apply_env_overrides`'s own doc comment).
#[test]
fn exec_env_flag_overrides_an_existing_variable_in_place() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-env-override:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            env: vec!["PATH=/bin".to_string()],
            ..Default::default()
        },
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-env-override:latest",
        &["/bin/sh", "-c", "sleep 5"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let exec = ociman(
        storage_dir.path(),
        &[
            "exec",
            "--env",
            "PATH=/custom/bin",
            &id,
            "/bin/sh",
            "-c",
            "echo \"$PATH\"",
        ],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&exec.stdout),
        "/custom/bin\n",
        "PATH should be overridden in place, not shadowed by an earlier, still-first entry"
    );

    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "--force", &id]);
}

/// `exec --env-file` reads `KEY=value` entries from a real file and
/// applies them the same way `-e`/`--env` does, but always loses to
/// `-e`/`--env` for a shared key regardless of flag order — matching
/// real `podman exec --env-file`'s own identical precedence
/// (`RunArgs::env_file`'s own doc comment has the full detail; this
/// mirrors `ociman_run.rs`'s own `run_env_file_flag_*` tests for
/// `exec` specifically).
#[test]
fn exec_env_file_flag_reads_entries_and_loses_to_env_flag_for_a_shared_key() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-env-file:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            env: vec!["PATH=/bin".to_string()],
            ..Default::default()
        },
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-env-file:latest",
        &["/bin/sh", "-c", "sleep 5"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let env_file_dir = tempfile::tempdir().unwrap();
    let env_file_path = env_file_dir.path().join("env.list");
    std::fs::write(&env_file_path, "PATH=/from-file/bin\nEXTRA=from-file\n").unwrap();

    let exec = ociman(
        storage_dir.path(),
        &[
            "exec",
            "--env-file",
            env_file_path.to_str().unwrap(),
            // `-e` given before `--env-file` on the command line --
            // still wins, precedence is fixed, not order-dependent.
            "--env",
            "PATH=/from-flag/bin",
            &id,
            "/bin/sh",
            "-c",
            "echo \"$PATH\" \"$EXTRA\"",
        ],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&exec.stdout),
        "/from-flag/bin from-file\n",
        "-e should win over --env-file for the shared PATH key; EXTRA comes from the file alone"
    );

    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "--force", &id]);
}

#[test]
fn exec_user_flag_resolves_a_named_user_via_the_containers_own_etc_passwd() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image_with_files(
        &store,
        "ociman-test/exec-named-user:latest",
        &busybox,
        &["sh", "id"],
        &[(
            "etc/passwd",
            b"root:x:0:0:root:/root:/bin/sh\napp:x:1000:1000:App:/home/app:/bin/sh\n".as_slice(),
        )],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-named-user:latest",
        &["/bin/sh", "-c", "sleep 5"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    // "root" is the one name that can fully succeed today (it
    // resolves to uid 0, the only container uid this rootless runtime
    // can map) — see docs/design/0024.
    let exec = ociman(
        storage_dir.path(),
        &["exec", "--user", "root", &id, "/bin/sh", "-c", "true"],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );

    // A named user that resolves fine (via the same /etc/passwd) but
    // to a non-root uid still hits the same "can't map it" wall a
    // numeric one would.
    let exec_nonroot = ociman(
        storage_dir.path(),
        &["exec", "--user", "app", &id, "/bin/sh", "-c", "true"],
    );
    assert!(
        !exec_nonroot.status.success(),
        "a named user resolving to a non-root uid should still be rejected"
    );

    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "--force", &id]);
}

/// `ociman exec --privileged` (matching real `podman exec
/// --privileged` exactly, checked directly against `~/git/podman/
/// libpod/oci_conmon_exec_linux.go`'s own `setProcessCapabilitiesExec`):
/// the exec'd process's own real `CapBnd` (its bounding capability
/// set, read straight back from `/proc/self/status` inside the
/// container, the kernel's own ground truth) genuinely grows to the
/// full set when given, strictly larger than the container's own
/// default (podman's own 11-capability default, `ContainerConfig::
/// default()`'s own implicit choice) without it.
#[test]
fn exec_privileged_genuinely_grants_the_full_bounding_capability_set() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-privileged:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-privileged:latest",
        &["/bin/sh", "-c", "sleep 5"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    fn cap_bnd(status_output: &[u8]) -> u64 {
        let text = String::from_utf8_lossy(status_output);
        let line = text
            .lines()
            .find(|l| l.starts_with("CapBnd:"))
            .unwrap_or_else(|| panic!("no CapBnd line in: {text:?}"));
        let hex = line.split_whitespace().nth(1).unwrap();
        u64::from_str_radix(hex, 16).unwrap()
    }

    let unprivileged = ociman(
        storage_dir.path(),
        &["exec", &id, "/bin/sh", "-c", "cat /proc/self/status"],
    );
    assert!(
        unprivileged.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unprivileged.stderr)
    );
    let unprivileged_bnd = cap_bnd(&unprivileged.stdout);

    let privileged = ociman(
        storage_dir.path(),
        &[
            "exec",
            "--privileged",
            &id,
            "/bin/sh",
            "-c",
            "cat /proc/self/status",
        ],
    );
    assert!(
        privileged.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&privileged.stderr)
    );
    let privileged_bnd = cap_bnd(&privileged.stdout);

    assert!(
        privileged_bnd > unprivileged_bnd,
        "privileged CapBnd ({privileged_bnd:#x}) should be strictly larger than the \
         container's own default ({unprivileged_bnd:#x})"
    );
    // Every bit the container's own default already had must still be
    // present too -- `--privileged` only ever adds, never removes.
    assert_eq!(privileged_bnd & unprivileged_bnd, unprivileged_bnd);

    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "--force", &id]);
}

/// `ociman exec --preserve-fds` (0351), matching real `podman exec
/// --preserve-fds` exactly (checked directly against a real installed
/// `podman exec --help`) -- correcting this project's own earlier,
/// mistaken belief (design `0294`) that podman lacks this flag on
/// `exec` at all. By default, every fd above stdio is closed before
/// the exec'd process ever runs; `--preserve-fds N` keeps exactly the
/// first `N` of them instead.
///
/// Same real `pre_exec`/`dup2`-onto-fd-3 technique
/// `ocirun_exec.rs`'s own identical test already established.
#[test]
fn exec_preserve_fds_closes_extra_fds_by_default_but_keeps_them_with_the_flag() {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;

    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-preserve-fds:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-preserve-fds:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let run_with_fd3_open = |extra_args: &[&str]| -> std::process::Output {
        let marker = tempfile::NamedTempFile::new().unwrap();
        let raw_fd = marker.as_file().as_raw_fd();
        let mut cmd = Command::new(bin_path("ociman"));
        cmd.env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
            .env_remove("OCI_TOOLS_LOG")
            .arg("exec")
            .args(extra_args)
            .args([
                &id,
                "/bin/sh",
                "-c",
                "test -e /proc/self/fd/3 && echo fd3-present || echo fd3-absent",
            ]);
        // SAFETY: only calls `dup2(2)`/`fcntl(2)` (both async-signal-
        // safe, no allocation) in the forked-but-not-yet-exec'd child,
        // only ever affecting that child's own fd table.
        #[allow(unsafe_code)]
        unsafe {
            cmd.pre_exec(move || {
                if libc::dup2(raw_fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let out = cmd.output().expect("failed to spawn ociman exec");
        drop(marker);
        out
    };

    let without_flag = run_with_fd3_open(&[]);
    assert!(
        without_flag.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&without_flag.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&without_flag.stdout).trim(),
        "fd3-absent",
        "fd 3 must be closed by default, matching real podman: {without_flag:?}"
    );

    let with_flag = run_with_fd3_open(&["--preserve-fds", "1"]);
    assert!(
        with_flag.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&with_flag.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&with_flag.stdout).trim(),
        "fd3-present",
        "fd 3 must be preserved with --preserve-fds 1: {with_flag:?}"
    );

    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "--force", &id]);
}

/// `--preserve-fds N` fails fast, before ever exec'ing anything at
/// all, if fewer than `N` fds are actually open starting at fd 3 --
/// matching real podman's own identical upfront `IsFdInherited` check
/// exactly.
#[test]
fn exec_preserve_fds_rejects_a_claim_with_no_matching_open_fd() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-preserve-fds-reject:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-preserve-fds-reject:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let out = ociman(
        storage_dir.path(),
        &["exec", "--preserve-fds", "5", &id, "true"],
    );
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is not available"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "--force", &id]);
}

/// A real, previously-unnoticed bug this closes (0385): before `-i`/
/// `--interactive` existed, `ociman exec` always forwarded whatever
/// stdin its own caller had, unconditionally -- unlike real `podman
/// exec`'s own checked-directly default (`-i` absent) of never
/// connecting the exec'd process's stdin at all (`~/git/podman/cmd/
/// podman/containers/exec.go`: `AttachInput`/`InputStream` are only
/// ever set when `-i` is given).
#[test]
fn exec_without_interactive_never_forwards_real_stdin() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-stdin-default:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-stdin-default:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "exec",
            &id,
            "/bin/sh",
            "-c",
            "if read -t 5 line; then echo GOT:$line; else echo NOINPUT; fi",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ociman exec");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello-from-host-stdin\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "NOINPUT",
        "without --interactive, ociman exec should never forward real host stdin"
    );

    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "--force", &id]);
}

/// `ociman exec -i`/`--interactive` (0385): the exec'd process's own
/// stdin must be this process's own real stdin, matching real `podman
/// exec -i` exactly.
#[test]
fn exec_interactive_forwards_real_stdin() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-stdin-interactive:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-stdin-interactive:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "exec",
            "--interactive",
            &id,
            "/bin/sh",
            "-c",
            "if read -t 5 line; then echo GOT:$line; else echo NOINPUT; fi",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn ociman exec");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello-from-host-stdin\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "GOT:hello-from-host-stdin",
        "--interactive should forward this process's own real stdin to the exec'd process"
    );

    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "--force", &id]);
}

/// `ociman exec --latest`/`-l` (matching real `podman exec --latest`
/// exactly) execs into the single, real most-recently-*created*
/// container instead of naming one explicitly.
#[test]
fn exec_latest_execs_into_the_most_recently_created_running_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-latest:latest",
        &busybox,
        &["sh", "test", "touch"],
        ContainerConfig::default(),
    );

    let mut older = ociman_run_detached_named(
        storage_dir.path(),
        "exec-latest-older",
        "ociman-test/exec-latest:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "exec-latest-older",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    // A real, distinguishable creation-time gap.
    std::thread::sleep(Duration::from_secs(2));

    // Only the *newer* container's own command ever creates this
    // marker file -- proving `--latest` genuinely targeted it, not
    // merely that some exec succeeded against something.
    let mut newer = ociman_run_detached_named(
        storage_dir.path(),
        "exec-latest-newer",
        "ociman-test/exec-latest:latest",
        &["/bin/sh", "-c", "touch /newer-marker && sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "exec-latest-newer",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    // A real, if tiny, race between the container's own status
    // flipping to "running" and its own `touch /newer-marker` shell
    // command actually finishing -- retried briefly rather than a
    // single fixed sleep, matching this project's own established
    // poll-with-timeout convention.
    let deadline = Instant::now() + Duration::from_secs(5);
    let exec = loop {
        let attempt = ociman(
            storage_dir.path(),
            &["exec", "--latest", "test", "-f", "/newer-marker"],
        );
        if attempt.status.success() || Instant::now() >= deadline {
            break attempt;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        exec.status.success(),
        "--latest must have targeted the newer container: stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );

    ociman(storage_dir.path(), &["kill", "-a"]);
    older.wait().ok();
    newer.wait().ok();
    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// `ociman exec --cidfile` (matching real `podman exec --cidfile`
/// exactly, genuinely present in real podman's own source despite not
/// appearing in an older installed `podman 4.9.3 --help`'s own
/// output) reads the container ID from the file's own first line.
#[test]
fn exec_cidfile_reads_the_container_id_from_a_file() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-cidfile:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-cidfile:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let cidfile = storage_dir.path().join("cid");
    std::fs::write(&cidfile, format!("{id}\ntrailing garbage ignored")).unwrap();

    let exec = ociman(
        storage_dir.path(),
        &["exec", "--cidfile", cidfile.to_str().unwrap(), "true"],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );

    ociman(storage_dir.path(), &["kill", &id]);
    run.wait().ok();
}

/// `--latest` and `--cidfile` together is a real, immediate error,
/// matching real podman's own exact wording.
#[test]
fn exec_latest_and_cidfile_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let cidfile = storage_dir.path().join("cid");
    std::fs::write(&cidfile, "whatever").unwrap();
    let out = ociman(
        storage_dir.path(),
        &[
            "exec",
            "--latest",
            "--cidfile",
            cidfile.to_str().unwrap(),
            "true",
        ],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("--latest and --cidfile can not be used together"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// No container reference and no `--latest`/`--cidfile` at all is a
/// real, immediate error, matching real podman's own exact wording
/// (confirmed live against an installed `podman 4.9.3`).
#[test]
fn exec_with_nothing_at_all_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["exec"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(
            "exec requires the name or ID of a container or the --latest or --cidfile flag"
        ),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `--latest` with no command at all is a real, immediate error,
/// matching real podman's own exact wording (confirmed live against
/// an installed `podman 4.9.3`: `must provide a non-empty command to
/// start an exec session: invalid argument`).
#[test]
fn exec_latest_with_no_command_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-latest-no-cmd:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-latest-no-cmd:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

    let out = ociman(storage_dir.path(), &["exec", "--latest"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("must provide a non-empty command to start an exec session"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    ociman(storage_dir.path(), &["kill", &id]);
    run.wait().ok();
}

/// A leading `/` on the container reference is stripped, matching
/// real podman's own identical docker-compatibility quirk
/// (`strings.TrimPrefix(args[0], "/")`).
#[test]
fn exec_strips_a_leading_slash_from_the_container_reference() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-leading-slash:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let mut run = ociman_run_detached_named(
        storage_dir.path(),
        "exec-leading-slash-ctr",
        "ociman-test/exec-leading-slash:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    assert_eq!(
        wait_for_container_status_by_name(
            storage_dir.path(),
            "exec-leading-slash-ctr",
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let exec = ociman(
        storage_dir.path(),
        &["exec", "/exec-leading-slash-ctr", "true"],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );

    ociman(storage_dir.path(), &["kill", "exec-leading-slash-ctr"]);
    run.wait().ok();
}

/// `ociman exec --detach`/`-d` (`docs/design/0534`), matching real
/// `podman exec --detach`/`-d` exactly: the invocation itself returns
/// immediately (exit `0`, printing the exec'd process's own real
/// host-visible pid, this project's own closest honest equivalent to
/// real podman's own opaque, persisted exec-session id -- see
/// `Command::Exec::detach`'s own doc comment for why), well before
/// the (deliberately much longer) command it started actually
/// finishes -- proven by a real wall-clock bound on the `exec
/// --detach` call itself, then a *second*, ordinary (non-detached)
/// `exec` polling for the detached command's own real, delayed side
/// effect (a marker file, written only after its own longer sleep) to
/// prove it really did keep running in the background. The container
/// itself stays completely unaffected throughout.
#[test]
fn exec_detach_returns_immediately_and_prints_the_exec_pid() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-detach:latest",
        &busybox,
        &["sh", "sleep", "cat"],
        ContainerConfig::default(),
    );
    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-detach:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    // Neither `Stdio::piped()` nor the `ociman` helper above (which
    // captures stdout/stderr via a real pipe through `Command::
    // output()`) is safe here: the detached grandchild below inherits
    // whatever stdio this invocation itself has, so a captured *pipe*
    // -- for *any* of stdin/stdout/stderr, including just stdout,
    // which this test genuinely needs to capture the printed pid --
    // would never see `EOF` (hanging this call) until *that* process
    // *also* exits, ~3s later, hiding the very thing this test is
    // trying to prove. The exact same real hazard `ocirun_exec.rs`'s
    // own identical `0533` test already found and documented for
    // `ocirun exec --detach`; a real *file* (unlike a pipe) has no
    // such "every writer must close it first" `EOF` semantics at all,
    // so redirecting stdout there instead and reading it back only
    // *after* this invocation's own `status()` returns sidesteps the
    // hazard completely.
    let stdout_file = tempfile::NamedTempFile::new().unwrap();
    let started = Instant::now();
    let exec = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "exec",
            "--detach",
            &id,
            "/bin/sh",
            "-c",
            "sleep 3; echo done > /marker.txt",
        ])
        .stdin(Stdio::null())
        .stdout(stdout_file.reopen().unwrap())
        .stderr(Stdio::null())
        .status()
        .expect("failed to spawn ociman exec --detach");
    let elapsed = started.elapsed();
    assert!(exec.success(), "ociman exec --detach failed: {exec:?}");
    assert!(
        elapsed < Duration::from_secs(2),
        "--detach should return almost immediately, not block on the full 3s sleep: {elapsed:?}"
    );
    let printed = std::fs::read_to_string(stdout_file.path())
        .unwrap()
        .trim()
        .to_string();
    let exec_pid: i32 = printed
        .parse()
        .unwrap_or_else(|e| panic!("--detach should print a bare pid, got {printed:?}: {e}"));
    assert!(exec_pid > 0);

    // The container itself is unaffected: still running.
    assert_eq!(
        wait_for_container_status(
            storage_dir.path(),
            &id,
            "running",
            Duration::from_millis(200)
        ),
        "running"
    );

    // The detached command really did keep running in the background
    // and eventually wrote its own marker -- checked via a second,
    // ordinary (non-detached) `exec`, polled until it succeeds (the
    // marker file may not exist quite yet the instant this loop
    // starts).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let check = ociman(
            storage_dir.path(),
            &["exec", &id, "/bin/cat", "/marker.txt"],
        );
        if check.status.success() {
            assert_eq!(String::from_utf8_lossy(&check.stdout), "done\n");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the detached command never wrote its own marker file"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    ociman(storage_dir.path(), &["kill", &id]);
    run.wait().ok();
}

/// A real, checked-directly divergence from a non-detached `exec`:
/// `--detach` always exits `0`, regardless of whatever the detached
/// command will *eventually* exit with -- matching real `podman exec
/// --detach`'s own identical `return nil` (never surfacing any later
/// exit code back to this invocation's own exit status at all).
#[test]
fn exec_detach_exits_zero_even_though_the_detached_command_will_eventually_fail() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-detach-fail:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );
    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-detach-fail:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let exec = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["exec", "--detach", &id, "/bin/sh", "-c", "exit 7"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("failed to spawn ociman exec --detach");
    assert!(
        exec.success(),
        "--detach should exit 0 regardless of the detached command's own eventual exit code: \
         {exec:?}"
    );

    ociman(storage_dir.path(), &["kill", &id]);
    run.wait().ok();
}
