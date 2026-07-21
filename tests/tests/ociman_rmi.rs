//! `ociman rmi` integration tests: removing an image's own tag/digest
//! pointer from local storage, matching real `docker rmi`/`podman
//! rmi` — including the "refuses while a container still depends on
//! it, unless `--force`" policy (see `docs/design/0102`). Same fully
//! offline seeded-image approach `ociman_run.rs`/`ociman_inspect.rs`
//! established.

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

fn wait_for_status(storage_root: &Path, id: &str, want: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let out = ociman(storage_root, &["ps", "-a", "--json"]);
        if let Ok(views) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
            && let Some(status) = views
                .as_array()
                .and_then(|entries| entries.iter().find(|e| e["id"] == id))
                .and_then(|e| e["status"].as_str())
        {
            if status == want || Instant::now() >= deadline {
                return status.to_string();
            }
        } else if Instant::now() >= deadline {
            return String::new();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn rmi_removes_a_real_image_no_longer_resolvable_afterward() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-basic:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-basic:latest")
            .unwrap()
            .is_some()
    );

    let rmi = ociman(storage_dir.path(), &["rmi", "ociman-test/rmi-basic:latest"]);
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&rmi.stdout).trim(),
        "docker.io/ociman-test/rmi-basic:latest"
    );

    // The real, on-disk store no longer resolves it -- not just "the
    // CLI printed success", but the actual pointer is gone.
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-basic:latest")
            .unwrap()
            .is_none()
    );

    // And `ociman images`/`inspect` agree.
    let images = ociman(storage_dir.path(), &["images", "--json"]);
    let views: serde_json::Value = serde_json::from_slice(&images.stdout).unwrap();
    assert!(views.as_array().unwrap().is_empty(), "{views:?}");

    let inspect = ociman(
        storage_dir.path(),
        &["inspect", "ociman-test/rmi-basic:latest"],
    );
    assert!(!inspect.status.success());
}

#[test]
fn rmi_of_an_unknown_reference_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let rmi = ociman(
        storage_dir.path(),
        &["rmi", "ociman-test/never-pulled:latest"],
    );
    assert!(!rmi.status.success());
    assert!(
        String::from_utf8_lossy(&rmi.stderr).contains("no such image"),
        "{}",
        String::from_utf8_lossy(&rmi.stderr)
    );
}

#[test]
fn rmi_refuses_an_image_still_used_by_a_stopped_container_without_force() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-in-use:latest",
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
        &[
            "run",
            "--name",
            "rmi-dependent",
            "ociman-test/rmi-in-use:latest",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let rmi = ociman(
        storage_dir.path(),
        &["rmi", "ociman-test/rmi-in-use:latest"],
    );
    assert!(!rmi.status.success());
    let stderr = String::from_utf8_lossy(&rmi.stderr);
    assert!(stderr.contains("in use"), "{stderr}");
    assert!(stderr.contains("--force"), "{stderr}");

    // Refused, so the image and the container are both still there.
    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-in-use:latest")
            .unwrap()
            .is_some()
    );
    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(!String::from_utf8_lossy(&ps.stdout).trim().is_empty());
}

#[test]
fn rmi_force_removes_a_stopped_dependent_container_and_the_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-force-stopped:latest",
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
        &[
            "run",
            "--name",
            "rmi-force-stopped",
            "ociman-test/rmi-force-stopped:latest",
        ],
    );
    assert!(run.status.success());

    let rmi = ociman(
        storage_dir.path(),
        &["rmi", "--force", "ociman-test/rmi-force-stopped:latest"],
    );
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );

    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-force-stopped:latest")
            .unwrap()
            .is_none()
    );
    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(
        String::from_utf8_lossy(&ps.stdout).trim().is_empty(),
        "the dependent container should have been removed too"
    );
}

#[test]
fn rmi_force_kills_and_removes_a_still_running_dependent_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-force-running:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/rmi-force-running:latest",
        &["--name", "rmi-force-running", "--", "sleep", "30"],
    );
    // 20s, matching the established generous ceiling every other
    // `wait_for_status`-style poll in this test suite uses (`ociman_
    // kill.rs`/`ociman_stop.rs`) — a tight one is genuinely flaky
    // under CI/parallel-test-suite CPU contention, not a bug in the
    // container reaching "running" itself (see git history: "loosen
    // the run -d timing assertion").
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty(), "container never appeared in `ps`");
    let status = wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20));
    assert_eq!(status, "running");

    let rmi = ociman(
        storage_dir.path(),
        &["rmi", "--force", "ociman-test/rmi-force-running:latest"],
    );
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );

    assert!(
        store
            .resolve_image("docker.io/ociman-test/rmi-force-running:latest")
            .unwrap()
            .is_none()
    );
    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(
        String::from_utf8_lossy(&ps.stdout).trim().is_empty(),
        "the still-running dependent container should have been killed and removed too"
    );

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn rmi_json_reports_the_canonical_reference_and_any_removed_containers() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rmi-json:latest",
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
        &[
            "run",
            "--name",
            "rmi-json-dep",
            "ociman-test/rmi-json:latest",
        ],
    );
    assert!(run.status.success());
    let dependent_id = only_container_id(storage_dir.path(), Duration::from_secs(10));

    let rmi = ociman(
        storage_dir.path(),
        &["--json", "rmi", "--force", "ociman-test/rmi-json:latest"],
    );
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&rmi.stdout).unwrap();
    assert_eq!(view["reference"], "docker.io/ociman-test/rmi-json:latest");
    assert_eq!(
        view["removed_containers"].as_array().unwrap(),
        &[serde_json::Value::String(dependent_id)]
    );
}
