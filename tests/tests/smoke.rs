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

/// Every `CLAP_BINS` entry *except* `ocicri` — since 0212, a bare
/// `ocicri` invocation is real, valid, default behavior (starting the
/// CRI gRPC server and blocking forever, matching real `cri-o`'s own
/// identical "invoking it just *is* running the daemon" behavior —
/// `main.rs`'s own module doc comment), not an error to report on
/// stderr and exit nonzero from the way every other binary here still
/// legitimately does until it's given a real command. Exercised by its
/// own real, socket-connecting integration test instead
/// (`ocicri_version.rs`), which a quick `Command::output()` call
/// (this file's own `run` helper, which waits for the child to exit)
/// could never do for a process that's supposed to keep running.
const BINS_THAT_ERROR_ON_BARE_INVOCATION: &[&str] =
    &["ocirun", "ociman", "ocibox", "ociboot", "ociboot-init"];

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
    for bin in BINS_THAT_ERROR_ON_BARE_INVOCATION {
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
        if !BINS_THAT_ERROR_ON_BARE_INVOCATION.contains(bin) {
            // `ocicri`: see `BINS_THAT_ERROR_ON_BARE_INVOCATION`'s own
            // doc comment -- a bare invocation is real, valid default
            // behavior (starting the server), not an error at all.
            continue;
        }
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
    // `ocicri` is checked separately, just below: unlike every other bin
    // here, a bare `ocicri --json` is real, valid default behavior (it
    // starts the server and blocks forever, see
    // `BINS_THAT_ERROR_ON_BARE_INVOCATION`'s own doc comment), so this
    // loop can't use the simple `run`-and-check-stderr helper for it at
    // all -- there's no stderr to check yet by the time a real server
    // would still be happily running.
    for bin in CLAP_BINS {
        if *bin == "ocicri" {
            continue;
        }
        let out = run(bin, &["--json"]);
        let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
        assert!(
            !stderr.contains("unexpected argument"),
            "{bin}: --json must be a recognized global flag: {stderr:?}"
        );
    }
}

/// `ocicri --json` specifically: confirmed by spawning it (with a real,
/// temporary `--listen` socket path so it doesn't fight over the real
/// default one with any other test) and checking it's still alive a
/// short while later, rather than the usual quick `run`-and-inspect-
/// stderr helper -- a real *argument-parsing* failure (which is all
/// this test cares about) would have made it exit almost immediately;
/// still running confirms `--json` parsed fine and the server
/// actually started, exactly the same real signal
/// `ocicri_version.rs`'s own tests already rely on more thoroughly
/// (a real gRPC call succeeding).
#[test]
fn ocicri_json_flag_is_accepted_and_the_server_still_starts() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("ocicri.sock");
    let mut child = Command::new(bin_path("ocicri"))
        .env_remove("OCI_TOOLS_LOG")
        .args(["--json", "--listen", socket_path.to_str().unwrap()])
        .spawn()
        .expect("failed to spawn ocicri");

    std::thread::sleep(std::time::Duration::from_millis(300));
    let still_running = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        still_running,
        "ocicri --json should still be running a moment later, not have exited already \
         (a real argument-parsing failure would exit almost immediately)"
    );
}
