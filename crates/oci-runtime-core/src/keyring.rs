//! Joining a fresh, container-scoped session keyring before `exec`,
//! matching real `runc run`/`crun run`'s own unconditional default —
//! opt-out via `--no-new-keyring` (see `launch::ChildSetup::
//! no_new_keyring`'s own doc comment for exactly where this is called
//! from). Without this, every container previously silently shared
//! the host's own session keyring with no isolation at all — a real,
//! previously-unnoticed gap, confirmed by grepping this crate's entire
//! tree for `keyctl`/`keyring` before this file existed: zero hits
//! anywhere.
//!
//! `libc` has no `keyctl(2)` wrapper at all (real glibc doesn't either
//! — `man 2 keyctl`: "glibc provides no wrapper for keyctl(),
//! necessitating the use of syscall(2)") and doesn't publicly expose
//! `KEYCTL_JOIN_SESSION_KEYRING` either (checked directly against the
//! vendored `libc` crate's own source: the only definition lives in a
//! `pub(crate)`-only module its own doc comment says is deliberately
//! not re-exported) — so, matching real crun's own identical
//! `syscall_keyctl_join` (`~/git/crun/src/libcrun/linux.c`), this
//! defines its own local constant and calls the raw syscall directly,
//! exactly like crun does in C.

use std::ffi::CString;
use std::io;

/// `KEYCTL_JOIN_SESSION_KEYRING` (`linux/keyctl.h`) — not part of
/// `libc`'s own public API (see this module's doc comment), so
/// defined locally, matching real crun's own identical `#define
/// KEYCTL_JOIN_SESSION_KEYRING 0x1`.
const KEYCTL_JOIN_SESSION_KEYRING: libc::c_int = 1;

/// Join (creating it first if it doesn't already exist) a session
/// keyring named `name` — matching real `runc run`/`crun run`'s own
/// unconditional default (`keys.JoinSessionKeyring`/
/// `syscall_keyctl_join`), called at the same point in the child's own
/// setup sequence both reference runtimes use it: before rootfs/
/// `pivot_root`, before capability drop/seccomp, before `exec` (see
/// `launch::ChildSetup::mount_pivot_and_exec`'s own call site).
///
/// No special capability is required for this — checked directly
/// against `man 2 keyctl`/`man 7 session-keyring`:
/// `KEYCTL_JOIN_SESSION_KEYRING` is an ordinary, unprivileged operation
/// for a *fresh* named keyring (only subscribing to an *existing* one
/// this process didn't itself create requires `search` permission on
/// it, an irrelevant case here), and nothing about it is conditioned
/// on the calling process's own user-namespace identity — this
/// project's own default rootless (fresh user namespace) setup is no
/// obstacle at all.
///
/// `ENOSYS` (a kernel built with `CONFIG_KEYS` disabled, or genuinely
/// ancient) is tolerated, not fatal — matching both real runc's own
/// warning-only handling and real crun's identical choice; any other
/// error is real and surfaced.
pub fn join_session_keyring(name: &str) -> io::Result<()> {
    let name = CString::new(name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "keyring name contains a NUL byte",
        )
    })?;
    // SAFETY: `libc::SYS_keyctl` (`__NR_keyctl`) has a fixed-arity C
    // `long keyctl(int operation, ...)` varargs signature; for
    // `KEYCTL_JOIN_SESSION_KEYRING` the kernel only ever reads
    // `operation` and the `name` pointer (a valid, NUL-terminated,
    // live `CString` for the entire call), never the remaining
    // positions this real syscall's own varargs ABI still requires
    // filling — matching real crun's own identical `syscall
    // (__NR_keyctl, KEYCTL_JOIN_SESSION_KEYRING, name, 0)` call
    // exactly (which likewise leaves further positions unfilled; the
    // kernel never reads past what each `keyctl` operation actually
    // needs).
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::syscall(
            libc::SYS_keyctl,
            KEYCTL_JOIN_SESSION_KEYRING,
            name.as_ptr(),
            0,
            0,
            0,
        )
    };
    if rc < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOSYS) {
            return Ok(());
        }
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_session_keyring_with_a_real_name_succeeds_or_is_tolerated() {
        // A real, live syscall against whatever kernel this test
        // actually runs on -- either genuinely joins/creates a session
        // keyring (the overwhelmingly likely outcome on any modern
        // Linux CI/dev box) or is tolerated as ENOSYS; either way this
        // must not return any *other* error.
        join_session_keyring("oci-runtime-core-test-keyring").unwrap();
    }

    #[test]
    fn join_session_keyring_rejects_a_name_containing_a_nul_byte() {
        assert!(join_session_keyring("bad\0name").is_err());
    }
}
