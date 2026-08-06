//! `ocicri version` (`docs/design/0532`): a real, local, human-run CLI
//! subcommand — genuinely different from `RuntimeService.Version`
//! (the kubelet-facing gRPC RPC already covered by
//! `ocicri_version.rs`). Matches real `crio version`'s own "display
//! detailed version information" (checked directly, `~/git/cri-o/
//! internal/criocli/version.go:17-49`) for the subset of fields this
//! project has an honest value for.

use std::process::Command;

use oci_tools_tests::bin_path;

fn ocicri(args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ocicri"))
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ocicri")
}

#[test]
fn version_prints_a_real_version_git_commit_and_platform() {
    let out = ocicri(&["version"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("Version:"), "{stdout:?}");
    assert!(stdout.contains("GitCommit:"), "{stdout:?}");
    assert!(stdout.contains("Platform:"), "{stdout:?}");
    // A real, current build version -- matches this crate's own
    // Cargo.toml, not a hardcoded/stale placeholder.
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")), "{stdout:?}");
}

#[test]
fn version_json_emits_version_git_commit_and_platform_fields() {
    let out = ocicri(&["version", "--json"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("version --json output was not valid JSON: {e}"));
    assert_eq!(report["version"], env!("CARGO_PKG_VERSION"));
    assert!(report["git_commit"].is_string());
    assert!(
        report["platform"].as_str().unwrap().contains('/'),
        "{report:?}"
    );
}

/// The global `--json` flag (not a second, `version`-local one, unlike
/// real `crio version --json`/`-j`) is what selects JSON output here
/// -- matching every other `ocicri`/`ociman` command's own identical
/// convention.
#[test]
fn version_uses_the_global_json_flag_not_a_local_one() {
    let unrecognized = ocicri(&["version", "-j"]);
    assert!(!unrecognized.status.success());

    let global = ocicri(&["--json", "version"]);
    assert!(
        global.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&global.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&global.stdout)
        .unwrap_or_else(|e| panic!("version --json output was not valid JSON: {e}"));
    assert_eq!(report["version"], env!("CARGO_PKG_VERSION"));
}

/// A bare invocation (no subcommand at all) is completely unaffected
/// by this addition -- it still starts the real server, matching real
/// bare `crio`'s own identical `app.Action` default. Verified here by
/// confirming the server actually binds its own real socket, then
/// killing it -- not just that the process doesn't immediately exit.
#[test]
fn no_subcommand_still_starts_the_real_server() {
    let storage_dir = tempfile::tempdir().unwrap();
    let socket_path = storage_dir.path().join("ocicri.sock");
    let mut child = Command::new(bin_path("ocicri"))
        .env_remove("OCI_TOOLS_LOG")
        .args(["--listen", socket_path.to_str().unwrap()])
        .spawn()
        .expect("failed to spawn ocicri");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !socket_path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "ocicri never bound its own socket at {}",
            socket_path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(socket_path.exists());

    let _ = child.kill();
    let _ = child.wait();
}
