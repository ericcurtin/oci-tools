//! `ociboot list` integration tests: exercises the actual built
//! `ociboot` binary against real, synthetic BLS entry directories
//! (constructed the same way `oci_bls`'s own unit tests build them,
//! matching the real spec's own worked examples), not `oci_bls`'s
//! internal API directly -- this is a CLI-surface test, `oci_bls`
//! itself already has its own thorough unit test coverage for
//! `scan_entries`/`sort_entries`/`parse_suffix`.

use std::process::Command;

use oci_tools_tests::bin_path;

fn write_entry(dir: &std::path::Path, file_name: &str, title: &str, version: &str) {
    std::fs::write(
        dir.join(file_name),
        format!("title {title}\nversion {version}\nlinux /linux\n"),
    )
    .unwrap();
}

#[test]
fn list_prints_real_entries_in_the_real_specs_own_sort_order() {
    let dir = tempfile::tempdir().unwrap();
    // No `sort-key` set on either entry, so the real spec's own rule
    // 4 (fall back to the *file name*, decreasing version order) is
    // what actually decides the order here -- the file names below
    // encode the version themselves, matching a real BLS installation
    // (see `entry.rs`'s own worked example), deliberately written to
    // disk in a different order than the expected sorted output so a
    // pass here can't be an accident of `read_dir`'s own incidental
    // order.
    write_entry(
        dir.path(),
        "6a9857a3-3.8.0-2.fc19.x86_64.conf",
        "Fedora old",
        "3.8.0-2.fc19.x86_64",
    );
    write_entry(
        dir.path(),
        "6a9857a3-3.9.0-1.fc19.x86_64.conf",
        "Fedora new",
        "3.9.0-1.fc19.x86_64",
    );

    let out = Command::new(bin_path("ociboot"))
        .args(["list", "--boot-dir"])
        .arg(dir.path())
        .output()
        .expect("failed to spawn ociboot list");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let new_pos = stdout.find("Fedora new").expect("new entry present");
    let old_pos = stdout.find("Fedora old").expect("old entry present");
    assert!(
        new_pos < old_pos,
        "expected the newer version first (real BLS sort order): {stdout}"
    );
}

#[test]
fn list_sorts_a_boot_counted_bad_entry_last_regardless_of_version() {
    let dir = tempfile::tempdir().unwrap();
    write_entry(dir.path(), "bad+0.conf", "Bad rollback", "9.9.9");
    write_entry(dir.path(), "good.conf", "Good deploy", "1.0.0");

    let out = Command::new(bin_path("ociboot"))
        .args(["list", "--boot-dir"])
        .arg(dir.path())
        .output()
        .expect("failed to spawn ociboot list");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Bad rollback (9.9.9) [bad]"), "{stdout}");
    let good_pos = stdout.find("Good deploy").expect("good entry present");
    let bad_pos = stdout.find("Bad rollback").expect("bad entry present");
    assert!(
        good_pos < bad_pos,
        "a bad boot-counted entry must sort last even with a higher version: {stdout}"
    );
}

#[test]
fn list_tolerates_non_conf_clutter_in_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    write_entry(dir.path(), "real.conf", "Real entry", "1.0.0");
    std::fs::write(dir.path().join("README.txt"), "not an entry\n").unwrap();
    std::fs::create_dir(dir.path().join("subdir")).unwrap();

    let out = Command::new(bin_path("ociboot"))
        .args(["list", "--boot-dir"])
        .arg(dir.path())
        .output()
        .expect("failed to spawn ociboot list");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    assert!(stdout.contains("Real entry"));
}

#[test]
fn list_on_an_empty_directory_says_so_and_exits_success() {
    let dir = tempfile::tempdir().unwrap();

    let out = Command::new(bin_path("ociboot"))
        .args(["list", "--boot-dir"])
        .arg(dir.path())
        .output()
        .expect("failed to spawn ociboot list");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no boot entries found"), "{stdout}");
}

#[test]
fn list_on_a_missing_directory_is_a_real_surfaced_error() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist");

    let out = Command::new(bin_path("ociboot"))
        .args(["list", "--boot-dir"])
        .arg(&missing)
        .output()
        .expect("failed to spawn ociboot list");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("scanning boot entries"), "{stderr}");
}

#[test]
fn no_subcommand_is_a_real_error_not_a_silent_success() {
    let out = Command::new(bin_path("ociboot"))
        .output()
        .expect("failed to spawn ociboot");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no subcommand given"), "{stderr}");
}
