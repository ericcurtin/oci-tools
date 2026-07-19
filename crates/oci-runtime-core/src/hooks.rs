//! OCI runtime-spec lifecycle hooks: all six real hook points are
//! executed. `prestart`/`createRuntime`/`poststart`/`poststop` are
//! wired into [`crate::launch::run_reporting_pid`]/
//! `run_pre_pivot_hooks` (see `docs/design/0026`/`0035`);
//! `createContainer`/`startContainer` run from *inside* the forked
//! child itself, at the specific pre-/post-`pivot_root` points the
//! real spec requires (see `crate::launch::ChildSetup::
//! run_container_hooks`, `docs/design/0087`).
//!
//! Unlike everything else in this crate, a hook process is an ordinary,
//! independent process with no namespace/rootfs concerns of its own —
//! `std::process::Command` (not this crate's own `fork`/`exec`
//! primitives, which exist specifically for the *container's* process)
//! is exactly the right tool here.

use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use oci_spec_types::runtime::Hook;

/// The `state` JSON piped to each hook's stdin, matching the OCI
/// runtime-spec's own `State` schema exactly (verified against the
/// real vendored `opencontainers/runtime-spec` Go module,
/// `specs-go/state.go`) — deliberately not [`crate::state::StateView`]
/// (which carries extra fields, `rootfs`/`created`, for this crate's
/// own CLI convenience that aren't part of the spec's hook-facing
/// state at all).
#[derive(Debug, serde::Serialize)]
pub struct HookState<'a> {
    /// Runtime-spec version of the bundle this container was created
    /// from.
    #[serde(rename = "ociVersion")]
    pub oci_version: &'a str,
    /// The container ID.
    pub id: &'a str,
    /// `"creating"`/`"created"`/`"running"`/`"stopped"`, matching the
    /// runtime-spec's own `ContainerState` values exactly.
    pub status: &'a str,
    /// PID of the container's init process; `0` once stopped (matching
    /// [`crate::state::StateView::pid`]'s own convention).
    pub pid: i32,
    /// Absolute path to the bundle directory.
    pub bundle: String,
    /// Annotations copied from `config.json`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

/// Run every hook in `hooks` in order, each with `state` (already-
/// serialized JSON bytes) piped to its stdin.
///
/// If `keep_going` is `false`, the first hook to fail (nonzero exit,
/// killed by a signal, spawn error, or timeout) stops the rest and is
/// returned as the error — matching real runc/crun's own handling of
/// every hook point except `poststop`, which always runs every hook
/// regardless of earlier failures (`keep_going = true`): the container
/// has already exited by the time `poststop` runs, so one broken
/// cleanup script shouldn't prevent another's from running.
pub fn run(hooks: &[Hook], state: &[u8], keep_going: bool) -> io::Result<()> {
    let mut first_error = None;
    for hook in hooks {
        if let Err(e) = run_one(hook, state) {
            if !keep_going {
                return Err(e);
            }
            first_error.get_or_insert(e);
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Run a single hook to completion (or until it times out), writing
/// `state` to its stdin.
fn run_one(hook: &Hook, state: &[u8]) -> io::Result<()> {
    let mut command = Command::new(&hook.path);
    // `hook.args` has "the same semantics as execv's argv" per the real
    // spec (config.md's own `args` field doc) — meaning `args[0]` is
    // conventionally the program's own name, exactly like a shell's own
    // `argv[0]`, not an *additional* argument on top of one `Command`
    // already generates from `hook.path`. `Command::args` alone has no
    // way to override `argv[0]` (it always mirrors the program path
    // given to `Command::new`), so `arg0` (Unix-specific) is needed to
    // set it explicitly from `args[0]` instead, with the rest of
    // `args` following as the real additional arguments.
    if let Some((arg0, rest)) = hook.args.split_first() {
        command.arg0(arg0);
        command.args(rest);
    }
    // An empty `env` inherits the runtime's own ambient environment
    // (`Command`'s own default); a non-empty one replaces it entirely
    // — see `Hook::env`'s own doc comment for why (checked against
    // real crun's `do_hooks`, not the spec prose alone).
    if !hook.env.is_empty() {
        command.env_clear();
        for kv in &hook.env {
            if let Some((key, value)) = kv.split_once('=') {
                command.env(key, value);
            }
        }
    }
    command.stdin(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("spawning hook {:?}: {e}", hook.path)))?;

    if let Some(mut stdin) = child.stdin.take() {
        // Best-effort: a hook that doesn't actually read its stdin
        // (the spec doesn't strictly require it to) shouldn't fail
        // over a broken pipe on our end.
        let _ = stdin.write_all(state);
    }

    let timeout = hook
        .timeout
        .map(|secs| Duration::from_secs(secs.max(0) as u64));
    let status = wait_with_timeout(&mut child, timeout)?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "hook {:?} exited with {status}",
            hook.path
        )));
    }
    Ok(())
}

/// [`std::process::Child::wait`], but killing (and reaping) the child
/// if `timeout` elapses first — `std::process::Child` has no built-in
/// wait-with-timeout, so this polls `try_wait` (the standard,
/// documented way to implement one without an extra dependency).
fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Option<Duration>,
) -> io::Result<std::process::ExitStatus> {
    let Some(timeout) = timeout else {
        return child.wait();
    };
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "hook timed out"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_hooks_list_is_a_no_op() {
        assert!(run(&[], b"{}", false).is_ok());
        assert!(run(&[], b"{}", true).is_ok());
    }

    #[test]
    fn a_hook_receives_state_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.json");
        let hook = Hook {
            path: "/bin/sh".to_string(),
            args: vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("cat > {}", out.display()),
            ],
            env: vec![],
            timeout: None,
        };
        run(std::slice::from_ref(&hook), b"{\"id\":\"abc\"}", false).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "{\"id\":\"abc\"}");
    }

    #[test]
    fn a_failing_hook_is_reported_as_an_error() {
        let hook = Hook {
            path: "/bin/sh".to_string(),
            args: vec!["sh".to_string(), "-c".to_string(), "exit 7".to_string()],
            env: vec![],
            timeout: None,
        };
        let err = run(std::slice::from_ref(&hook), b"{}", false).unwrap_err();
        assert!(err.to_string().contains('7'), "{err}");
    }

    #[test]
    fn keep_going_runs_every_hook_even_after_an_earlier_failure() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("second-ran");
        let failing = Hook {
            path: "/bin/sh".to_string(),
            args: vec!["sh".to_string(), "-c".to_string(), "exit 1".to_string()],
            env: vec![],
            timeout: None,
        };
        let second = Hook {
            path: "/bin/sh".to_string(),
            args: vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("touch {}", out.display()),
            ],
            env: vec![],
            timeout: None,
        };
        let err = run(&[failing, second], b"{}", true).unwrap_err();
        assert!(err.to_string().contains('1'), "{err}");
        assert!(
            out.exists(),
            "the second hook should still have run despite the first one failing"
        );
    }

    #[test]
    fn not_keep_going_stops_at_the_first_failure() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("second-ran");
        let failing = Hook {
            path: "/bin/sh".to_string(),
            args: vec!["sh".to_string(), "-c".to_string(), "exit 1".to_string()],
            env: vec![],
            timeout: None,
        };
        let second = Hook {
            path: "/bin/sh".to_string(),
            args: vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("touch {}", out.display()),
            ],
            env: vec![],
            timeout: None,
        };
        let _ = run(&[failing, second], b"{}", false);
        assert!(
            !out.exists(),
            "the second hook should not have run: the first one already failed"
        );
    }

    #[test]
    fn empty_env_inherits_the_ambient_environment() {
        // SAFETY: this test doesn't run concurrently with anything
        // else that reads/writes this specific env var.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("OCI_TOOLS_HOOK_TEST_AMBIENT", "ambient-value");
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let hook = Hook {
            path: "/bin/sh".to_string(),
            args: vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo \"$OCI_TOOLS_HOOK_TEST_AMBIENT\" > {}", out.display()),
            ],
            env: vec![],
            timeout: None,
        };
        run(std::slice::from_ref(&hook), b"{}", false).unwrap();
        assert_eq!(
            std::fs::read_to_string(&out).unwrap().trim(),
            "ambient-value"
        );
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("OCI_TOOLS_HOOK_TEST_AMBIENT");
        }
    }

    #[test]
    fn non_empty_env_replaces_the_ambient_environment_entirely() {
        // SAFETY: same as above.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("OCI_TOOLS_HOOK_TEST_SHOULD_NOT_LEAK", "leaked");
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let hook = Hook {
            path: "/bin/sh".to_string(),
            args: vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"got:${{OCI_TOOLS_HOOK_TEST_SHOULD_NOT_LEAK:-unset}},$OCI_TOOLS_HOOK_TEST_ONLY\" > {}",
                    out.display()
                ),
            ],
            env: vec!["OCI_TOOLS_HOOK_TEST_ONLY=only-value".to_string()],
            timeout: None,
        };
        run(std::slice::from_ref(&hook), b"{}", false).unwrap();
        assert_eq!(
            std::fs::read_to_string(&out).unwrap().trim(),
            "got:unset,only-value"
        );
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("OCI_TOOLS_HOOK_TEST_SHOULD_NOT_LEAK");
        }
    }

    #[test]
    fn a_hook_that_outlives_its_timeout_is_killed_and_reported() {
        let hook = Hook {
            path: "/bin/sh".to_string(),
            args: vec!["sh".to_string(), "-c".to_string(), "sleep 30".to_string()],
            env: vec![],
            timeout: Some(1),
        };
        let started = Instant::now();
        let err = run(std::slice::from_ref(&hook), b"{}", false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "should have been killed well before the real 30s sleep finished"
        );
    }
}
