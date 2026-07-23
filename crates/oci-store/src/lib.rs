//! Content-addressed OCI blob store and image metadata database.
//!
//! Layout on disk, rooted at whatever directory [`Store::open`] is given
//! (e.g. `~/.local/share/oci-tools/storage` for rootless `ociman`, or
//! `/ociboot/store` on the state partition):
//!
//! ```text
//! <root>/
//!   blobs/sha256/<hex>                 content-addressed blobs (manifests,
//!                                       configs, layers), one file per digest
//!   images/<registry>/<repo>/<ref>.json  one pointer file per tag/digest
//!                                       reference, holding the manifest
//!                                       digest it currently resolves to
//! ```
//!
//! Blobs are ingested atomically: written to a temp file in `blobs/sha256/`,
//! hashed while streaming, and renamed into place only after the digest is
//! known (and, for registry pulls, verified against the expected digest) —
//! so a crash mid-download never leaves a corrupt or half-written blob at
//! its final path.
//!
//! Garbage collection is mark-and-sweep rather than incrementally
//! ref-counted: every image pointer's manifest is walked (following image
//! indexes to their selected children) to compute the reachable blob set,
//! then any blob not in it is removed. This is equivalent to ref-counting
//! (a blob survives iff at least one live reference reaches it) but immune
//! to counter-drift bugs from crashes between increment/decrement pairs.
//!
//! One store implementation serves `ociman` (container storage), `ocicri`
//! (CRI image service), and `ociboot` (`/ociboot/store` on the state
//! partition).

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use oci_spec_types::Digest;
use oci_spec_types::digest::Sha256Writer;
use oci_spec_types::image::Manifest;

mod describe;
mod images;
mod resolve;
mod rootfs_cache;

pub use describe::ImageSummary;
pub use images::{ImageRecord, ImagesError};
pub use resolve::{
    ResolvedImage, is_untagged_reference, resolve_by_id_only, resolve_by_reference_or_id,
    untagged_reference,
};
pub use rootfs_cache::{
    CachePruneReport, cache_dir_for, cache_root, dir_size, dir_stats, ensure_cached, prune,
};

/// Errors returned by [`Store`] operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Filesystem I/O failure.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// A downloaded/ingested blob's content did not hash to the digest the
    /// caller expected.
    #[error("digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch {
        /// The digest the caller expected (e.g. from a manifest descriptor).
        expected: Digest,
        /// The digest actually computed from the streamed content.
        actual: Digest,
    },
    /// Image pointer metadata error.
    #[error(transparent)]
    Images(#[from] ImagesError),
    /// A manifest blob referenced by a pointer failed to parse.
    #[error("blob {digest} does not look like a valid manifest: {source}")]
    InvalidManifest {
        /// The manifest blob's digest.
        digest: Digest,
        /// The JSON parse error.
        #[source]
        source: serde_json::Error,
    },
    /// An image pointer resolved to a multi-platform index rather than a
    /// single-platform manifest. oci-tools always stores the
    /// platform-resolved manifest under a pointer (see `oci_registry::pull`),
    /// so this indicates the store was written by something else, or a
    /// future feature (preserving indexes verbatim) that hasn't landed yet.
    #[error("image pointer resolves to a multi-platform index ({digest}), not a manifest")]
    UnexpectedIndex {
        /// The index blob's digest.
        digest: Digest,
    },
    /// A layer descriptor's own media type isn't one
    /// [`oci_layer::compression_for_media_type`] recognizes — surfaced by
    /// [`crate::ensure_cached`] while extracting a manifest's own layers.
    #[error("unsupported layer media type: {media_type:?}")]
    UnsupportedLayerMediaType {
        /// The unrecognized media type string.
        media_type: String,
    },
    /// [`resolve::resolve_by_id_only`]'s own real or short image ID
    /// fallback matched more than one genuinely different image (a
    /// real, if rare in practice, hex-prefix collision) — matching
    /// real `docker`/`podman`'s own identical "multiple IDs found"
    /// refusal rather than silently guessing one.
    #[error("image ID {spec:?} is ambiguous: matches {count} different images")]
    AmbiguousId {
        /// The real or short ID string that was given.
        spec: String,
        /// How many genuinely different images (distinct manifest
        /// digests) it matched.
        count: usize,
    },
}

/// Result alias for [`StoreError`].
pub type Result<T> = std::result::Result<T, StoreError>;

/// Outcome of ingesting a blob: its digest and byte size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingested {
    /// The blob's content digest (always sha256; oci-tools never writes
    /// blobs under any other algorithm).
    pub digest: Digest,
    /// The blob's size in bytes.
    pub size: u64,
}

/// Blobs removed, and bytes reclaimed, by a [`Store::gc`] run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Digests of blobs that were deleted.
    pub removed: Vec<Digest>,
    /// Total bytes reclaimed.
    pub reclaimed_bytes: u64,
}

/// A content-addressed OCI blob store plus image tag/digest pointer
/// metadata, rooted at a single directory on a plain ext4/xfs filesystem.
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Open (creating if necessary) a store rooted at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let store = Store { root };
        fs::create_dir_all(store.blobs_dir())?;
        fs::create_dir_all(store.images_dir())?;
        Ok(store)
    }

    /// The store's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory holding content-addressed blobs (`blobs/sha256/`).
    pub fn blobs_dir(&self) -> PathBuf {
        self.root.join("blobs").join("sha256")
    }

    /// Directory holding image tag/digest pointer files (`images/`).
    fn images_dir(&self) -> PathBuf {
        self.root.join("images")
    }

    /// The on-disk path a blob with digest `digest` would live at, whether
    /// or not it currently exists.
    pub fn blob_path(&self, digest: &Digest) -> PathBuf {
        self.blobs_dir().join(digest.hex())
    }

    /// Whether a blob with this digest is already stored.
    pub fn has_blob(&self, digest: &Digest) -> bool {
        self.blob_path(digest).is_file()
    }

    /// The size of an already-stored blob.
    pub fn blob_size(&self, digest: &Digest) -> Result<u64> {
        Ok(fs::metadata(self.blob_path(digest))?.len())
    }

    /// Open an already-stored blob for reading.
    pub fn open_blob(&self, digest: &Digest) -> Result<File> {
        Ok(File::open(self.blob_path(digest))?)
    }

    /// Read an already-stored blob fully into memory (for manifests and
    /// configs, which are small; layers should be streamed via
    /// [`Store::open_blob`] instead).
    pub fn read_blob(&self, digest: &Digest) -> Result<Vec<u8>> {
        fs::read(self.blob_path(digest)).map_err(StoreError::Io)
    }

    /// Stream `reader` into the store, computing its digest as it goes.
    /// If a blob with the resulting digest already exists, the new content
    /// is discarded (content-addressed dedup); otherwise it is atomically
    /// renamed into place. Returns the digest and size either way.
    pub fn ingest(&self, reader: impl Read) -> Result<Ingested> {
        self.ingest_impl(reader, None)
    }

    /// Like [`Store::ingest`], but also verifies the streamed content
    /// hashes to `expected` (the digest advertised by a manifest
    /// descriptor), returning [`StoreError::DigestMismatch`] and discarding
    /// the content if it does not. Registry pulls must use this, never the
    /// unchecked [`Store::ingest`], so a malicious or misbehaving registry
    /// can never poison local storage.
    pub fn ingest_verified(&self, reader: impl Read, expected: &Digest) -> Result<u64> {
        let ingested = self.ingest_impl(reader, Some(expected))?;
        Ok(ingested.size)
    }

    fn ingest_impl(&self, mut reader: impl Read, expected: Option<&Digest>) -> Result<Ingested> {
        let blobs_dir = self.blobs_dir();
        let mut tmp = tempfile::NamedTempFile::new_in(&blobs_dir)?;
        let mut hasher = Sha256Writer::new();
        let mut size: u64 = 0;
        let mut buf = [0u8; 128 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            io::Write::write_all(&mut hasher, &buf[..n])?;
            io::Write::write_all(tmp.as_file_mut(), &buf[..n])?;
            size += n as u64;
        }
        let digest = hasher.finish_digest();

        if let Some(expected) = expected
            && &digest != expected
        {
            return Err(StoreError::DigestMismatch {
                expected: expected.clone(),
                actual: digest,
            });
        }

        let dest = self.blob_path(&digest);
        if dest.is_file() {
            // Already have it (content-addressed dedup); drop the temp file.
            drop(tmp);
        } else {
            tmp.persist(&dest).map_err(|e| e.error)?;
        }
        Ok(Ingested { digest, size })
    }

    /// Record (creating or overwriting) the manifest digest that
    /// `reference` currently resolves to.
    pub fn put_image(&self, record: &ImageRecord) -> Result<()> {
        images::put(&self.images_dir(), record).map_err(StoreError::from)
    }

    /// Look up the stored pointer for `reference`, if any.
    pub fn resolve_image(&self, reference: &str) -> Result<Option<ImageRecord>> {
        images::get(&self.images_dir(), reference).map_err(StoreError::from)
    }

    /// Remove the stored pointer for `reference` (the underlying blobs are
    /// only removed by a later [`Store::gc`]). Returns whether a pointer
    /// existed.
    pub fn remove_image(&self, reference: &str) -> Result<bool> {
        images::remove(&self.images_dir(), reference).map_err(StoreError::from)
    }

    /// List every stored image pointer.
    pub fn list_images(&self) -> Result<Vec<ImageRecord>> {
        images::list(&self.images_dir()).map_err(StoreError::from)
    }

    /// Compute the reachable blob set from every stored image pointer and
    /// delete everything else in `blobs/sha256/`.
    pub fn gc(&self) -> Result<GcReport> {
        let mut reachable = std::collections::HashSet::new();
        for record in self.list_images()? {
            self.mark_reachable(&record.manifest_digest, &mut reachable)?;
        }

        let mut report = GcReport::default();
        for entry in fs::read_dir(self.blobs_dir())? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(hex) = file_name.to_str() else {
                continue;
            };
            // Skip temp files from in-flight/interrupted ingests.
            if hex.starts_with(".tmp") {
                continue;
            }
            if reachable.contains(hex) {
                continue;
            }
            let len = entry.metadata()?.len();
            fs::remove_file(entry.path())?;
            report.reclaimed_bytes += len;
            if let Ok(digest) = Digest::parse(&format!("sha256:{hex}")) {
                report.removed.push(digest);
            }
        }
        Ok(report)
    }

    /// Mark `digest` (a manifest, index, config, or layer) and everything
    /// it transitively references as reachable.
    fn mark_reachable(
        &self,
        digest: &Digest,
        reachable: &mut std::collections::HashSet<String>,
    ) -> Result<()> {
        if !reachable.insert(digest.hex().to_string()) {
            return Ok(()); // already visited
        }
        if !self.has_blob(digest) {
            // Pointer refers to a blob we no longer have (shouldn't happen
            // in normal operation); nothing further to walk.
            return Ok(());
        }
        let bytes = self.read_blob(digest)?;
        // We don't have the original Content-Type here; sniff the body
        // instead (Manifest::parse falls back to sniffing when given None).
        let Ok(manifest) = Manifest::parse(&bytes, None) else {
            return Ok(()); // not a manifest (e.g. a layer or config blob)
        };
        match manifest {
            Manifest::Image(image) => {
                self.mark_reachable(&image.config.digest, reachable)?;
                for layer in &image.layers {
                    reachable.insert(layer.digest.hex().to_string());
                }
            }
            Manifest::Index(index) => {
                // Recursion handles both plain image-manifest children and
                // nested indexes uniformly (Manifest::parse sniffs which
                // one each child blob is).
                for entry in &index.manifests {
                    self.mark_reachable(&entry.digest, reachable)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_spec_types::image::{
        Descriptor, ImageConfig, ImageManifest, MEDIA_TYPE_IMAGE_CONFIG,
        MEDIA_TYPE_IMAGE_LAYER_GZIP, MEDIA_TYPE_IMAGE_MANIFEST, RootFs,
    };
    use std::collections::BTreeMap;

    fn temp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn ingest_then_read_round_trips() {
        let (_dir, store) = temp_store();
        let ingested = store.ingest(&b"hello"[..]).unwrap();
        assert_eq!(ingested.size, 5);
        assert!(store.has_blob(&ingested.digest));
        assert_eq!(store.read_blob(&ingested.digest).unwrap(), b"hello");
        assert_eq!(store.blob_size(&ingested.digest).unwrap(), 5);
    }

    #[test]
    fn ingest_is_content_addressed_dedup() {
        let (_dir, store) = temp_store();
        let a = store.ingest(&b"same content"[..]).unwrap();
        let b = store.ingest(&b"same content"[..]).unwrap();
        assert_eq!(a.digest, b.digest);
        // Only one file on disk.
        let count = fs::read_dir(store.blobs_dir()).unwrap().count();
        assert_eq!(count, 1);
    }

    #[test]
    fn ingest_verified_rejects_mismatch() {
        let (_dir, store) = temp_store();
        let wrong = oci_spec_types::digest::sha256(b"not the content");
        let err = store.ingest_verified(&b"hello"[..], &wrong).unwrap_err();
        assert!(matches!(err, StoreError::DigestMismatch { .. }));
        // Rejected content must not linger as a stray temp file.
        assert_eq!(fs::read_dir(store.blobs_dir()).unwrap().count(), 0);
    }

    #[test]
    fn ingest_verified_accepts_match() {
        let (_dir, store) = temp_store();
        let expected = oci_spec_types::digest::sha256(b"hello");
        let size = store.ingest_verified(&b"hello"[..], &expected).unwrap();
        assert_eq!(size, 5);
        assert!(store.has_blob(&expected));
    }

    fn sample_manifest_bytes(store: &Store) -> (Digest, Digest, Digest) {
        let config = ImageConfig {
            architecture: Some("arm64".to_string()),
            os: Some("linux".to_string()),
            rootfs: RootFs {
                kind: "layers".to_string(),
                diff_ids: vec![],
            },
            ..Default::default()
        };
        let config_bytes = serde_json::to_vec(&config).unwrap();
        let config_ingested = store.ingest(&config_bytes[..]).unwrap();

        let layer_ingested = store.ingest(&b"layer content"[..]).unwrap();

        let manifest = ImageManifest {
            schema_version: 2,
            media_type: Some(MEDIA_TYPE_IMAGE_MANIFEST.to_string()),
            config: Descriptor {
                media_type: MEDIA_TYPE_IMAGE_CONFIG.to_string(),
                digest: config_ingested.digest.clone(),
                size: config_ingested.size,
                urls: vec![],
                annotations: BTreeMap::new(),
                platform: None,
            },
            layers: vec![Descriptor {
                media_type: MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
                digest: layer_ingested.digest.clone(),
                size: layer_ingested.size,
                urls: vec![],
                annotations: BTreeMap::new(),
                platform: None,
            }],
            annotations: BTreeMap::new(),
        };
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_ingested = store.ingest(&manifest_bytes[..]).unwrap();
        (
            manifest_ingested.digest,
            config_ingested.digest,
            layer_ingested.digest,
        )
    }

    #[test]
    fn gc_keeps_reachable_and_removes_orphans() {
        let (_dir, store) = temp_store();
        let (manifest_digest, config_digest, layer_digest) = sample_manifest_bytes(&store);
        store
            .put_image(&ImageRecord {
                reference: "docker.io/library/ubuntu:latest".to_string(),
                manifest_digest: manifest_digest.clone(),
            })
            .unwrap();

        // An orphan blob unrelated to any image pointer.
        let orphan = store.ingest(&b"nobody references me"[..]).unwrap();

        let report = store.gc().unwrap();
        assert_eq!(report.removed, vec![orphan.digest.clone()]);
        assert!(!store.has_blob(&orphan.digest));

        assert!(store.has_blob(&manifest_digest));
        assert!(store.has_blob(&config_digest));
        assert!(store.has_blob(&layer_digest));
    }

    #[test]
    fn gc_removes_everything_once_pointer_is_removed() {
        let (_dir, store) = temp_store();
        let (manifest_digest, config_digest, layer_digest) = sample_manifest_bytes(&store);
        let reference = "docker.io/library/ubuntu:latest";
        store
            .put_image(&ImageRecord {
                reference: reference.to_string(),
                manifest_digest: manifest_digest.clone(),
            })
            .unwrap();

        assert!(store.remove_image(reference).unwrap());
        assert!(store.resolve_image(reference).unwrap().is_none());

        store.gc().unwrap();
        assert!(!store.has_blob(&manifest_digest));
        assert!(!store.has_blob(&config_digest));
        assert!(!store.has_blob(&layer_digest));
    }

    #[test]
    fn list_and_resolve_images() {
        let (_dir, store) = temp_store();
        let (manifest_digest, _config, _layer) = sample_manifest_bytes(&store);
        store
            .put_image(&ImageRecord {
                reference: "docker.io/library/ubuntu:latest".to_string(),
                manifest_digest: manifest_digest.clone(),
            })
            .unwrap();
        store
            .put_image(&ImageRecord {
                reference: "quay.io/foo/bar:v1".to_string(),
                manifest_digest: manifest_digest.clone(),
            })
            .unwrap();

        let images = store.list_images().unwrap();
        assert_eq!(images.len(), 2);

        let resolved = store
            .resolve_image("docker.io/library/ubuntu:latest")
            .unwrap()
            .unwrap();
        assert_eq!(resolved.manifest_digest, manifest_digest);
        assert!(
            store
                .resolve_image("docker.io/library/missing:latest")
                .unwrap()
                .is_none()
        );
    }
}
