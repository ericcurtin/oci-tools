//! `ocicri publish` (`docs/design/0565`): a real, local, human-run CLI
//! subcommand — a real, faithful no-op, matching real `crio publish`'s
//! own genuinely inert `Action: func(c *cli.Context) error { return
//! nil }` exactly (checked directly, `~/git/cri-o/internal/criocli/
//! publish.go:7-25`). See `Command::Publish`'s own doc comment for
//! the full, checked-directly reasoning, including a correction of
//! this project's own earlier, mistaken "systemd-notify-socket
//! publisher" characterization (`docs/design/0532`).

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
fn publish_with_no_flags_is_a_silent_success() {
    let out = ocicri(&["publish"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "publish should print nothing: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// `--topic`/`--namespace` (matching real `crio publish --topic`/
/// `--namespace` exactly) are accepted and immediately discarded --
/// real crio's own identical flags are never actually read by its own
/// inert `Action` either.
#[test]
fn publish_accepts_topic_and_namespace_flags_and_still_no_ops() {
    let out = ocicri(&["publish", "--topic", "/tasks/exit", "--namespace", "k8s.io"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
}

/// A bare invocation (no subcommand at all) is completely unaffected
/// by this addition -- it still starts the real server, matching real
/// bare `crio`'s own identical `app.Action` default (the same
/// regression check `ocicri_version_cli.rs`'s own identical test
/// already establishes for `version`/`wipe`).
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
