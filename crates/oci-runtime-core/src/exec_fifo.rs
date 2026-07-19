//! The "exec fifo" synchronization primitive `create`/`start` use to
//! keep the container's own init process blocked in between the two —
//! ported from real `runc`'s own mechanism
//! (`libcontainer/container_linux.go`'s `createExecFifo`/`handleFifo`,
//! `standard_init_linux.go`'s reopen-via-`/proc/self/fd`), not
//! reinvented from first principles: both the two-sided blocking-open
//! trick and the pivot_root-survival dance below are easy to get subtly
//! wrong.
//!
//! A POSIX FIFO's `open(2)` blocks (absent `O_NONBLOCK`) until a peer
//! opens the *other* end: opening for **write** blocks until a reader
//! shows up, and vice versa. `create`'s container process opens the
//! fifo for *write* ([`wait_for_start`]) — so it sits blocked doing
//! nothing else — until `start` opens it for *read*
//! ([`signal_start`]), which is what actually unblocks the write-open;
//! the container process then writes one byte (proving it's alive and
//! really is about to `exec`, not just coincidentally unblocked) and
//! `start` reads it back before returning.
//!
//! # Surviving `pivot_root`
//!
//! The container process only actually waits on the fifo as the very
//! last step before `exec` — after `pivot_root` has already swapped its
//! view of `/` to the container's own rootfs (see `docs/design/`'s
//! launch note). An ordinary `open(2)` **by path** at that point would
//! resolve against the *container's* root, not wherever the fifo
//! actually lives on the host, and fail outright (caught by actually
//! running this against a real kernel, not by inspection — the exact
//! failure `ENOENT` reported was the first sign anything was wrong
//! here). [`open_path`] gets a reference to the fifo *before*
//! `pivot_root` (an `O_PATH` open: it doesn't block on the FIFO the way
//! a real read/write open would, and — being a plain file descriptor —
//! is completely unaffected by a later mount-namespace/root change);
//! [`wait_for_start`] then re-opens that same fd *after* `pivot_root`
//! via `/proc/self/fd/<n>`, which — being a `/proc` magic symlink
//! resolved against the *kernel's* record of what that fd refers to,
//! not the calling process's current root — still finds the real fifo
//! regardless.

use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::path::Path;

use rustix::fs::{CWD, Mode, OFlags, mkfifoat, open};

/// Filename of the exec fifo within a container's state directory
/// (matches runc's own convention: alongside `state.json`, not in the
/// bundle directory, so it's unique per container even if a bundle is
/// reused).
pub const FILENAME: &str = "exec.fifo";

/// Create the fifo at `path`, owner-only (`0600`) since only this
/// runtime's own `create`/`start` invocations ever need to touch it.
pub fn create(path: &Path) -> io::Result<()> {
    mkfifoat(CWD, path, Mode::from_raw_mode(0o600)).map_err(io::Error::from)
}

/// Get an `O_PATH` reference to the fifo at `path`, safe to hold across
/// a later `pivot_root`/mount-namespace change (see this module's own
/// doc comment) and pass to [`wait_for_start`] afterward. Call this
/// *before* `pivot_root`, while `path` still resolves against the same
/// root the fifo was actually created in.
pub fn open_path(path: &Path) -> io::Result<OwnedFd> {
    open(path, OFlags::PATH | OFlags::CLOEXEC, Mode::empty()).map_err(io::Error::from)
}

/// The container-process side (called from the forked child `create`
/// sets up, after `pivot_root` — see [`open_path`]): block until
/// `start` opens the read end, then write one byte to confirm
/// readiness and unblock `start`'s own read.
pub fn wait_for_start(path_fd: &OwnedFd) -> io::Result<()> {
    let reopen_path = format!("/proc/self/fd/{}", path_fd.as_raw_fd());
    let mut file = std::fs::OpenOptions::new().write(true).open(reopen_path)?;
    file.write_all(&[0])?;
    Ok(())
}

/// The `start` side: unblock the container process by opening the read
/// end (which is what lets its blocked write-open above proceed), then
/// read the one confirmation byte. Runs on the host, outside any
/// container mount namespace, so a plain by-path open is fine here.
pub fn signal_start(path: &Path) -> io::Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut buf = [0u8; 1];
    let n = file.read(&mut buf)?;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "exec fifo closed without the container process signalling readiness \
             (it may have died during create)",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn signal_start_unblocks_wait_for_start_and_exchanges_one_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILENAME);
        create(&path).unwrap();
        let path_fd = open_path(&path).unwrap();

        let (tx, rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            wait_for_start(&path_fd).unwrap();
            tx.send(()).unwrap();
        });

        // Give the writer thread a moment to actually reach its
        // blocking open before we unblock it — not required for
        // correctness (signal_start would just block until it does
        // either way), only to make a hang here more obviously "the
        // writer never got there" than an unrelated flake.
        std::thread::sleep(Duration::from_millis(20));
        signal_start(&path).unwrap();

        rx.recv_timeout(Duration::from_secs(5))
            .expect("wait_for_start should have unblocked and completed");
        writer.join().unwrap();
    }

    #[test]
    fn open_path_survives_the_original_path_disappearing() {
        // Simulates the `pivot_root` scenario this exists for: once
        // `open_path` has a reference, removing the original directory
        // entry (a real container's `pivot_root` doesn't remove
        // anything, but it does make the original *path* unreachable —
        // functionally the same problem this fd-based reopen sidesteps)
        // doesn't stop `wait_for_start` from reaching the same
        // underlying fifo via `/proc/self/fd/<n>`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILENAME);
        create(&path).unwrap();
        let path_fd = open_path(&path).unwrap();
        // The fd table is process- (not thread-)wide, so this raw
        // number is still valid to reopen via `/proc/self/fd` from any
        // thread, including this one, after handing the `OwnedFd`
        // itself off to the writer thread below.
        let raw_fd = path_fd.as_raw_fd();
        std::fs::remove_file(&path).unwrap();

        let (tx, rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            wait_for_start(&path_fd).unwrap();
            tx.send(()).unwrap();
        });

        std::thread::sleep(Duration::from_millis(20));
        let mut reopened = std::fs::File::open(format!("/proc/self/fd/{raw_fd}")).unwrap();
        let mut buf = [0u8; 1];
        reopened.read_exact(&mut buf).unwrap();

        rx.recv_timeout(Duration::from_secs(5))
            .expect("wait_for_start should have unblocked and completed");
        writer.join().unwrap();
    }
}
