//! `ocibox list`/`ocibox rm` integration tests: exercises the actual
//! built `ocibox` binary — `ocibox create`'s own tests
//! (`ocibox_create.rs`) already cover image resolution and rootfs
//! extraction directly; this covers the rest of the family that makes
//! `create` actually manageable.

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
fn list_on_an_empty_store_says_so_and_exits_success() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let list = ocibox(storage_dir.path(), &["list"]);
    assert!(
        list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&list.stdout).trim(), "no boxes");
}

#[test]
fn list_shows_every_created_box_sorted_by_name() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/list-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    for name in ["zeta", "alpha", "mid"] {
        let create = ocibox(
            storage_dir.path(),
            &[
                "create",
                "--image",
                "ocibox-test/list-base:latest",
                "--name",
                name,
            ],
        );
        assert!(create.status.success());
    }

    let list = ocibox(storage_dir.path(), &["list"]);
    assert!(list.status.success());
    let stdout = String::from_utf8_lossy(&list.stdout);
    let names: Vec<&str> = stdout
        .lines()
        .skip(1) // header
        .map(|line| line.split_whitespace().next().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["alpha", "mid", "zeta"],
        "boxes should be sorted by name, not creation order: {stdout:?}"
    );
}

/// `list --no-color` (0515): accepted for real CLI compatibility, but
/// changes nothing at all -- this project's own list output has no
/// ANSI color codes anywhere in the first place (see `Command::List`'s
/// own doc comment for the full, checked-directly reasoning). Proven
/// here by comparing `list`'s own output with and without the flag,
/// byte for byte.
#[test]
fn list_no_color_flag_is_accepted_and_behaves_identically() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/list-no-color:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/list-no-color:latest",
            "--name",
            "colorbox",
        ],
    );
    assert!(create.status.success());

    let plain = ocibox(storage_dir.path(), &["list"]);
    assert!(plain.status.success());
    let no_color = ocibox(storage_dir.path(), &["list", "--no-color"]);
    assert!(no_color.status.success());
    assert_eq!(plain.stdout, no_color.stdout);
    assert!(
        !String::from_utf8_lossy(&plain.stdout).contains('\u{1b}'),
        "there should be no ANSI escape codes in this project's own list output at all"
    );
}

#[test]
fn list_json_reports_every_field_of_the_persisted_record() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/list-json-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/list-json-base:latest",
            "--name",
            "jsonbox",
        ],
    );
    assert!(create.status.success());

    let list = ocibox(storage_dir.path(), &["--json", "list"]);
    assert!(list.status.success());
    let view: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let boxes = view.as_array().unwrap();
    assert_eq!(boxes.len(), 1);
    assert_eq!(boxes[0]["name"], "jsonbox");
    assert_eq!(
        boxes[0]["image"],
        "docker.io/ocibox-test/list-json-base:latest"
    );
    assert!(
        boxes[0]["manifest_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(boxes[0]["created"].as_str().is_some());
}

/// `ls` is a real alias for `list`, matching real `distrobox list`'s
/// own identical alias.
#[test]
fn ls_is_an_alias_for_list() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let ls = ocibox(storage_dir.path(), &["ls"]);
    assert!(ls.status.success());
    assert_eq!(String::from_utf8_lossy(&ls.stdout).trim(), "no boxes");
}

#[test]
fn rm_removes_a_real_box_entirely() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/rm-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/rm-base:latest",
            "--name",
            "rmbox",
        ],
    );
    assert!(create.status.success());
    let box_dir = storage_dir.path().join("boxes").join("rmbox");
    assert!(box_dir.is_dir());

    let rm = ocibox(storage_dir.path(), &["rm", "rmbox"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&rm.stdout).trim(), "rmbox");
    assert!(!box_dir.exists(), "the whole box directory should be gone");

    let list = ocibox(storage_dir.path(), &["list"]);
    assert_eq!(String::from_utf8_lossy(&list.stdout).trim(), "no boxes");
}

/// `rm --rm-home` (0405): a real, checked-directly no-op -- real
/// distrobox's own `--rm-home` only ever removes a box's own custom
/// home when driven by a real interactive terminal session (which
/// `ocibox` has no equivalent of at all), so the one real mode this
/// project can ever run in never removes it either, matching real
/// distrobox's own actual behavior under that same mode exactly (see
/// `Command::Rm`'s own doc comment for the full, checked-directly
/// reasoning). Proven here against a real custom `--home` directory
/// that survives `rm --rm-home` completely untouched, even though the
/// box's own storage directory itself is still genuinely removed.
#[test]
fn rm_rm_home_flag_is_a_real_no_op_and_never_removes_the_custom_home() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/rm-home-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let custom_home = tempfile::tempdir().unwrap();
    std::fs::write(custom_home.path().join("canary.txt"), b"still here").unwrap();

    let mut create = Command::new(bin_path("ocibox"));
    create
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "create",
            "--image",
            "ocibox-test/rm-home-base:latest",
            "--name",
            "rmhomebox",
            "--home",
        ])
        .arg(custom_home.path());
    let create = create.output().expect("failed to spawn ocibox create");
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    let box_dir = storage_dir.path().join("boxes").join("rmhomebox");
    assert!(box_dir.is_dir());

    let rm = ocibox(storage_dir.path(), &["rm", "rmhomebox", "--rm-home"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert!(
        !box_dir.exists(),
        "the box's own storage directory is still genuinely removed"
    );
    assert!(
        custom_home.path().join("canary.txt").exists(),
        "the real, custom --home directory must survive --rm-home completely untouched"
    );
}

/// `rm --yes`/`-Y` (0514): accepted for real CLI compatibility, but
/// changes nothing at all -- real distrobox's own `--yes`/`-Y` only
/// ever skips real interactive confirmation prompts this project has
/// none of in the first place (see `Command::Rm`'s own doc comment
/// for the full, checked-directly reasoning). Proven here against a
/// real box, which is still genuinely removed exactly as a plain
/// `rm` would.
#[test]
fn rm_yes_flag_is_accepted_and_behaves_identically() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/rm-yes:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/rm-yes:latest",
            "--name",
            "yesbox",
        ],
    );
    assert!(create.status.success());
    let box_dir = storage_dir.path().join("boxes").join("yesbox");
    assert!(box_dir.is_dir());

    let rm = ocibox(storage_dir.path(), &["rm", "yesbox", "--yes"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&rm.stdout).trim(), "yesbox");
    assert!(!box_dir.exists(), "the whole box directory should be gone");
}

/// `rm` of a name that simply doesn't resolve to any real box is a
/// warning, not a hard error (0321 — a real correction: previously
/// this hard-errored, before checking real `distrobox`'s own actual
/// behavior directly, `~/git/distrobox/pkg/commands/rm.go`'s own
/// `warnUnknownContainers`/`Execute`, traced all the way to `cmd/
/// distrobox/main.go`'s own top-level error handling — real
/// `distrobox rm somename` on a name that doesn't exist prints a
/// warning and exits `0`, never non-zero).
#[test]
fn rm_of_an_unknown_name_prints_a_warning_but_still_succeeds() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let rm = ocibox(storage_dir.path(), &["rm", "doesnotexist"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert!(
        String::from_utf8_lossy(&rm.stderr).contains("no such box"),
        "{}",
        String::from_utf8_lossy(&rm.stderr)
    );
}

/// A real, checked-directly security concern: `rm`'s own `name`
/// argument must never be usable to escape `boxes_root` via `/`/`..`
/// components -- confirmed directly that a path-traversal attempt is
/// rejected as an invalid name outright, long before any real
/// `remove_dir_all` call could ever be reached.
#[test]
fn rm_rejects_a_path_traversal_attempt_in_the_name() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    // A real, harmless canary file outside `boxes_root` entirely --
    // if the path-traversal attempt below were ever allowed through,
    // this is what would prove it (it must still exist afterward).
    let canary = storage_dir.path().join("canary.txt");
    std::fs::write(&canary, b"still here").unwrap();

    let rm = ocibox(storage_dir.path(), &["rm", "../canary.txt"]);
    assert!(!rm.status.success());
    assert!(
        String::from_utf8_lossy(&rm.stderr).contains("invalid box name"),
        "{}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert!(canary.is_file(), "the canary file must survive untouched");
}

/// `ocibox rm --all` (matching real `distrobox rm --all`): removes
/// every existing box in one call, sorted by name (same order `list`
/// itself reports them in), leaving the store genuinely empty
/// afterward.
#[test]
fn rm_all_removes_every_box() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/rm-all-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    for name in ["zeta", "alpha", "mid"] {
        let create = ocibox(
            storage_dir.path(),
            &[
                "create",
                "--image",
                "ocibox-test/rm-all-base:latest",
                "--name",
                name,
            ],
        );
        assert!(create.status.success());
    }

    let rm = ocibox(storage_dir.path(), &["rm", "--all"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    let stdout = String::from_utf8_lossy(&rm.stdout).into_owned();
    let removed: Vec<&str> = stdout.lines().collect();
    assert_eq!(removed, vec!["alpha", "mid", "zeta"]);

    let list = ocibox(storage_dir.path(), &["list"]);
    assert_eq!(String::from_utf8_lossy(&list.stdout).trim(), "no boxes");
    assert!(!storage_dir.path().join("boxes").join("alpha").exists());
    assert!(!storage_dir.path().join("boxes").join("mid").exists());
    assert!(!storage_dir.path().join("boxes").join("zeta").exists());
}

/// `rm NAME1 NAME2` (0321, a real, previously-unsupported gap:
/// `ocibox rm` only ever accepted exactly one name before this, unlike
/// real `distrobox rm NAME [NAME...]`) removes every one of the real
/// boxes named, printing each in turn, and tolerates one unresolvable
/// name among them (a warning only, not an aborting error, matching
/// real distrobox's own identical tolerance -- see [`cmd_rm`]'s own
/// doc comment) while still removing every real box that *was* named.
#[test]
fn rm_accepts_multiple_names_and_tolerates_an_unresolvable_one() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/rm-multi-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    for name in ["boxone", "boxtwo"] {
        let create = ocibox(
            storage_dir.path(),
            &[
                "create",
                "--image",
                "ocibox-test/rm-multi-base:latest",
                "--name",
                name,
            ],
        );
        assert!(create.status.success());
    }

    let rm = ocibox(
        storage_dir.path(),
        &["rm", "boxone", "boxtwo", "does-not-exist"],
    );
    assert!(
        rm.status.success(),
        "an unresolvable name among several must not abort the whole call, stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    let stdout = String::from_utf8_lossy(&rm.stdout).into_owned();
    let removed: Vec<&str> = stdout.lines().collect();
    assert_eq!(removed, vec!["boxone", "boxtwo"]);
    assert!(
        String::from_utf8_lossy(&rm.stderr).contains("does-not-exist"),
        "{}",
        String::from_utf8_lossy(&rm.stderr)
    );

    assert!(!storage_dir.path().join("boxes").join("boxone").exists());
    assert!(!storage_dir.path().join("boxes").join("boxtwo").exists());
}

/// `rm --all` on an already-empty store is a real, silent no-op:
/// nothing to remove, nothing printed, exit success -- there was
/// never a box to report a failure or a name for.
#[test]
fn rm_all_on_an_empty_store_is_a_silent_success() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let rm = ocibox(storage_dir.path(), &["rm", "--all"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert!(rm.stdout.is_empty());
}

/// `rm` with neither a name nor `--all` is a clear, real error rather
/// than an ambiguous silent no-op.
#[test]
fn rm_requires_a_name_or_all() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let neither = ocibox(storage_dir.path(), &["rm"]);
    assert!(!neither.status.success());
    assert!(
        String::from_utf8_lossy(&neither.stderr).contains("no box name given"),
        "{}",
        String::from_utf8_lossy(&neither.stderr)
    );
}

/// Giving both a name and `--all` is *not* an error at all (0321 — a
/// real correction, checked directly against real `distrobox`'s own
/// `getContainersToRemove`): `--all` simply takes full priority,
/// silently ignoring whatever names were also given -- proven here
/// with a real box whose own name doesn't match the (nonexistent) one
/// given alongside `--all`, yet still gets removed, exactly matching
/// real `distrobox rm somebox --all`'s own identical behavior (still
/// removes *every* box, not just `somebox`).
#[test]
fn rm_all_takes_priority_over_any_names_also_given() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/rm-all-priority:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/rm-all-priority:latest",
            "--name",
            "realbox",
        ],
    );
    assert!(create.status.success());
    let box_dir = storage_dir.path().join("boxes").join("realbox");
    assert!(box_dir.is_dir());

    let both = ocibox(
        storage_dir.path(),
        &["rm", "somebox-does-not-exist", "--all"],
    );
    assert!(
        both.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&both.stderr)
    );
    assert!(
        !box_dir.exists(),
        "--all should have removed the real box despite the unrelated name also given"
    );
}
