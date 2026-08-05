//! `ocibox create` integration tests: exercises the actual built
//! `ocibox` binary against a real, seeded image in local storage (the
//! same `oci_tools_tests::seed_image` fixture `ociman_run.rs`/
//! `ociboot_build_image.rs` already use) — confirming image
//! resolution (via the now-shared `oci_registry::resolve_or_pull`,
//! 0204), real per-box rootfs extraction, and the persisted
//! `box.json` record all actually work together end to end.

use std::path::Path;
use std::process::Command;

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image};

fn ocibox(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ocibox")
}

#[test]
fn create_extracts_a_real_rootfs_and_persists_a_box_record() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/create-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/create-base:latest",
            "--name",
            "testbox",
        ],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&create.stdout).trim(), "testbox");

    let box_dir = storage_dir.path().join("boxes").join("testbox");
    let rootfs = box_dir.join("rootfs");
    assert!(rootfs.join("bin").join("busybox").is_file());
    // A symlinked applet from `seed_image`'s own rootfs -- confirms a
    // real, full layer extraction happened, not just an empty
    // directory.
    assert!(rootfs.join("bin").join("sh").exists());

    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(box_dir.join("box.json")).unwrap()).unwrap();
    assert_eq!(record["name"], "testbox");
    assert_eq!(record["image"], "docker.io/ocibox-test/create-base:latest");
    assert!(
        record["manifest_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(record["created"].as_str().is_some());
}

/// `--yes`/`-Y` is accepted for real CLI compatibility with real
/// `distrobox create --yes`/`-Y` (checked directly, `~/git/distrobox/
/// pkg/commands/create.go`'s own `askPullImage`) but changes nothing
/// at all: this project has no interactive confirmation prompt to
/// skip in the first place, the same "nothing to skip" reasoning
/// `ocibox rm --force`'s own doc comment already gives.
#[test]
fn create_yes_flag_is_accepted_and_behaves_identically() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/create-yes:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/create-yes:latest",
            "--name",
            "yesbox",
            "--yes",
        ],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&create.stdout).trim(), "yesbox");

    // The short flag `-Y` behaves identically.
    let create_short = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/create-yes:latest",
            "--name",
            "yesbox2",
            "-Y",
        ],
    );
    assert!(create_short.status.success(), "{create_short:?}");
}

#[test]
fn create_refuses_a_name_already_in_use() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/dup-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let args = [
        "create",
        "--image",
        "ocibox-test/dup-base:latest",
        "--name",
        "dupbox",
    ];
    let first = ocibox(storage_dir.path(), &args);
    assert!(first.status.success());

    let second = ocibox(storage_dir.path(), &args);
    assert!(!second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already exists"),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
}

#[test]
fn create_rejects_an_invalid_box_name() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/badname-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/badname-base:latest",
            "--name",
            "not a valid name",
        ],
    );
    assert!(!create.status.success());
    assert!(
        String::from_utf8_lossy(&create.stderr).contains("invalid box name"),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
    assert!(
        !storage_dir.path().join("boxes").exists(),
        "an invalid name should never even create the boxes directory"
    );
}

/// `--hostname` (0344) longer than 64 characters is a clear,
/// immediate error -- matching real `distrobox create --hostname`'s
/// own identical `ErrHostnameTooLong` exactly (`~/git/distrobox/pkg/
/// commands/create.go`'s own `maxHostnameLength`) -- and, same as an
/// invalid box name, leaves no half-created box directory behind.
#[test]
fn create_rejects_a_hostname_over_64_characters() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/badhostname-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let too_long = "a".repeat(65);
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/badhostname-base:latest",
            "--name",
            "testbox",
            "--hostname",
            &too_long,
        ],
    );
    assert!(!create.status.success());
    assert!(
        String::from_utf8_lossy(&create.stderr).contains("hostname too long"),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
    assert!(
        !storage_dir.path().join("boxes").exists(),
        "a too-long hostname should never even create the boxes directory"
    );
}

/// An unknown reference, with no `--pull` and nothing already stored
/// (real `distrobox create`'s own default is closer to "pull only if
/// needed", matching this project's own `PullPolicy::Missing` default
/// here), is a clear error naming the underlying resolve failure --
/// and, critically, leaves no half-created box directory behind.
#[test]
fn create_of_an_unresolvable_reference_leaves_no_box_directory_behind() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    // An unreachable registry host -- guaranteed to fail the pull
    // attempt itself, not just "not found locally" (this project's
    // own `PullPolicy::Missing` default always attempts a real pull
    // when nothing is stored yet).
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "127.0.0.1:1/doesnotexist:latest",
            "--name",
            "testbox",
        ],
    );
    assert!(!create.status.success());
    assert!(
        !storage_dir.path().join("boxes").join("testbox").exists(),
        "a failed create should leave no box directory behind at all"
    );
}

/// `--platform` (0403): a real, valid value matching this test host's
/// own actual platform still resolves and extracts a real rootfs
/// exactly as before this flag existed -- confirming the CLI plumbing
/// itself (not the underlying platform-selection logic, already
/// proven end to end against a real multi-platform index by
/// `ociman_platform.rs`'s own tests, reused here completely
/// unchanged) is genuinely wired through to `create_box`.
#[test]
fn create_platform_flag_accepts_a_value_matching_the_host_and_succeeds() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/platform-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let host = oci_spec_types::image::Platform::host();
    let platform_value = format!("{}/{}", host.os, host.architecture);

    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/platform-base:latest",
            "--name",
            "platformbox",
            "--platform",
            &platform_value,
        ],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let rootfs = storage_dir
        .path()
        .join("boxes")
        .join("platformbox")
        .join("rootfs");
    assert!(rootfs.join("bin").join("busybox").is_file());
}

/// A malformed `--platform` value is a real, immediate CLI error
/// (surfaced through the shared `oci_spec_types::image::
/// parse_platform_spec`, `docs/design/0403`), leaving no half-created
/// box directory behind -- the same "a failed create leaves nothing
/// behind" guarantee `create_of_an_unresolvable_reference_leaves_no_
/// box_directory_behind` already proves for a failed pull.
#[test]
fn create_platform_flag_rejects_an_invalid_value() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/platform-invalid:latest",
            "--name",
            "badplatformbox",
            "--platform",
            "linux",
        ],
    );
    assert!(!create.status.success());
    assert!(
        String::from_utf8_lossy(&create.stderr).contains("missing an architecture"),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
    assert!(
        !storage_dir
            .path()
            .join("boxes")
            .join("badplatformbox")
            .exists(),
        "an invalid --platform value should leave no box directory behind at all"
    );
}

/// `ocibox create --clone`/`-c` (matching real `distrobox create
/// --clone` exactly for this project's own simpler model, see
/// `Command::Create::clone`'s own doc comment): a real copy of the
/// source box's own current `rootfs/` -- including a write made after
/// the source's own creation, proving this genuinely copies the
/// box's own *current* state, not just re-extracts the original
/// image -- plus its own `image`/`env`/`working_dir`, under the new
/// name.
#[test]
fn create_clone_copies_the_source_boxs_own_current_rootfs_and_record() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/clone-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/clone-base:latest",
            "--name",
            "source-box",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    // A real write to the source box's own rootfs, after creation --
    // the clone must carry this forward too, proving it's a genuine
    // copy of the *current* state.
    let source_rootfs = storage_dir
        .path()
        .join("boxes")
        .join("source-box")
        .join("rootfs");
    std::fs::write(source_rootfs.join("canary.txt"), b"from the source box").unwrap();

    let clone = ocibox(
        storage_dir.path(),
        &["create", "--clone", "source-box", "--name", "cloned-box"],
    );
    assert!(
        clone.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&clone.stdout).trim(), "cloned-box");

    let clone_rootfs = storage_dir
        .path()
        .join("boxes")
        .join("cloned-box")
        .join("rootfs");
    assert!(clone_rootfs.join("bin").join("busybox").is_file());
    assert_eq!(
        std::fs::read(clone_rootfs.join("canary.txt")).unwrap(),
        b"from the source box"
    );
    // A real, independent copy -- writing to the clone's own rootfs
    // must never affect the source's.
    std::fs::write(clone_rootfs.join("clone-only.txt"), b"only in the clone").unwrap();
    assert!(!source_rootfs.join("clone-only.txt").exists());

    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            storage_dir
                .path()
                .join("boxes")
                .join("cloned-box")
                .join("box.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(record["name"], "cloned-box");
    assert_eq!(record["image"], "docker.io/ocibox-test/clone-base:latest");
}

/// `--clone` of an unknown source box is a clear error, leaving no
/// half-created box directory behind.
#[test]
fn create_clone_of_an_unknown_source_box_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();

    let clone = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--clone",
            "does-not-exist",
            "--name",
            "cloned-box",
        ],
    );
    assert!(!clone.status.success());
    assert!(
        !storage_dir.path().join("boxes").join("cloned-box").exists(),
        "a failed clone should leave no box directory behind at all"
    );
}

/// `--image` and `--clone` together, or neither at all, are both
/// clear, immediate errors.
#[test]
fn create_requires_exactly_one_of_image_or_clone() {
    let storage_dir = tempfile::tempdir().unwrap();

    let neither = ocibox(storage_dir.path(), &["create", "--name", "somebox"]);
    assert!(!neither.status.success());
    assert!(
        String::from_utf8_lossy(&neither.stderr)
            .contains("exactly one of --image or --clone must be given"),
        "{}",
        String::from_utf8_lossy(&neither.stderr)
    );

    let both = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/whatever:latest",
            "--clone",
            "whatever",
            "--name",
            "somebox",
        ],
    );
    assert!(!both.status.success());
    assert!(
        String::from_utf8_lossy(&both.stderr)
            .contains("exactly one of --image or --clone must be given"),
        "{}",
        String::from_utf8_lossy(&both.stderr)
    );
}

/// `--hostname`/`--home` given explicitly at `--clone` time are
/// independent of the clone source -- exactly like an ordinary
/// `--image` create, matching `Command::Create::clone`'s own doc
/// comment.
#[test]
fn create_clone_still_honors_its_own_explicit_hostname_and_home() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/clone-overrides-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/clone-overrides-base:latest",
            "--name",
            "override-source",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let custom_home = storage_dir.path().join("custom-home");
    let clone = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--clone",
            "override-source",
            "--name",
            "override-clone",
            "--hostname",
            "custom-clone-hostname",
            "--home",
            custom_home.to_str().unwrap(),
        ],
    );
    assert!(
        clone.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&clone.stderr)
    );

    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            storage_dir
                .path()
                .join("boxes")
                .join("override-clone")
                .join("box.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(record["hostname"], "custom-clone-hostname");
    assert_eq!(
        record["custom_home"],
        serde_json::json!(custom_home.to_str().unwrap())
    );
}
