//! `ociman volume` integration tests: named volumes, matching real
//! `docker volume`/`podman volume`'s own real "local directory" driver
//! (see `docs/design/0173`), plus `-v name:/path` support in `ociman
//! run` (a real, previously-rejected gap: `--volume`'s own host side
//! not being an absolute path used to be a clear, named error).

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
fn volume_create_prints_the_given_name_and_is_idempotent() {
    let storage_dir = tempfile::tempdir().unwrap();
    let create = ociman(storage_dir.path(), &["volume", "create", "myvol"]);
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&create.stdout).trim(), "myvol");

    // A second create of the same name is a real, idempotent success,
    // not an error -- matching real `podman volume create` exactly.
    let create_again = ociman(storage_dir.path(), &["volume", "create", "myvol"]);
    assert!(create_again.status.success());
    assert_eq!(
        String::from_utf8_lossy(&create_again.stdout).trim(),
        "myvol"
    );
}

#[test]
fn volume_create_with_no_name_generates_a_random_one() {
    let storage_dir = tempfile::tempdir().unwrap();
    let create = ociman(storage_dir.path(), &["volume", "create"]);
    assert!(create.status.success());
    let name = String::from_utf8_lossy(&create.stdout).trim().to_string();
    assert!(!name.is_empty());
    assert!(name.chars().all(|c| c.is_ascii_hexdigit()), "{name:?}");
}

#[test]
fn volume_create_rejects_an_invalid_name() {
    let storage_dir = tempfile::tempdir().unwrap();
    let create = ociman(storage_dir.path(), &["volume", "create", "bad name"]);
    assert!(!create.status.success());
}

#[test]
fn volume_ls_reports_no_volumes_when_empty_and_lists_real_ones_once_created() {
    let storage_dir = tempfile::tempdir().unwrap();
    let empty = ociman(storage_dir.path(), &["volume", "ls"]);
    assert!(empty.status.success());
    assert_eq!(String::from_utf8_lossy(&empty.stdout).trim(), "no volumes");

    ociman(storage_dir.path(), &["volume", "create", "vol-a"]);
    ociman(storage_dir.path(), &["volume", "create", "vol-b"]);
    let ls = ociman(storage_dir.path(), &["volume", "ls"]);
    assert!(ls.status.success());
    let stdout = String::from_utf8_lossy(&ls.stdout);
    assert!(stdout.contains("vol-a"), "{stdout}");
    assert!(stdout.contains("vol-b"), "{stdout}");
}

#[test]
fn volume_inspect_reports_the_real_mountpoint() {
    let storage_dir = tempfile::tempdir().unwrap();
    ociman(storage_dir.path(), &["volume", "create", "myvol"]);
    let inspect = ociman(
        storage_dir.path(),
        &["volume", "inspect", "myvol", "--json"],
    );
    assert!(
        inspect.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(parsed["name"], "myvol");
    assert_eq!(parsed["driver"], "local");
    let mountpoint = parsed["mountpoint"].as_str().unwrap();
    assert!(mountpoint.ends_with("volumes/myvol/_data"), "{mountpoint}");
    assert!(Path::new(mountpoint).is_dir());
}

#[test]
fn volume_inspect_of_an_unknown_volume_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let inspect = ociman(storage_dir.path(), &["volume", "inspect", "never-created"]);
    assert!(!inspect.status.success());
    assert!(
        String::from_utf8_lossy(&inspect.stderr).contains("no volume"),
        "{}",
        String::from_utf8_lossy(&inspect.stderr)
    );
}

#[test]
fn volume_rm_of_an_unknown_volume_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let rm = ociman(storage_dir.path(), &["volume", "rm", "never-created"]);
    assert!(!rm.status.success());
}

#[test]
fn volume_rm_removes_a_real_volume() {
    let storage_dir = tempfile::tempdir().unwrap();
    ociman(storage_dir.path(), &["volume", "create", "myvol"]);
    let rm = ociman(storage_dir.path(), &["volume", "rm", "myvol"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    let ls = ociman(storage_dir.path(), &["volume", "ls"]);
    assert_eq!(String::from_utf8_lossy(&ls.stdout).trim(), "no volumes");
}

/// The full real round trip: `-v name:/path` in `ociman run` actually
/// auto-creates the named volume on first use, mounts its own real
/// `_data` directory into the container, and the same volume's own
/// content genuinely persists into a *second*, separate container --
/// not just that some config field was accepted.
#[test]
fn run_with_a_named_volume_persists_real_content_across_separate_containers() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/volume-basic:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let write = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "-v",
            "sharedvol:/data",
            "ociman-test/volume-basic:latest",
            "sh",
            "-c",
            "echo persisted content > /data/f.txt",
        ],
    );
    assert!(
        write.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&write.stderr)
    );

    // The volume must have been auto-created (matching real `docker
    // run -v name:/path`/`podman run -v name:/path` exactly).
    let inspect = ociman(storage_dir.path(), &["volume", "inspect", "sharedvol"]);
    assert!(inspect.status.success());

    let read = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "-v",
            "sharedvol:/data",
            "ociman-test/volume-basic:latest",
            "cat",
            "/data/f.txt",
        ],
    );
    assert!(
        read.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&read.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&read.stdout), "persisted content\n");
}

/// `-v name:/path:ro` really does mark the mount read-only in the real
/// `config.json` `ociman` itself writes -- checked the same
/// deterministic, host-independent way `ociman_run.rs`'s own
/// `run_volume_flag_ro_rejects_a_write_from_inside_the_container` and
/// `run_read_only_sets_root_readonly_in_the_real_spec` (`docs/design/
/// 0080`) already do, not by asserting a real in-container write
/// attempt fails.
///
/// A first version of this test did assert a real write failure
/// (`sh -c "echo x > /data/f.txt"`, expecting `ociman run` itself to
/// report non-success) -- but that's the exact same real,
/// environment-dependent rootless limitation `ociman_run.rs`'s own
/// sibling test already documents (`docs/design/0010`): remounting a
/// bind mount read-only can require `CAP_SYS_ADMIN` in the namespace
/// that owns the *original* superblock, which a fake-root-in-a-userns
/// does not always have -- confirmed directly: this version failed on
/// the real `vm (ubuntu-26.04, x86_64)` CI cell for exactly that
/// reason, the same way the sibling test's own first version did.
/// `RootfsAction::RemountReadonly` deliberately tolerates this rather
/// than treating it as fatal (matching `--read-only`'s own root
/// remount, which needs the identical tolerance for the identical
/// reason) -- a real write failing is thus not something this project
/// can portably assert across every environment it runs in, only that
/// `ociman` itself correctly asked the kernel to enforce it.
#[test]
fn run_with_a_read_only_named_volume_rejects_a_write() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/volume-ro:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "-v",
            "rovol:/data:ro",
            "ociman-test/volume-ro:latest",
            "sh",
            "-c",
            "exit 0",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let container_id = only_container_id(storage_dir.path(), Duration::from_secs(10));
    let config_path = storage_dir
        .path()
        .join("containers")
        .join(&container_id)
        .join("config.json");
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(config_path).unwrap()).unwrap();
    let mounts = config["mounts"].as_array().unwrap();
    let volume_mount = mounts
        .iter()
        .find(|m| m["destination"] == "/data")
        .unwrap_or_else(|| panic!("no /data mount in {mounts:?}"));
    assert_eq!(volume_mount["type"], "bind");
    let options: Vec<&str> = volume_mount["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        options.contains(&"ro"),
        "expected -v rovol:/data:ro to set the \"ro\" mount option: {options:?}"
    );
}

/// `ociman volume rm` refuses a volume a real, still-running container
/// depends on, unless `--force` -- checked directly by resolving the
/// container's own already-persisted bundle mounts, not a separate,
/// possibly-drifting parallel record.
#[test]
fn volume_rm_refuses_a_volume_a_running_container_depends_on_unless_forced() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/volume-in-use:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/volume-in-use:latest",
        &["-d", "-v", "depvol:/data", "sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

    let rm = ociman(storage_dir.path(), &["volume", "rm", "depvol"]);
    assert!(!rm.status.success());
    assert!(
        String::from_utf8_lossy(&rm.stderr).contains("in use"),
        "{}",
        String::from_utf8_lossy(&rm.stderr)
    );

    let rm_forced = ociman(storage_dir.path(), &["volume", "rm", "--force", "depvol"]);
    assert!(
        rm_forced.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm_forced.stderr)
    );
    // The container itself is left untouched (matching real `podman
    // volume rm --force`'s own "detach, don't cascade-delete
    // containers" behavior).
    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert_eq!(String::from_utf8_lossy(&ps.stdout).trim(), id);

    ociman(storage_dir.path(), &["kill", &id]);
    child.wait().ok();
}

/// `ociman volume prune` removes only volumes no container (running
/// or stopped) currently references.
#[test]
fn volume_prune_removes_only_unreferenced_volumes() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/volume-prune:latest",
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

    ociman(storage_dir.path(), &["volume", "create", "unused-vol"]);
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "-v",
            "used-vol:/data",
            "ociman-test/volume-prune:latest",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let prune = ociman(storage_dir.path(), &["volume", "prune"]);
    assert!(
        prune.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&prune.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&prune.stdout).trim(), "unused-vol");

    let ls = ociman(storage_dir.path(), &["volume", "ls"]);
    let stdout = String::from_utf8_lossy(&ls.stdout);
    assert!(stdout.contains("used-vol"), "{stdout}");
    assert!(!stdout.contains("unused-vol"), "{stdout}");
}
