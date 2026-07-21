//! Wiring `oci_runtime_core::overlay`'s own feasibility probe (0108)
//! and `oci_store::rootfs_cache`'s own per-manifest-digest extraction
//! cache (0109) into `ociman run`'s own real container startup — the
//! actual fix for the gap 0107 documented (real `podman run` faster
//! than `ociman run` for a real multi-thousand-file image, since
//! `ociman` fully re-extracts every layer's own files from scratch on
//! every single invocation).
//!
//! # The shape: one ordinary overlay entry in the bundle's own spec
//!
//! No change to `oci_runtime_core` at all was needed for this: its
//! own mount-application code (`oci_runtime_core::rootfs::
//! plan_rootfs_setup`/`launch::execute_rootfs_action`) already
//! applies an arbitrary `spec.mounts` entry generically, with no
//! fstype allowlist. A container using this module's own
//! [`RootlessOverlayRootfs`] gets exactly one extra entry
//! (`destination: "/"`, `type: "overlay"`, `lowerdir=`/`upperdir=`/
//! `workdir=` options) prepended to its own `spec.mounts`, and its own
//! per-container `rootfs/` directory is left **empty** — the overlay
//! mount itself, applied inside the container's own already-existing
//! fresh mount namespace, is what actually populates it, entirely
//! read-only from the shared cache below a private, per-container
//! writable layer.
//!
//! # Falling back is always safe, and always the same code either way
//!
//! [`decide`] never *asserts* overlay support — it asks 0108's own
//! probe (cached across invocations, see [`rootless_overlay_supported_
//! cached`]'s own doc comment for why re-probing every single `ociman
//! run` would itself be a real, measurable regression), and any
//! failure building the cache or preparing per-container directories
//! degrades to [`RootfsSetup::Extract`] — the exact same "extract
//! every layer directly into this container's own `rootfs/`
//! directory" code `ociman run` has always used, byte-for-byte
//! unchanged. A container never partially commits to the overlay path
//! and then fails partway through; [`decide`] either returns a fully
//! prepared overlay plan or falls all the way back, before
//! `cmd_run`'s own single call site ever has to choose between two
//! different further code paths of its own.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use oci_spec_types::Digest;
use oci_spec_types::image::ImageManifest;
use oci_spec_types::runtime::Mount;
use oci_store::Store;

/// How a container's own `rootfs/` directory gets populated —
/// [`decide`]'s own result.
pub enum RootfsSetup {
    /// The real, current, always-correct fallback: every layer gets
    /// extracted directly into the container's own `rootfs/`
    /// directory, exactly like every `ociman run` before this module
    /// existed. `user_resolve_root` is always the same as the
    /// container's own `rootfs/` directory in this case.
    Extract,
    /// A real rootless overlay mount will populate the container's
    /// own `rootfs/` directory instead — nothing needs to be
    /// extracted into it at all. `mount` must be prepended (not
    /// appended) to the bundle's own `spec.mounts`, ahead of every
    /// other entry, since those (`/proc`, `/dev`, ...) are all
    /// subdirectories of the root this mount itself provides.
    Overlay {
        /// The overlay `spec.mounts` entry itself.
        mount: Mount,
        /// Where to resolve the container's own declared `USER`
        /// against (`resolve_user`'s own root argument) — the
        /// *cache* directory, not the (still-empty, until the
        /// container actually starts) `rootfs/` directory itself:
        /// this runs on the host, before the container — and
        /// therefore its own overlay mount — exists at all, but the
        /// cache already holds byte-identical content to what the
        /// merged view will show (nothing has written to the
        /// container's own private upper layer yet).
        user_resolve_root: PathBuf,
    },
}

/// Decide (and, for the overlay case, fully prepare — building/
/// reusing the rootfs cache, creating this container's own private
/// `upper`/`work` directories) how `bundle_dir`'s own `rootfs/`
/// directory should be populated for a container of `manifest`
/// (digest `manifest_digest`, already ingested into `store`).
///
/// Never fails outright: any real problem past the initial support
/// check (building the cache, creating the container's own upper/
/// work directories) is logged (`tracing::warn!`) and treated as "use
/// [`RootfsSetup::Extract`]" instead, exactly like `overlay_supported`
/// being `false` from the start would — a real, current environment
/// this ran successfully in earlier could still hit a real, transient
/// problem (disk full building the cache, ...) on any single
/// invocation, and that should degrade gracefully, not fail a
/// container a moment before this feature existed would have started
/// successfully.
pub fn decide(
    store: &Store,
    bundle_dir: &Path,
    manifest_digest: &Digest,
    manifest: &ImageManifest,
) -> RootfsSetup {
    if !rootless_overlay_supported_cached(store.root()) {
        return RootfsSetup::Extract;
    }
    match prepare_overlay(store, bundle_dir, manifest_digest, manifest) {
        Ok(setup) => setup,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "rootless overlay rootfs setup failed; falling back to a direct layer extraction"
            );
            RootfsSetup::Extract
        }
    }
}

fn prepare_overlay(
    store: &Store,
    bundle_dir: &Path,
    manifest_digest: &Digest,
    manifest: &ImageManifest,
) -> anyhow::Result<RootfsSetup> {
    let cache_root = store.root().join("rootfs-cache");
    let cache_dir = oci_store::ensure_cached(store, &cache_root, manifest_digest, manifest)
        .context("building/reusing the rootfs cache")?;

    let upper = bundle_dir.join("upper");
    let work = bundle_dir.join("work");
    std::fs::create_dir_all(&upper).with_context(|| format!("creating {}", upper.display()))?;
    std::fs::create_dir_all(&work).with_context(|| format!("creating {}", work.display()))?;

    let options = vec![
        format!("lowerdir={}", cache_dir.display()),
        format!("upperdir={}", upper.display()),
        format!("workdir={}", work.display()),
    ];
    let mount = Mount {
        destination: "/".to_string(),
        source: Some("overlay".to_string()),
        kind: Some("overlay".to_string()),
        options,
    };

    Ok(RootfsSetup::Overlay {
        mount,
        user_resolve_root: cache_dir,
    })
}

/// [`oci_runtime_core::overlay::rootless_overlay_supported`], but
/// probed at most once per `storage_root` rather than once per
/// `ociman run` invocation: the real probe forks and does a real
/// `unshare(CLONE_NEWUSER|CLONE_NEWNS)` + mount cycle, real,
/// measurable cost (comparable to a large fraction of a lightweight
/// container's own total startup time, per `docs/design/0105`'s own
/// numbers) that would otherwise be paid on *every single* `ociman
/// run`, including — worse — on every invocation in an environment
/// that turns out *not* to support it, where paying that cost would
/// be a pure, ongoing regression with no matching benefit at all. The
/// real environment this answer depends on (kernel, MAC policy)
/// essentially never changes between two `ociman run` invocations
/// against the same storage root in practice, so a plain persisted
/// marker file is the right trade — the same kind of "detect once,
/// remember the answer" real container engines themselves already do
/// for analogous capability checks.
fn rootless_overlay_supported_cached(storage_root: &Path) -> bool {
    let marker = storage_root.join(".rootless-overlay-supported");
    read_or_compute_cached_bool(&marker, || {
        let scratch = storage_root.join(".rootless-overlay-probe");
        oci_runtime_core::overlay::rootless_overlay_supported(&scratch)
    })
}

/// The cacheable part of [`rootless_overlay_supported_cached`],
/// factored out so it has a real, direct unit test of its own without
/// ever touching the actual (fork+`unshare`-based, unsafe to call from
/// a plain `#[test]` — see `oci_runtime_core::overlay`'s own doc
/// comment) probe: reads `true`/`false` from `marker` if it already
/// holds one of those two exact strings, otherwise calls `compute`
/// once and persists whichever result it returns (tolerating a write
/// failure silently — worst case, a later invocation just probes
/// again, no different from `marker` never having existed at all).
fn read_or_compute_cached_bool(marker: &Path, compute: impl FnOnce() -> bool) -> bool {
    if let Ok(content) = std::fs::read_to_string(marker) {
        match content.trim() {
            "true" => return true,
            "false" => return false,
            _ => {} // Unrecognized/corrupt content -- fall through and re-probe.
        }
    }
    let value = compute();
    let _ = std::fs::write(marker, if value { "true" } else { "false" });
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn computes_and_persists_on_first_use() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let called = Cell::new(false);

        let result = read_or_compute_cached_bool(&marker, || {
            called.set(true);
            true
        });

        assert!(result);
        assert!(called.get());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "true");
    }

    #[test]
    fn reuses_an_already_cached_true_without_recomputing() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        std::fs::write(&marker, "true").unwrap();
        let called = Cell::new(false);

        let result = read_or_compute_cached_bool(&marker, || {
            called.set(true);
            false // would prove a recompute happened, if it did
        });

        assert!(result);
        assert!(!called.get(), "a cached value must not be recomputed");
    }

    #[test]
    fn reuses_an_already_cached_false_without_recomputing() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        std::fs::write(&marker, "false").unwrap();
        let called = Cell::new(false);

        let result = read_or_compute_cached_bool(&marker, || {
            called.set(true);
            true
        });

        assert!(!result);
        assert!(!called.get(), "a cached value must not be recomputed");
    }

    #[test]
    fn recomputes_on_corrupt_marker_content() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        std::fs::write(&marker, "not a real boolean").unwrap();
        let called = Cell::new(false);

        let result = read_or_compute_cached_bool(&marker, || {
            called.set(true);
            true
        });

        assert!(result);
        assert!(called.get());
    }

    #[test]
    fn tolerates_a_marker_directory_it_cannot_write_to() {
        // `marker`'s own parent doesn't exist, so both the read and
        // the write fail -- `compute`'s own result must still be
        // returned rather than panicking.
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("no/such/parent/marker");

        let result = read_or_compute_cached_bool(&marker, || true);

        assert!(result);
    }
}
