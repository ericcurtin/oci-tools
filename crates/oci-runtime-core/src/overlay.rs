//! Feasibility probe for a real, unprivileged (rootless) overlayfs
//! mount — the piece 0107's own "what this doesn't fix, and why not
//! attempted here" section named as this project's own next real
//! priority for closing the measured `ociman run` performance gap
//! against real `podman` on a large, real-world image (see
//! `docs/design/0107`/`0108`).
//!
//! **This module started as a pure, read-only capability check with
//! nothing calling it yet** — matching this project's own long-
//! established pattern of landing a groundwork primitive as its own
//! increment before a later one wires it in (e.g. 0039-0049's parser/
//! diff/export/commit primitives, wired into `ociman build` for the
//! first time only at 0050). That wiring landed at 0110: `ociman`'s
//! own `rootfs_setup::rootless_overlay_supported_cached` (cached
//! across invocations, since a real `fork`+`unshare` probe isn't free)
//! calls this exact function to decide whether a given `ociman run`
//! gets the overlay-rootfs optimization at all — see that function's
//! own doc comment for the caching layer, and `docs/design/0110` for
//! how it's wired into `ociman run`'s own image-based rootfs setup.
//!
//! # What it checks, and why a real probe rather than a kernel-version guess
//!
//! Whether the current environment can really do, inside the exact
//! same rootless user+mount namespace shape every container this
//! project creates already uses (`unshare(CLONE_NEWUSER|CLONE_NEWNS)`
//! then a self `uid_map`/`gid_map`, see [`crate::namespaces`]):
//!
//! ```text
//! mount("overlay", merged, "overlay", 0, "lowerdir=<L>,upperdir=<U>,workdir=<W>")
//! ```
//!
//! Real, current kernels (verified directly this session: a real
//! `unshare --user --map-root-user --mount` shell test on this
//! session's own dev host — kernel 6.17 — successfully mounted a real
//! overlay this way, and a write through the merged view landed in
//! `upperdir` without ever touching `lowerdir`'s own content, exactly
//! the copy-on-write semantics this would rely on) generally allow an
//! unprivileged user namespace to mount overlayfs, but this is *not*
//! guaranteed the way plain user-namespace creation itself is close to
//! being: real-world mandatory-access-control policies have
//! historically restricted it separately (Ubuntu's own AppArmor, for
//! instance, gates unprivileged overlay mounts more aggressively than
//! it gates unprivileged user namespaces generally — the same kind of
//! distro-specific hardening `docs/design/0012`'s own "A real distro-
//! hardening gap" section already found for plain user namespaces).
//! Only actually trying it, on the exact real environment this process
//! is running in, is trustworthy — a kernel-version check alone would
//! give a false "yes" on a kernel new enough but a distro/policy that
//! still says no.
//!
//! # Why no automated syscall test in this crate
//!
//! Same reason as [`crate::namespaces`] and [`crate::launch`] (which
//! has none of its own either, for identical reasons — see that
//! module's own established precedent): `unshare(2)` with
//! `CLONE_NEWUSER` fails with `EINVAL` when the calling *process* has
//! more than one thread, and `cargo test`'s own harness runs every
//! test on its own spawned thread even filtered down to one test —
//! multi-threaded from this probe's own point of view regardless of
//! which single test happens to call it. [`rootless_overlay_supported`]
//! itself forks first (a forked child is always single-threaded,
//! regardless of the parent's own thread count — the same reasoning
//! [`crate::process::fork_and_wait`]'s own doc comment already
//! establishes), which handles the kernel's own `unshare(2)`
//! precondition correctly either way — but calling *into* that fork
//! from a thread of an already-multithreaded test binary still risks
//! the separate, real hazard [`crate::process`]'s own module doc
//! describes (a lock held by a *different* thread of the parent at the
//! moment of `fork()` staying locked forever in the child). Testing
//! this for real needs a genuinely fresh, single-threaded process — a
//! freshly exec'd binary, the same shape every other fork+`unshare`
//! path in this crate already requires; no dedicated end-to-end test
//! of this exact function exists in *this* crate for that reason, but
//! `ociman run` (0110) exercises it for real on every invocation
//! through `ociman`'s own freshly-exec'd process, and `tests/tests/
//! ociman_run.rs`'s own real, running-binary tests cover the result
//! indirectly (a `.rootless-overlay-supported` marker file lets tests
//! force the answer either way rather than depend on this probe's own
//! real, environment-dependent result).

use std::path::Path;

use oci_spec_types::runtime::LinuxIdMapping;
use rustix::thread::UnshareFlags;

use crate::{namespaces, process};

/// Whether this environment can really mount a real, unprivileged
/// overlayfs inside a rootless user+mount namespace — see this
/// module's own doc comment for exactly what's tried and why a live
/// probe, not a version guess.
///
/// `scratch_dir` must be a path this call can freely create (and, by
/// the time this returns, has already removed) a handful of temporary
/// subdirectories under — it does not need to exist yet, but its
/// parent must. Every mount this probe creates lives entirely inside
/// the forked child's own private mount namespace, torn down
/// automatically by the kernel the instant that child exits, so no
/// explicit `umount(2)` of its own is ever needed; only the plain
/// scratch directories themselves need removing afterward, which this
/// function always does before returning, regardless of the real
/// result.
pub fn rootless_overlay_supported(scratch_dir: &Path) -> bool {
    let lower = scratch_dir.join("lower");
    let upper = scratch_dir.join("upper");
    let work = scratch_dir.join("work");
    let merged = scratch_dir.join("merged");

    let dirs_ready = [&lower, &upper, &work, &merged]
        .into_iter()
        .all(|dir| std::fs::create_dir_all(dir).is_ok());

    let result = dirs_ready && probe_in_a_fresh_namespace(&lower, &upper, &work, &merged);

    // A real overlay mount's own kernel-internal bookkeeping locks
    // its own `workdir/work` subdirectory down to mode `0000` (caught
    // directly: a first pass at this cleanup left exactly that behind,
    // `find`/`remove_dir_all` both refusing it with a real `EACCES`
    // even though this same process owns it) — restoring a normal,
    // traversable mode everywhere under `scratch_dir` first, tolerantly
    // (this is a best-effort cleanup step, not something worth failing
    // the whole probe's own real, already-computed `result` over),
    // makes the removal below actually able to reach every entry.
    reset_permissions_for_removal(scratch_dir);
    let _ = std::fs::remove_dir_all(scratch_dir);
    result
}

/// Best-effort `chmod 0700` every directory under (and including)
/// `root`, deepest-first, so a subsequent `remove_dir_all` can always
/// traverse and unlink everything regardless of what a real overlay
/// mount's own kernel bookkeeping left behind. Errors are silently
/// tolerated throughout: this is a cleanup nicety on top of an
/// already-computed real result, not something worth surfacing a
/// failure for on its own.
///
/// Used by [`rootless_overlay_supported`]'s own probe cleanup, and by
/// [`crate::state::StateStore::remove`] as a fallback when removing a
/// real container's own directory fails outright: a container whose
/// own rootfs used a real overlay mount (`ociman run`'s own
/// `rootfs_setup` module, `docs/design/0110`) leaves exactly the same
/// locked-down `workdir/work` subdirectory behind in its own bundle
/// directory once it exits, for the identical kernel-level reason —
/// caught directly (not assumed to generalize) by that crate's own
/// real `ociman rm`/`--rm` integration test failing with a real
/// `EACCES`/`EPERM` the moment a container actually using the overlay
/// path was removed for the first time.
pub fn reset_permissions_for_removal(root: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    // Fixed up *before* attempting to list `root`'s own children, not
    // after: a directory locked to mode `0000` (exactly what a real
    // overlay `workdir` leaves behind) can't be `read_dir`'d at all
    // until its own mode is restored first — fixing it only after
    // recursing would never actually reach that point.
    let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700));

    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            reset_permissions_for_removal(&entry.path());
        }
    }
}

/// Fork, then attempt the real probe sequence in the child (see this
/// module's own doc comment for why a fork is required here at all,
/// beyond the usual "don't corrupt the calling process's own
/// namespaces" reasoning: `unshare(CLONE_NEWUSER)` is otherwise
/// one-way for the calling process).
fn probe_in_a_fresh_namespace(lower: &Path, upper: &Path, work: &Path, merged: &Path) -> bool {
    // SAFETY: `probe_child`'s own body (below) only performs plain,
    // ordinary filesystem/syscall operations and always ends by
    // calling `std::process::exit` — matching `fork_and_wait`'s own
    // documented contract for `child_body`. This crate's own binaries
    // call into container-creation code paths with the identical
    // shape (fork now, `unshare(NEWUSER)` in the child) from a
    // process that is single-threaded at the point it matters (its
    // own `main`, before spawning anything else) — the same
    // precondition applies to any future caller of this function.
    #[allow(unsafe_code)]
    let status = unsafe {
        process::fork_and_wait(|| {
            let ok = probe_child(lower, upper, work, merged);
            std::process::exit(if ok { 0 } else { 1 });
        })
    };
    matches!(status, Ok(status) if process::exit_code_from_wait_status(status) == 0)
}

/// The forked child's own body: `unshare` a fresh user+mount
/// namespace, map the calling user to root inside it (the same
/// `write_id_mappings` self-mapping every real rootless container
/// this project creates already uses), then attempt the real overlay
/// mount. Returns whether every step succeeded.
fn probe_child(lower: &Path, upper: &Path, work: &Path, merged: &Path) -> bool {
    // Read *before* `unshare(CLONE_NEWUSER)`, not after: the mapping
    // `write_id_mappings` needs is expressed in terms of the *parent*
    // (pre-unshare) namespace's own uid/gid — reading it after
    // unsharing would instead see the fresh namespace's own unmapped
    // "overflow" id (`65534`), producing a mapping that maps
    // container-side root to itself, not to this process's own real
    // caller — a real bug this session's own manual verification
    // caught directly (`write_id_mappings` failing with a genuine
    // `EPERM`, not a hypothetical ordering concern).
    //
    // SAFETY: `geteuid`/`getegid` take no arguments and can't fail —
    // always sound.
    #[allow(unsafe_code)]
    let (euid, egid) = unsafe { (libc::geteuid(), libc::getegid()) };

    if namespaces::unshare(UnshareFlags::NEWUSER | UnshareFlags::NEWNS).is_err() {
        return false;
    }

    let uid_mappings = [LinuxIdMapping {
        container_id: 0,
        host_id: euid,
        size: 1,
    }];
    let gid_mappings = [LinuxIdMapping {
        container_id: 0,
        host_id: egid,
        size: 1,
    }];
    if namespaces::write_id_mappings(Path::new("/proc"), "self", &uid_mappings, &gid_mappings)
        .is_err()
    {
        return false;
    }

    let options = vec![
        format!("lowerdir={}", lower.display()),
        format!("upperdir={}", upper.display()),
        format!("workdir={}", work.display()),
    ];
    let parsed = oci_mount::parse_mount_options(&options);
    oci_mount::mount(Some("overlay"), merged, Some("overlay"), &parsed).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one part of this module safely testable from a plain
    /// `#[test]` (see this module's own top doc comment for why the
    /// real, happy-path probe isn't): a `scratch_dir` whose own
    /// subdirectories can't actually be created at all never reaches
    /// the fork/`unshare` step, so it's exercised the ordinary way —
    /// a regular file sitting where one of the four scratch
    /// subdirectories needs to go makes `create_dir_all` fail with a
    /// real `ENOTDIR`, not a hypothetical one.
    #[test]
    fn a_scratch_dir_whose_subdirectories_cannot_be_created_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        // Block "lower" (the first of the four) with a plain file.
        std::fs::write(dir.path().join("lower"), b"not a directory").unwrap();

        assert!(!rootless_overlay_supported(dir.path()));
    }

    /// Whatever the real result, the scratch directory itself is
    /// always cleaned up — checked directly (not just implied by the
    /// function returning at all), since a leaked scratch directory
    /// per `ociman run` invocation would be exactly the kind of
    /// slow-accumulating-cruft bug this project's own "ensure we don't
    /// run out of disk space" standard cares about.
    #[test]
    fn the_scratch_directory_is_always_removed_regardless_of_the_result() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();

        let _ = rootless_overlay_supported(&scratch);

        assert!(!scratch.exists());
    }
}
