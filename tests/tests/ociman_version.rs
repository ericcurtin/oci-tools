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
