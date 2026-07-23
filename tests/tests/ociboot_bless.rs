//! `ociboot bless` integration tests: exercises the actual built
//! `ociboot` binary against real, synthetic BLS entry files —
//! `oci_bls::boot_count`'s own `parse_suffix`/`decrement_tries_left`/
//! `increment_tries_done`/`format_suffix` already have thorough unit
//! test coverage of the pure computation; this is a CLI-surface test
//! confirming the real file rename this command performs actually
//! implements the UAPI Boot Loader Specification's own "Boot
//! counting" section exactly: "If the operating system considers the
//! boot as successful, it removes the counter altogether and the
//! entry becomes 'good'" (fetched and checked directly, not assumed,
//! before writing this).

use std::path::Path;
use std::process::Command;

use oci_tools_tests::bin_path;

fn ociboot_bless(entry: &Path) -> std::process::Output {
    Command::new(bin_path("ociboot"))
        .args(["bless", "--entry"])
        .arg(entry)
        .output()
        .expect("failed to spawn ociboot bless")
}

fn write_entry(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
}

#[test]
fn bless_strips_a_tries_left_only_counting_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("deploy+3.conf");
    write_entry(&entry, "title Test OS\nversion 1.0\nlinux /linux\n");

    let bless = ociboot_bless(&entry);
    assert!(
        bless.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&bless.stderr)
    );
    let new_path = dir.path().join("deploy.conf");
    assert_eq!(
        String::from_utf8_lossy(&bless.stdout).trim(),
        new_path.to_str().unwrap()
    );
    assert!(!entry.exists(), "the old, counted file name should be gone");
    assert!(
        new_path.exists(),
        "the new, uncounted file name should exist"
    );
    assert_eq!(
        std::fs::read_to_string(&new_path).unwrap(),
        "title Test OS\nversion 1.0\nlinux /linux\n",
        "blessing must never touch the entry's own content"
    );
}

#[test]
fn bless_strips_a_tries_left_and_tries_done_counting_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("deploy+2-1.conf");
    write_entry(&entry, "title Test OS\nversion 1.0\nlinux /linux\n");

    let bless = ociboot_bless(&entry);
    assert!(bless.status.success());
    let new_path = dir.path().join("deploy.conf");
    assert!(new_path.exists());
    assert!(!entry.exists());
}

/// Even a "bad" entry (`tries_left` already at zero) can still be
/// blessed — the spec draws no distinction here: any real counting
/// suffix at all is stripped by a successful boot, "indeterminate"
/// or "bad" alike (the entry was still successfully booted, however
/// close it came to running out of attempts first).
#[test]
fn bless_strips_the_suffix_even_from_an_already_bad_entry() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("deploy+0-3.conf");
    write_entry(&entry, "title Test OS\nversion 1.0\nlinux /linux\n");

    let bless = ociboot_bless(&entry);
    assert!(bless.status.success());
    assert!(dir.path().join("deploy.conf").exists());
}

/// An entry with no counting suffix at all (already "good", or never
/// boot-counted to begin with) is a harmless no-op — not an error,
/// and the file is left completely untouched (not even renamed to
/// itself).
#[test]
fn bless_of_an_already_good_entry_is_a_harmless_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("deploy.conf");
    write_entry(&entry, "title Test OS\nversion 1.0\nlinux /linux\n");

    let bless = ociboot_bless(&entry);
    assert!(
        bless.status.success(),
        "blessing an already-good entry should succeed, not error"
    );
    assert!(
        String::from_utf8_lossy(&bless.stdout).contains("not boot-counted"),
        "{}",
        String::from_utf8_lossy(&bless.stdout)
    );
    assert!(
        entry.exists(),
        "the file should be left exactly where it was"
    );
}

/// Blessing the very same entry twice in a row (the second call
/// necessarily sees the already-blessed, no-longer-counted file name)
/// is exactly the same harmless no-op as blessing a never-counted
/// entry — confirms the whole operation is genuinely idempotent.
#[test]
fn blessing_twice_in_a_row_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("deploy+5-0.conf");
    write_entry(&entry, "title Test OS\nversion 1.0\nlinux /linux\n");

    let first = ociboot_bless(&entry);
    assert!(first.status.success());
    let blessed_path = dir.path().join("deploy.conf");
    assert!(blessed_path.exists());

    let second = ociboot_bless(&blessed_path);
    assert!(second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("not boot-counted"),
        "{}",
        String::from_utf8_lossy(&second.stdout)
    );
    assert!(blessed_path.exists(), "still there, untouched");
}

/// A nonexistent entry file is a clear, real error (the underlying
/// `rename(2)` failing) -- never silently treated as "already good".
#[test]
fn blessing_a_nonexistent_entry_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("does-not-exist+3.conf");

    let bless = ociboot_bless(&entry);
    assert!(!bless.status.success());
}
