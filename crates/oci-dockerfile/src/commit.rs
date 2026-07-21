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
//! This module owns exactly that handoff, plus recording the result
//! into an image being built ([`record_layer`]/[`record_empty_history`]),
//! and nothing more: it does not parse a Dockerfile, run a `RUN` step,
//! or decide *when* to diff a rootfs (that's `ociman build`'s own job
//! — `bin/ociman/src/build.rs`'s `run_instruction`/`copy_instruction`,
//! see this crate's own top-level doc comment) — it only turns an
//! already-computed [`oci_layer::Change`] list plus the live rootfs it
//! was computed from into one new, real, stored layer, and knows how
//! to fold that (or a non-layer-producing instruction) into an
//! [`ImageConfig`]/manifest layer list the build executor assembles
//! stage by stage.

use std::collections::BTreeMap;
use std::io;
use std::time::SystemTime;

use oci_layer::Change;
use oci_spec_types::Digest;
use oci_spec_types::image::{Descriptor, HistoryEntry, ImageConfig, MEDIA_TYPE_IMAGE_LAYER_GZIP};
use oci_spec_types::time::format_rfc3339_utc;
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

/// Record `committed` (as just produced by [`commit_layer`]) into an
/// image being built: append its own [`Descriptor`] to `layers` (the
/// manifest's own layer list a future build executor is assembling)
/// and its own `diff_id`, plus a new non-empty [`HistoryEntry`]
/// timestamped now, to `config`'s own `rootfs.diff_ids`/`history`
/// (both bottom-layer-first, matching `layers`' own append order —
/// this function only ever appends to both together, so the two lists
/// can never drift out of the relative order the image-spec requires
/// between them).
///
/// `created_by` is a free-form description of the instruction that
/// produced this layer (real `docker build`'s own convention is
/// something shell-quoted like `RUN /bin/sh -c "..."`; this function
/// doesn't prescribe a format, since it has no idea yet what a future
/// build executor's own instruction text will look like).
pub fn record_layer(
    config: &mut ImageConfig,
    layers: &mut Vec<Descriptor>,
    committed: &CommittedLayer,
    created_by: impl Into<String>,
) {
    layers.push(committed.descriptor.clone());
    config.rootfs.diff_ids.push(committed.diff_id.clone());
    config.history.push(HistoryEntry {
        created: Some(format_rfc3339_utc(SystemTime::now())),
        created_by: Some(created_by.into()),
        author: None,
        comment: None,
        empty_layer: false,
    });
}

/// Record a build instruction that produced *no* new layer (e.g.
/// `ENV`/`LABEL`/`CMD`/`WORKDIR`/`ARG` — anything that only changes
/// `config`'s own runtime defaults, not the rootfs) as a history-only
/// entry: no `rootfs.diff_ids` entry, `empty_layer: true` — matching
/// real `docker build`'s own `history` shape exactly (`docker history`
/// on any real image shows these interleaved with real layer-producing
/// entries, most with no corresponding layer size at all).
pub fn record_empty_history(config: &mut ImageConfig, created_by: impl Into<String>) {
    config.history.push(HistoryEntry {
        created: Some(format_rfc3339_utc(SystemTime::now())),
        created_by: Some(created_by.into()),
        author: None,
        comment: None,
        empty_layer: true,
    });
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

    #[test]
    fn record_layer_keeps_layers_and_diff_ids_in_the_same_relative_order() {
        let (_store_dir, store) = temp_store();
        let root = tempfile::tempdir().unwrap();
        let before = Snapshot::capture(root.path()).unwrap();

        write_file(&root.path().join("a.txt"), b"first");
        let diff_a = changes(root.path(), &before).unwrap();
        let committed_a = commit_layer(&store, root.path(), &diff_a).unwrap();

        let before2 = Snapshot::capture(root.path()).unwrap();
        write_file(&root.path().join("b.txt"), b"second");
        let diff_b = changes(root.path(), &before2).unwrap();
        let committed_b = commit_layer(&store, root.path(), &diff_b).unwrap();

        let mut config = ImageConfig::default();
        let mut layers = Vec::new();
        record_layer(&mut config, &mut layers, &committed_a, "RUN echo a");
        record_layer(&mut config, &mut layers, &committed_b, "RUN echo b");

        assert_eq!(layers, vec![committed_a.descriptor, committed_b.descriptor]);
        assert_eq!(
            config.rootfs.diff_ids,
            vec![committed_a.diff_id, committed_b.diff_id]
        );
        assert_eq!(config.history.len(), 2);
        assert!(!config.history[0].empty_layer);
        assert!(!config.history[1].empty_layer);
        assert_eq!(config.history[0].created_by.as_deref(), Some("RUN echo a"));
        assert_eq!(config.history[1].created_by.as_deref(), Some("RUN echo b"));
        // A real, present-day timestamp, not a placeholder -- loosely
        // sanity-checked by prefix rather than pinned to one instant.
        assert!(
            config.history[0]
                .created
                .as_ref()
                .unwrap()
                .starts_with("20")
        );
    }

    #[test]
    fn record_empty_history_touches_only_history_not_rootfs_or_layers() {
        let mut config = ImageConfig::default();
        let layers: Vec<Descriptor> = Vec::new();

        record_empty_history(&mut config, "ENV FOO=bar");

        assert!(layers.is_empty());
        assert!(config.rootfs.diff_ids.is_empty());
        assert_eq!(config.history.len(), 1);
        assert!(config.history[0].empty_layer);
        assert_eq!(config.history[0].created_by.as_deref(), Some("ENV FOO=bar"));
    }
}
