//! Push orchestration: upload an already-stored image's own manifest,
//! config, and layers to a registry — the exact mirror of [`crate::
//! pull`]'s own orchestration, the other direction. Shared by `ociman
//! push` today; any future binary that needs to push an image goes
//! through exactly this code path, never re-implements it.

use oci_spec_types::Digest;
use oci_spec_types::Reference;
use oci_spec_types::image::MEDIA_TYPE_IMAGE_MANIFEST;
use oci_store::{ImageRecord, Store};

use crate::{Client, RegistryError};

/// Errors from [`push`].
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    /// A registry request failed.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// Reading from local storage failed.
    #[error(transparent)]
    Store(#[from] oci_store::StoreError),
}

/// Push `record` (an already-stored image, e.g. from `ociman pull` or
/// `ociman build`) to `reference`'s own repository on the registry,
/// tagged as `reference`'s own tag (or, for a digest reference, pushed
/// content-addressed only, no tag). Skips any blob the registry
/// already has ([`Client::blob_exists`]) — the same real cross-push
/// deduplication a real `docker push`/`podman push` also relies on,
/// checked directly against a real local `registry:2` instance.
pub fn push(
    client: &mut Client,
    store: &Store,
    reference: &Reference,
    record: &ImageRecord,
) -> Result<(), PushError> {
    let manifest = store.image_manifest(record)?;

    push_blob_if_missing(client, store, reference, &manifest.config.digest)?;
    for layer in &manifest.layers {
        push_blob_if_missing(client, store, reference, &layer.digest)?;
    }

    // The real, already-stored bytes -- never re-serialized, so the
    // manifest the registry ends up with is byte-for-byte identical to
    // what `record.manifest_digest` already names (a re-serialization
    // could otherwise produce different bytes for the same logical
    // content: different key order, whitespace, etc. -- a real, if
    // subtle, correctness risk this avoids entirely by construction).
    let manifest_bytes = store.read_blob(&record.manifest_digest)?;
    let media_type = manifest
        .media_type
        .as_deref()
        .unwrap_or(MEDIA_TYPE_IMAGE_MANIFEST);
    client.push_manifest(
        reference,
        &reference.manifest_ref(),
        media_type,
        &manifest_bytes,
    )?;
    Ok(())
}

fn push_blob_if_missing(
    client: &mut Client,
    store: &Store,
    reference: &Reference,
    digest: &Digest,
) -> Result<(), PushError> {
    if client.blob_exists(reference, digest)? {
        return Ok(());
    }
    let file = store.open_blob(digest)?;
    client.upload_blob(reference, digest, file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Credentials;
    use oci_spec_types::digest::sha256;
    use oci_spec_types::image::{
        Descriptor, MEDIA_TYPE_IMAGE_CONFIG, MEDIA_TYPE_IMAGE_LAYER_GZIP, RootFs,
    };
    use std::collections::{BTreeMap, HashSet};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// A minimal anonymous (no-auth) HTTP/1.1 mock registry implementing
    /// just enough of the real OCI Distribution Spec's own push protocol
    /// (checked directly against a real local `registry:2` instance
    /// during this feature's own development, not assumed from the spec
    /// text alone) to exercise [`push`] end to end: `HEAD .../blobs/
    /// <digest>` (404 unless `already_has` names it), `POST .../blobs/
    /// uploads/` (202 + a `Location` header), `PUT <location>?digest=...`
    /// (verifies the uploaded body really hashes to the claimed digest,
    /// the same real check a real registry performs), and `PUT .../
    /// manifests/<ref>`. Every blob/manifest `PUT` this mock actually
    /// receives is recorded in `uploaded`/`manifest_puts` so tests can
    /// assert on exactly what did (and, for the dedup test, did not)
    /// get uploaded.
    /// `(manifest_ref, body)` for every real `PUT .../manifests/...`
    /// this mock has received so far.
    type ManifestPuts = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

    struct MockRegistry {
        addr: std::net::SocketAddr,
        uploaded: Arc<Mutex<HashSet<String>>>,
        manifest_puts: ManifestPuts,
    }

    impl MockRegistry {
        fn start(already_has: HashSet<String>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let uploaded = Arc::new(Mutex::new(HashSet::new()));
            let manifest_puts = Arc::new(Mutex::new(Vec::new()));
            let uploaded_clone = uploaded.clone();
            let manifest_puts_clone = manifest_puts.clone();
            thread::spawn(move || {
                while let Ok((stream, _)) = listener.accept() {
                    Self::handle(stream, &already_has, &uploaded_clone, &manifest_puts_clone);
                }
            });
            MockRegistry {
                addr,
                uploaded,
                manifest_puts,
            }
        }

        fn handle(
            mut stream: TcpStream,
            already_has: &HashSet<String>,
            uploaded: &Arc<Mutex<HashSet<String>>>,
            manifest_puts: &ManifestPuts,
        ) {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut parts = request_line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").to_string();

            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some(idx) = line.to_ascii_lowercase().find("content-length:") {
                    content_length = line[idx + "content-length:".len()..]
                        .trim()
                        .parse()
                        .unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).unwrap();

            let write_status = |stream: &mut TcpStream, status: u16, extra_headers: &str| {
                let text = match status {
                    200 => "OK",
                    201 => "Created",
                    202 => "Accepted",
                    404 => "Not Found",
                    _ => "Error",
                };
                let resp = format!(
                    "HTTP/1.1 {status} {text}\r\n{extra_headers}Content-Length: 0\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(resp.as_bytes()).unwrap();
            };

            if method == "HEAD" && path.contains("/blobs/") {
                let digest = path.rsplit('/').next().unwrap_or("");
                if already_has.contains(digest) {
                    write_status(&mut stream, 200, "");
                } else {
                    write_status(&mut stream, 404, "");
                }
            } else if method == "POST" && path.ends_with("/blobs/uploads/") {
                let repo = path
                    .strip_prefix("/v2/")
                    .unwrap()
                    .strip_suffix("/blobs/uploads/")
                    .unwrap();
                let location = format!("/v2/{repo}/blobs/uploads/test-upload-id");
                write_status(&mut stream, 202, &format!("Location: {location}\r\n"));
            } else if method == "PUT" && path.contains("/blobs/uploads/") {
                let digest_param = path.split("digest=").nth(1).unwrap_or("").to_string();
                let computed = sha256(&body).to_string();
                assert_eq!(
                    digest_param, computed,
                    "the uploaded body must really hash to the digest the PUT claimed"
                );
                uploaded.lock().unwrap().insert(computed);
                write_status(&mut stream, 201, "");
            } else if method == "PUT" && path.contains("/manifests/") {
                let manifest_ref = path.rsplit('/').next().unwrap_or("").to_string();
                manifest_puts.lock().unwrap().push((manifest_ref, body));
                write_status(&mut stream, 201, "");
            } else {
                write_status(&mut stream, 404, "");
            }
        }
    }

    fn seed_store_with_a_real_image() -> (tempfile::TempDir, Store, ImageRecord, Digest, Digest) {
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

        let layer_bytes = b"a fake layer tarball, real content".to_vec();

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let config_ingested = store.ingest(&config_bytes[..]).unwrap();
        let layer_ingested = store.ingest(&layer_bytes[..]).unwrap();

        let manifest = oci_spec_types::image::ImageManifest {
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

        let record = ImageRecord {
            reference: "placeholder".to_string(),
            manifest_digest: manifest_ingested.digest,
        };
        (
            dir,
            store,
            record,
            config_ingested.digest,
            layer_ingested.digest,
        )
    }

    #[test]
    fn push_uploads_every_missing_blob_and_the_manifest() {
        let (_dir, store, mut record, config_digest, layer_digest) = seed_store_with_a_real_image();
        let mock = MockRegistry::start(HashSet::new());
        record.reference = format!("{}/testrepo:latest", mock.addr);

        let mut client =
            Client::with_options(Credentials::empty(), std::iter::once(mock.addr.to_string()));
        let reference = Reference::parse(&record.reference).unwrap();
        push(&mut client, &store, &reference, &record).unwrap();

        let uploaded = mock.uploaded.lock().unwrap();
        assert!(uploaded.contains(&config_digest.to_string()));
        assert!(uploaded.contains(&layer_digest.to_string()));

        let manifest_puts = mock.manifest_puts.lock().unwrap();
        assert_eq!(manifest_puts.len(), 1);
        assert_eq!(manifest_puts[0].0, "latest");
        // The exact, already-stored manifest bytes -- never re-serialized.
        assert_eq!(
            manifest_puts[0].1,
            store.read_blob(&record.manifest_digest).unwrap()
        );
    }

    #[test]
    fn push_skips_a_blob_the_registry_already_has() {
        let (_dir, store, mut record, config_digest, layer_digest) = seed_store_with_a_real_image();
        // The registry already has the config blob (a real, if less
        // common, case: a base image's own config shared across many
        // built images) -- only the layer should actually get uploaded.
        let mut already_has = HashSet::new();
        already_has.insert(config_digest.to_string());
        let mock = MockRegistry::start(already_has);
        record.reference = format!("{}/testrepo:latest", mock.addr);

        let mut client =
            Client::with_options(Credentials::empty(), std::iter::once(mock.addr.to_string()));
        let reference = Reference::parse(&record.reference).unwrap();
        push(&mut client, &store, &reference, &record).unwrap();

        let uploaded = mock.uploaded.lock().unwrap();
        assert!(
            !uploaded.contains(&config_digest.to_string()),
            "a blob the registry already has must never be re-uploaded"
        );
        assert!(uploaded.contains(&layer_digest.to_string()));
    }
}
