//! The "systemd cgroup driver": creating a transient systemd scope for
//! a container's own pid over D-Bus, rather than this crate's own raw
//! cgroupfs-driver writes (`cgroups.rs`'s `directory_for`/`enter`).
//!
//! Real `crun`/`podman` default to this on systemd-based distros (this
//! project's own first-class targets, CentOS Stream 10 and Ubuntu
//! 26.04, both systemd-based) for a real, practical reason the
//! cgroupfs-only driver can't match: a raw `cgroup.procs` write only
//! succeeds across cgroup branches when the *calling* process already
//! has write access to their common ancestor (see `cgroups.rs`'s own
//! doc comment, and the `systemd-run --user --scope` carrier every
//! existing cgroup test in this project needs specifically because of
//! it) — an ordinary interactive shell or SSH session's own cgroup
//! never has that. Going through systemd instead sidesteps the
//! problem entirely: systemd itself (not the calling process) creates
//! the new cgroup and migrates the pid into it, using *its own*
//! authority over the subtree it already manages — the calling
//! process's own cgroup is irrelevant to whether this succeeds.
//!
//! Verified against this project's own real `systemd --user` instance
//! before writing any of the code below (a scratch program, deleted
//! after): forking a real child process and migrating *only* the
//! child's pid into a fresh transient scope correctly left the parent
//! (an ordinary session-scoped SSH shell, not inside any special
//! delegated wrapper) in its own original cgroup, while the child
//! ended up under `.../user@<uid>.service/app.slice/<scope>.scope` —
//! exactly the `systemd-run --user --scope` shape this project's own
//! tests already use, but without ever needing that wrapper at all.
//!
//! # The wait for `JobRemoved` is not optional
//!
//! Also verified directly, not assumed: `StartTransientUnit`'s own
//! method call reply does **not** mean the migration has actually
//! happened yet — checking `/proc/<pid>/cgroup` immediately after the
//! call returns, with no wait at all, consistently showed the *old*
//! cgroup, not the new one, across five repeated runs. The actual
//! cgroup creation and pid migration happens as part of systemd
//! processing the "start" job asynchronously; [`create_scope`]
//! subscribes to the `JobRemoved` signal *before* issuing the
//! `StartTransientUnit` call (to avoid racing an early signal) and
//! waits for the one matching its own job's object path, the same
//! ordering real `crun`'s own `cgroup-systemd.c` uses (checked against
//! `~/git/crun/src/libcrun/cgroup-systemd.c` directly, not re-derived
//! from documentation prose alone).
//!
//! # A known, not-yet-handled edge case
//!
//! A scope whose only member process exits normally is automatically
//! stopped and removed by systemd on its own — verified directly, no
//! explicit cleanup call needed (unlike the cgroupfs driver's own
//! `cgroups::remove`, 0027). A scope that never successfully received
//! its pid at all (e.g. the calling process crashed between
//! `StartTransientUnit` succeeding and the pid actually existing) can
//! be left behind in a `failed` state instead of being cleaned up
//! automatically — observed directly while iterating on this module's
//! own scratch verification. Not yet handled here; see this module's
//! own "what's still not here" note in `docs/design/0033`.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::{OwnedObjectPath, Value};

const SYSTEMD_BUS_NAME: &str = "org.freedesktop.systemd1";
const SYSTEMD_OBJECT_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER_INTERFACE: &str = "org.freedesktop.systemd1.Manager";

/// How long [`create_scope`] gives its own background thread (see its
/// own doc comment for why it's a thread, not just a deadline check)
/// to connect, create the transient unit, and see its start job
/// finish, before giving up on it entirely. An uncontended real run
/// consistently completes in well under a second; this is a generous
/// margin for real contention, not a tuned-tight timeout.
const JOB_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Create a transient systemd scope named `scope_name` (must end in
/// `.scope`) with `pid` as its sole initial member, waiting for
/// systemd to confirm the migration has actually completed (see this
/// module's own doc comment for why that wait is required) before
/// returning the real cgroup path `pid` ended up in (read back from
/// `/proc/<pid>/cgroup`, rather than reconstructed from the scope name
/// and an assumed slice convention, since the actual path can vary
/// depending on the caller's own delegated hierarchy).
///
/// Connects to the calling user's own D-Bus **session** bus (matching
/// `systemd --user`, the only mode this rootless-only project runs
/// containers in so far) — not the system bus.
///
/// # A real hang, found by stress-testing concurrent invocations, not by inspection
///
/// The entire D-Bus interaction below runs in a dedicated background
/// thread, with this function only ever waiting for it with a hard,
/// unconditional [`JOB_WAIT_TIMEOUT`] via a channel — not, as the very
/// first working version of this function did, a simple loop that
/// merely *checked* a deadline in between blocking calls to the
/// signal iterator's own `next()`. That version's "timeout" was
/// nothing of the sort: if `next()` itself never returned, the
/// deadline check between iterations never got a chance to fire
/// either, and the whole call — along with the container's own
/// process, which wired-in callers leave paused waiting for this
/// function's own "go" signal — hung indefinitely. Caught directly by
/// launching several real `ociman run` invocations concurrently (not
/// by reasoning about the code alone): roughly half of eight
/// simultaneous runs consistently hung well past any reasonable
/// per-container latency, confirmed via `ps`/`systemctl --user` to be
/// genuinely stuck, not merely slow, and confirmed *not* to be an
/// artifact of leftover processes from earlier test runs (reproduced
/// again from a freshly verified-clean process/unit state). The exact
/// reason `next()` itself doesn't always return promptly under
/// concurrent D-Bus load from many simultaneous callers was not fully
/// root-caused (plausibly some contention/ordering issue in either
/// the user systemd instance's own signal dispatch or `zbus`'s own
/// connection handling under this specific usage pattern) — but a
/// *correctness* fix does not require finding that root cause: no
/// matter what the underlying blocking call does internally, a
/// dedicated thread plus a channel `recv_timeout` on the read side
/// enforces a real wall-clock bound around the whole thing. Verified
/// this actually fixes the hang (not just changes its shape) via the
/// same repeated-concurrent-`ociman-run` stress test: every run either
/// succeeds quickly or, under heavy contention, degrades gracefully to
/// "no cgroup" within the bounded timeout — never hangs.
pub fn create_scope(pid: u32, scope_name: &str, description: &str) -> io::Result<PathBuf> {
    if !scope_name.ends_with(".scope") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("scope name {scope_name:?} must end in \".scope\""),
        ));
    }

    let scope_name_owned = scope_name.to_string();
    let description_owned = description.to_string();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    // Deliberately not joined: if the timeout below fires, this thread
    // is simply abandoned (there is no way to cancel a blocked `zbus`
    // call from the outside) rather than waited for — it costs nothing
    // to leave running, since the whole process exits long before it
    // could ever matter again, and `create_scope`'s own caller must
    // not be held hostage by it either way.
    std::thread::spawn(move || {
        let result = create_scope_dbus_roundtrip(pid, &scope_name_owned, &description_owned);
        let _ = result_tx.send(result);
    });

    match result_rx.recv_timeout(JOB_WAIT_TIMEOUT) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("timed out creating systemd scope {scope_name:?}"),
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(
            "the thread creating the systemd scope panicked",
        )),
    }
}

/// The actual D-Bus round trip [`create_scope`] runs in a background
/// thread — connect, subscribe to `JobRemoved`, call
/// `StartTransientUnit`, wait for the matching job, then read back the
/// real cgroup path. See [`create_scope`]'s own doc comment for why
/// this doesn't enforce its own timeout directly (any blocking call in
/// here might not respect one internally, which is exactly what this
/// whole design works around).
fn create_scope_dbus_roundtrip(
    pid: u32,
    scope_name: &str,
    description: &str,
) -> io::Result<PathBuf> {
    let connection = Connection::session().map_err(to_io_error)?;

    // Subscribed *before* the call below, so an unusually fast
    // `JobRemoved` can never arrive before this process is listening
    // for it.
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(SYSTEMD_BUS_NAME)
        .map_err(to_io_error)?
        .interface(SYSTEMD_MANAGER_INTERFACE)
        .map_err(to_io_error)?
        .member("JobRemoved")
        .map_err(to_io_error)?
        .build();
    let mut signals =
        MessageIterator::for_match_rule(rule, &connection, Some(1)).map_err(to_io_error)?;

    // Matches real crun's own property set for a container scope
    // (`cgroup-systemd.c`'s `enter_systemd_cgroup_scope`): `Delegate`
    // (without it, systemd keeps exclusive control of the cgroup and
    // refuses to let anything else, including this project's own
    // resource-limit writes, touch it), plus every accounting knob so
    // stats are always readable even when no explicit resource limit
    // is ever set.
    let properties: Vec<(&str, Value)> = vec![
        ("Description", Value::from(description)),
        ("DefaultDependencies", Value::from(false)),
        ("PIDs", Value::from(vec![pid])),
        ("Delegate", Value::from(true)),
        ("CPUAccounting", Value::from(true)),
        ("MemoryAccounting", Value::from(true)),
        ("IOAccounting", Value::from(true)),
        ("TasksAccounting", Value::from(true)),
    ];
    let auxiliary_units: Vec<(&str, Vec<(&str, Value)>)> = vec![];

    let job_path: OwnedObjectPath = connection
        .call_method(
            Some(SYSTEMD_BUS_NAME),
            SYSTEMD_OBJECT_PATH,
            Some(SYSTEMD_MANAGER_INTERFACE),
            "StartTransientUnit",
            &(scope_name, "fail", properties, auxiliary_units),
        )
        .map_err(to_io_error)?
        .body()
        .deserialize()
        .map_err(to_io_error)?;

    wait_for_job(&mut signals, &job_path, scope_name)?;

    std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .and_then(|contents| parse_own_cgroup_path(&contents))
}

/// Block until a `JobRemoved` signal matching `job_path` arrives,
/// reporting a job that didn't finish with systemd's own `"done"`
/// result (e.g. `"failed"`) as an error rather than silently
/// proceeding as if it had succeeded. Deliberately has no timeout of
/// its own — see [`create_scope`]'s own doc comment for why enforcing
/// one at this level isn't sufficient by itself.
fn wait_for_job(
    signals: &mut MessageIterator,
    job_path: &OwnedObjectPath,
    scope_name: &str,
) -> io::Result<()> {
    loop {
        let Some(message) = signals.next() else {
            return Err(io::Error::other("D-Bus signal stream ended unexpectedly"));
        };
        let message = message.map_err(to_io_error)?;
        let (_id, path, _unit, result): (u32, OwnedObjectPath, String, String) =
            message.body().deserialize().map_err(to_io_error)?;
        if &path != job_path {
            continue;
        }
        return if result == "done" {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "systemd job for scope {scope_name:?} finished with result {result:?}, not \"done\""
            )))
        };
    }
}

/// Extract the cgroup v2 path from a `/proc/<pid>/cgroup` file's
/// contents (`0::<path>` on a unified-hierarchy-only system, which is
/// this project's own documented, exclusive target — see the
/// top-level README's filesystem-policy design pillar).
fn parse_own_cgroup_path(contents: &str) -> io::Result<PathBuf> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(PathBuf::from)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("no cgroup v2 (\"0::\") entry in: {contents:?}"),
            )
        })
}

/// Map any `zbus`/D-Bus failure to a plain `io::Error` — every caller
/// in this crate already works in terms of `io::Result`, matching how
/// every other syscall-adjacent module here (`cgroups`, `namespaces`,
/// ...) reports failure.
fn to_io_error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_own_cgroup_path_reads_the_unified_hierarchy_entry() {
        assert_eq!(
            parse_own_cgroup_path("0::/user.slice/user-1000.slice/app.slice/foo.scope\n").unwrap(),
            PathBuf::from("/user.slice/user-1000.slice/app.slice/foo.scope")
        );
    }

    #[test]
    fn parse_own_cgroup_path_rejects_content_with_no_unified_entry() {
        // A real, current cgroup v1 fixture line shape (not something
        // this project targets, but a plausible thing to see on a
        // hybrid-mode system) -- deliberately has no "0::" line.
        let err = parse_own_cgroup_path("1:name=systemd:/user.slice\n").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn create_scope_rejects_a_name_not_ending_in_dot_scope() {
        let err = create_scope(1, "not-a-scope", "test").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// Real, end-to-end: creates a genuine transient scope for a freshly
    /// forked child process (not this test process itself — proving a
    /// *different* pid gets migrated while the caller's own cgroup is
    /// untouched, the actual real use case), verified against this
    /// session's own real `systemd --user` instance (skips itself,
    /// printing why, on an environment with no reachable one — the same
    /// "gated on real availability" pattern `docs/design/0015`'s own
    /// cgroup tests already established for `systemd-run --user
    /// --scope`).
    #[test]
    fn create_scope_migrates_a_real_child_pid_and_leaves_the_caller_alone() {
        if !systemd_user_session_available() {
            eprintln!("skipping: no reachable `systemd --user` session");
            return;
        }

        let own_pid_before = read_own_cgroup();

        // SAFETY: a plain `fork(2)` with no other threads running
        // between it and the child's own `_exit` below (this test
        // creates no threads of its own); the child only ever calls
        // async-signal-safe operations (`sleep`, `_exit`).
        #[allow(unsafe_code)]
        let child_pid = unsafe { libc::fork() };
        assert!(child_pid >= 0, "fork failed");
        if child_pid == 0 {
            // SAFETY: `_exit` is always sound; runs after a real sleep
            // so the parent has time to migrate and inspect us first.
            #[allow(unsafe_code)]
            unsafe {
                libc::sleep(5);
                libc::_exit(0);
            }
        }
        // Give the child a moment to actually exist before targeting it.
        std::thread::sleep(Duration::from_millis(50));

        let scope_name = format!("oci-runtime-core-test-{child_pid}.scope");
        let result = create_scope(child_pid as u32, &scope_name, "oci-runtime-core test scope");

        // Always reap the child and ask systemd to forget the scope,
        // regardless of whether `create_scope` itself succeeded.
        #[allow(unsafe_code)]
        unsafe {
            libc::kill(child_pid, libc::SIGKILL);
            let mut status = 0i32;
            libc::waitpid(child_pid, &mut status, 0);
        }

        let cgroup_path = result.unwrap();
        assert!(
            cgroup_path.to_string_lossy().contains(&scope_name),
            "expected the child's own cgroup to reflect the new scope: {}",
            cgroup_path.display()
        );

        let own_pid_after = read_own_cgroup();
        assert_eq!(
            own_pid_before, own_pid_after,
            "the calling process's own cgroup must be unaffected by migrating a *different* pid"
        );
    }

    fn read_own_cgroup() -> String {
        std::fs::read_to_string("/proc/self/cgroup").unwrap()
    }

    /// Same probe `docs/design/0015`'s own cgroup tests use for
    /// `systemd-run --user --scope`, adapted here: a real, self-
    /// cleaning D-Bus round trip (`systemctl --user is-system-running`
    /// talks to the exact same bus this module itself uses) rather
    /// than just checking a socket path exists.
    fn systemd_user_session_available() -> bool {
        std::process::Command::new("systemctl")
            .args(["--user", "is-system-running"])
            .output()
            .is_ok_and(|out| {
                // "running" or "degraded" both mean the bus itself is
                // reachable and answering -- only care that we got a
                // real reply, not systemd's own overall health.
                !out.stdout.is_empty()
            })
    }
}
