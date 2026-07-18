//! Smoke tests: every binary in the workspace runs, reports a consistent
//! `--version` (with embedded git hash), prints usable `--help`, and fails
//! loudly (never silently succeeds) when invoked without a command.

use std::process::{Command, Output};

use oci_tools_tests::bin_path;

/// clap-based binaries sharing `oci-cli-common`.
const CLAP_BINS: &[&str] = &["ocirun", "ociman", "ocicri", "ocibox", "ociboot"];

/// All workspace binaries, including the dependency-free `ociboot-init`.
const ALL_BINS: &[&str] = &[
    "ocirun",
    "ociman",
    "ocicri",
    "ocibox",
    "ociboot",
    "ociboot-init",
];

fn run(name: &str, args: &[&str]) -> Output {
    Command::new(bin_path(name))
        .args(args)
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .unwrap_or_else(|err| panic!("failed to spawn {name}: {err}"))
}

#[test]
fn version_reports_name_version_and_git_hash() {
    let pkg_version = env!("CARGO_PKG_VERSION"); // workspace-wide version
    for bin in ALL_BINS {
        let out = run(bin, &["--version"]);
        assert!(out.status.success(), "{bin} --version failed: {out:?}");

        let stdout = String::from_utf8(out.stdout).expect("utf-8 version output");
        assert!(
            stdout.starts_with(&format!("{bin} {pkg_version} (git ")),
            "{bin}: unexpected version line: {stdout:?}"
        );
        assert!(
            stdout.trim_end().ends_with(')'),
            "{bin}: unexpected version line: {stdout:?}"
        );
    }
}

#[test]
fn help_exits_zero_and_mentions_usage() {
    for bin in ALL_BINS {
        let out = run(bin, &["--help"]);
        assert!(out.status.success(), "{bin} --help failed: {out:?}");

        let stdout = String::from_utf8(out.stdout).expect("utf-8 help output");
        assert!(
            stdout.contains("Usage"),
            "{bin}: --help output has no Usage section: {stdout:?}"
        );
    }
}

#[test]
fn bare_invocation_is_a_loud_error() {
    for bin in ALL_BINS {
        let out = run(bin, &[]);
        assert!(
            !out.status.success(),
            "{bin} with no arguments must fail until commands are implemented"
        );

        let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
        assert!(
            !stderr.trim().is_empty(),
            "{bin}: bare invocation must explain itself on stderr"
        );
    }
}

#[test]
fn clap_bins_render_error_chain_format() {
    for bin in CLAP_BINS {
        let out = run(bin, &[]);
        let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
        assert!(
            stderr.starts_with("error: "),
            "{bin}: expected shared `error: ...` rendering, got: {stderr:?}"
        );
    }
}

#[test]
fn invalid_log_filter_is_rejected_with_context() {
    // `foo=bar=baz` is not a valid EnvFilter directive; the shared logging
    // init must reject it through the shared error path.
    let out = run("ociman", &["--log-level", "foo=bar=baz"]);
    assert!(!out.status.success());

    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("invalid log filter"),
        "expected log-filter error, got: {stderr:?}"
    );
}

#[test]
fn json_flag_is_accepted_globally() {
    // --json parses on every clap binary (output support lands per-command).
    for bin in CLAP_BINS {
        let out = run(bin, &["--json"]);
        let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
        assert!(
            !stderr.contains("unexpected argument"),
            "{bin}: --json must be a recognized global flag: {stderr:?}"
        );
    }
}
