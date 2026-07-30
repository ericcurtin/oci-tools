//! `ocivmm create`/`list`/`rm`/`cp` integration tests (0323): closes a
//! real, previously-total test-coverage gap — unlike every sibling
//! binary (`ociman`, `ocirun`, `ocicri`, `ocibox`), `ocivmm` had zero
//! integration tests under `tests/tests/` before this note, only the
//! small inline `#[cfg(test)]` unit-test block at the bottom of
//! `bin/ocivmm/src/main.rs` (name validation, env merging, exit-status
//! parsing, systemd unit rendering).
//!
//! `ocivmm run`'s own actual VM boot needs a real KVM host (x86_64/
//! Linux only today, `docs/design/0248`) or a real macOS/Apple Silicon
//! HVF host (`docs/design/0249`, itself still incomplete) — neither is
//! available on this project's own aarch64 Linux dev/CI host, so that
//! half stays untested here, matching the exact same reasoning
//! `ci/vm-test`'s own x86_64-only dogfooding job already establishes.
//! `create`'s own *success* path likewise needs a real distro image
//! (`centos:stream10`/`ubuntu:26.04`) plus real network access to run
//! its own package manager (`provision_vm`) — genuinely heavier than
//! any other binary's own offline-seeded-image test fixture, so this
//! file instead covers exactly what's both real and fully offline-
//! testable: `create`'s own upfront, checked-directly "the image can't
//! provision a kernel" rejection (a real, seeded busybox image
//! genuinely has neither `dnf` nor `apt-get`), and `list`/`rm`/`cp`,
//! none of which need a real, successfully-provisioned VM at all --
//! `list`/`rm` only ever read/write a directory tree and a small
//! `vm.json` record, so a directly-seeded record (a real, valid
//! `VmRecord` shape, not exercising `create` itself) exercises the
//! identical code path a real `create` would have populated.

use std::path::Path;
use std::process::Command;

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image};

fn ocivmm(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ocivmm"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ocivmm")
}

/// Directly seeds a real, valid `vm.json` record (and an empty
/// `rootfs.img` placeholder) for `name`, matching exactly the shape
/// `ocivmm create` itself would have written -- without needing a
/// real, successfully-provisioned VM (real network access + a real
/// distro package-manager run) to get there. `list`/`rm` only ever
/// read/write this record and the directory it lives in, so this
/// exercises the identical code path.
fn seed_vm_record(storage_root: &Path, name: &str, image: &str, created: &str) {
    let vm_dir = storage_root.join("vms").join(name);
    std::fs::create_dir_all(&vm_dir).unwrap();
    let record = serde_json::json!({
        "name": name,
        "image": image,
        "manifest_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000",
        "created": created,
        "env": [],
    });
    std::fs::write(
        vm_dir.join("vm.json"),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();
    // `rm`/`cp`'s own existence checks look for this file specifically
    // (`vms_root().join(name).join("rootfs.img")`), not the directory
    // alone -- an empty placeholder is enough for every test here,
    // none of which actually loop-mount it.
    std::fs::write(vm_dir.join("rootfs.img"), []).unwrap();
}

#[test]
fn list_on_an_empty_store_says_so_and_exits_success() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let list = ocivmm(storage_dir.path(), &["list"]);
    assert!(
        list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&list.stdout).trim(), "no VMs");

    let ls = ocivmm(storage_dir.path(), &["ls"]);
    assert!(ls.status.success());
    assert_eq!(String::from_utf8_lossy(&ls.stdout).trim(), "no VMs");
}

#[test]
fn list_shows_every_seeded_vm_sorted_by_name() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    seed_vm_record(
        storage_dir.path(),
        "zeta",
        "ubuntu:26.04",
        "2026-01-01T00:00:00Z",
    );
    seed_vm_record(
        storage_dir.path(),
        "alpha",
        "centos:stream10",
        "2026-01-02T00:00:00Z",
    );

    let list = ocivmm(storage_dir.path(), &["list"]);
    assert!(
        list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let stdout = String::from_utf8_lossy(&list.stdout);
    let alpha_pos = stdout.find("alpha").expect("alpha should be listed");
    let zeta_pos = stdout.find("zeta").expect("zeta should be listed");
    assert!(
        alpha_pos < zeta_pos,
        "should be sorted by name (alpha before zeta): {stdout:?}"
    );
    assert!(stdout.contains("centos:stream10"), "{stdout:?}");
    assert!(stdout.contains("ubuntu:26.04"), "{stdout:?}");
}

#[test]
fn list_json_reports_every_field_of_the_persisted_record() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    seed_vm_record(
        storage_dir.path(),
        "jsonvm",
        "centos:stream10",
        "2026-03-04T05:06:07Z",
    );

    let list = ocivmm(storage_dir.path(), &["list", "--json"]);
    assert!(
        list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let records = json.as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["name"], "jsonvm");
    assert_eq!(records[0]["image"], "centos:stream10");
    assert_eq!(records[0]["created"], "2026-03-04T05:06:07Z");
}

#[test]
fn rm_removes_a_real_vm_directory() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    seed_vm_record(
        storage_dir.path(),
        "rmvm",
        "ubuntu:26.04",
        "2026-01-01T00:00:00Z",
    );
    let vm_dir = storage_dir.path().join("vms").join("rmvm");
    assert!(vm_dir.is_dir());

    let rm = ocivmm(storage_dir.path(), &["rm", "rmvm"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&rm.stdout).trim(), "rmvm");
    assert!(!vm_dir.exists(), "the whole VM directory should be gone");

    let list = ocivmm(storage_dir.path(), &["list"]);
    assert_eq!(String::from_utf8_lossy(&list.stdout).trim(), "no VMs");
}

#[test]
fn rm_of_an_unknown_name_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let rm = ocivmm(storage_dir.path(), &["rm", "doesnotexist"]);
    assert!(!rm.status.success());
    assert!(
        String::from_utf8_lossy(&rm.stderr).contains("no such VM"),
        "{}",
        String::from_utf8_lossy(&rm.stderr)
    );
}

#[test]
fn rm_all_removes_every_vm() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    for name in ["zeta", "alpha", "mid"] {
        seed_vm_record(
            storage_dir.path(),
            name,
            "ubuntu:26.04",
            "2026-01-01T00:00:00Z",
        );
    }

    let rm = ocivmm(storage_dir.path(), &["rm", "--all"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    let stdout = String::from_utf8_lossy(&rm.stdout).into_owned();
    let removed: Vec<&str> = stdout.lines().collect();
    assert_eq!(removed, vec!["alpha", "mid", "zeta"]);

    let list = ocivmm(storage_dir.path(), &["list"]);
    assert_eq!(String::from_utf8_lossy(&list.stdout).trim(), "no VMs");
}

/// Multiple explicit names in one call (mirroring `ocibox rm`'s own
/// already-established multi-name support, `0321`): every one is
/// genuinely removed, in the order given, not just the first.
#[test]
fn rm_accepts_multiple_explicit_names_in_one_call() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    for name in ["first", "second", "third"] {
        seed_vm_record(
            storage_dir.path(),
            name,
            "ubuntu:26.04",
            "2026-01-01T00:00:00Z",
        );
    }

    let rm = ocivmm(storage_dir.path(), &["rm", "first", "third"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    let stdout = String::from_utf8_lossy(&rm.stdout).into_owned();
    let removed: Vec<&str> = stdout.lines().collect();
    assert_eq!(removed, vec!["first", "third"]);

    assert!(!storage_dir.path().join("vms").join("first").exists());
    assert!(storage_dir.path().join("vms").join("second").is_dir());
    assert!(!storage_dir.path().join("vms").join("third").exists());
}

/// One unresolvable name among several genuine ones must abort the
/// *whole* call before removing anything at all -- matching this
/// project's own established "resolve everything first" multi-target
/// convention (`ociman rm`/`kill`/`stop`, `0310`-`0318`), not a
/// partial removal of only the names that did resolve.
#[test]
fn rm_with_one_unresolvable_name_among_several_removes_nothing() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    seed_vm_record(
        storage_dir.path(),
        "realvm",
        "ubuntu:26.04",
        "2026-01-01T00:00:00Z",
    );

    let rm = ocivmm(storage_dir.path(), &["rm", "realvm", "doesnotexist"]);
    assert!(!rm.status.success());
    assert!(
        String::from_utf8_lossy(&rm.stderr).contains("no such VM"),
        "{}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert!(
        storage_dir.path().join("vms").join("realvm").is_dir(),
        "the resolvable VM must survive untouched since the whole call aborted"
    );
}

#[test]
fn rm_all_on_an_empty_store_is_a_silent_success() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let rm = ocivmm(storage_dir.path(), &["rm", "--all"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert!(rm.stdout.is_empty());
}

#[test]
fn rm_requires_exactly_one_of_name_or_all() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    seed_vm_record(
        storage_dir.path(),
        "somevm",
        "ubuntu:26.04",
        "2026-01-01T00:00:00Z",
    );

    let neither = ocivmm(storage_dir.path(), &["rm"]);
    assert!(!neither.status.success());
    assert!(
        String::from_utf8_lossy(&neither.stderr).contains("no VM name given"),
        "{}",
        String::from_utf8_lossy(&neither.stderr)
    );

    let both = ocivmm(storage_dir.path(), &["rm", "somevm", "--all"]);
    assert!(!both.status.success());
    assert!(
        String::from_utf8_lossy(&both.stderr).contains("cannot give both"),
        "{}",
        String::from_utf8_lossy(&both.stderr)
    );
    // Neither form should have touched the real VM.
    assert!(storage_dir.path().join("vms").join("somevm").is_dir());
}

/// A real, checked-directly security concern: `rm`'s own `name`
/// argument must never be usable to escape `vms_root` via `/`/`..`
/// components -- the same real hazard `ocibox rm`'s own identical
/// charset validation guards against (`0206`).
#[test]
fn rm_rejects_a_path_traversal_attempt_in_the_name() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let canary = storage_dir.path().join("canary.txt");
    std::fs::write(&canary, b"still here").unwrap();

    let rm = ocivmm(storage_dir.path(), &["rm", "../canary.txt"]);
    assert!(!rm.status.success());
    assert!(
        String::from_utf8_lossy(&rm.stderr).contains("invalid VM name"),
        "{}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert!(canary.is_file(), "the canary file must survive untouched");
}

/// `ocivmm create`'s own real, checked-directly upfront rejection: a
/// plain busybox-based image (this project's own established offline
/// test fixture) genuinely has neither `dnf` nor `apt-get`, so it can
/// never provision a kernel + systemd -- matching `provision_vm`'s own
/// real, deliberate `has_pkg_manager` gate, `docs/design/0248`. Also
/// confirms a failed `create` leaves no half-created VM directory
/// behind, the same real promise `ocibox create` already makes.
#[test]
fn create_of_an_image_with_no_package_manager_is_a_clear_error_and_leaves_no_vm_behind() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocivmm-test/create-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = ocivmm(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocivmm-test/create-base:latest",
            "--name",
            "novm",
        ],
    );
    assert!(!create.status.success());
    assert!(
        String::from_utf8_lossy(&create.stderr).contains("neither dnf nor apt-get"),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
    assert!(
        !storage_dir.path().join("vms").join("novm").exists(),
        "a failed create must leave no half-created VM directory behind"
    );

    let list = ocivmm(storage_dir.path(), &["list"]);
    assert_eq!(String::from_utf8_lossy(&list.stdout).trim(), "no VMs");
}

/// `create` refuses reusing a name that's already a real VM,
/// mirroring `ocibox create`'s own identical refusal.
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
        "ocivmm-test/create-dup:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    seed_vm_record(
        storage_dir.path(),
        "existing",
        "ubuntu:26.04",
        "2026-01-01T00:00:00Z",
    );

    let create = ocivmm(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocivmm-test/create-dup:latest",
            "--name",
            "existing",
        ],
    );
    assert!(!create.status.success());
    assert!(
        String::from_utf8_lossy(&create.stderr).contains("already exists"),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
}

#[test]
fn cp_requires_exactly_one_side_to_be_a_vm_path() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    seed_vm_record(
        storage_dir.path(),
        "cpvm",
        "ubuntu:26.04",
        "2026-01-01T00:00:00Z",
    );
    let host_src = storage_dir.path().join("src.txt");
    std::fs::write(&host_src, b"hi").unwrap();
    let host_dst = storage_dir.path().join("dst.txt");

    let neither = ocivmm(
        storage_dir.path(),
        &["cp", host_src.to_str().unwrap(), host_dst.to_str().unwrap()],
    );
    assert!(!neither.status.success());
    assert!(
        String::from_utf8_lossy(&neither.stderr).contains("one side must be VMNAME:PATH"),
        "{}",
        String::from_utf8_lossy(&neither.stderr)
    );

    let both = ocivmm(
        storage_dir.path(),
        &["cp", "cpvm:/etc/hostname", "cpvm:/tmp/copy"],
    );
    assert!(!both.status.success());
    assert!(
        String::from_utf8_lossy(&both.stderr).contains("only one side may be VMNAME:PATH"),
        "{}",
        String::from_utf8_lossy(&both.stderr)
    );
}

#[test]
fn cp_of_an_unknown_vm_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let host_dst = storage_dir.path().join("dst.txt");

    let cp = ocivmm(
        storage_dir.path(),
        &[
            "cp",
            "doesnotexist:/etc/hostname",
            host_dst.to_str().unwrap(),
        ],
    );
    assert!(!cp.status.success());
    assert!(
        String::from_utf8_lossy(&cp.stderr).contains("no such VM"),
        "{}",
        String::from_utf8_lossy(&cp.stderr)
    );
}
