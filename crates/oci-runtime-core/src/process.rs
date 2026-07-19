//! `fork(2)` + `waitpid(2)`, the one primitive `oci-tools` needs that
//! `rustix` deliberately never wraps.
//!
//! `rustix` omits raw `fork()` on purpose: a forked child inherits only
//! the calling thread, so if the parent process has more than one thread,
//! any lock (allocator, `Mutex`, ...) held by a *different* thread at the
//! moment of `fork()` stays locked forever in the child, which never gets
//! to run the thread that would unlock it — the classic "only
//! async-signal-safe calls are sound between `fork()` and `exec()`/`_exit()`
//! in a multithreaded process" rule. Rather than provide a footgun,
//! `rustix` leaves it out; this module is the one place `oci-tools` picks
//! it up directly from `libc` (see the workspace `Cargo.toml` for why that
//! doesn't conflict with the "one crate per capability" rule — `libc`
//! isn't an alternative to `rustix` here, it's the gap `rustix` leaves on
//! purpose).

// This whole module is a thin wrapper around raw, `unsafe` FFI calls
// (`fork`/`_exit`/`waitpid`) — that's its entire purpose, unlike the rest
// of the workspace, which is `unsafe`-free apart from a few individually
// justified call sites elsewhere. A module-level allow here (rather than
// one at every call site, including in tests that exercise it) matches
// how `rustix` itself annotates its own raw-syscall modules.
#![allow(unsafe_code)]

use std::io;

/// Fork the calling process. `child_body` runs in the child and is
/// expected to end by calling `std::process::exit`, [`std::os::unix::
/// process::CommandExt::exec`], or a raw `_exit` — `oci-tools`' actual use
/// (container creation) execs the container's process at the end of a
/// successful `child_body`, or calls `std::process::exit` on failure. If
/// `child_body` returns normally anyway (a bug — it never should), the
/// child calls `_exit(127)` immediately rather than let a forked child
/// unwind back into whatever the parent would have done next. The parent
/// waits for the child (retrying on `EINTR`) and returns its raw
/// `waitpid(2)` status.
///
/// # Safety
///
/// Standard `fork(2)` safety rules apply for the code that runs in
/// `child_body`: if the calling process has more than one thread, only
/// async-signal-safe operations are guaranteed sound until `child_body`
/// calls `exec`/`_exit`. Callers with a multithreaded process (nothing in
/// this workspace's binaries currently spawns extra threads before
/// calling this, but a caller must not assume that stays true forever)
/// must keep `child_body` to async-signal-safe operations only, or fork
/// before doing anything that could spawn a thread.
pub unsafe fn fork_and_wait(child_body: impl FnOnce()) -> io::Result<i32> {
    // SAFETY: forwarded from this function's own contract.
    let pid = unsafe { fork(child_body) }?;
    wait(pid)
}

/// Fork the calling process, same `child_body` contract as
/// [`fork_and_wait`], but return the child's pid to the parent
/// immediately rather than waiting for it to exit — for callers that
/// need to do something with the pid (record it, hand it to another
/// process via a pipe, ...) before or instead of waiting, e.g. `create`
/// leaving the container's init process running in the background
/// (reparented to the nearest subreaper once this process's own parent
/// eventually exits, same as any other backgrounded Unix process — no
/// extra double-fork/daemonization needed for that).
///
/// # Safety
///
/// Same as [`fork_and_wait`]. Additionally, since this doesn't wait,
/// the caller becomes responsible for eventually reaping the child
/// (via [`wait`] or otherwise) to avoid leaving a zombie, unless it
/// intentionally exits without reaping (leaving the child to be
/// reparented and reaped by init/a subreaper, as `create` does).
pub unsafe fn fork(child_body: impl FnOnce()) -> io::Result<i32> {
    // SAFETY: raw `fork(2)`; the function's own safety contract (above)
    // is what callers must uphold for `child_body`.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        child_body();
        // child_body should have exec'd or _exit'd; if it didn't, don't
        // let this forked child fall back into any of the parent's own
        // code (e.g. this function's own remaining lines, or its caller).
        // SAFETY: `_exit` is always sound to call.
        unsafe { libc::_exit(127) };
    }
    Ok(pid)
}

/// `waitpid(2)` for `pid` (retrying on `EINTR`), returning the raw
/// status. `pid` must be a child of the calling process (e.g. one
/// [`fork`] just returned).
pub fn wait(pid: i32) -> io::Result<i32> {
    let mut status: i32 = 0;
    loop {
        // SAFETY: `&mut status` is a valid pointer for the duration of
        // the call; passing a `pid` that isn't actually our own child is
        // a logic error the kernel reports as `ECHILD`, not unsound.
        let ret = unsafe { libc::waitpid(pid, &mut status, 0) };
        if ret >= 0 {
            break;
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
    Ok(status)
}

/// `kill(2)`: send `signal` to `pid`. A raw syscall (not `rustix::
/// process::kill_process`, which only accepts its own typed `Signal`
/// enum) so any numeric signal a caller asks for — including ones with
/// no named constant, like realtime signals — can be sent, matching
/// what `runc kill <id> <n>` itself accepts.
pub fn kill(pid: i32, signal: i32) -> io::Result<()> {
    // SAFETY: plain `kill(2)` call with no pointers.
    let ret = unsafe { libc::kill(pid, signal) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Whether a process with this PID currently exists (`kill(pid, 0)`
/// per `kill(2)`'s own documented no-op-signal-check convention).
pub fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 sends nothing; only checks the pid exists and is
    // signalable.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Decode a raw `waitpid(2)` status the way a shell would: the exit code
/// if the process exited normally, or `128 + signal` if it was killed by
/// a signal (the same convention `sh`/`bash`/every other OCI runtime CLI
/// uses for its own process exit code).
pub fn exit_code_from_wait_status(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        // Stopped/continued: waitpid(2) with no WUNTRACED/WCONTINUED
        // never reports these, so this is unreachable in practice; still
        // handled rather than panicking.
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// # Safety
    /// The child's whole body is one async-signal-safe call (`_exit`
    /// never returns and touches no shared state), so this is sound
    /// regardless of how many threads this test binary has.
    unsafe fn exit_with(code: i32) {
        // SAFETY: see above.
        unsafe { libc::_exit(code) };
    }

    #[test]
    fn fork_and_wait_reports_the_childs_exit_code() {
        let status = unsafe { fork_and_wait(|| exit_with(42)) }.unwrap();
        assert_eq!(exit_code_from_wait_status(status), 42);
    }

    #[test]
    fn fork_and_wait_reports_success() {
        let status = unsafe { fork_and_wait(|| exit_with(0)) }.unwrap();
        assert_eq!(exit_code_from_wait_status(status), 0);
    }

    #[test]
    fn fork_returns_the_childs_pid_without_waiting() {
        let pid = unsafe { fork(|| exit_with(0)) }.unwrap();
        assert!(pid > 0);
        // Reap it ourselves so it doesn't linger as a zombie for the
        // rest of this test binary's life.
        let status = wait(pid).unwrap();
        assert_eq!(exit_code_from_wait_status(status), 0);
    }

    #[test]
    fn fork_then_wait_composes_to_the_same_result_as_fork_and_wait() {
        let pid = unsafe { fork(|| exit_with(42)) }.unwrap();
        let status = wait(pid).unwrap();
        assert_eq!(exit_code_from_wait_status(status), 42);
    }

    #[test]
    fn alive_is_true_for_this_process_and_false_for_a_reaped_child() {
        assert!(alive(std::process::id() as i32));

        let pid = unsafe { fork(|| exit_with(0)) }.unwrap();
        wait(pid).unwrap();
        assert!(!alive(pid));
    }

    #[test]
    fn kill_with_signal_zero_matches_alive() {
        let pid = std::process::id() as i32;
        assert!(kill(pid, 0).is_ok());

        let child = unsafe { fork(|| exit_with(0)) }.unwrap();
        wait(child).unwrap();
        assert!(kill(child, 0).is_err());
    }

    #[test]
    fn kill_actually_terminates_a_child() {
        // A long-lived child (blocks forever) that we kill instead of
        // letting exit on its own.
        let pid = unsafe {
            fork(|| {
                libc::pause();
                exit_with(0);
            })
        }
        .unwrap();
        assert!(alive(pid));
        kill(pid, libc::SIGKILL).unwrap();
        let status = wait(pid).unwrap();
        assert!(libc::WIFSIGNALED(status));
        assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
    }

    #[test]
    fn multiple_forks_get_independent_exit_codes() {
        for expected in [0, 1, 7, 42, 200] {
            let status = unsafe { fork_and_wait(move || exit_with(expected)) }.unwrap();
            assert_eq!(exit_code_from_wait_status(status), expected);
        }
    }

    #[test]
    fn exit_code_from_signaled_status_is_128_plus_signal() {
        // Build a synthetic "killed by SIGKILL" wait status the same way
        // the kernel would report it, without actually sending a signal:
        // WIFSIGNALED is true when the low 7 bits are nonzero and not
        // 0x7f; the signal number occupies those low 7 bits.
        let sigkill = libc::SIGKILL;
        let status = sigkill; // low bits = signal number, matches WIFSIGNALED's encoding
        assert!(libc::WIFSIGNALED(status));
        assert_eq!(exit_code_from_wait_status(status), 128 + sigkill);
    }
}
