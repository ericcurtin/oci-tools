//! `ociman search --list-tags` integration tests (`docs/design/0371`):
//! the CLI-surface, no-network-needed coverage -- `oci_registry::
//! Client::list_tags`'s own real HTTP/pagination/entry-filtering logic
//! already has its own thorough mock-registry test coverage in
//! `crates/oci-registry/src/client.rs`, including a real, manually-
//! verified end-to-end round trip against a real `docker.io` registry
//! during this feature's own development (the same "manually verified
//! against a real registry, not part of automated CI" precedent
//! `ociman push`'s own `0127` already established).

use std::path::Path;
use std::process::Command;

use oci_tools_tests::bin_path;

fn ociman(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociman")
}

/// Real `podman search` *without* `--list-tags` is free-text search
/// across every configured registry -- deliberately not implemented
/// here (see `Command::Search`'s own doc comment). A real, immediate
/// error, with no network attempt at all: an unresolvable term would
/// otherwise hang/fail slowly on a real DNS lookup first if this
/// check ran any later.
#[test]
fn search_without_list_tags_is_a_clear_error_before_any_network_attempt() {
    let storage_dir = tempfile::tempdir().unwrap();
    let search = ociman(
        storage_dir.path(),
        &["search", "this-host-does-not-resolve-at-all.invalid/repo"],
    );
    assert!(!search.status.success());
    assert!(
        String::from_utf8_lossy(&search.stderr)
            .contains("free-text registry search isn't supported"),
        "{}",
        String::from_utf8_lossy(&search.stderr)
    );
}

/// An empty `TERM` is a real, immediate reference-parse error, again
/// before any network attempt.
#[test]
fn search_list_tags_of_an_empty_term_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let search = ociman(storage_dir.path(), &["search", "--list-tags", ""]);
    assert!(!search.status.success());
    assert!(
        String::from_utf8_lossy(&search.stderr).contains("parsing image reference"),
        "{}",
        String::from_utf8_lossy(&search.stderr)
    );
}
