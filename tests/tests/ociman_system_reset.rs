//! `ociman system reset` integration tests (`docs/design/0367`): wipes
//! every container (any status, including genuinely running/paused
//! ones), volume, and image back to a pristine empty state in one
//! call — matching real `podman system reset`'s own core effect
//! exactly, but deliberately scoped to only what `ociman` itself owns
//! under the shared storage root (unlike real podman's own literal
//! `graphRoot`/`runRoot` deletion): a sibling binary's own state
//! sharing the same root (`ocibox`'s `boxes/`, `ocicri`'s
//! `cri-containers/`/`cri-sandboxes/`/`cri-bundles/`) must survive
//! completely untouched.

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

#[test]
fn reset_on_an_empty_store_succeeds_silently() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let reset = ociman(storage_dir.path(), &["system", "reset"]);
    assert!(
        reset.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&reset.stderr)
    );
    assert!(
        reset.stdout.is_empty() && reset.stderr.is_empty(),
        "matching real `podman system reset`'s own identical silent completion: \
         stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&reset.stdout),
        String::from_utf8_lossy(&reset.stderr)
    );
}

/// The real, main case: a stopped container, a genuinely *running*
/// one, a volume, and an image are all removed -- a running
/// container is never a special case real `podman system reset`
/// exempts either (checked directly, `~/git/podman/libpod/reset.go`'s
/// own `RemoveContainerAndDependencies(ctx, c, force: true, ...)`).
#[test]
fn reset_removes_every_container_volume_and_image_regardless_of_status() {
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
        "ociman-test/system-reset:latest",
        &busybox,
        &["sh", "true", "sleep"],
        ContainerConfig::default(),
    );

    // A real, already-stopped container.
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/system-reset:latest", "true"],
    );
    assert!(run.status.success(), "{run:?}");
    let stopped_id =
        String::from_utf8_lossy(&ociman(storage_dir.path(), &["ps", "-a", "-q"]).stdout)
            .trim()
            .to_string();
    assert!(!stopped_id.is_empty());

    // A real volume.
    let volume_create = ociman(storage_dir.path(), &["volume", "create", "reset-vol"]);
    assert!(volume_create.status.success(), "{volume_create:?}");

    // A genuinely *running* container.
    let run_detached = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "-d",
            "ociman-test/system-reset:latest",
            "sleep",
            "30",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn ociman run -d");
    assert!(run_detached.status.success(), "{run_detached:?}");

    let running_id =
        String::from_utf8_lossy(&ociman(storage_dir.path(), &["ps", "-a", "-q"]).stdout)
            .lines()
            .find(|id| *id != stopped_id)
            .map(str::to_string)
            .expect("a second (the detached) container should now exist");

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let inspect = ociman(storage_dir.path(), &["inspect", &running_id, "--json"]);
        let json: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
        if json["status"] == "running" || Instant::now() >= deadline {
            assert_eq!(json["status"], "running", "{json:?}");
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // Confirm the pre-reset state actually has two containers, one
    // volume, and one image before proceeding.
    assert_eq!(
        String::from_utf8_lossy(&ociman(storage_dir.path(), &["ps", "-a", "-q"]).stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .count(),
        2,
        "expected exactly two containers before reset"
    );
    assert_eq!(
        String::from_utf8_lossy(&ociman(storage_dir.path(), &["volume", "ls", "-q"]).stdout).trim(),
        "reset-vol"
    );
    assert_eq!(
        String::from_utf8_lossy(&ociman(storage_dir.path(), &["images", "-q"]).stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .count(),
        1,
        "expected exactly one image before reset"
    );

    let reset = ociman(storage_dir.path(), &["system", "reset"]);
    assert!(
        reset.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&reset.stderr)
    );

    assert!(
        String::from_utf8_lossy(&ociman(storage_dir.path(), &["ps", "-a", "-q"]).stdout)
            .trim()
            .is_empty(),
        "every container, running or not, should be gone after reset"
    );
    assert!(
        String::from_utf8_lossy(&ociman(storage_dir.path(), &["volume", "ls", "-q"]).stdout)
            .trim()
            .is_empty(),
        "every volume should be gone after reset"
    );
    assert!(
        String::from_utf8_lossy(&ociman(storage_dir.path(), &["images", "-q"]).stdout)
            .trim()
            .is_empty(),
        "every image should be gone after reset"
    );
}

/// A sibling binary's own state, sharing the same storage root, must
/// survive `ociman system reset` completely untouched -- this project
/// deliberately doesn't follow real podman's own literal `graphRoot`/
/// `runRoot` deletion, since (unlike real podman) that root is shared
/// across every binary here.
#[test]
fn reset_never_touches_a_sibling_binarys_own_storage() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let sibling_dirs = [
        storage_dir.path().join("boxes").join("some-box"),
        storage_dir.path().join("cri-containers").join("some-id"),
        storage_dir.path().join("cri-sandboxes").join("some-id"),
        storage_dir.path().join("cri-bundles").join("some-id"),
    ];
    for dir in &sibling_dirs {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("marker"), b"keep me").unwrap();
    }

    let reset = ociman(storage_dir.path(), &["system", "reset"]);
    assert!(reset.status.success(), "{reset:?}");

    for dir in &sibling_dirs {
        assert!(
            dir.join("marker").is_file(),
            "{} should have survived reset untouched",
            dir.display()
        );
    }
}

/// `--force`/`-f` is accepted (real CLI compatibility with `podman
/// system reset --force`) but changes nothing: this project has no
/// interactive confirmation prompt to skip in the first place.
#[test]
fn reset_force_is_accepted_and_behaves_identically() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let reset = ociman(storage_dir.path(), &["system", "reset", "--force"]);
    assert!(reset.status.success(), "{reset:?}");
}
