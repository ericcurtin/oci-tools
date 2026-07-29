//! `ocirun spec` integration tests: exercises the built binary end to end
//! (not just the `oci_spec_types::runtime` unit tests), covering the
//! `--bundle`/`-b` and `--rootless` flags and the "don't clobber an
//! existing config.json" guard.

use std::process::Command;

use oci_tools_tests::bin_path;

fn run_spec(dir: &std::path::Path, extra_args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ocirun"))
        .arg("spec")
        .args(extra_args)
        .current_dir(dir)
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun")
}

#[test]
fn spec_writes_a_valid_default_config() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_spec(dir.path(), &[]);
    assert!(out.status.success(), "ocirun spec failed: {out:?}");

    let config_path = dir.path().join("config.json");
    assert!(config_path.exists());
    let raw = std::fs::read_to_string(&config_path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");

    assert_eq!(json["ociVersion"], "1.2.1");
    assert_eq!(json["process"]["args"], serde_json::json!(["sh"]));
    assert_eq!(json["root"]["path"], "rootfs");
    assert_eq!(json["hostname"], "ocirun");
    assert!(
        json["linux"]["namespaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|ns| ns["type"] == "network"),
        "default spec must include a network namespace"
    );
}

#[test]
fn spec_refuses_to_overwrite_existing_config() {
    let dir = tempfile::tempdir().unwrap();
    let first = run_spec(dir.path(), &[]);
    assert!(first.status.success());

    let second = run_spec(dir.path(), &[]);
    assert!(!second.status.success());
    let stderr = String::from_utf8(second.stderr).unwrap();
    assert!(
        stderr.starts_with("error: "),
        "expected the shared error rendering, got: {stderr:?}"
    );
    assert!(stderr.contains("exists"), "got: {stderr:?}");
}

#[test]
fn spec_rootless_drops_network_namespace_and_adds_user_namespace() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_spec(dir.path(), &["--rootless"]);
    assert!(
        out.status.success(),
        "ocirun spec --rootless failed: {out:?}"
    );

    let raw = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap();

    let namespaces = json["linux"]["namespaces"].as_array().unwrap();
    let kinds: Vec<&str> = namespaces
        .iter()
        .map(|ns| ns["type"].as_str().unwrap())
        .collect();
    assert!(
        !kinds.contains(&"network"),
        "rootless must drop the network namespace"
    );
    assert!(
        kinds.contains(&"user"),
        "rootless must add a user namespace"
    );
    assert!(
        json["linux"]["resources"].is_null(),
        "rootless must drop cgroup resources"
    );

    let uid_mappings = json["linux"]["uidMappings"].as_array().unwrap();
    assert_eq!(uid_mappings.len(), 1);
    assert_eq!(uid_mappings[0]["containerID"], 0);
}

#[test]
fn spec_accepts_explicit_bundle_directory() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("bundle");
    std::fs::create_dir(&bundle).unwrap();

    let out = Command::new(bin_path("ocirun"))
        .args(["spec", "--bundle"])
        .arg(&bundle)
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun");
    assert!(out.status.success(), "ocirun spec --bundle failed: {out:?}");
    assert!(bundle.join("config.json").exists());
}

/// `ocirun spec -f/--file` (matching real `crun spec -f/--file`
/// exactly, checked directly against an installed `crun 1.14.1`; real
/// `runc spec` has no equivalent flag at all): writes to the given
/// path instead of `config.json`, relative to `--bundle` when both
/// are given.
#[test]
fn spec_file_writes_to_the_given_path_instead_of_config_json() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_spec(dir.path(), &["--file", "custom-name.json"]);
    assert!(out.status.success(), "ocirun spec --file failed: {out:?}");

    assert!(!dir.path().join("config.json").exists());
    let custom_path = dir.path().join("custom-name.json");
    assert!(custom_path.exists());
    let raw = std::fs::read_to_string(&custom_path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    assert_eq!(json["ociVersion"], "1.2.1");
}

/// A relative `--file` is resolved against `--bundle`, the same way
/// real `crun`'s own `chdir(bundle)`-then-relative-`fname` sequence
/// resolves it.
#[test]
fn spec_file_is_resolved_relative_to_an_explicit_bundle_directory() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("bundle");
    std::fs::create_dir(&bundle).unwrap();

    let out = Command::new(bin_path("ocirun"))
        .args(["spec", "--bundle"])
        .arg(&bundle)
        .args(["--file", "nested.json"])
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun");
    assert!(out.status.success(), "ocirun spec failed: {out:?}");
    assert!(bundle.join("nested.json").exists());
    assert!(!bundle.join("config.json").exists());
}

/// Unlike the default `config.json` destination (refused outright if
/// it already exists), an explicit `--file` target is silently
/// overwritten -- a real, checked-directly `crun` quirk (its own
/// `access(where, F_OK)` pre-check only runs when no `-f` was given),
/// verified directly against an installed `crun 1.14.1` before
/// porting it here.
#[test]
fn spec_file_silently_overwrites_an_existing_file_unlike_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let custom_path = dir.path().join("custom.json");
    std::fs::write(&custom_path, b"not a real spec at all").unwrap();

    let out = run_spec(dir.path(), &["--file", "custom.json"]);
    assert!(
        out.status.success(),
        "an explicit --file target should be overwritten, not refused: {out:?}"
    );
    let raw = std::fs::read_to_string(&custom_path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).expect("a real, freshly-written spec");
    assert_eq!(json["ociVersion"], "1.2.1");
}
