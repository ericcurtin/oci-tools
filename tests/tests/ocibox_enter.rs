//! `ocibox enter` integration tests: exercises the actual built
//! `ocibox` binary launching a real container (via the same shared
//! `oci_runtime_core::launch`/`Bundle`/`validate` two-phase lifecycle
//! `ociman run`/`ocirun run` already use) inside an already-`create`d
//! box's own rootfs. Confirms: real exit-code forwarding (both success
//! and nonzero), default-shell detection when no `COMMAND` is given,
//! the box's own rootfs persisting a write across two separate `enter`
//! invocations (even though the container *process* itself does not,
//! see this project's own `Command::Enter` doc comment for why not
//! yet), and a clear error for an unknown box name.

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

/// Seeds a real busybox-based image and `create`s a box from it,
/// returning the storage dir (kept alive for the caller) and the
/// box's own name.
fn make_box(storage_dir: &tempfile::TempDir, name: &str) {
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/enter-base:latest",
        &busybox_path().expect("busybox not found on $PATH"),
        &["sh", "cat", "echo"],
        ContainerConfig::default(),
    );
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/enter-base:latest",
            "--name",
            name,
        ],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
}

#[test]
fn enter_runs_an_explicit_command_and_forwards_its_exit_code() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");

    let ok = ocibox(
        storage_dir.path(),
        &["enter", "testbox", "--", "/bin/sh", "-c", "exit 0"],
    );
    assert_eq!(
        ok.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&ok.stderr)
    );

    let failing = ocibox(
        storage_dir.path(),
        &["enter", "testbox", "--", "/bin/sh", "-c", "exit 42"],
    );
    assert_eq!(
        failing.status.code(),
        Some(42),
        "stderr: {}",
        String::from_utf8_lossy(&failing.stderr)
    );
}

/// `trailing_var_arg`/`allow_hyphen_values` (`docs/design/0544`): a
/// command whose own arguments look like flags (`/bin/sh -c ...`)
/// parses correctly with *no* explicit `--` at all -- matching real
/// `distrobox enter`'s own identical behavior (checked directly,
/// `~/git/distrobox/internal/cli/parse.go`'s own `PrepareArgs`/
/// `splitExecCommand`). Before this fix, this exact invocation was a
/// real, immediate clap parse error (`unexpected argument '-c'
/// found`).
#[test]
fn enter_runs_a_command_with_its_own_flag_like_arguments_without_a_leading_double_dash() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");

    let ok = ocibox(
        storage_dir.path(),
        &["enter", "testbox", "/bin/sh", "-c", "exit 0"],
    );
    assert!(
        ok.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ok.stderr)
    );

    let failing = ocibox(
        storage_dir.path(),
        &["enter", "testbox", "/bin/sh", "-c", "exit 42"],
    );
    assert_eq!(
        failing.status.code(),
        Some(42),
        "stderr: {}",
        String::from_utf8_lossy(&failing.stderr)
    );
}

#[test]
fn enter_defaults_to_a_shell_when_no_command_is_given() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");

    // Busybox's own seeded rootfs here has no `/bin/bash`, only
    // `/bin/sh` (via the `"sh"` applet symlink) -- confirms the
    // `/bin/bash`-then-`/bin/sh` fallback actually reaches `/bin/sh`
    // rather than failing outright.
    let out = ocibox(storage_dir.path(), &["enter", "testbox"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn enter_persists_rootfs_writes_across_separate_invocations() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");

    let write = ocibox(
        storage_dir.path(),
        &[
            "enter",
            "testbox",
            "--",
            "/bin/sh",
            "-c",
            "echo persisted-marker > /marker.txt",
        ],
    );
    assert!(
        write.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&write.stderr)
    );

    // A wholly separate `enter` invocation -- a fresh container
    // process each time (see this test module's own doc comment) --
    // still sees the file the first invocation wrote, since only the
    // *process* is per-invocation, not the box's own rootfs.
    let read = ocibox(
        storage_dir.path(),
        &["enter", "testbox", "--", "/bin/cat", "/marker.txt"],
    );
    assert!(
        read.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&read.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&read.stdout).trim(),
        "persisted-marker"
    );
}

#[test]
fn enter_bind_mounts_a_real_existing_home() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    let home_dir = tempfile::tempdir().unwrap();
    std::fs::write(home_dir.path().join("canary.txt"), b"real-host-home").unwrap();

    let out = Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .env("HOME", home_dir.path())
        .args([
            "enter",
            "testbox",
            "--",
            "/bin/cat",
            &format!("{}/canary.txt", home_dir.path().display()),
        ])
        .output()
        .expect("failed to spawn ocibox");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "real-host-home");
}

/// `ocibox create --home` (matching real `distrobox create --home`/
/// `-H` exactly): a custom home given at `create` time always wins
/// over the real, ambient `$HOME` -- auto-created if it doesn't exist
/// yet (matching real distrobox's own `os.MkdirAll`), genuinely
/// bind-mounted (a write inside the box lands on the *custom* host
/// path, never the ambient `$HOME`), and used as the box's own
/// process `cwd`.
#[test]
fn enter_uses_a_custom_home_directory_given_at_create_time() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/custom-home-base:latest",
        &busybox,
        &["sh", "touch"],
        ContainerConfig::default(),
    );

    let custom_home_parent = tempfile::tempdir().unwrap();
    let custom_home = custom_home_parent.path().join("not-yet-created");
    assert!(
        !custom_home.exists(),
        "the test's own setup must not pre-create this"
    );
    let ambient_home = tempfile::tempdir().unwrap();

    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/custom-home-base:latest",
            "--name",
            "customhomebox",
            "--home",
            custom_home.to_str().unwrap(),
        ],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let out = Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .env("HOME", ambient_home.path())
        .args([
            "enter",
            "customhomebox",
            "--",
            "/bin/sh",
            "-c",
            "echo $PWD && touch marker",
        ])
        .output()
        .expect("failed to spawn ocibox");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        custom_home.to_str().unwrap(),
        "the box's own cwd must be the custom home, not the ambient $HOME"
    );
    assert!(
        custom_home.join("marker").exists(),
        "the custom home must be auto-created and genuinely bind-mounted"
    );
    assert!(
        !ambient_home.path().join("marker").exists(),
        "the ambient $HOME must never be used once a custom home was given at create time"
    );
}

/// `--volume HOST-DIR:CONTAINER-DIR` (`docs/design/0397`), matching
/// real `distrobox create --volume` exactly — a real, previously-
/// missing bind mount, given once at `create` time and applied by
/// every later `enter`: a write from inside the box genuinely lands
/// on the real host directory.
#[test]
fn enter_bind_mounts_a_real_extra_volume_given_at_create_time() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/volume-base:latest",
        &busybox,
        &["sh", "touch"],
        ContainerConfig::default(),
    );

    let host_dir = tempfile::tempdir().unwrap();

    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/volume-base:latest",
            "--name",
            "volumebox",
            "--volume",
            &format!("{}:/mnt/data", host_dir.path().to_str().unwrap()),
        ],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let out = ocibox(
        storage_dir.path(),
        &[
            "enter",
            "volumebox",
            "--",
            "/bin/sh",
            "-c",
            "touch /mnt/data/marker",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        host_dir.path().join("marker").exists(),
        "a write inside /mnt/data must genuinely land on the real host directory"
    );
}

/// `--volume ...:ro` genuinely rejects a write from inside the box,
/// matching real `docker run -v`/`podman run -v ...:ro` exactly.
#[test]
fn enter_read_only_volume_rejects_a_write_from_inside_the_box() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/volume-ro-base:latest",
        &busybox,
        &["sh", "touch"],
        ContainerConfig::default(),
    );

    let host_dir = tempfile::tempdir().unwrap();

    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/volume-ro-base:latest",
            "--name",
            "volumerobox",
            "--volume",
            &format!("{}:/mnt/data:ro", host_dir.path().to_str().unwrap()),
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let out = ocibox(
        storage_dir.path(),
        &[
            "enter",
            "volumerobox",
            "--",
            "/bin/sh",
            "-c",
            "touch /mnt/data/marker",
        ],
    );
    assert!(
        !out.status.success(),
        "a write to a :ro volume must be rejected"
    );
    assert!(!host_dir.path().join("marker").exists());
}

/// An invalid `--volume` value is a real, immediate CLI error at
/// `create` time -- the box never even gets created.
#[test]
fn create_rejects_an_invalid_volume_value() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/volume-invalid-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let out = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/volume-invalid-base:latest",
            "--name",
            "volumeinvalidbox",
            "--volume",
            "not-absolute:/mnt/data",
        ],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("absolute"),
        "{out:?}"
    );
    assert!(
        !storage_dir.path().join("boxes/volumeinvalidbox").exists(),
        "an invalid --volume must leave no half-created box behind"
    );
}

/// A real, previously-unnoticed bug this fixes (0292): every box,
/// regardless of its own real name, used to report the literal
/// hostname `ocirun` -- a copy-paste artifact of `Spec::example()`'s
/// own hardcoded template default, never overridden by `enter_spec`.
/// Now defaults to the box's own name, the same "default to this
/// resource's own identity" convention `ociman run` already
/// established for containers.
#[test]
fn enter_reports_the_boxs_own_name_as_its_hostname() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "my-real-box-name");

    let out = ocibox(
        storage_dir.path(),
        &[
            "enter",
            "my-real-box-name",
            "--",
            "/bin/cat",
            "/proc/sys/kernel/hostname",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "my-real-box-name",
        "the box's own hostname must match its own real name, not the shared spec template's \
         hardcoded default"
    );
}

/// `ocibox create --hostname` (0344) overrides the box-name default,
/// matching real `distrobox create --hostname` exactly (checked
/// directly, `~/git/distrobox/pkg/commands/create.go`'s own
/// `makeContainerHostname`) -- the given value is used verbatim by
/// every later `enter`, even though it's genuinely different from the
/// box's own real name.
#[test]
fn enter_reports_an_explicit_create_hostname_override() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/hostname-override:latest",
        &busybox_path().unwrap(),
        &["sh", "cat"],
        ContainerConfig::default(),
    );
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/hostname-override:latest",
            "--name",
            "hostname-box",
            "--hostname",
            "totally-different-hostname",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let out = ocibox(
        storage_dir.path(),
        &[
            "enter",
            "hostname-box",
            "--",
            "/bin/cat",
            "/proc/sys/kernel/hostname",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "totally-different-hostname",
        "an explicit --hostname at create time must override the box-name default"
    );
}

#[test]
fn enter_of_an_unknown_box_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let out = ocibox(storage_dir.path(), &["enter", "no-such-box"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no such box"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `enter --yes`/`-y` (0522): accepted for real CLI compatibility,
/// but changes nothing -- this project's own `enter` has no auto-
/// create-a-missing-box flow at all for real distrobox's own
/// `--yes` to skip a prompt in front of (see `Command::Enter`'s own
/// doc comment for the full, checked-directly reasoning). Proven
/// here both ways: still a clear error on an unknown box, and still
/// a real, successful enter on a real one.
#[test]
fn enter_yes_flag_is_accepted_and_behaves_identically() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let unknown = ocibox(storage_dir.path(), &["enter", "--yes", "no-such-box"]);
    assert!(!unknown.status.success());
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("no such box"),
        "{}",
        String::from_utf8_lossy(&unknown.stderr)
    );

    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    make_box(&storage_dir, "yesbox");
    let known = ocibox(
        storage_dir.path(),
        &["enter", "--yes", "yesbox", "--", "/bin/sh", "-c", "exit 0"],
    );
    assert!(
        known.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&known.stderr)
    );
}

/// `--no-tty`/`-T` (and real distrobox's own second alias, `-H`) is
/// accepted for real CLI compatibility but changes nothing: this
/// project's own `enter` never allocates a PTY at all regardless (see
/// `Command::Enter::no_tty`'s own doc comment for the full,
/// checked-directly reasoning).
#[test]
fn enter_no_tty_flag_and_both_real_aliases_are_accepted_and_behave_identically() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    make_box(&storage_dir, "notty-box");

    let baseline = ocibox(
        storage_dir.path(),
        &["enter", "notty-box", "--", "/bin/sh", "-c", "echo baseline"],
    );
    assert!(baseline.status.success(), "{baseline:?}");

    for flag in ["--no-tty", "-T", "-H"] {
        let out = ocibox(
            storage_dir.path(),
            &["enter", flag, "notty-box", "--", "/bin/sh", "-c", "echo hi"],
        );
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "hi",
            "{flag} changed real output: {out:?}"
        );
    }
}

/// `enter --root`/`-r` (`docs/design/0540`): accepted for real CLI
/// compatibility with real distrobox's own cross-cutting `--root`,
/// but changes nothing at all -- this project has no rootful/
/// rootless distinction of any kind (see `Command::Create::root`'s
/// own doc comment for the full, checked-directly reasoning).
#[test]
fn enter_root_flag_is_accepted_and_behaves_identically() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    make_box(&storage_dir, "rootenterbox");

    let out = ocibox(
        storage_dir.path(),
        &[
            "enter",
            "--root",
            "rootenterbox",
            "--",
            "/bin/sh",
            "-c",
            "echo hi",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
}

/// `ocibox enter`'s own default behavior (a real, previously-missing
/// merge this project's `enter` has never performed before now):
/// the box's own `PATH` gets the real *host*'s own `$PATH` merged in,
/// not just whatever the box's own image declared -- matching real
/// `distrobox enter`'s own identical default (`--clean-path` is the
/// opt-*out*, checked directly, `~/git/distrobox/internal/cli/
/// enter.go`'s own `clean-path`/`"c"` flag, default `false`).
#[test]
fn enter_merges_the_real_hosts_own_path_into_the_boxs_own_by_default() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");

    let out = Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .env("PATH", "/home/user/.local/bin:/usr/bin")
        .args(["enter", "testbox", "--", "/bin/sh", "-c", "echo $PATH"])
        .output()
        .expect("failed to spawn ocibox enter");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "/home/user/.local/bin:/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin:/sbin:/bin\n",
        "expected the real host PATH given to `ocibox enter` itself to be merged into the \
         box's own, with only the still-missing standard dirs appended and the whole thing \
         FHS-reordered"
    );
}

/// `ocibox enter --clean-path`/`-c` (matching real `distrobox enter
/// --clean-path` exactly) resets `PATH` to the bare FHS standard,
/// discarding the real host `PATH` given to `ocibox enter` itself
/// entirely -- proving the flag actually reaches `enter_spec`, not
/// just that it parses.
#[test]
fn enter_clean_path_resets_to_the_bare_fhs_standard_ignoring_the_real_host_path() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");

    let out = Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .env("PATH", "/home/user/.local/bin:/usr/bin")
        .args([
            "enter",
            "--clean-path",
            "testbox",
            "--",
            "/bin/sh",
            "-c",
            "echo $PATH",
        ])
        .output()
        .expect("failed to spawn ocibox enter");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n",
    );
}

/// `ocibox enter`'s own default (no `--no-workdir`) real host-cwd-
/// forwarding, matching real `distrobox enter`'s own identical
/// `GetWorkDir` behavior exactly (`~/git/distrobox/pkg/
/// containermanager/containermanager.go`) for the case where the
/// real host's own current directory is inside the box's own bind-
/// mounted `$HOME`: the box starts there too (not at bare `$HOME`),
/// and `$PWD` is set to match.
#[test]
fn enter_forwards_the_real_hosts_own_current_working_directory_when_inside_home() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    let home_dir = tempfile::tempdir().unwrap();
    let subdir = home_dir.path().join("project");
    std::fs::create_dir(&subdir).unwrap();

    let out = Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .env("HOME", home_dir.path())
        .current_dir(&subdir)
        .args(["enter", "testbox", "--", "/bin/sh", "-c", "pwd; echo $PWD"])
        .output()
        .expect("failed to spawn ocibox enter");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected = format!("{}\n{}\n", subdir.display(), subdir.display());
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
}

/// `ocibox enter --no-workdir` restores the pre-existing (home-only)
/// behavior exactly, ignoring the real host's own current working
/// directory entirely — matching real `distrobox enter --no-workdir`/
/// `-nw` exactly (`~/git/distrobox/internal/cli/enter.go`).
#[test]
fn enter_no_workdir_flag_starts_from_home_instead_of_the_real_cwd() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    let home_dir = tempfile::tempdir().unwrap();
    let subdir = home_dir.path().join("project");
    std::fs::create_dir(&subdir).unwrap();

    let out = Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .env("HOME", home_dir.path())
        .current_dir(&subdir)
        .args([
            "enter",
            "--no-workdir",
            "testbox",
            "--",
            "/bin/sh",
            "-c",
            "pwd",
        ])
        .output()
        .expect("failed to spawn ocibox enter");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\n", home_dir.path().display())
    );
}

/// `enter --verbose`/`-v` (`docs/design/0557`) is a real, checked-
/// directly behavior change -- unlike `--no-tty`/`--root` above, this
/// one genuinely forces this invocation's own log filter to `debug`,
/// making the `"ocibox starting"` debug line (suppressed by the
/// default `warn` filter) actually appear on stderr. Both the long
/// and short form are checked; a plain `enter` with neither is
/// confirmed to still succeed without that line.
#[test]
fn enter_verbose_flag_and_its_short_alias_force_debug_level_logging() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    make_box(&storage_dir, "verbose-box");

    let baseline = ocibox(
        storage_dir.path(),
        &["enter", "verbose-box", "--", "/bin/echo", "hi"],
    );
    assert!(baseline.status.success(), "{baseline:?}");
    assert!(
        !String::from_utf8_lossy(&baseline.stderr).contains("ocibox starting"),
        "the default `warn` filter should suppress the debug line: {baseline:?}"
    );

    for flag in ["--verbose", "-v"] {
        let out = ocibox(
            storage_dir.path(),
            &["enter", flag, "verbose-box", "--", "/bin/echo", "hi"],
        );
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("ocibox starting"),
            "{flag} should have forced debug-level logging: {out:?}"
        );
    }
}

/// `enter --verbose` unconditionally overrides even an explicit,
/// conflicting `--log-level` -- matching real distrobox's own
/// identical unconditional `--log-level debug` override (checked
/// directly, see `Command::Enter::verbose`'s own doc comment).
#[test]
fn enter_verbose_overrides_an_explicit_conflicting_log_level() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    make_box(&storage_dir, "verbose-override-box");

    let out = ocibox(
        storage_dir.path(),
        &[
            "--log-level",
            "error",
            "enter",
            "--verbose",
            "verbose-override-box",
            "--",
            "/bin/echo",
            "hi",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("ocibox starting"),
        "--verbose should win over an explicit --log-level error: {out:?}"
    );
}
