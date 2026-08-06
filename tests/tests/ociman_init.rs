//! `ociman init` (`docs/design/0530`): real `podman init`/`podman
//! container init`'s own real, dual-registered subcommand --
//! "initialize one or more containers, creating the OCI spec and
//! mounts for inspection" (real podman's own doc string). This
//! project's own `create` already does the equivalent real
//! OCI-runtime `create` step eagerly, so a `Stopped` container is a
//! real, faithful no-op success here, while a `Created`/`Running`/
//! `Paused` one is a real, reported "already created in runtime"
//! error -- matching real podman's own identical eligibility check
//! exactly (`~/git/podman/libpod/container_api.go`'s own
//! `initUnlocked`).

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

fn ociman_run_detached(storage_root: &Path, image: &str, container_args: &[&str]) {
    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "-d", image])
        .args(container_args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn ociman run -d");
    assert!(
        out.status.success(),
        "ociman run -d failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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

fn all_ids(storage_root: &Path) -> Vec<String> {
    let out = ociman(storage_root, &["ps", "-a", "-q"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn seed(storage_root: &Path, image: &str, busybox: &Path) {
    seed_image(
        &Store::open(storage_root).unwrap(),
        image,
        busybox,
        &["sh", "true", "sleep"],
        ContainerConfig::default(),
    );
}

/// A `Created` (never-started) container is a real, reported
/// "already created in runtime" error -- this project's own
/// `Status::Created` maps onto real podman's own post-`Init`,
/// already-initialized `Created` state, never its pre-`Init`
/// `Configured` one.
#[test]
fn init_on_a_created_container_is_a_real_already_created_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(
        storage_dir.path(),
        "ociman-test/init-created:latest",
        &busybox,
    );
    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/init-created:latest", "true"],
    );
    assert!(create.status.success());
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let init = ociman(storage_dir.path(), &["init", &id]);
    assert!(!init.status.success());
    assert!(
        String::from_utf8_lossy(&init.stderr).contains("already been created in runtime"),
        "{init:?}"
    );
    // Untouched -- still `Created`.
    assert_eq!(inspect_json(storage_dir.path(), &id)["status"], "created");
}

/// A `Stopped` container is eligible, but a real, faithful no-op:
/// this project's own `start` always does a full, fresh launch from
/// the bundle regardless of prior status, so there's no separate,
/// in-advance "reinitialize the runtime container" step to actually
/// perform.
#[test]
fn init_on_a_stopped_container_is_a_real_no_op_success() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(
        storage_dir.path(),
        "ociman-test/init-stopped:latest",
        &busybox,
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/init-stopped:latest", "true"],
    );
    assert!(run.status.success(), "{run:?}");
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");
    assert_eq!(inspect_json(storage_dir.path(), &id)["status"], "stopped");

    let init = ociman(storage_dir.path(), &["init", &id]);
    assert!(
        init.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&init.stdout).trim(), id);
    // Still `Stopped` -- a real no-op, not a status change.
    assert_eq!(inspect_json(storage_dir.path(), &id)["status"], "stopped");
}

/// A genuinely `Running` container is also a real, reported error --
/// matching real podman's own identical refusal for anything short
/// of `Configured`/`Stopped`/`Exited`.
#[test]
fn init_on_a_running_container_is_a_real_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(
        storage_dir.path(),
        "ociman-test/init-running:latest",
        &busybox,
    );
    ociman_run_detached(
        storage_dir.path(),
        "ociman-test/init-running:latest",
        &["sleep", "30"],
    );
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let init = ociman(storage_dir.path(), &["init", &id]);
    assert!(!init.status.success());
    assert!(
        String::from_utf8_lossy(&init.stderr).contains("already been created in runtime"),
        "{init:?}"
    );

    ociman(storage_dir.path(), &["kill", &id]);
}

/// `--all` sweeps every container, oldest first, and *silently
/// tolerates* an ineligible one -- its id is still printed, as if
/// successful, matching real `ContainerInit`'s own exact
/// `errors.Is(err, ErrCtrStateInvalid)` swallowing under `--all`.
#[test]
fn init_all_tolerates_an_ineligible_container_and_still_prints_its_id() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(storage_dir.path(), "ociman-test/init-all:latest", &busybox);

    // One `Created` (ineligible) and one `Stopped` (eligible).
    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/init-all:latest", "true"],
    );
    assert!(create.status.success());
    let created_id = String::from_utf8_lossy(&create.stdout).trim().to_string();
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/init-all:latest", "true"],
    );
    assert!(run.status.success());
    let stopped_id = all_ids(storage_dir.path())
        .into_iter()
        .find(|id| id != &created_id)
        .expect("a second container should now exist");

    let init = ociman(storage_dir.path(), &["init", "--all"]);
    assert!(
        init.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let mut printed: Vec<String> = String::from_utf8_lossy(&init.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    printed.sort();
    let mut expected = vec![created_id, stopped_id];
    expected.sort();
    assert_eq!(printed, expected);
}

/// `--latest` on a genuinely empty container store is a real,
/// ordinary hard error -- unlike `ociman container cleanup`'s own
/// real, checked-directly divergence (0529), real `init` has no
/// analogous "conmon lost the race" special case for this.
#[test]
fn init_latest_on_an_empty_store_is_a_real_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let init = ociman(storage_dir.path(), &["init", "--latest"]);
    assert!(!init.status.success());
    assert!(
        String::from_utf8_lossy(&init.stderr).contains("no container has been created yet"),
        "{init:?}"
    );
}

/// Giving even one unresolvable explicit name aborts the *whole*
/// call with a real, immediate error before touching anything --
/// unlike `ociman container cleanup`'s own deliberate whole-call
/// *silent* success inversion (0529), real `ContainerInit` never
/// swallows a resolution failure at all.
#[test]
fn init_with_one_unresolvable_name_aborts_the_whole_call_with_a_real_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(
        storage_dir.path(),
        "ociman-test/init-unresolvable:latest",
        &busybox,
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/init-unresolvable:latest", "true"],
    );
    assert!(run.status.success());
    let real_id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");

    let init = ociman(storage_dir.path(), &["init", &real_id, "does-not-exist"]);
    assert!(!init.status.success());
    // Nothing printed at all -- the real container was never even
    // attempted, unlike `cleanup`'s own silent-success inversion.
    assert!(String::from_utf8_lossy(&init.stdout).trim().is_empty());
}

/// Matches real podman's own exact validation, checked directly
/// (`CheckAllLatestAndIDFile`, `ignoreArgLen = false`): a bare
/// invocation with no target at all is a real, immediate error.
#[test]
fn init_with_no_target_at_all_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let init = ociman(storage_dir.path(), &["init"]);
    assert!(!init.status.success());
    assert!(
        String::from_utf8_lossy(&init.stderr).contains("you must provide at least one name or id")
    );
}

/// `--all` and `--latest` together is a real, immediate error.
#[test]
fn init_all_and_latest_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let init = ociman(storage_dir.path(), &["init", "--all", "--latest"]);
    assert!(!init.status.success());
    assert!(
        String::from_utf8_lossy(&init.stderr)
            .contains("--all and --latest cannot be used together")
    );
}
