//! Applying `process.oomScoreAdj` (a real, immediate `/proc/self/
//! oom_score_adj` write) before a container's own init process
//! `exec`s — matching real `crun`'s own `libcrun_set_oom` exactly
//! (`~/git/crun/src/libcrun/linux.c:4447-4467`): only ever called from
//! its own two container-creation-time call sites, never from `crun
//! exec`'s own path at all (checked directly, no call from
//! `container.c`'s own exec handling) — this project's own
//! counterpart is likewise only ever wired into [`crate::launch`],
//! never [`crate::exec`].
//!
//! No name-table lookup needed (unlike [`crate::rlimits`]): the
//! runtime-spec's own `oomScoreAdj` is already a plain integer, not a
//! symbolic name.

use std::io;
use std::path::Path;

/// Write `value` to `<proc_root>/self/oom_score_adj`, a real,
/// unprivileged-when-increasing (root-required-when-decreasing below
/// the process's own current value) per-process OOM-killer heuristic
/// adjustment — no-op when `value` is `None` (the common,
/// unconfigured case: this project's own containers get whatever
/// `oom_score_adj` they inherited from their own parent, exactly like
/// every other process on the system, matching real `crun`'s own
/// identical `oom_score_adj_present` guard).
///
/// A real, out-of-kernel-range value (outside `-1000..=1000`) is a
/// real `EINVAL` from the kernel itself, surfaced here as an ordinary
/// `io::Error` — the same "let the kernel's own rejection speak for
/// itself" precedent real `crun` already follows (no client-side range
/// pre-validation of its own either, checked directly).
pub fn apply(proc_root: &Path, value: Option<i32>) -> io::Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    std::fs::write(proc_root.join("self/oom_score_adj"), value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_a_real_no_op_that_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        // No `self/` directory created at all -- if this tried to
        // write anything, it would fail with a real `NotFound` rather
        // than silently succeeding.
        assert!(apply(dir.path(), None).is_ok());
    }

    #[test]
    fn some_writes_the_real_decimal_value() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("self")).unwrap();
        apply(dir.path(), Some(-500)).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("self/oom_score_adj")).unwrap(),
            "-500"
        );
    }

    #[test]
    fn a_missing_proc_self_surfaces_a_real_io_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let err = apply(dir.path(), Some(0)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
