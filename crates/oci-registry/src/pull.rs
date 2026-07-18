//! Pull orchestration: resolve a reference against a registry, follow a
//! multi-platform index down to the platform we run on if necessary, and
//! land the manifest/config/layers in an `oci_store::Store`.
//!
//! Shared by `ociman pull`/`images`/`inspect` today; `ocicri`'s
//! ImageService and `ociboot upgrade`/`switch` reuse it later — every
//! binary that needs to pull an image goes through exactly this code path,
//! never re-implements it.

use oci_spec_types::image::{ImageManifest, Manifest, Platform};
use oci_spec_types::{Digest, Reference};
use oci_store::{ImageRecord, Store};

use crate::{Client, RegistryError};

/// Errors from [`pull`].
#[derive(Debug, thiserror::Error)]
pub enum PullError {
    /// A registry request failed.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// Writing to local storage failed.
    #[error(transparent)]
    Store(#[from] oci_store::StoreError),
    /// A manifest or config blob's JSON did not parse.
    #[error("failed to parse {what}: {source}")]
    InvalidJson {
        /// What failed to parse (`"manifest"` or `"image config"`).
        what: &'static str,
        /// The underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// The registry returned a multi-platform index with no manifest
    /// matching the requested platform.
    #[error("{reference}: no manifest for platform {os}/{architecture}{variant}")]
    NoMatchingPlatform {
        /// The reference that was being pulled.
        reference: String,
        /// The OS that was requested.
        os: String,
        /// The architecture that was requested.
        architecture: String,
        /// The variant that was requested, formatted as `" (variant ...)"`
        /// or empty.
        variant: String,
    },
}

/// Pull `reference` for `platform` (pass [`Platform::host`] to match the
/// machine oci-tools is running on) into `store`, fetching only blobs the
/// store does not already have. Returns the stored pointer record.
pub fn pull(
    client: &mut Client,
    store: &Store,
    reference: &Reference,
    platform: &Platform,
) -> Result<ImageRecord, PullError> {
    let top = client.pull_manifest(reference)?;
    let parsed = Manifest::parse(&top.bytes, top.content_type.as_deref()).map_err(|source| {
        PullError::InvalidJson {
            what: "manifest",
            source,
        }
    })?;

    let (manifest_bytes, manifest): (Vec<u8>, ImageManifest) = match parsed {
        Manifest::Image(image) => (top.bytes, *image),
        Manifest::Index(index) => {
            let selected = index
                .select(platform)
                .ok_or_else(|| PullError::NoMatchingPlatform {
                    reference: reference.to_string(),
                    os: platform.os.clone(),
                    architecture: platform.architecture.clone(),
                    variant: platform
                        .variant
                        .as_ref()
                        .map(|v| format!(" (variant {v})"))
                        .unwrap_or_default(),
                })?;
            let child = client.pull_manifest_at(reference, &selected.digest.to_string())?;
            let image: ImageManifest =
                serde_json::from_slice(&child.bytes).map_err(|source| PullError::InvalidJson {
                    what: "manifest",
                    source,
                })?;
            (child.bytes, image)
        }
    };

    let ingested_manifest = store.ingest(&manifest_bytes[..])?;

    fetch_blob_if_missing(client, store, reference, &manifest.config.digest)?;
    for layer in &manifest.layers {
        fetch_blob_if_missing(client, store, reference, &layer.digest)?;
    }

    let record = ImageRecord {
        reference: reference.to_string(),
        manifest_digest: ingested_manifest.digest,
    };
    store.put_image(&record)?;
    Ok(record)
}

fn fetch_blob_if_missing(
    client: &mut Client,
    store: &Store,
    reference: &Reference,
    digest: &Digest,
) -> Result<(), PullError> {
    if store.has_blob(digest) {
        return Ok(());
    }
    let reader = client.pull_blob(reference, digest)?;
    store.ingest_verified(reader, digest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Credentials;
    use oci_spec_types::digest::sha256;
    use oci_spec_types::image::{
        Descriptor, MEDIA_TYPE_IMAGE_CONFIG, MEDIA_TYPE_IMAGE_LAYER_GZIP,
        MEDIA_TYPE_IMAGE_MANIFEST, RootFs,
    };
    use std::collections::BTreeMap;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    /// A minimal anonymous (no-auth) HTTP/1.1 mock registry: serves canned
    /// bodies for exact request paths from a fixed route table, 404s
    /// anything else, one connection per request.
    struct MockRegistry {
        addr: std::net::SocketAddr,
    }

    impl MockRegistry {
        fn start(routes: HashMap<String, (&'static str, Vec<u8>)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            thread::spawn(move || {
                while let Ok((stream, _)) = listener.accept() {
                    Self::handle(stream, &routes);
                }
            });
            MockRegistry { addr }
        }

        fn handle(mut stream: TcpStream, routes: &HashMap<String, (&'static str, Vec<u8>)>) {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_string();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
            }

            match routes.get(&path) {
                Some((content_type, body)) => {
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    stream.write_all(header.as_bytes()).unwrap();
                    stream.write_all(body).unwrap();
                }
                None => {
                    let resp =
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    stream.write_all(resp.as_bytes()).unwrap();
                }
            }
        }
    }

    #[test]
    fn pull_stores_manifest_config_and_layers() {
        let config = oci_spec_types::image::ImageConfig {
            architecture: Some("arm64".to_string()),
            os: Some("linux".to_string()),
            rootfs: RootFs {
                kind: "layers".to_string(),
                diff_ids: vec![],
            },
            ..Default::default()
        };
        let config_bytes = serde_json::to_vec(&config).unwrap();
        let config_digest = sha256(&config_bytes);

        let layer_bytes = b"a fake layer tarball".to_vec();
        let layer_digest = sha256(&layer_bytes);

        let manifest = ImageManifest {
            schema_version: 2,
            media_type: Some(MEDIA_TYPE_IMAGE_MANIFEST.to_string()),
            config: Descriptor {
                media_type: MEDIA_TYPE_IMAGE_CONFIG.to_string(),
                digest: config_digest.clone(),
                size: config_bytes.len() as u64,
                urls: vec![],
                annotations: BTreeMap::new(),
                platform: None,
            },
            layers: vec![Descriptor {
                media_type: MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
                digest: layer_digest.clone(),
                size: layer_bytes.len() as u64,
                urls: vec![],
                annotations: BTreeMap::new(),
                platform: None,
            }],
            annotations: BTreeMap::new(),
        };
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();

        let mut routes = HashMap::new();
        routes.insert(
            "/v2/testrepo/manifests/latest".to_string(),
            (MEDIA_TYPE_IMAGE_MANIFEST, manifest_bytes.clone()),
        );
        routes.insert(
            format!("/v2/testrepo/blobs/{config_digest}"),
            ("application/octet-stream", config_bytes.clone()),
        );
        routes.insert(
            format!("/v2/testrepo/blobs/{layer_digest}"),
            ("application/octet-stream", layer_bytes.clone()),
        );
        let mock = MockRegistry::start(routes);

        let mut client =
            Client::with_options(Credentials::empty(), std::iter::once(mock.addr.to_string()));
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let reference = Reference::parse(&format!("{}/testrepo:latest", mock.addr)).unwrap();

        let record = pull(&mut client, &store, &reference, &Platform::host()).unwrap();
        assert_eq!(record.reference, reference.to_string());

        assert!(store.has_blob(&config_digest));
        assert!(store.has_blob(&layer_digest));
        let summary = store.image_summary(&record).unwrap();
        assert_eq!(summary.layer_count, 1);
        assert_eq!(summary.architecture.as_deref(), Some("arm64"));

        // Pulling again must not re-fetch blobs already on disk: remove the
        // routes for the blobs (keep only the manifest) and confirm a
        // second pull still succeeds purely from local dedup.
        let mut routes_manifest_only = HashMap::new();
        routes_manifest_only.insert(
            "/v2/testrepo/manifests/latest".to_string(),
            (MEDIA_TYPE_IMAGE_MANIFEST, manifest_bytes),
        );
        let mock2 = MockRegistry::start(routes_manifest_only);
        let mut client2 = Client::with_options(
            Credentials::empty(),
            std::iter::once(mock2.addr.to_string()),
        );
        let reference2 = Reference::parse(&format!("{}/testrepo:latest", mock2.addr)).unwrap();
        // Blobs live in the same store keyed by content digest, so the
        // second (blob-less) mock still succeeds.
        let record2 = pull(&mut client2, &store, &reference2, &Platform::host()).unwrap();
        assert_eq!(record2.manifest_digest, record.manifest_digest);
    }
}
