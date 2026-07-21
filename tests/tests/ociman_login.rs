//! `ociman login`/`ociman logout` integration tests: exercises the
//! actual built `ociman` binary's own CLI surface against a real,
//! on-disk auth file (via `$REGISTRY_AUTH_FILE`, taking priority over
//! every other candidate location so these tests never touch a real
//! user's own credentials) -- `oci_registry::credentials`'s own
//! `set`/`unset` already have thorough unit test coverage of their
//! own; this is a CLI-surface test on top of it.

use std::path::Path;
use std::process::Command;

use oci_tools_tests::bin_path;

fn ociman(auth_file: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("REGISTRY_AUTH_FILE", auth_file)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociman")
}

#[test]
fn login_writes_real_credentials_ociman_pull_could_actually_use() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    let login = ociman(
        &auth_file,
        &[
            "login",
            "quay.io",
            "--username",
            "myuser",
            "--password",
            "mypass",
        ],
    );
    assert!(
        login.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&login.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&login.stdout).trim(),
        "Login Succeeded!"
    );

    let root: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&auth_file).unwrap()).unwrap();
    // `base64("myuser:mypass")`, checked directly.
    assert_eq!(root["auths"]["quay.io"]["auth"], "bXl1c2VyOm15cGFzcw==");

    // Real `0o600` permissions, matching real podman/docker.
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(&auth_file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn login_json_reports_the_registry_and_auth_file_path() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    let login = ociman(
        &auth_file,
        &[
            "--json",
            "login",
            "ghcr.io",
            "--username",
            "u",
            "--password",
            "p",
        ],
    );
    assert!(login.status.success());
    let view: serde_json::Value = serde_json::from_slice(&login.stdout).unwrap();
    assert_eq!(view["registry"], "ghcr.io");
    assert_eq!(view["auth_file"], auth_file.to_str().unwrap());
}

#[test]
fn login_to_a_second_registry_preserves_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    assert!(
        ociman(&auth_file, &["login", "quay.io", "-u", "a", "-p", "b"])
            .status
            .success()
    );
    assert!(
        ociman(&auth_file, &["login", "ghcr.io", "-u", "c", "-p", "d"])
            .status
            .success()
    );

    let root: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&auth_file).unwrap()).unwrap();
    assert!(root["auths"]["quay.io"].is_object());
    assert!(root["auths"]["ghcr.io"].is_object());
}

#[test]
fn logout_removes_only_the_named_registry() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");
    assert!(
        ociman(&auth_file, &["login", "quay.io", "-u", "a", "-p", "b"])
            .status
            .success()
    );
    assert!(
        ociman(&auth_file, &["login", "ghcr.io", "-u", "c", "-p", "d"])
            .status
            .success()
    );

    let logout = ociman(&auth_file, &["logout", "quay.io"]);
    assert!(
        logout.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&logout.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&logout.stdout).trim(),
        "Removed login credentials for quay.io"
    );

    let root: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&auth_file).unwrap()).unwrap();
    assert!(root["auths"].get("quay.io").is_none());
    assert!(root["auths"]["ghcr.io"].is_object());
}

#[test]
fn logout_of_a_registry_never_logged_into_is_a_real_no_op_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    let logout = ociman(&auth_file, &["--json", "logout", "never-seen.example"]);
    assert!(
        logout.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&logout.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&logout.stdout).unwrap();
    assert_eq!(view["removed"], false);
}
