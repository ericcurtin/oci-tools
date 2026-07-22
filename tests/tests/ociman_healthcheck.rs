//! `ociman healthcheck run` integration tests: running a container's
//! own image-declared `HEALTHCHECK` test once, matching real `podman
//! healthcheck run` (see `docs/design/0172`) -- verified directly
//! against a real installed `podman healthcheck run` during this
//! feature's own development (identical output/exit-code shape:
//! nothing printed and exit `0` when healthy, `unhealthy` printed and
//! exit `1` when not), since a real `podman` binary is not a
//! dependency this automated suite can assume is present everywhere
//! it runs.

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

fn healthcheck_test_file_exists() -> ContainerConfig {
    ContainerConfig {
        cmd: Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "true".to_string(),
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
    }
}

#[test]
fn healthcheck_run_of_an_unknown_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["healthcheck", "run", "never-existed"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("does not exist"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn healthcheck_run_on_an_already_stopped_container_prints_stopped_and_exits_nonzero() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/healthcheck-stopped:latest",
        &busybox,
        &["sh", "test"],
        healthcheck_test_file_exists(),
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/healthcheck-stopped:latest"],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    let id = String::from_utf8_lossy(&ps.stdout).trim().to_string();
    assert!(!id.is_empty());

    let out = ociman(storage_dir.path(), &["healthcheck", "run", &id]);
    assert!(!out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "stopped");
}

#[test]
fn healthcheck_run_of_a_container_with_no_healthcheck_defined_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/healthcheck-none:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/healthcheck-none:latest",
        &["-d", "sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

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

/// The real, convincing check: a genuinely running container whose
/// own image declares a real `HEALTHCHECK`, actually exec'd twice --
/// once before the test file exists (`unhealthy`) and once after
/// (`healthy`, nothing printed, exit `0`) -- not just that some stored
/// config field round-trips.
#[test]
fn healthcheck_run_actually_execs_the_test_and_reports_the_real_result() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/healthcheck-live:latest",
        &busybox,
        &["sh", "test", "touch", "rm"],
        healthcheck_test_file_exists(),
    );
    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/healthcheck-live:latest",
        &["-d", "sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

    let unhealthy = ociman(storage_dir.path(), &["healthcheck", "run", &id]);
    assert!(!unhealthy.status.success());
    assert_eq!(
        String::from_utf8_lossy(&unhealthy.stdout).trim(),
        "unhealthy"
    );

    let touch = ociman(storage_dir.path(), &["exec", &id, "touch", "/healthy"]);
    assert!(
        touch.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&touch.stderr)
    );

    let healthy = ociman(storage_dir.path(), &["healthcheck", "run", &id]);
    assert!(
        healthy.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&healthy.stderr)
    );
    assert!(
        String::from_utf8_lossy(&healthy.stdout).trim().is_empty(),
        "nothing should be printed for a healthy result: {:?}",
        String::from_utf8_lossy(&healthy.stdout)
    );

    // `--ignore-result` still reports the real text but always exits 0.
    let touch_remove = ociman(storage_dir.path(), &["exec", &id, "rm", "/healthy"]);
    assert!(touch_remove.status.success());
    let ignored = ociman(
        storage_dir.path(),
        &["healthcheck", "run", "--ignore-result", &id],
    );
    assert!(
        ignored.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ignored.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&ignored.stdout).trim(), "unhealthy");

    ociman(storage_dir.path(), &["kill", &id]);
    child.wait().ok();
}
