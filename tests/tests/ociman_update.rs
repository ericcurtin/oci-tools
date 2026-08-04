//! `ociman update` integration tests: changing a running container's
//! real cgroup resource limits in place, matching real `podman
//! update` for the same subset of resource flags `ociman run` itself
//! already supports (see `docs/design/0171`). Same fully offline
//! seeded-image approach `ociman_kill.rs`/`ociman_stop.rs` established,
//! including the same `spawn()`+detached-stdio+poll concurrency
//! pattern for a container that needs to still be running while a
//! separate invocation acts on it.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use oci_spec_types::image::{ContainerConfig, HealthcheckConfig};
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

/// The real cgroup v2 file this container's own memory limit lives in
/// (`cgroup.freeze`'s own sibling directory this project's own
/// `oci_runtime_core::cgroups` already resolves elsewhere) -- read
/// directly rather than through another layer of this project's own
/// code, so this test is checking the real, final kernel-visible
/// effect, not just that some internal function was called.
fn real_cgroup_dir_for(storage_root: &Path, id: &str) -> std::path::PathBuf {
    let containers = oci_runtime_core::StateStore::open(storage_root.join("containers")).unwrap();
    let state = containers.load(id).unwrap();
    let pid = state.pid.expect("running container must have a pid");
    oci_runtime_core::cgroups::cgroup_dir_for_running_pid(Path::new("/sys/fs/cgroup"), pid)
        .expect("resolving real cgroup for a running container")
}

#[test]
fn update_of_an_unknown_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let update = ociman(
        storage_dir.path(),
        &["update", "--memory", "64m", "never-existed"],
    );
    assert!(!update.status.success());
    assert!(
        String::from_utf8_lossy(&update.stderr).contains("does not exist"),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
}

#[test]
fn update_with_no_resource_or_health_flags_at_all_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/update-no-flags:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );
    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/update-no-flags:latest",
        &["-d", "sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

    let update = ociman(storage_dir.path(), &["update", &id]);
    assert!(!update.status.success());
    assert!(
        String::from_utf8_lossy(&update.stderr).contains("no resource or health flags"),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );

    ociman(storage_dir.path(), &["kill", &id]);
    child.wait().ok();
}

#[test]
fn update_of_an_already_stopped_container_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/update-stopped:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "true".to_string(),
            ]),
            ..Default::default()
        },
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/update-stopped:latest"],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    let id = String::from_utf8_lossy(&ps.stdout).trim().to_string();
    assert!(!id.is_empty());

    let update = ociman(storage_dir.path(), &["update", "--memory", "64m", &id]);
    assert!(!update.status.success());
    assert!(
        String::from_utf8_lossy(&update.stderr).contains("not running"),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
}

/// The real, convincing check: update a genuinely running container's
/// `--memory`/`--cpus`/`--pids-limit`, then read the real cgroup v2
/// accounting files back directly to confirm the kernel itself now
/// enforces the new limits -- not just that `ociman update` exited
/// `0`.
#[test]
fn update_changes_the_real_live_cgroup_limits_of_a_running_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/update-live:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );
    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/update-live:latest",
        &["-d", "sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

    let update = ociman(
        storage_dir.path(),
        &[
            "update",
            "--memory",
            "64m",
            "--memory-reservation",
            "32m",
            "--cpus",
            "0.5",
            "--pids-limit",
            "42",
            &id,
        ],
    );
    assert!(
        update.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&update.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&update.stdout).trim(), id);

    let cgroup_dir = real_cgroup_dir_for(storage_dir.path(), &id);
    let memory_max = std::fs::read_to_string(cgroup_dir.join("memory.max")).unwrap();
    assert_eq!(memory_max.trim(), (64 * 1024 * 1024).to_string());

    // `--memory-reservation` (0401): a real, previously-missing flag,
    // written to the same real cgroup v2 `memory.low` file `ociman
    // run --memory-reservation`'s own end-to-end test proves via the
    // systemd scope's own `MemoryLow` property -- here checked
    // directly against the raw cgroupfs file instead, since `ociman
    // update` writes it that way regardless of which driver created
    // the cgroup in the first place (systemd's own `Delegate=true`
    // leaves the real cgroupfs directly writable).
    let memory_low = std::fs::read_to_string(cgroup_dir.join("memory.low")).unwrap();
    assert_eq!(memory_low.trim(), (32 * 1024 * 1024).to_string());

    let cpu_max = std::fs::read_to_string(cgroup_dir.join("cpu.max")).unwrap();
    // 0.5 CPUs -> a 50_000us quota over the fixed 100_000us period,
    // matching `resources_from_cli`'s own conversion.
    assert_eq!(cpu_max.trim(), "50000 100000");

    let pids_max = std::fs::read_to_string(cgroup_dir.join("pids.max")).unwrap();
    assert_eq!(pids_max.trim(), "42");

    ociman(storage_dir.path(), &["kill", &id]);
    child.wait().ok();
}

/// `ociman update --health-cmd` (0441) needs no live container at
/// all -- a `created`-but-never-`start`ed container can still have
/// its healthcheck updated, matching real podman's own identical
/// "persisted config change" scope (unlike a resource-flag update,
/// which still genuinely needs a real, running cgroup): the override
/// is then picked up by a *later* `start`, proving it genuinely
/// persisted rather than merely appearing to succeed.
#[test]
fn update_health_cmd_on_a_created_but_never_started_container_persists_without_running() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/update-health-created:latest",
        &busybox,
        &["sh", "test", "touch", "sleep"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 30".to_string(),
            ]),
            ..Default::default()
        },
    );

    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/update-health-created:latest"],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();
    assert!(!id.is_empty());

    let update = ociman(
        storage_dir.path(),
        &["update", "--health-cmd", "test -f /update-healthy", &id],
    );
    assert!(
        update.status.success(),
        "a health-only update must not require a running container: stderr: {}",
        String::from_utf8_lossy(&update.stderr)
    );

    let start = ociman(storage_dir.path(), &["start", &id]);
    assert!(
        start.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

    let unhealthy = ociman(storage_dir.path(), &["healthcheck", "run", &id]);
    assert!(!unhealthy.status.success());
    assert_eq!(
        String::from_utf8_lossy(&unhealthy.stdout).trim(),
        "unhealthy"
    );

    ociman(
        storage_dir.path(),
        &["exec", &id, "touch", "/update-healthy"],
    );
    let healthy = ociman(storage_dir.path(), &["healthcheck", "run", &id]);
    assert!(
        healthy.status.success(),
        "the persisted --health-cmd override from create-time update must still apply: stderr: \
         {}",
        String::from_utf8_lossy(&healthy.stderr)
    );

    ociman(storage_dir.path(), &["kill", &id]);
}

/// `ociman update --health-interval` alone (no `--health-cmd`) is a
/// real, genuine *partial* update: the container's own existing
/// `--health-cmd` (given at `run` time here) is preserved untouched,
/// only the interval actually changes -- matching real podman's own
/// checked-directly `GetNewHealthCheckConfig` exactly (a real,
/// deliberate divergence from `create`'s own all-or-nothing rule, see
/// `Command::Update::health_cmd`'s own doc comment).
#[test]
fn update_health_interval_alone_preserves_the_existing_health_cmd() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/update-health-interval:latest",
        &busybox,
        &["sh", "test", "touch"],
        ContainerConfig::default(),
    );

    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/update-health-interval:latest",
        &["-d", "--health-cmd", "test -f /a", "sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

    let update = ociman(
        storage_dir.path(),
        &["update", "--health-interval", "5s", &id],
    );
    assert!(
        update.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&update.stderr)
    );

    let unhealthy = ociman(storage_dir.path(), &["healthcheck", "run", &id]);
    assert!(!unhealthy.status.success());
    assert_eq!(
        String::from_utf8_lossy(&unhealthy.stdout).trim(),
        "unhealthy"
    );

    ociman(storage_dir.path(), &["exec", &id, "touch", "/a"]);
    let healthy = ociman(storage_dir.path(), &["healthcheck", "run", &id]);
    assert!(
        healthy.status.success(),
        "the original --health-cmd must still be the one exec'd: stderr: {}",
        String::from_utf8_lossy(&healthy.stderr)
    );

    ociman(storage_dir.path(), &["kill", &id]);
    child.wait().ok();
}

/// `ociman update --health-retries` alone, on a container whose only
/// healthcheck is the *image's* own declared one (no CLI override at
/// all yet), rebuilds from that image-declared command -- proving
/// [`resolve_effective_healthcheck`]'s own image fallback is genuinely
/// used as the baseline, not just this project's own bare defaults.
#[test]
fn update_health_retries_alone_rebuilds_from_the_images_own_declared_command() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/update-health-retries:latest",
        &busybox,
        &["sh", "test", "touch", "sleep"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 30".to_string(),
            ]),
            healthcheck: Some(HealthcheckConfig {
                test: vec![
                    "CMD".to_string(),
                    "test".to_string(),
                    "-f".to_string(),
                    "/image-healthy".to_string(),
                ],
                ..Default::default()
            }),
            ..Default::default()
        },
    );

    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/update-health-retries:latest",
        &["-d"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

    let update = ociman(
        storage_dir.path(),
        &["update", "--health-retries", "5", &id],
    );
    assert!(
        update.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&update.stderr)
    );

    let unhealthy = ociman(storage_dir.path(), &["healthcheck", "run", &id]);
    assert!(!unhealthy.status.success());
    assert_eq!(
        String::from_utf8_lossy(&unhealthy.stdout).trim(),
        "unhealthy"
    );

    ociman(
        storage_dir.path(),
        &["exec", &id, "touch", "/image-healthy"],
    );
    let healthy = ociman(storage_dir.path(), &["healthcheck", "run", &id]);
    assert!(
        healthy.status.success(),
        "the image's own declared healthcheck command must still be the one exec'd: stderr: {}",
        String::from_utf8_lossy(&healthy.stderr)
    );

    ociman(storage_dir.path(), &["kill", &id]);
    child.wait().ok();
}

/// `ociman update --no-healthcheck` disables even an image's own
/// declared `HEALTHCHECK`, matching real `podman update
/// --no-healthcheck` exactly.
#[test]
fn update_no_healthcheck_disables_even_an_image_declared_one() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/update-no-healthcheck:latest",
        &busybox,
        &["sh", "test", "sleep"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 30".to_string(),
            ]),
            healthcheck: Some(HealthcheckConfig {
                test: vec![
                    "CMD".to_string(),
                    "test".to_string(),
                    "-f".to_string(),
                    "/healthy".to_string(),
                ],
                ..Default::default()
            }),
            ..Default::default()
        },
    );

    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/update-no-healthcheck:latest",
        &["-d"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

    let update = ociman(storage_dir.path(), &["update", "--no-healthcheck", &id]);
    assert!(
        update.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&update.stderr)
    );

    let out = ociman(storage_dir.path(), &["healthcheck", "run", &id]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no healthcheck defined"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    ociman(storage_dir.path(), &["kill", &id]);
    child.wait().ok();
}

/// `--no-healthcheck` combined with *any* other health flag is a
/// real, immediate error -- a real, checked-directly *broader*
/// restriction than `create`'s own (which only conflicts with
/// `--health-cmd` specifically), matching real podman's own exact
/// wording (`~/git/podman/libpod/healthcheck_config.go`).
#[test]
fn update_no_healthcheck_combined_with_any_other_health_flag_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/update-health-conflict:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 30".to_string(),
            ]),
            ..Default::default()
        },
    );

    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/update-health-conflict:latest",
        &["-d"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

    let update = ociman(
        storage_dir.path(),
        &["update", "--no-healthcheck", "--health-interval", "5s", &id],
    );
    assert!(!update.status.success());
    assert!(
        String::from_utf8_lossy(&update.stderr)
            .contains("cannot specify both --no-healthcheck and other HealthCheck flags"),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );

    ociman(storage_dir.path(), &["kill", &id]);
    child.wait().ok();
}

/// `ociman update --health-retries 0` succeeds, unlike `ociman create
/// --health-cmd ... --health-retries 0` -- a real, checked-directly
/// upstream quirk (see `Command::Update::health_retries`'s own doc
/// comment): `update`'s own real merge logic always calls
/// `MakeHealthCheckFromCli` with `isStartup=true`, which skips the
/// `retries >= 1` validation regardless of which real healthcheck
/// kind is actually being updated.
#[test]
fn update_health_retries_zero_succeeds_unlike_create() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/update-health-retries-zero:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--health-cmd",
            "true",
            "--health-retries",
            "0",
            "ociman-test/update-health-retries-zero:latest",
        ],
    );
    assert!(
        !create.status.success(),
        "create must still enforce retries >= 1: {create:?}"
    );

    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/update-health-retries-zero:latest",
        &["-d", "sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

    let update = ociman(
        storage_dir.path(),
        &[
            "update",
            "--health-cmd",
            "true",
            "--health-retries",
            "0",
            &id,
        ],
    );
    assert!(
        update.status.success(),
        "update must not enforce retries >= 1: stderr: {}",
        String::from_utf8_lossy(&update.stderr)
    );

    ociman(storage_dir.path(), &["kill", &id]);
    child.wait().ok();
}
