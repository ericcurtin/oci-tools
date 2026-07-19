//! Committing a build step's rootfs changes as a real, stored image
//! layer — the missing link this crate's own module doc has always
//! named ("layer commits via `oci-store`") and the one 0046/0047
//! (`oci-layer`'s diff/export/compress trio) each independently
//! flagged as their own immediate next gap: something has to actually
//! drive [`oci_store::Store::ingest`] with a compressed layer's bytes,
//! and hand back a [`Descriptor`] and `diff_id` shaped exactly the way
//! an image manifest's own layer list and an image config's own
//! `rootfs.diff_ids`/`history` need them.
//!
//! This module owns exactly that handoff and nothing more: it does not
//! parse a Dockerfile, run a `RUN` step, or decide *when* to diff a
//! rootfs (a future build executor's own job, still not implemented —
//! see this crate's own top-level doc comment) — it only turns an
//! already-computed [`oci_layer::Change`] list plus the live rootfs
//! it was computed from into one new, real, stored layer.

use std::collections::BTreeMap;
use std::io;

use oci_layer::Change;
use oci_spec_types::Digest;
use oci_spec_types::image::{Descriptor, MEDIA_TYPE_IMAGE_LAYER_GZIP};
use oci_store::Store;

/// Errors from [`commit_layer`].
#[derive(Debug, thiserror::Error)]
pub enum CommitLayerError {
    /// Reading the rootfs, writing the tar/gzip stream, or ingesting
    /// the result into the store failed.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The blob store itself rejected the ingest (e.g. a filesystem
    /// error at the store's own root).
    #[error(transparent)]
    Store(#[from] oci_store::StoreError),
}

/// A layer newly committed into a [`Store`] by [`commit_layer`]: a
/// manifest-ready [`Descriptor`] for the *compressed* blob now
/// present in the store, and the `diff_id` (the digest of the layer's
/// own *uncompressed* content) an image config's `rootfs.diff_ids`
/// entry for this same layer needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedLayer {
    /// Append this to the manifest's own `layers` list.
    pub descriptor: Descriptor,
    /// Append this to the image config's own `rootfs.diff_ids` list,
    /// in the same relative order as `descriptor` in the manifest's
    /// own `layers` list (both bottom-layer-first).
    pub diff_id: Digest,
}

/// Turn `changes` (as computed by [`oci_layer::changes`] against
/// `root`'s own live state) into one new layer: export it to an
/// uncompressed tar, gzip-compress it while computing its `diff_id`,
/// and ingest the compressed bytes into `store` — the three existing,
/// already-tested [`oci_layer`] primitives (`export`, then
/// `compress_for_storage`, then [`Store::ingest`]), driven back to
/// back in one pass, with no intermediate step exposed for a caller to
/// get out of order.
///
/// An empty `changes` list still produces a real (empty) layer rather
/// than being special-cased away: real `moby`/BuildKit do the same
/// (an empty `RUN` step, or one that happens to touch nothing on
/// disk, still commits a real, valid, if degenerate, layer) — deciding
/// whether an empty-diff layer is even worth committing at all is a
/// build-executor policy choice, not this function's own to make.
pub fn commit_layer(
    store: &Store,
    root: &std::path::Path,
    changes: &[Change],
) -> Result<CommittedLayer, CommitLayerError> {
    let mut tar_bytes = Vec::new();
    oci_layer::export(root, changes, &mut tar_bytes).map_err(io_from_layer_error)?;

    let mut compressed = Vec::new();
    let diff_id = oci_layer::compress_for_storage(tar_bytes.as_slice(), &mut compressed)
        .map_err(io_from_layer_error)?;

    let ingested = store.ingest(compressed.as_slice())?;

    Ok(CommittedLayer {
        descriptor: Descriptor {
            media_type: MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
            digest: ingested.digest,
            size: ingested.size,
            urls: vec![],
            annotations: BTreeMap::new(),
            platform: None,
        },
        diff_id,
    })
}

/// [`oci_layer::LayerError`] doesn't implement [`std::error::Error`]
/// in a way [`CommitLayerError::Io`] can wrap directly via `#[from]`
/// (it has its own variants beyond I/O, e.g. path-escape rejection,
/// that make more sense folded into a plain [`io::Error`] here than
/// given their own [`CommitLayerError`] variant, since neither
/// `export` nor `compress_for_storage` — the only two [`oci_layer`]
/// calls in this function — can ever actually produce those other
/// variants in practice: there is no untrusted tar input at this
/// point in the pipeline, only a live rootfs this same process just
/// diffed).
fn io_from_layer_error(err: oci_layer::LayerError) -> CommitLayerError {
    CommitLayerError::Io(io::Error::other(err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_layer::{ChangeKind, Snapshot, changes};
    use std::fs;

    fn write_file(path: &std::path::Path, content: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn temp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        (dir, store)
    }

    #[test]
    fn commits_a_real_diff_and_the_layer_is_readable_back_from_the_store() {
        let (_store_dir, store) = temp_store();
        let root = tempfile::tempdir().unwrap();
        write_file(&root.path().join("existing.txt"), b"already here");
        let before = Snapshot::capture(root.path()).unwrap();

        write_file(&root.path().join("new/file.txt"), b"brand new content");
        let diff = changes(root.path(), &before).unwrap();
        assert!(!diff.is_empty());

        let committed = commit_layer(&store, root.path(), &diff).unwrap();

        assert_eq!(committed.descriptor.media_type, MEDIA_TYPE_IMAGE_LAYER_GZIP);
        assert!(store.has_blob(&committed.descriptor.digest));
        assert_eq!(
            store.blob_size(&committed.descriptor.digest).unwrap(),
            committed.descriptor.size
        );

        // The stored blob is exactly what `compress_for_storage` would
        // itself produce from the very same diff -- ingest is a pure
        // pass-through of already-compressed bytes, not a
        // reprocessing step of its own.
        let mut expected_tar = Vec::new();
        oci_layer::export(root.path(), &diff, &mut expected_tar).unwrap();
        let mut expected_compressed = Vec::new();
        let expected_diff_id =
            oci_layer::compress_for_storage(expected_tar.as_slice(), &mut expected_compressed)
                .unwrap();
        assert_eq!(committed.diff_id, expected_diff_id);
        assert_eq!(
            store.read_blob(&committed.descriptor.digest).unwrap(),
            expected_compressed
        );
    }

    #[test]
    fn an_empty_change_list_still_commits_a_real_valid_layer() {
        let (_store_dir, store) = temp_store();
        let root = tempfile::tempdir().unwrap();

        let committed = commit_layer(&store, root.path(), &[]).unwrap();
        assert!(store.has_blob(&committed.descriptor.digest));
        // Real (if degenerate): an empty tar archive is still two
        // 512-byte zero blocks, a well-defined, valid tar stream, not
        // zero bytes.
        assert!(committed.descriptor.size > 0);
    }

    #[test]
    fn two_separately_committed_layers_get_independent_content_addressed_digests() {
        let (_store_dir, store) = temp_store();
        let root = tempfile::tempdir().unwrap();
        let before = Snapshot::capture(root.path()).unwrap();

        write_file(&root.path().join("a.txt"), b"first layer's content");
        let diff_a = changes(root.path(), &before).unwrap();
        let committed_a = commit_layer(&store, root.path(), &diff_a).unwrap();

        let before2 = Snapshot::capture(root.path()).unwrap();
        write_file(&root.path().join("b.txt"), b"second layer's content");
        let diff_b = changes(root.path(), &before2).unwrap();
        assert!(diff_b.iter().all(|c| c.kind != ChangeKind::Deleted));
        let committed_b = commit_layer(&store, root.path(), &diff_b).unwrap();

        assert_ne!(committed_a.descriptor.digest, committed_b.descriptor.digest);
        assert_ne!(committed_a.diff_id, committed_b.diff_id);
        assert!(store.has_blob(&committed_a.descriptor.digest));
        assert!(store.has_blob(&committed_b.descriptor.digest));
    }
}
