//! `ocibox ephemeral` integration tests: exercises the actual built
//! `ocibox` binary creating a disposable box under a real, random
//! name, running a command inside it, and always removing it again --
//! a pure composition of `create`/`enter`/`rm`, matching real
//! `distrobox ephemeral` exactly (see `bin/ocibox/src/main.rs`'s own
//! `Command::Ephemeral` doc comment for the citation).

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

fn seed_ephemeral_base(storage_dir: &Path, reference: &str) {
    let store = Store::open(storage_dir).unwrap();
    seed_image(
        &store,
        reference,
        &busybox_path().expect("busybox not found on $PATH"),
        &["sh", "cat", "echo"],
        ContainerConfig::default(),
    );
}

#[test]
fn ephemeral_runs_a_command_and_removes_the_box_afterward() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    seed_ephemeral_base(storage_dir.path(), "ocibox-test/ephemeral-base:latest");

    let out = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-base:latest",
            "--",
            "/bin/echo",
            "hello-ephemeral",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hello-ephemeral"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // No box left behind at all -- not under any name.
    assert!(
        !storage_dir.path().join("boxes").exists()
            || std::fs::read_dir(storage_dir.path().join("boxes"))
                .unwrap()
                .next()
                .is_none(),
        "ephemeral should leave no box behind"
    );
}

/// `trailing_var_arg`/`allow_hyphen_values` (`docs/design/0544`): a
/// command whose own arguments look like flags (`/bin/echo -n ...`)
/// parses correctly with *no* explicit `--` at all -- matching real
/// `distrobox ephemeral`'s own identical inherited behavior (see
/// `ocibox enter`'s own identical test/doc comment for the exact
/// real-distrobox citation). Before this fix, this exact invocation
/// was a real, immediate clap parse error (`unexpected argument '-n'
/// found`).
#[test]
fn ephemeral_runs_a_command_with_its_own_flag_like_arguments_without_a_leading_double_dash() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    seed_ephemeral_base(
        storage_dir.path(),
        "ocibox-test/ephemeral-no-dashdash:latest",
    );

    let out = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-no-dashdash:latest",
            "/bin/echo",
            "-n",
            "hello-ephemeral",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `-n` suppresses the trailing newline `echo` would otherwise
    // add -- proving it reached the real command as a real flag,
    // not just as a literal, un-flagged argument.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello-ephemeral");
}

/// `--yes`/`-Y` is accepted for real CLI compatibility with real
/// `distrobox ephemeral --yes`/`-Y` (inherited from `distrobox
/// create`'s own identical flag, checked directly, `~/git/distrobox/
/// internal/cli/ephemeral.go`) but changes nothing at all -- see
/// `ocibox create --yes`'s own doc comment for the identical real
/// reasoning.
#[test]
fn ephemeral_yes_flag_is_accepted_and_behaves_identically() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    seed_ephemeral_base(storage_dir.path(), "ocibox-test/ephemeral-yes-base:latest");

    let out = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-yes-base:latest",
            "--yes",
            "--",
            "/bin/echo",
            "hello-ephemeral-yes",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hello-ephemeral-yes"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// `ephemeral --root`/`-r` (`docs/design/0540`): accepted for real
/// CLI compatibility with real distrobox's own cross-cutting
/// `--root`, but changes nothing at all -- this project has no
/// rootful/rootless distinction of any kind (see `Command::
/// Create::root`'s own doc comment for the full, checked-directly
/// reasoning).
#[test]
fn ephemeral_root_flag_is_accepted_and_behaves_identically() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    seed_ephemeral_base(storage_dir.path(), "ocibox-test/ephemeral-root-base:latest");

    let out = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-root-base:latest",
            "--root",
            "--",
            "/bin/echo",
            "hello-ephemeral-root",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hello-ephemeral-root"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// `--absolutely-disable-root-password-i-am-really-positively-sure`
/// (`docs/design/0549`) is accepted for real CLI compatibility with
/// real `distrobox ephemeral`'s own identical inherited flag, but
/// changes nothing at all -- see `Command::Create::absolutely_
/// disable_root_password_i_am_really_positively_sure`'s own doc
/// comment for the full, checked-directly reasoning.
#[test]
fn ephemeral_nopasswd_flag_is_accepted_and_behaves_identically() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    seed_ephemeral_base(
        storage_dir.path(),
        "ocibox-test/ephemeral-nopasswd-base:latest",
    );

    let out = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-nopasswd-base:latest",
            "--absolutely-disable-root-password-i-am-really-positively-sure",
            "--",
            "/bin/echo",
            "hello-ephemeral-nopasswd",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hello-ephemeral-nopasswd"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn ephemeral_forwards_the_containers_own_nonzero_exit_code() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    seed_ephemeral_base(storage_dir.path(), "ocibox-test/ephemeral-exit-base:latest");

    let out = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-exit-base:latest",
            "--",
            "/bin/sh",
            "-c",
            "exit 7",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(7),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The box is still removed even though the command inside it
    // failed -- matching real `distrobox ephemeral`'s own identical
    // "always clean up" behavior.
    let boxes_dir = storage_dir.path().join("boxes");
    assert!(
        !boxes_dir.exists() || std::fs::read_dir(&boxes_dir).unwrap().next().is_none(),
        "ephemeral should remove its own box even after a nonzero exit"
    );
}

#[test]
fn ephemeral_defaults_to_a_shell_when_no_command_is_given() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    seed_ephemeral_base(
        storage_dir.path(),
        "ocibox-test/ephemeral-shell-base:latest",
    );

    let out = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-shell-base:latest",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Two separate `ephemeral` invocations against the same image get
/// two different, real, randomly-generated box names -- confirmed
/// indirectly: each call's own rootfs starts fresh (a file written in
/// one invocation is never visible in a separate one), unlike
/// `ocibox enter`'s own deliberate same-box persistence.
#[test]
fn ephemeral_invocations_never_share_state_with_each_other() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    seed_ephemeral_base(
        storage_dir.path(),
        "ocibox-test/ephemeral-fresh-base:latest",
    );

    let write = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-fresh-base:latest",
            "--",
            "/bin/sh",
            "-c",
            "echo marker > /marker.txt",
        ],
    );
    assert!(write.status.success());

    let read = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-fresh-base:latest",
            "--",
            "/bin/cat",
            "/marker.txt",
        ],
    );
    // A fresh box never has the previous ephemeral invocation's own
    // marker file -- `cat` on a nonexistent file fails.
    assert!(!read.status.success());
}

/// `ocibox ephemeral --home` -- inherited from `ocibox create`'s own
/// identical flag, matching real `distrobox ephemeral`'s own identical
/// inheritance of `--home`/`-H` from `distrobox create`.
#[test]
fn ephemeral_uses_a_custom_home_directory() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/ephemeral-custom-home:latest",
        &busybox,
        &["sh", "touch"],
        ContainerConfig::default(),
    );

    let custom_home_parent = tempfile::tempdir().unwrap();
    let custom_home = custom_home_parent.path().join("not-yet-created");
    let ambient_home = tempfile::tempdir().unwrap();

    let out = Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .env("HOME", ambient_home.path())
        .args([
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-custom-home:latest",
            "--home",
            custom_home.to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            "echo $PWD",
        ])
        .output()
        .expect("failed to spawn ocibox");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The box's own real command output comes first; a second,
    // trailing line is `remove_one_box`'s own real removed-name
    // announcement (`cmd_ephemeral`'s always-run cleanup step,
    // matching real `distrobox rm`'s own identical habit) -- same
    // reasoning `ephemeral_runs_a_command_and_removes_the_box_
    // afterward`'s own `.contains` check already accounts for.
    assert_eq!(
        stdout.lines().next().unwrap(),
        custom_home.to_str().unwrap(),
        "full stdout: {stdout:?}"
    );
}

#[test]
fn ephemeral_of_an_unresolvable_image_is_a_clear_error_and_leaves_no_box_behind() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let out = ocibox(
        storage_dir.path(),
        &["ephemeral", "--image", "127.0.0.1:1/doesnotexist:latest"],
    );
    assert!(!out.status.success());
    let boxes_dir = storage_dir.path().join("boxes");
    assert!(
        !boxes_dir.exists() || std::fs::read_dir(&boxes_dir).unwrap().next().is_none(),
        "a failed ephemeral create should leave no box directory behind at all"
    );
}

/// `ocibox ephemeral --clone`/`-c` (0477, closing `0476`'s own
/// deferred gap): real `distrobox ephemeral` inherits `--clone` from
/// `distrobox create` too (checked directly, `~/git/distrobox/
/// internal/cli/ephemeral.go`). A real, already-existing box's own
/// current rootfs (including a write made after its own creation) is
/// visible from inside the ephemeral clone, and the clone is still
/// fully removed afterward while the real, persistent source box
/// survives completely untouched.
#[test]
fn ephemeral_clone_sees_the_source_boxs_own_current_state_and_still_cleans_up() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    seed_ephemeral_base(
        storage_dir.path(),
        "ocibox-test/ephemeral-clone-base:latest",
    );

    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/ephemeral-clone-base:latest",
            "--name",
            "ephemeral-clone-source",
        ],
    );
    assert!(create.status.success(), "{create:?}");
    let source_rootfs = storage_dir
        .path()
        .join("boxes")
        .join("ephemeral-clone-source")
        .join("rootfs");
    std::fs::write(source_rootfs.join("marker.txt"), b"from the source box").unwrap();

    let out = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--clone",
            "ephemeral-clone-source",
            "--",
            "/bin/cat",
            "/marker.txt",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("from the source box"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // The real, persistent source box survives, completely untouched
    // -- only its own ephemeral clone was ever removed.
    let boxes: Vec<String> = std::fs::read_dir(storage_dir.path().join("boxes"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(boxes, vec!["ephemeral-clone-source"]);
    assert!(source_rootfs.join("marker.txt").exists());
}

/// `--image` and `--clone` together, or neither at all, are both
/// clear, immediate errors -- matching `ocibox create`'s own
/// identical validation.
#[test]
fn ephemeral_requires_exactly_one_of_image_or_clone() {
    let storage_dir = tempfile::tempdir().unwrap();

    let neither = ocibox(storage_dir.path(), &["ephemeral"]);
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
            "ephemeral",
            "--image",
            "ocibox-test/whatever:latest",
            "--clone",
            "whatever",
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
