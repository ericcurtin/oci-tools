//! A per-image-manifest-digest "golden" rootfs cache: extracting an
//! image's own real layer stack (`oci_layer::apply`, one call per
//! layer) is real, measurable work (see `docs/design/0106`'s own
//! `strace`-measured syscall counts) that's *identical* for every
//! container of the same image — before this module existed, `ociman
//! run` paid that cost fresh on every single invocation. Since
//! `docs/design/0110`, `ociman run` uses this cache as a real
//! overlay mount's own `lowerdir` (a cached, already-extracted rootfs
//! is only ever *safely* shared read-only this way — sharing it via a
//! plain recursive copy would still pay the same per-file write cost
//! this module's own whole point is to avoid paying more than once,
//! and sharing it via hardlinks would let a write inside *any* one
//! container silently corrupt the shared cache for *every other*
//! container of the same image, since a hardlink is the same
//! underlying inode, not an independent copy).
//!
//! [`prune`] closes the other half of the lifecycle this cache's own
//! existence introduces: every distinct manifest digest `ociman run`
//! has ever used leaves a real, uncompressed-on-disk cache entry
//! behind forever otherwise, unbounded disk growth this project's own
//! "ensure we don't run out of disk space" standard cares about
//! directly — see its own doc comment for how it decides what's still
//! needed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use oci_spec_types::Digest;
use oci_spec_types::image::Descriptor;

use crate::{Store, StoreError};

/// The deterministic cache directory for `manifest_digest`, under
/// `cache_root` — content-addressed the same way this crate's own
/// blob storage is: the exact same manifest digest always means the
/// exact same layer stack, so there is no separate invalidation
/// concept needed at all (an image never mutates in place; a new
/// build/pull that changes anything gets a new digest, and therefore
/// a different, independent cache directory).
pub fn cache_dir_for(cache_root: &Path, manifest_digest: &Digest) -> PathBuf {
    cache_root.join(manifest_digest.hex())
}

/// Ensure a fully-extracted rootfs exists at
/// [`cache_dir_for`]`(cache_root, manifest_digest)`, building it (via
/// the same per-layer `oci_layer::apply` sequence `ociman run`'s own
/// current, uncached extraction already uses) if this is the first
/// time this exact manifest digest has ever been needed. Returns the
/// cache directory's own path either way.
///
/// **Concurrency-safe by construction, not by locking**: a real build
/// happens entirely inside a fresh, randomly-named temporary directory
/// (under `cache_root`, guaranteeing the final `rename` below is a
/// same-filesystem, atomic directory move — never a slow cross-device
/// copy), which only ever becomes visible at the real cache path via
/// one atomic `rename(2)`. If a second, concurrent caller loses that
/// race (some *other* caller's own `rename` won first), its own
/// now-redundant temporary build is simply discarded — cheap relative
/// to the alternative (a lock file, and everything that can go wrong
/// holding one across a potentially-slow real extraction) precisely
/// because losing the race is harmless: the winner's own result is
/// byte-for-byte identical (the same manifest digest can only ever
/// mean the same real layer content).
pub fn ensure_cached(
    store: &Store,
    cache_root: &Path,
    manifest_digest: &Digest,
    layers: &[Descriptor],
) -> Result<PathBuf, StoreError> {
    let dest = cache_dir_for(cache_root, manifest_digest);
    if dest.is_dir() {
        return Ok(dest);
    }

    std::fs::create_dir_all(cache_root)?;
    let build_dir = tempfile::tempdir_in(cache_root)?;

    for layer in layers {
        let compression =
            oci_layer::compression_for_media_type(&layer.media_type).ok_or_else(|| {
                StoreError::UnsupportedLayerMediaType {
                    media_type: layer.media_type.clone(),
                }
            })?;
        let blob = store.open_blob(&layer.digest)?;
        oci_layer::apply(blob, compression, build_dir.path())
            .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
    }

    match std::fs::rename(build_dir.path(), &dest) {
        Ok(()) => {
            // `build_dir`'s own `Drop` would otherwise try to
            // `remove_dir_all` a path that no longer exists at all
            // (this `rename` just moved it to `dest`) — harmless
            // either way (`tempfile::TempDir`'s own `Drop` already
            // silently discards a cleanup failure), but `mem::forget`
            // makes that explicit rather than relying on an
            // implementation detail of a dependency.
            std::mem::forget(build_dir);
        }
        Err(_) if dest.is_dir() => {
            // Lost a real race to a concurrent caller building the
            // exact same cache entry — its own result is already
            // there, byte-for-byte identical to what this attempt
            // would have produced (same manifest digest, same real
            // layer content). `build_dir`'s own `Drop` cleans up this
            // now-redundant attempt.
        }
        Err(e) => return Err(e.into()),
    }

    Ok(dest)
}

/// Cache entries removed, and bytes reclaimed, by a [`prune`] run —
/// the same shape [`crate::GcReport`] already established for this
/// crate's own blob garbage collection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CachePruneReport {
    /// Manifest digests whose own cache entry was removed.
    pub removed: Vec<Digest>,
    /// Total bytes reclaimed.
    pub reclaimed_bytes: u64,
}

/// Remove every cache entry under `cache_root` whose own manifest
/// digest no longer resolves to *any* image reference in `store` —
/// mark-and-sweep, the same real approach [`crate::Store::gc`] already
/// uses for blob storage, just against a much smaller "reachable" set
/// (this cache is keyed directly by manifest digest, so "reachable"
/// here is simply the digest half of every [`crate::ImageRecord`]
/// [`crate::Store::list_images`] returns — no manifest/config/layer
/// graph walk needed the way blob reachability requires, since this
/// cache has no equivalent of a shared layer blob two different
/// manifests might both still need).
///
/// A build already in progress (a real `ensure_cached` call's own
/// scratch `tempfile::tempdir_in` directory, not yet renamed into
/// place) is recognized by its own leading `.tmp` prefix — the same
/// convention [`crate::Store::gc`] itself already established for the
/// identical concern on the blob side — and left alone rather than
/// treated as an orphaned entry.
pub fn prune(store: &Store, cache_root: &Path) -> Result<CachePruneReport, StoreError> {
    let reachable: HashSet<String> = store
        .list_images()?
        .into_iter()
        .map(|record| record.manifest_digest.hex().to_string())
        .collect();

    let mut report = CachePruneReport::default();
    let entries = match std::fs::read_dir(cache_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(hex) = file_name.to_str() else {
            continue;
        };
        if hex.starts_with(".tmp") || reachable.contains(hex) {
            continue;
        }
        let path = entry.path();
        let size = dir_size(&path).unwrap_or(0);
        std::fs::remove_dir_all(&path)?;
        report.reclaimed_bytes += size;
        if let Ok(digest) = Digest::parse(&format!("sha256:{hex}")) {
            report.removed.push(digest);
        }
    }
    Ok(report)
}

/// Total size in bytes of every regular file under `dir`, recursively
/// — [`prune`]'s own "how much did removing this cache entry actually
/// reclaim" figure.
///
/// **Counts each real inode once, not once per directory entry.** A
/// real cache entry for a hardlink-heavy image (`docs/design/0106`'s
/// own busybox example: every applet a separate hardlink to one real
/// `busybox` binary) would otherwise have that one binary's own size
/// added again for every single hardlinked name pointing at it —
/// caught directly, not assumed: a first pass at this function
/// reported reclaiming ~490 MB for a real cache entry whose own
/// actual on-disk usage (confirmed with `du` before and after a real
/// `ociman prune`) was a few MB, exactly this over-count. `seen`
/// tracks `(dev, ino)` pairs (`std::os::unix::fs::MetadataExt`) so a
/// later hardlink to an already-counted inode contributes nothing
/// further, matching what real disk usage actually recovers.
///
/// Best-effort otherwise: a file that vanishes between being listed
/// and `stat`ed (a real, if rare, race with a concurrent removal of
/// the very entry [`prune`] itself is about to remove) is simply
/// skipped rather than failing the whole size calculation — this
/// figure is already advisory, not something anything else depends on
/// for correctness.
/// A directory's own real, on-disk size — every regular file's own
/// byte length, symlinks never followed (a symlink's own inode is a
/// handful of bytes for the link text itself, not its target's size),
/// and a hardlinked file (this project's own known-common real shape
/// for a base image's own applet layout, `docs/design/0106`) counted
/// exactly once no matter how many names point at the same real
/// inode (`(dev, ino)`-deduplicated) — the exact same real bug
/// `docs/design/0111` found and fixed for [`prune`]'s own reporting.
/// `pub`, not just crate-private, so `ociman prune`'s own build-
/// scratch sweep (`docs/design/0121`) can reuse this directly rather
/// than risk reintroducing that same hardlink-double-counting bug in
/// a second, independent implementation.
pub fn dir_size(dir: &Path) -> std::io::Result<u64> {
    let mut seen = HashSet::new();
    dir_size_inner(dir, &mut seen)
}

fn dir_size_inner(dir: &Path, seen: &mut HashSet<(u64, u64)>) -> std::io::Result<u64> {
    use std::os::unix::fs::MetadataExt as _;

    let mut total = 0u64;
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            total += dir_size_inner(&entry.path(), seen).unwrap_or(0);
        } else if let Ok(metadata) = entry.metadata() {
            // A symlink's own `metadata()` (not `symlink_metadata()`)
            // would follow it and double-count (or reach entirely
            // outside `dir`) the target -- skip symlinks' own size
            // entirely, matching how disk usage really works (a
            // symlink's own inode is a handful of bytes for the
            // link text itself, not its target's size).
            if file_type.is_symlink() {
                continue;
            }
            if seen.insert((metadata.dev(), metadata.ino())) {
                total += metadata.len();
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_spec_types::digest::sha256;
    use oci_spec_types::image::{Descriptor, ImageManifest, MEDIA_TYPE_IMAGE_LAYER_GZIP};
    use std::collections::BTreeMap;

    fn temp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        (dir, store)
    }

    /// A real, minimal single-layer manifest whose one layer is a
    /// real gzip-compressed tar containing one file — ingested into
    /// `store` directly, the same offline approach every other test
    /// in this workspace uses for a "real, structurally valid" image
    /// without a real registry pull.
    fn seed_one_layer_manifest(store: &Store, file_name: &str, content: &[u8]) -> ImageManifest {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(content.len() as u64);
        builder
            .append_data(&mut header, file_name, content)
            .unwrap();
        let tar_bytes = builder.into_inner().unwrap();

        let mut compressed = Vec::new();
        let mut encoder =
            flate2::write::GzEncoder::new(&mut compressed, flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &tar_bytes).unwrap();
        encoder.finish().unwrap();

        let ingested = store.ingest(compressed.as_slice()).unwrap();
        ImageManifest {
            schema_version: 2,
            media_type: None,
            config: Descriptor {
                media_type: "application/vnd.oci.image.config.v1+json".to_string(),
                digest: sha256(b"unused-in-this-test-config"),
                size: 0,
                urls: vec![],
                annotations: BTreeMap::new(),
                platform: None,
            },
            layers: vec![Descriptor {
                media_type: MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
                digest: ingested.digest,
                size: ingested.size,
                urls: vec![],
                annotations: BTreeMap::new(),
                platform: None,
            }],
            annotations: BTreeMap::new(),
        }
    }

    #[test]
    fn ensure_cached_builds_a_real_extracted_rootfs_on_first_use() {
        let (_dir, store) = temp_store();
        let cache_root = tempfile::tempdir().unwrap();
        let manifest = seed_one_layer_manifest(&store, "hello.txt", b"hello cache");
        let digest = sha256(b"fake-manifest-digest-one");

        let cache_dir =
            ensure_cached(&store, cache_root.path(), &digest, &manifest.layers).unwrap();

        assert_eq!(cache_dir, cache_dir_for(cache_root.path(), &digest));
        assert_eq!(
            std::fs::read(cache_dir.join("hello.txt")).unwrap(),
            b"hello cache"
        );
    }

    #[test]
    fn ensure_cached_reuses_an_already_built_cache_without_rebuilding() {
        let (_dir, store) = temp_store();
        let cache_root = tempfile::tempdir().unwrap();
        let manifest = seed_one_layer_manifest(&store, "hello.txt", b"first build");
        let digest = sha256(b"fake-manifest-digest-two");

        let first = ensure_cached(&store, cache_root.path(), &digest, &manifest.layers).unwrap();

        // Mutate the cache directly (something only a real second
        // *build* would ever undo) -- if `ensure_cached` rebuilt
        // instead of reusing, this change would be gone.
        std::fs::write(first.join("hello.txt"), b"mutated by the test").unwrap();

        let second = ensure_cached(&store, cache_root.path(), &digest, &manifest.layers).unwrap();

        assert_eq!(first, second);
        assert_eq!(
            std::fs::read(second.join("hello.txt")).unwrap(),
            b"mutated by the test",
            "a cache hit must not rebuild/overwrite the existing directory"
        );
    }

    #[test]
    fn different_manifest_digests_get_independent_cache_directories() {
        let (_dir, store) = temp_store();
        let cache_root = tempfile::tempdir().unwrap();
        let manifest_a = seed_one_layer_manifest(&store, "a.txt", b"content a");
        let manifest_b = seed_one_layer_manifest(&store, "b.txt", b"content b");
        let digest_a = sha256(b"digest-a");
        let digest_b = sha256(b"digest-b");

        let dir_a =
            ensure_cached(&store, cache_root.path(), &digest_a, &manifest_a.layers).unwrap();
        let dir_b =
            ensure_cached(&store, cache_root.path(), &digest_b, &manifest_b.layers).unwrap();

        assert_ne!(dir_a, dir_b);
        assert!(dir_a.join("a.txt").exists());
        assert!(!dir_a.join("b.txt").exists());
        assert!(dir_b.join("b.txt").exists());
        assert!(!dir_b.join("a.txt").exists());
    }

    #[test]
    fn ensure_cached_leaves_no_leftover_temporary_build_directory() {
        let (_dir, store) = temp_store();
        let cache_root = tempfile::tempdir().unwrap();
        let manifest = seed_one_layer_manifest(&store, "hello.txt", b"content");
        let digest = sha256(b"fake-manifest-digest-three");

        let cache_dir =
            ensure_cached(&store, cache_root.path(), &digest, &manifest.layers).unwrap();

        // Exactly one entry under `cache_root`: the real cache
        // directory itself, no stray `tempfile::tempdir_in` scratch
        // directory left behind from the build.
        let entries: Vec<_> = std::fs::read_dir(cache_root.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(entries, vec![cache_dir]);
    }

    #[test]
    fn prune_removes_a_cache_entry_no_image_references_anymore() {
        let (_dir, store) = temp_store();
        let cache_root = tempfile::tempdir().unwrap();
        let manifest = seed_one_layer_manifest(&store, "hello.txt", b"orphaned content");
        let digest = sha256(b"fake-manifest-digest-orphan");
        let cache_dir =
            ensure_cached(&store, cache_root.path(), &digest, &manifest.layers).unwrap();
        assert!(cache_dir.exists());
        // Deliberately no `store.put_image` for this digest at all --
        // nothing references it.

        let report = prune(&store, cache_root.path()).unwrap();

        assert_eq!(report.removed, vec![digest]);
        assert!(report.reclaimed_bytes > 0);
        assert!(!cache_dir.exists());
    }

    #[test]
    fn prune_keeps_a_cache_entry_a_real_image_still_references() {
        let (_dir, store) = temp_store();
        let cache_root = tempfile::tempdir().unwrap();
        let manifest = seed_one_layer_manifest(&store, "hello.txt", b"still needed");
        let digest = sha256(b"fake-manifest-digest-kept");
        let cache_dir =
            ensure_cached(&store, cache_root.path(), &digest, &manifest.layers).unwrap();
        store
            .put_image(&crate::ImageRecord {
                reference: "docker.io/library/kept:latest".to_string(),
                manifest_digest: digest.clone(),
            })
            .unwrap();

        let report = prune(&store, cache_root.path()).unwrap();

        assert!(report.removed.is_empty());
        assert_eq!(report.reclaimed_bytes, 0);
        assert!(cache_dir.exists());
        assert_eq!(
            std::fs::read(cache_dir.join("hello.txt")).unwrap(),
            b"still needed"
        );
    }

    #[test]
    fn prune_ignores_an_in_progress_build_directory() {
        let (_dir, store) = temp_store();
        let cache_root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cache_root.path()).unwrap();
        let scratch = tempfile::tempdir_in(cache_root.path()).unwrap();
        std::fs::write(scratch.path().join("partial"), b"not done yet").unwrap();

        let report = prune(&store, cache_root.path()).unwrap();

        assert!(report.removed.is_empty());
        assert!(
            scratch.path().exists(),
            "an in-progress build must survive prune"
        );
    }

    #[test]
    fn prune_on_a_missing_cache_root_is_a_real_no_op_not_an_error() {
        let (_dir, store) = temp_store();
        let cache_root = tempfile::tempdir().unwrap();
        let missing = cache_root.path().join("never-created");

        let report = prune(&store, &missing).unwrap();

        assert_eq!(report, CachePruneReport::default());
    }

    #[test]
    fn prune_with_two_images_keeps_only_the_still_referenced_one() {
        let (_dir, store) = temp_store();
        let cache_root = tempfile::tempdir().unwrap();
        let manifest_a = seed_one_layer_manifest(&store, "a.txt", b"content a");
        let manifest_b = seed_one_layer_manifest(&store, "b.txt", b"content b");
        let digest_a = sha256(b"digest-prune-a");
        let digest_b = sha256(b"digest-prune-b");
        let dir_a =
            ensure_cached(&store, cache_root.path(), &digest_a, &manifest_a.layers).unwrap();
        let dir_b =
            ensure_cached(&store, cache_root.path(), &digest_b, &manifest_b.layers).unwrap();
        store
            .put_image(&crate::ImageRecord {
                reference: "docker.io/library/a:latest".to_string(),
                manifest_digest: digest_a.clone(),
            })
            .unwrap();

        let report = prune(&store, cache_root.path()).unwrap();

        assert_eq!(report.removed, vec![digest_b]);
        assert!(dir_a.exists());
        assert!(!dir_b.exists());
    }

    /// The real bug `dir_size`'s own doc comment describes, caught
    /// directly (a real ~490 MB reported reclaim for what `du` showed
    /// was really a few MB) rather than by inspection alone: a real
    /// hardlink-heavy layer (matching real `docs/design/0106`'s own
    /// busybox example — one real binary, many hardlinked names
    /// pointing at it) must not have its own single real size counted
    /// once per hardlinked name.
    #[test]
    fn prune_reclaimed_bytes_counts_a_hardlinked_file_once_not_once_per_link() {
        let (_dir, store) = temp_store();
        let cache_root = tempfile::tempdir().unwrap();

        let content = vec![b'x'; 4096];
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o755);
        header.set_size(content.len() as u64);
        builder
            .append_data(&mut header, "bin/real", content.as_slice())
            .unwrap();
        // Ten more names, all real hardlinks to the one real file
        // above -- matching a real busybox image's own applet shape,
        // just with a round number for an easy assertion.
        for name in ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"] {
            let mut link_header = tar::Header::new_gnu();
            link_header.set_entry_type(tar::EntryType::Link);
            link_header.set_mode(0o755);
            link_header.set_size(0);
            builder
                .append_link(&mut link_header, format!("bin/{name}"), "bin/real")
                .unwrap();
        }
        let tar_bytes = builder.into_inner().unwrap();
        let mut compressed = Vec::new();
        let mut encoder =
            flate2::write::GzEncoder::new(&mut compressed, flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &tar_bytes).unwrap();
        encoder.finish().unwrap();
        let ingested = store.ingest(compressed.as_slice()).unwrap();

        let manifest = ImageManifest {
            schema_version: 2,
            media_type: None,
            config: Descriptor {
                media_type: "application/vnd.oci.image.config.v1+json".to_string(),
                digest: sha256(b"unused-config"),
                size: 0,
                urls: vec![],
                annotations: BTreeMap::new(),
                platform: None,
            },
            layers: vec![Descriptor {
                media_type: MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
                digest: ingested.digest,
                size: ingested.size,
                urls: vec![],
                annotations: BTreeMap::new(),
                platform: None,
            }],
            annotations: BTreeMap::new(),
        };
        let digest = sha256(b"digest-hardlink-dedup");
        ensure_cached(&store, cache_root.path(), &digest, &manifest.layers).unwrap();

        let report = prune(&store, cache_root.path()).unwrap();

        assert_eq!(report.removed, vec![digest]);
        // Eleven hardlinked names, one real 4096-byte file -- not
        // eleven times that.
        assert_eq!(report.reclaimed_bytes, 4096);
    }
}
