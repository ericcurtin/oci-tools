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

/// Like [`ociman`], but pipes `stdin_bytes` to the child's stdin
/// instead of leaving it closed -- for exercising `--password-stdin`.
fn ociman_with_stdin(auth_file: &Path, args: &[&str], stdin_bytes: &[u8]) -> std::process::Output {
    use std::io::Write as _;
    let mut child = Command::new(bin_path("ociman"))
        .env("REGISTRY_AUTH_FILE", auth_file)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ociman");
    child.stdin.take().unwrap().write_all(stdin_bytes).unwrap();
    child.wait_with_output().unwrap()
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

/// `login --tls-verify` (0527): accepted for real CLI compatibility,
/// but changes nothing at all -- this project's own `cmd_login` never
/// makes a real registry connection in the first place, so there is
/// nothing for `--tls-verify` to affect either way (see
/// `Command::Login`'s own doc comment for the full, checked-directly
/// reasoning). Proven here both ways (`=true`/`=false`) writing the
/// identical real credentials.
#[test]
fn login_tls_verify_flag_is_accepted_and_behaves_identically() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    let login_true = ociman(
        &auth_file,
        &[
            "login",
            "quay.io",
            "--username",
            "myuser",
            "--password",
            "mypass",
            "--tls-verify=true",
        ],
    );
    assert!(
        login_true.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&login_true.stderr)
    );
    let root: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&auth_file).unwrap()).unwrap();
    assert_eq!(root["auths"]["quay.io"]["auth"], "bXl1c2VyOm15cGFzcw==");

    let login_false = ociman(
        &auth_file,
        &[
            "login",
            "ghcr.io",
            "--username",
            "otheruser",
            "--password",
            "otherpass",
            "--tls-verify=false",
        ],
    );
    assert!(
        login_false.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&login_false.stderr)
    );
    let root: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&auth_file).unwrap()).unwrap();
    // `base64("otheruser:otherpass")`, checked directly.
    assert_eq!(
        root["auths"]["ghcr.io"]["auth"],
        "b3RoZXJ1c2VyOm90aGVycGFzcw=="
    );
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

#[test]
fn logout_all_removes_every_registry_at_once() {
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

    let logout = ociman(&auth_file, &["logout", "--all"]);
    assert!(
        logout.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&logout.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&logout.stdout).trim(),
        "Removed login credentials for all registries"
    );

    let root: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&auth_file).unwrap()).unwrap();
    assert_eq!(root["auths"], serde_json::json!({}));
}

#[test]
fn logout_all_together_with_a_registry_is_a_real_error() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    let logout = ociman(&auth_file, &["logout", "--all", "quay.io"]);
    assert!(!logout.status.success());
    assert!(
        String::from_utf8_lossy(&logout.stderr).contains("--all takes no arguments"),
        "stderr: {}",
        String::from_utf8_lossy(&logout.stderr)
    );
}

#[test]
fn logout_with_neither_a_registry_nor_all_is_a_real_error() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    let logout = ociman(&auth_file, &["logout"]);
    assert!(!logout.status.success());
    assert!(
        String::from_utf8_lossy(&logout.stderr)
            .contains("please provide a registry to log out from"),
        "stderr: {}",
        String::from_utf8_lossy(&logout.stderr)
    );
}

#[test]
fn login_password_stdin_writes_the_same_credentials_as_password() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    let login = ociman_with_stdin(
        &auth_file,
        &[
            "login",
            "quay.io",
            "--username",
            "myuser",
            "--password-stdin",
        ],
        b"mypass\n",
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
    // Same `base64("myuser:mypass")` fixture the plain `--password`
    // test already uses -- proves the stdin path produces exactly
    // the same credentials.
    assert_eq!(root["auths"]["quay.io"]["auth"], "bXl1c2VyOm15cGFzcw==");
}

#[test]
fn login_password_stdin_concatenates_multiple_lines_with_no_separator() {
    // Real podman's own checked-directly quirk: `bufio.Scanner` strips
    // each line's own trailing newline and nothing is re-inserted --
    // multiple stdin lines become one, run-together password.
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    let login = ociman_with_stdin(
        &auth_file,
        &["login", "quay.io", "--username", "user", "--password-stdin"],
        b"pass\nword\n",
    );
    assert!(login.status.success());

    let root: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&auth_file).unwrap()).unwrap();
    // `base64("user:password")`, checked directly -- proves the two
    // stdin lines "pass" and "word" really were joined with no
    // separator into "password", not e.g. "pass\nword" or "pass
    // word".
    assert_eq!(root["auths"]["quay.io"]["auth"], "dXNlcjpwYXNzd29yZA==");
}

#[test]
fn login_rejects_both_password_and_password_stdin_together() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    let login = ociman_with_stdin(
        &auth_file,
        &[
            "login",
            "quay.io",
            "--username",
            "user",
            "--password",
            "pass",
            "--password-stdin",
        ],
        b"other\n",
    );
    assert!(!login.status.success());
    assert!(
        String::from_utf8_lossy(&login.stderr)
            .contains("can't specify both --password-stdin and --password"),
        "stderr: {}",
        String::from_utf8_lossy(&login.stderr)
    );
}

#[test]
fn login_with_neither_password_nor_password_stdin_is_a_real_error() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    let login = ociman(&auth_file, &["login", "quay.io", "--username", "user"]);
    assert!(!login.status.success());
    assert!(
        String::from_utf8_lossy(&login.stderr)
            .contains("either --password or --password-stdin is required"),
        "stderr: {}",
        String::from_utf8_lossy(&login.stderr)
    );
}

/// `login --get-login` (0528): prints the username already logged in
/// to a registry, matching real `podman login --get-login` exactly.
#[test]
fn login_get_login_prints_the_username_already_logged_in() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    assert!(
        ociman(
            &auth_file,
            &["login", "quay.io", "-u", "myuser", "-p", "mypass"],
        )
        .status
        .success()
    );

    let get_login = ociman(&auth_file, &["login", "--get-login", "quay.io"]);
    assert!(
        get_login.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&get_login.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&get_login.stdout).trim(), "myuser");
}

/// `--get-login` on a registry never logged into is a real error,
/// matching real podman's own exact `"not logged into %s"` wording
/// (`~/git/container-libs/common/pkg/auth/auth.go:163`).
#[test]
fn login_get_login_on_a_registry_never_logged_into_is_a_real_error() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    let get_login = ociman(&auth_file, &["login", "--get-login", "never-seen.example"]);
    assert!(!get_login.status.success());
    assert!(
        String::from_utf8_lossy(&get_login.stderr).contains("not logged into never-seen.example"),
        "stderr: {}",
        String::from_utf8_lossy(&get_login.stderr)
    );
}

/// `--get-login` ignores `--username`/`--password` entirely rather
/// than erroring on them or the missing `--password-stdin` -- matches
/// real `auth.Login`'s own early return before ever looking at any of
/// them (see `Command::Login::get_login`'s own doc comment).
#[test]
fn login_get_login_ignores_username_and_password_when_given_alongside() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    assert!(
        ociman(&auth_file, &["login", "quay.io", "-u", "real", "-p", "pw"])
            .status
            .success()
    );

    let get_login = ociman(
        &auth_file,
        &[
            "login",
            "--get-login",
            "quay.io",
            "--username",
            "ignored",
            "--password",
            "ignored",
        ],
    );
    assert!(
        get_login.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&get_login.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&get_login.stdout).trim(), "real");
}

/// `login` without `--username` and without `--get-login` is a real
/// error -- this project has no interactive prompt to fall back to at
/// all (see `Command::Login::username`'s own doc comment).
#[test]
fn login_without_username_and_without_get_login_is_a_real_error() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    let login = ociman(&auth_file, &["login", "quay.io", "--password", "pw"]);
    assert!(!login.status.success());
    assert!(
        String::from_utf8_lossy(&login.stderr)
            .contains("--username is required unless --get-login is given"),
        "stderr: {}",
        String::from_utf8_lossy(&login.stderr)
    );
}

#[test]
fn login_get_login_json_reports_the_registry_and_username() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");

    assert!(
        ociman(&auth_file, &["login", "ghcr.io", "-u", "u", "-p", "p"])
            .status
            .success()
    );

    let get_login = ociman(&auth_file, &["--json", "login", "--get-login", "ghcr.io"]);
    assert!(get_login.status.success());
    let view: serde_json::Value = serde_json::from_slice(&get_login.stdout).unwrap();
    assert_eq!(view["registry"], "ghcr.io");
    assert_eq!(view["username"], "u");
}
