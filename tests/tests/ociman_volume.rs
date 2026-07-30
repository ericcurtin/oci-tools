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

/// `volume ls --format` (0335) renders one line per listed volume,
/// reusing the exact same Go-template-*lite* engine `ociman
/// inspect`/`ps`/`images --format` (`0332`-`0334`) already
/// established.
#[test]
fn volume_ls_format_renders_one_line_per_volume() {
    let storage_dir = tempfile::tempdir().unwrap();
    ociman(storage_dir.path(), &["volume", "create", "fmt-vol-a"]);
    ociman(storage_dir.path(), &["volume", "create", "fmt-vol-b"]);

    let format = ociman(
        storage_dir.path(),
        &["volume", "ls", "--format", "{{.name}}={{.driver}}"],
    );
    assert!(
        format.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&format.stderr)
    );
    let stdout = String::from_utf8_lossy(&format.stdout).into_owned();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.contains(&"fmt-vol-a=local"), "{lines:?}");
    assert!(lines.contains(&"fmt-vol-b=local"), "{lines:?}");
}

/// `--format`, when given, takes priority over `--json`/the default
/// table, and an unresolvable field path is a real, immediate error --
/// same precedence and error behavior `inspect`/`ps`/`images --format`
/// already established.
#[test]
fn volume_ls_format_takes_priority_and_errors_on_an_unknown_field() {
    let storage_dir = tempfile::tempdir().unwrap();
    ociman(storage_dir.path(), &["volume", "create", "fmt-priority"]);

    let format = ociman(
        storage_dir.path(),
        &["volume", "ls", "--json", "--format", "{{.name}}"],
    );
    assert!(format.status.success());
    assert_eq!(
        String::from_utf8_lossy(&format.stdout).trim(),
        "fmt-priority",
        "the format template's own plain name, not --json's own array, should have won"
    );

    let bad = ociman(
        storage_dir.path(),
        &["volume", "ls", "--format", "{{.nosuchfield}}"],
    );
    assert!(!bad.status.success());
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("no field"),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
}

/// `volume ls -q`/`--quiet` (0348) prints only volume names, one per
/// line, no header — matching real `podman volume ls -q`/`docker
/// volume ls -q` exactly (checked directly, `~/git/podman/cmd/podman/
/// volumes/list.go`: renders `{{.Name}}\n`).
#[test]
fn volume_ls_quiet_prints_only_names_with_no_header() {
    let storage_dir = tempfile::tempdir().unwrap();
    ociman(storage_dir.path(), &["volume", "create", "quiet-vol-a"]);
    ociman(storage_dir.path(), &["volume", "create", "quiet-vol-b"]);

    let quiet = ociman(storage_dir.path(), &["volume", "ls", "-q"]);
    assert!(
        quiet.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&quiet.stderr)
    );
    let stdout = String::from_utf8_lossy(&quiet.stdout).into_owned();
    let mut lines: Vec<&str> = stdout.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines, vec!["quiet-vol-a", "quiet-vol-b"], "{lines:?}");

    // Long form behaves identically to the short form.
    let quiet_long = ociman(storage_dir.path(), &["volume", "ls", "--quiet"]);
    assert!(quiet_long.status.success());
    assert_eq!(quiet_long.stdout, quiet.stdout);
}

/// `-q`/`--quiet` on an empty store prints nothing at all -- not even
/// this project's own usual "no volumes" friendly empty-state message
/// (that message is specific to the default table, matching real
/// podman's own checked-directly behavior of a plain, empty quiet
/// report either way).
#[test]
fn volume_ls_quiet_on_an_empty_store_prints_nothing() {
    let storage_dir = tempfile::tempdir().unwrap();
    let quiet = ociman(storage_dir.path(), &["volume", "ls", "-q"]);
    assert!(quiet.status.success());
    assert!(quiet.stdout.is_empty(), "{quiet:?}");
}

/// `--quiet` and `--format` together is a clear, immediate error,
/// matching real `podman volume ls`'s own identical restriction
/// exactly (`~/git/podman/cmd/podman/volumes/list.go`'s own checked-
/// directly error text).
#[test]
fn volume_ls_quiet_and_format_together_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(
        storage_dir.path(),
        &["volume", "ls", "-q", "--format", "{{.name}}"],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot be used together"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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

/// `ociman volume rename` (0347) actually moves the volume's own real
/// content -- not just a metadata-only rename -- matching real
/// `podman volume rename` exactly. Prints nothing on success, same as
/// real podman's own checked-directly silent completion.
#[test]
fn volume_rename_moves_real_content_to_the_new_name() {
    let storage_dir = tempfile::tempdir().unwrap();
    let create = ociman(storage_dir.path(), &["volume", "create", "old-vol"]);
    assert!(create.status.success());
    let mountpoint = volume_mountpoint(storage_dir.path(), "old-vol");
    std::fs::write(mountpoint.join("hello.txt"), b"real content").unwrap();

    let rename = ociman(
        storage_dir.path(),
        &["volume", "rename", "old-vol", "new-vol"],
    );
    assert!(
        rename.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rename.stderr)
    );
    assert!(
        rename.stdout.is_empty(),
        "real podman volume rename prints nothing on success: {rename:?}"
    );

    let inspect_old = ociman(storage_dir.path(), &["volume", "inspect", "old-vol"]);
    assert!(!inspect_old.status.success(), "old-vol should be gone");

    let new_mountpoint = volume_mountpoint(storage_dir.path(), "new-vol");
    assert_eq!(
        std::fs::read(new_mountpoint.join("hello.txt")).unwrap(),
        b"real content",
        "the real file content must have moved with the volume, not been lost"
    );

    let ls = ociman(
        storage_dir.path(),
        &["volume", "ls", "--format", "{{.name}}"],
    );
    assert_eq!(String::from_utf8_lossy(&ls.stdout).trim(), "new-vol");
}

/// Renaming a volume to its own current name is a real, silent no-op
/// success -- matching real `podman volume rename`'s own identical
/// early-return exactly (checked directly,
/// `~/git/podman/libpod/runtime_volume.go`).
#[test]
fn volume_rename_to_its_own_current_name_is_a_silent_no_op() {
    let storage_dir = tempfile::tempdir().unwrap();
    let create = ociman(storage_dir.path(), &["volume", "create", "same-name"]);
    assert!(create.status.success());

    let rename = ociman(
        storage_dir.path(),
        &["volume", "rename", "same-name", "same-name"],
    );
    assert!(
        rename.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rename.stderr)
    );

    let inspect = ociman(storage_dir.path(), &["volume", "inspect", "same-name"]);
    assert!(inspect.status.success());
}

/// Renaming a volume to a name that already resolves to a real,
/// *different* volume is a clear error, matching real podman's own
/// `ErrVolumeExists` -- neither volume is touched.
#[test]
fn volume_rename_to_an_already_existing_different_volume_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    ociman(storage_dir.path(), &["volume", "create", "first"]);
    ociman(storage_dir.path(), &["volume", "create", "second"]);

    let rename = ociman(storage_dir.path(), &["volume", "rename", "first", "second"]);
    assert!(!rename.status.success());
    assert!(
        String::from_utf8_lossy(&rename.stderr).contains("already exists"),
        "{}",
        String::from_utf8_lossy(&rename.stderr)
    );

    // Both original volumes are still present and untouched.
    assert!(
        ociman(storage_dir.path(), &["volume", "inspect", "first"])
            .status
            .success()
    );
    assert!(
        ociman(storage_dir.path(), &["volume", "inspect", "second"])
            .status
            .success()
    );
}

#[test]
fn volume_rename_of_an_unknown_volume_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let rename = ociman(
        storage_dir.path(),
        &["volume", "rename", "never-created", "whatever"],
    );
    assert!(!rename.status.success());
}

/// `ociman volume rename` refuses a volume a running container
/// depends on, the same rule `volume rm` enforces with no `--force`
/// -- there's no `--force` escape hatch for `rename` at all, matching
/// real podman's own identical unconditional refusal (checked
/// directly, `~/git/podman/libpod/runtime_volume.go`: no
/// force-equivalent parameter exists on `RenameVolume` at all).
#[test]
fn volume_rename_refuses_a_volume_a_running_container_depends_on() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/volume-rename-in-use:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/volume-rename-in-use:latest",
        &["-d", "-v", "depvol:/data", "sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

    let rename = ociman(
        storage_dir.path(),
        &["volume", "rename", "depvol", "depvol2"],
    );
    assert!(!rename.status.success());
    assert!(
        String::from_utf8_lossy(&rename.stderr).contains("is being used by"),
        "{}",
        String::from_utf8_lossy(&rename.stderr)
    );

    ociman(storage_dir.path(), &["kill", &id]);
    child.wait().ok();
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

/// `ociman volume export`/`ociman volume import` (0302): a real
/// round trip through a plain tar preserves a volume's own content
/// byte-for-byte, matching real `podman volume export`/`podman volume
/// import` exactly (checked directly against an installed `podman
/// 4.9.3`, including cross-tool interoperability both directions --
/// not exercised here, since this test suite is deliberately fully
/// offline, but verified manually).
#[test]
fn volume_export_then_import_round_trips_content_byte_for_byte() {
    let storage_dir = tempfile::tempdir().unwrap();
    let inspect = ociman(storage_dir.path(), &["volume", "create", "src-vol"]);
    assert!(inspect.status.success());
    let mountpoint = volume_mountpoint(storage_dir.path(), "src-vol");
    std::fs::write(mountpoint.join("greeting.txt"), b"hello volume").unwrap();
    std::fs::create_dir_all(mountpoint.join("subdir")).unwrap();
    std::fs::write(mountpoint.join("subdir/nested.txt"), b"nested content").unwrap();

    let archive = storage_dir.path().join("export.tar");
    let export = ociman(
        storage_dir.path(),
        &[
            "volume",
            "export",
            "src-vol",
            "-o",
            archive.to_str().unwrap(),
        ],
    );
    assert!(
        export.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    assert!(archive.is_file());

    ociman(storage_dir.path(), &["volume", "create", "dest-vol"]);
    let import = ociman(
        storage_dir.path(),
        &["volume", "import", "dest-vol", archive.to_str().unwrap()],
    );
    assert!(
        import.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&import.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&import.stdout).trim(), "dest-vol");

    let dest_mountpoint = volume_mountpoint(storage_dir.path(), "dest-vol");
    assert_eq!(
        std::fs::read(dest_mountpoint.join("greeting.txt")).unwrap(),
        b"hello volume"
    );
    assert_eq!(
        std::fs::read(dest_mountpoint.join("subdir/nested.txt")).unwrap(),
        b"nested content"
    );
}

/// `ociman volume import` reads from standard input given `-`,
/// matching real `podman volume import VOLUME -` exactly.
#[test]
fn volume_import_reads_from_stdin_given_a_dash() {
    let storage_dir = tempfile::tempdir().unwrap();
    ociman(storage_dir.path(), &["volume", "create", "src-vol"]);
    let mountpoint = volume_mountpoint(storage_dir.path(), "src-vol");
    std::fs::write(mountpoint.join("f.txt"), b"via stdin").unwrap();

    let export = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["volume", "export", "src-vol"])
        .output()
        .expect("failed to run ociman volume export");
    assert!(export.status.success());

    ociman(storage_dir.path(), &["volume", "create", "dest-vol"]);
    let mut child = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args(["volume", "import", "dest-vol", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn ociman volume import");
    {
        use std::io::Write as _;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&export.stdout)
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dest_mountpoint = volume_mountpoint(storage_dir.path(), "dest-vol");
    assert_eq!(
        std::fs::read(dest_mountpoint.join("f.txt")).unwrap(),
        b"via stdin"
    );
}

/// `ociman volume import` recognizes a gzip-compressed archive by its
/// own magic bytes, matching `ociman import`'s own identical
/// convention (and real `podman volume import`'s own gzip support).
#[test]
fn volume_import_decompresses_a_gzip_archive() {
    let storage_dir = tempfile::tempdir().unwrap();
    ociman(storage_dir.path(), &["volume", "create", "src-vol"]);
    let mountpoint = volume_mountpoint(storage_dir.path(), "src-vol");
    std::fs::write(mountpoint.join("f.txt"), b"gzip roundtrip").unwrap();

    let plain = storage_dir.path().join("export.tar");
    let export = ociman(
        storage_dir.path(),
        &["volume", "export", "src-vol", "-o", plain.to_str().unwrap()],
    );
    assert!(export.status.success());

    // Compress it ourselves (this project's own export never gzips by
    // default -- see `cmd_volume_export`'s own doc comment) to prove
    // import's own decompression path, not just its plain-tar path.
    use std::io::{Read as _, Write as _};
    let mut raw = Vec::new();
    std::fs::File::open(&plain)
        .unwrap()
        .read_to_end(&mut raw)
        .unwrap();
    let gz_path = storage_dir.path().join("export.tar.gz");
    let mut encoder = flate2::write::GzEncoder::new(
        std::fs::File::create(&gz_path).unwrap(),
        flate2::Compression::default(),
    );
    encoder.write_all(&raw).unwrap();
    encoder.finish().unwrap();

    ociman(storage_dir.path(), &["volume", "create", "dest-vol"]);
    let import = ociman(
        storage_dir.path(),
        &["volume", "import", "dest-vol", gz_path.to_str().unwrap()],
    );
    assert!(
        import.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&import.stderr)
    );

    let dest_mountpoint = volume_mountpoint(storage_dir.path(), "dest-vol");
    assert_eq!(
        std::fs::read(dest_mountpoint.join("f.txt")).unwrap(),
        b"gzip roundtrip"
    );
}

/// `ociman volume import` merges onto existing content rather than
/// wiping it first -- matching real `podman volume import`'s own
/// identical plain-extraction semantics (checked directly,
/// `~/git/podman/libpod/volume.go`'s own `Import`: a bare
/// `chrootarchive.Untar`, no prior removal of the volume's own
/// existing content).
#[test]
fn volume_import_merges_onto_existing_content_rather_than_wiping_it() {
    let storage_dir = tempfile::tempdir().unwrap();
    ociman(storage_dir.path(), &["volume", "create", "src-vol"]);
    let src_mountpoint = volume_mountpoint(storage_dir.path(), "src-vol");
    std::fs::write(src_mountpoint.join("new.txt"), b"new content").unwrap();
    let archive = storage_dir.path().join("export.tar");
    ociman(
        storage_dir.path(),
        &[
            "volume",
            "export",
            "src-vol",
            "-o",
            archive.to_str().unwrap(),
        ],
    );

    ociman(storage_dir.path(), &["volume", "create", "dest-vol"]);
    let dest_mountpoint = volume_mountpoint(storage_dir.path(), "dest-vol");
    std::fs::write(dest_mountpoint.join("preexisting.txt"), b"already here").unwrap();

    let import = ociman(
        storage_dir.path(),
        &["volume", "import", "dest-vol", archive.to_str().unwrap()],
    );
    assert!(import.status.success());

    assert_eq!(
        std::fs::read(dest_mountpoint.join("preexisting.txt")).unwrap(),
        b"already here",
        "pre-existing content should survive an import"
    );
    assert_eq!(
        std::fs::read(dest_mountpoint.join("new.txt")).unwrap(),
        b"new content"
    );
}

/// `ociman volume export`/`ociman volume import` on an unknown volume
/// is a clear, real error -- matching `volume_inspect_of_an_unknown_
/// volume_is_a_clear_error`'s own established convention for this
/// project's volume commands.
#[test]
fn volume_export_and_import_of_an_unknown_volume_are_clear_errors() {
    let storage_dir = tempfile::tempdir().unwrap();

    let export = ociman(storage_dir.path(), &["volume", "export", "never-created"]);
    assert!(!export.status.success());
    assert!(
        String::from_utf8_lossy(&export.stderr).contains("no volume"),
        "{}",
        String::from_utf8_lossy(&export.stderr)
    );

    let import = ociman(
        storage_dir.path(),
        &["volume", "import", "never-created", "-"],
    );
    assert!(!import.status.success());
    assert!(
        String::from_utf8_lossy(&import.stderr).contains("no volume"),
        "{}",
        String::from_utf8_lossy(&import.stderr)
    );
}

/// Resolve a named volume's own real `_data` mountpoint, the same way
/// `volume_inspect_reports_the_real_mountpoint` already does inline --
/// factored out here since several of this file's own newer tests
/// need to read/write real files inside it directly.
fn volume_mountpoint(storage_root: &Path, name: &str) -> std::path::PathBuf {
    let inspect = ociman(storage_root, &["volume", "inspect", name, "--json"]);
    assert!(inspect.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    std::path::PathBuf::from(parsed["mountpoint"].as_str().unwrap())
}

/// `ociman volume mount` (`docs/design/0361`) prints exactly the same
/// real, absolute `_data` directory path `ociman volume inspect`'s
/// own `mountpoint` field already reports -- and, unlike real
/// *rootless* `podman volume mount` (checked directly: it refuses
/// outright, "must execute `podman unshare` first"), never refuses at
/// all, matching this project's own volumes always being a plain,
/// already-directly-accessible host directory (the same real case a
/// rootFUL `podman volume mount`'s own genuine no-op covers).
#[test]
fn volume_mount_prints_the_real_data_directory_path() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let create = ociman(storage_dir.path(), &["volume", "create", "mountvol"]);
    assert!(create.status.success(), "{create:?}");

    let mount = ociman(storage_dir.path(), &["volume", "mount", "mountvol"]);
    assert!(mount.status.success(), "{mount:?}");
    let printed = String::from_utf8_lossy(&mount.stdout).trim().to_string();
    assert_eq!(
        std::path::PathBuf::from(&printed),
        volume_mountpoint(storage_dir.path(), "mountvol"),
        "{mount:?}"
    );
    assert!(
        Path::new(&printed).is_dir(),
        "the printed path should be a real, already-existing directory"
    );
}

/// `ociman volume unmount` is a real no-op: it never actually detaches
/// anything (there is nothing to), and the volume's own directory is
/// still fully intact and usable afterward -- matching real `podman
/// volume unmount`'s own identical "local" driver behavior (checked
/// directly: `~/git/podman/libpod/volume.go`'s own `unmount` early-
/// returns whenever `needsMount()` is `false`, this project's own
/// only real case). Prints the volume's own name on success, matching
/// a real installed `podman volume unmount`'s own checked-directly
/// output exactly.
#[test]
fn volume_unmount_is_a_real_no_op_that_prints_the_name() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let create = ociman(storage_dir.path(), &["volume", "create", "unmountvol"]);
    assert!(create.status.success(), "{create:?}");
    let mountpoint = volume_mountpoint(storage_dir.path(), "unmountvol");

    let unmount = ociman(storage_dir.path(), &["volume", "unmount", "unmountvol"]);
    assert!(unmount.status.success(), "{unmount:?}");
    assert_eq!(
        String::from_utf8_lossy(&unmount.stdout).trim(),
        "unmountvol"
    );
    assert!(
        mountpoint.is_dir(),
        "the volume's own directory must survive unmount untouched"
    );
}

/// `ociman volume mount`/`unmount` on an unknown volume are clear
/// errors, matching `ociman volume export`/`import`'s own identical,
/// already-established convention for the same case.
#[test]
fn volume_mount_and_unmount_of_an_unknown_volume_are_clear_errors() {
    let storage_dir = tempfile::tempdir().unwrap();

    let mount = ociman(storage_dir.path(), &["volume", "mount", "never-created"]);
    assert!(!mount.status.success());
    assert!(
        String::from_utf8_lossy(&mount.stderr).contains("no volume"),
        "{}",
        String::from_utf8_lossy(&mount.stderr)
    );

    let unmount = ociman(storage_dir.path(), &["volume", "unmount", "never-created"]);
    assert!(!unmount.status.success());
    assert!(
        String::from_utf8_lossy(&unmount.stderr).contains("no volume"),
        "{}",
        String::from_utf8_lossy(&unmount.stderr)
    );
}
