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

    let mut status: i32 = 0;
    loop {
        // SAFETY: `pid` is our own just-forked child; `&mut status` is a
        // valid pointer for the duration of the call.
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
