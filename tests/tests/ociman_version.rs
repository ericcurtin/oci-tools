//! `ociman version` integration tests (0162): matches real `docker
//! version`/`podman version` exactly for the no-remote-server case
//! (this project has no daemon at all, so there's only ever the one
//! "client" half — checked directly against a real rootless `podman
//! version`, which shows the identical shape).

use std::process::Command;

use oci_tools_tests::bin_path;

fn ociman(args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociman")
}

#[test]
fn version_plain_text_reports_a_real_client_only_table() {
    let out = ociman(&["version"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("Client:       ociman\n"),
        "got: {stdout:?}"
    );
    assert!(stdout.contains("Version:      "), "got: {stdout:?}");
    assert!(stdout.contains("Git Commit:   "), "got: {stdout:?}");
    assert!(stdout.contains("OS/Arch:      linux/"), "got: {stdout:?}");
    // No `Server:` section at all -- this project has no daemon,
    // matching a real rootless `podman version`'s own identical shape.
    assert!(!stdout.contains("Server:"), "got: {stdout:?}");
}

#[test]
fn version_json_reports_the_same_real_fields() {
    let out = ociman(&["version", "--json"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(view["version"], env!("CARGO_PKG_VERSION"));
    assert!(view["git_commit"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(
        view["os_arch"]
            .as_str()
            .is_some_and(|s| s.starts_with("linux/")),
        "got: {view:?}"
    );
}

/// `--format`/`-f` (`docs/design/0563`, matching real `podman version
/// --format`/`docker version --format` exactly) renders one or more
/// placeholders via this project's own already-established Go-
/// template-*lite* engine, and takes priority over `--json`/the
/// default plain-text report when both are given -- the same
/// precedence `info`/`inspect`/`ps`/`images`/`volume ls --format`
/// already established.
#[test]
fn version_format_renders_multiple_fields_and_takes_priority_over_json() {
    let out = ociman(&[
        "version",
        "--json",
        "--format",
        "{{.version}} {{.git_commit}}",
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut parts = stdout.trim().split(' ');
    assert_eq!(parts.next(), Some(env!("CARGO_PKG_VERSION")));
    assert!(
        parts.next().is_some_and(|s| !s.is_empty()),
        "got: {stdout:?}"
    );
}

/// The short alias `-f` behaves identically to `--format`, matching
/// real `podman version -f`'s own identical short flag exactly
/// (checked directly, `~/git/podman/cmd/podman/system/version.go:
/// 39`).
#[test]
fn version_format_short_alias_behaves_identically() {
    let out = ociman(&["version", "-f", "{{.os_arch}}"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .starts_with("linux/"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// An unresolvable field path is a real, immediate error, matching
/// real Go templates' own "can't evaluate field" failure for a typo'd
/// field name rather than a silent empty string -- the same
/// convention `info --format`'s own identical test already
/// establishes.
#[test]
fn version_format_of_an_unknown_field_is_a_clear_error() {
    let out = ociman(&["version", "--format", "{{.nosuchfield}}"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no field"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
