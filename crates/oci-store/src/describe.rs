//! Reading back manifest/config metadata for already-stored images
//! (`ociman images`, `ociman inspect`, and eventually `ocicri`'s
//! ImageStatus RPC all go through here).

use oci_spec_types::Digest;
use oci_spec_types::image::{ImageConfig, ImageManifest, Manifest};

use crate::images::ImageRecord;
use crate::{Result, Store, StoreError};

/// A short summary of a stored image, cheap enough to compute for every
/// entry in `ociman images`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSummary {
    /// The reference this summary was looked up by (e.g.
    /// `docker.io/library/ubuntu:latest`).
    pub reference: String,
    /// Digest of the (single-platform) manifest the reference resolves to.
    pub manifest_digest: Digest,
    /// Total size in bytes: the config blob plus every layer blob.
    pub size: u64,
    /// Number of layers.
    pub layer_count: usize,
    /// Architecture from the image config, if the config blob is present.
    pub architecture: Option<String>,
    /// OS from the image config, if the config blob is present.
    pub os: Option<String>,
}

impl Store {
    /// Read back and parse the (single-platform) manifest a stored image
    /// pointer resolves to.
    ///
    /// oci-tools only ever stores the platform-resolved manifest under an
    /// image pointer (see `oci_registry::pull`), never a raw
    /// multi-platform index, so this errors on
    /// [`StoreError::UnexpectedIndex`] rather than accepting one.
    pub fn image_manifest(&self, record: &ImageRecord) -> Result<ImageManifest> {
        let bytes = self.read_blob(&record.manifest_digest)?;
        let parsed =
            Manifest::parse(&bytes, None).map_err(|source| StoreError::InvalidManifest {
                digest: record.manifest_digest.clone(),
                source,
            })?;
        match parsed {
            Manifest::Image(image) => Ok(*image),
            Manifest::Index(_) => Err(StoreError::UnexpectedIndex {
                digest: record.manifest_digest.clone(),
            }),
        }
    }

    /// Read back and parse the image config blob for a stored image
    /// pointer.
    pub fn image_config(&self, record: &ImageRecord) -> Result<ImageConfig> {
        let manifest = self.image_manifest(record)?;
        let bytes = self.read_blob(&manifest.config.digest)?;
        serde_json::from_slice(&bytes).map_err(|source| StoreError::InvalidManifest {
            digest: manifest.config.digest,
            source,
        })
    }

    /// A cheap-to-compute summary for `ociman images` (does not fail the
    /// whole listing if the config blob happens to be missing; it just
    /// leaves `architecture`/`os` unset).
    pub fn image_summary(&self, record: &ImageRecord) -> Result<ImageSummary> {
        let manifest = self.image_manifest(record)?;
        let size = manifest.config.size + manifest.layers.iter().map(|l| l.size).sum::<u64>();
        let config = self.image_config(record).ok();
        Ok(ImageSummary {
            reference: record.reference.clone(),
            manifest_digest: record.manifest_digest.clone(),
            size,
            layer_count: manifest.layers.len(),
            architecture: config.as_ref().and_then(|c| c.architecture.clone()),
            os: config.as_ref().and_then(|c| c.os.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_spec_types::image::{
        Descriptor, MEDIA_TYPE_IMAGE_CONFIG, MEDIA_TYPE_IMAGE_LAYER_GZIP,
        MEDIA_TYPE_IMAGE_MANIFEST, RootFs,
    };
    use std::collections::BTreeMap;

    fn store_with_image() -> (tempfile::TempDir, Store, ImageRecord) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

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
        let layer_ingested = store.ingest(&b"layer bytes"[..]).unwrap();

        let manifest = ImageManifest {
            schema_version: 2,
            media_type: Some(MEDIA_TYPE_IMAGE_MANIFEST.to_string()),
            config: Descriptor {
                media_type: MEDIA_TYPE_IMAGE_CONFIG.to_string(),
                digest: config_ingested.digest,
                size: config_ingested.size,
                urls: vec![],
                annotations: BTreeMap::new(),
                platform: None,
            },
            layers: vec![Descriptor {
                media_type: MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
                digest: layer_ingested.digest,
                size: layer_ingested.size,
                urls: vec![],
                annotations: BTreeMap::new(),
                platform: None,
            }],
            annotations: BTreeMap::new(),
        };
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_ingested = store.ingest(&manifest_bytes[..]).unwrap();

        let record = ImageRecord {
            reference: "docker.io/library/ubuntu:latest".to_string(),
            manifest_digest: manifest_ingested.digest,
        };
        store.put_image(&record).unwrap();
        (dir, store, record)
    }

    #[test]
    fn summary_reports_size_and_platform() {
        let (_dir, store, record) = store_with_image();
        let summary = store.image_summary(&record).unwrap();
        assert_eq!(summary.layer_count, 1);
        assert_eq!(summary.architecture.as_deref(), Some("arm64"));
        assert_eq!(summary.os.as_deref(), Some("linux"));
        assert!(summary.size > 0);
    }

    #[test]
    fn image_config_round_trips() {
        let (_dir, store, record) = store_with_image();
        let config = store.image_config(&record).unwrap();
        assert_eq!(config.architecture.as_deref(), Some("arm64"));
    }
}
