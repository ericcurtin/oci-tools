//! `ociman export` integration tests: writing a container's entire
//! current filesystem out as a real, flat tar, matching real `docker
//! export`/`podman export` (see `docs/design/0169`). Same fully
//! offline seeded-image approach `ociman_diff.rs`/`ociman_cp.rs`
//! established, including forcing `.rootless-overlay-supported` to
//! `false` so the container under test deterministically uses the
//! plain `RootfsSetup::Extract` layout `export` actually supports
//! (the same rootless-overlay-rootfs gap `cp`/`diff`/`commit` already
//! have).
//!
//! `export_of_a_still_running_container_completes_quickly_and_excludes_
//! live_mounts` is the one real regression test for a genuine bug
//! found and fixed during this feature's own development: exporting a
//! *running* container (whose `/proc`/`/sys` are actively bind-mounted
//! onto its own rootfs for the container's lifetime) previously walked
//! straight into those live, effectively-unbounded pseudo-filesystems
//! too, producing a many-hundred-megabyte archive instead of the real
//! few-megabyte image it should have been -- every other test here
//! uses an already-*stopped* container (whose mounts are already torn
//! down), which never exercised this at all.

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
/// plain-`Extract` rootfs setup deterministically first (see the
/// module's own doc comment).
fn seed_and_run_stopped_container(storage_root: &Path, image: &str, shell_command: &str) -> String {
    std::fs::write(storage_root.join(".rootless-overlay-supported"), "false").unwrap();
    let busybox = busybox_path().expect("busybox not found on $PATH");
    let store = Store::open(storage_root).unwrap();
    seed_image(
        &store,
        image,
        &busybox,
        &["sh", "ls"],
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

fn read_tar_paths(bytes: &[u8]) -> Vec<String> {
    let mut archive = tar::Archive::new(bytes);
    archive
        .entries()
        .unwrap()
        .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn export_of_an_unknown_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let output = tempfile::NamedTempFile::new().unwrap();
    let export = ociman(
        storage_dir.path(),
        &[
            "export",
            "-o",
            output.path().to_str().unwrap(),
            "never-existed",
        ],
    );
    assert!(!export.status.success());
}

#[test]
fn export_writes_every_real_file_verbatim_no_whiteouts_no_layer_semantics() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/export-basic:latest",
        "echo hi > /new-file.txt; rm /bin/sh",
    );

    let output_path = storage_dir.path().join("out.tar");
    let export = ociman(
        storage_dir.path(),
        &["export", "-o", output_path.to_str().unwrap(), &id],
    );
    assert!(
        export.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&export.stderr)
    );

    let archive_bytes = std::fs::read(&output_path).unwrap();
    let paths = read_tar_paths(&archive_bytes);
    // The new file this container itself added is present with real
    // content.
    assert!(paths.contains(&"new-file.txt".to_string()), "{paths:?}");
    // Unlike `ociman diff`'s own layer-diff view, `export` is the
    // *whole current tree*: `/bin/ls` (never touched by this
    // container) is still there even though `/bin/sh` was removed --
    // there's no "unchanged from base" concept for a plain filesystem
    // export at all.
    assert!(paths.contains(&"bin/ls".to_string()), "{paths:?}");
    assert!(!paths.contains(&"bin/sh".to_string()), "{paths:?}");

    let mut archive = tar::Archive::new(&archive_bytes[..]);
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().to_str() == Some("new-file.txt") {
            let mut content = String::new();
            std::io::Read::read_to_string(&mut entry, &mut content).unwrap();
            assert_eq!(content, "hi\n");
        }
    }
}

#[test]
fn export_with_no_output_flag_writes_the_archive_straight_to_stdout() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/export-stdout:latest",
        "echo hi > /new-file.txt",
    );

    let export = ociman(storage_dir.path(), &["export", &id]);
    assert!(
        export.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    let paths = read_tar_paths(&export.stdout);
    assert!(paths.contains(&"new-file.txt".to_string()), "{paths:?}");
}

/// The real regression test -- see this module's own doc comment.
#[test]
fn export_of_a_still_running_container_completes_quickly_and_excludes_live_mounts() {
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
        "ociman-test/export-running:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "--name",
            "export-running-test",
            "ociman-test/export-running:latest",
            "sh",
            "-c",
            "echo still running > /myfile.txt; sleep 30",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman run");

    // Wait for the container's own status to actually be "running"
    // (its own /proc/ /sys mounts only exist once launch has actually
    // gotten that far -- merely appearing in `ps` at all happens
    // earlier, while still `created`), matching this project's own
    // established wait-for-status polling pattern.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let ps = ociman(storage_dir.path(), &["ps", "--json"]);
        if let Ok(views) = serde_json::from_slice::<serde_json::Value>(&ps.stdout)
            && let Some(status) = views
                .as_array()
                .and_then(|entries| entries.iter().find(|e| e["name"] == "export-running-test"))
                .and_then(|e| e["status"].as_str())
            && status == "running"
        {
            break;
        }
        assert!(Instant::now() < deadline, "container never started running");
        std::thread::sleep(Duration::from_millis(20));
    }
    // A real, inherent race remains even after "running" is observed:
    // that status flips the moment the container process is launched,
    // not once its own shell script has gotten as far as actually
    // running the `echo` before its own `sleep 30` -- a short, fixed
    // grace period here rather than polling for the file itself
    // (which would need to reach back into the container's own
    // rootfs directly, duplicating what this test is actually trying
    // to exercise).
    std::thread::sleep(Duration::from_millis(300));

    let output_path = storage_dir.path().join("out.tar");
    let start = Instant::now();
    let export = ociman(
        storage_dir.path(),
        &[
            "export",
            "-o",
            output_path.to_str().unwrap(),
            "export-running-test",
        ],
    );
    let elapsed = start.elapsed();
    assert!(
        export.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    // Real busybox images are a few MB; a naive walk that fell into
    // /proc's own live, effectively-unbounded content previously
    // produced a many-hundred-megabyte archive that took a real,
    // multi-second-plus amount of time to produce. A generous, but
    // still real, upper bound on both.
    assert!(
        elapsed < Duration::from_secs(30),
        "export took {elapsed:?}, expected well under 30s"
    );
    let size = std::fs::metadata(&output_path).unwrap().len();
    assert!(
        size < 50 * 1024 * 1024,
        "export was {size} bytes, expected well under 50MB for a real busybox image"
    );

    let archive_bytes = std::fs::read(&output_path).unwrap();
    let paths = read_tar_paths(&archive_bytes);
    assert!(paths.contains(&"myfile.txt".to_string()), "{paths:?}");
    // The mount-point directories themselves are still archived (as
    // empty directories -- exactly what a real storage-driver-level
    // export would also show for them), just never recursed into.
    assert!(paths.contains(&"proc".to_string()), "{paths:?}");
    assert!(!paths.iter().any(|p| p.starts_with("proc/")), "{paths:?}");

    child.kill().ok();
    child.wait().ok();
}
