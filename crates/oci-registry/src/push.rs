//! Push orchestration: upload an already-stored image's own manifest,
//! config, and layers to a registry — the exact mirror of [`crate::
//! pull`]'s own orchestration, the other direction. Shared by `ociman
//! push` today; any future binary that needs to push an image goes
//! through exactly this code path, never re-implements it.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use oci_spec_types::Digest;
use oci_spec_types::Reference;
use oci_spec_types::image::MEDIA_TYPE_IMAGE_MANIFEST;
use oci_store::{ImageRecord, Store};

use crate::{Client, RegistryError};

/// The bound on simultaneous blob uploads a single [`push`] call ever
/// opens — matching real Docker's own identical default
/// (`max-concurrent-uploads`, `dockerd`'s own documented config,
/// checked directly — genuinely different from [`crate::pull`]'s own
/// `MAX_CONCURRENT_BLOB_FETCHES` because real Docker itself uses a
/// different default for each direction, 5 vs. 3, not a copy-paste
/// oversight). Mirrors `pull`'s own `docs/design/0256` reasoning
/// exactly: independent, content-addressed blobs have no ordering
/// dependency on each other, only on the manifest that names them
/// (uploaded last, after every blob it references — unchanged here).
const MAX_CONCURRENT_BLOB_UPLOADS: usize = 5;

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

    // The config blob plus every layer, uploaded with bounded
    // concurrency (`docs/design/0257`, the exact mirror of `pull`'s
    // own `docs/design/0256`) — `client` (the caller's own) still
    // handles the first blob inline with no extra thread at all when
    // there's only one blob total.
    let mut digests: Vec<&Digest> = Vec::with_capacity(1 + manifest.layers.len());
    digests.push(&manifest.config.digest);
    digests.extend(manifest.layers.iter().map(|layer| &layer.digest));
    push_blobs_concurrently(client, store, reference, &digests)?;

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

/// Uploads every one of `digests` not already on the registry, with up
/// to [`MAX_CONCURRENT_BLOB_UPLOADS`] real, independent connections in
/// flight at once — the exact mirror of `oci_registry::pull`'s own
/// `fetch_blobs_concurrently` (`docs/design/0256`/`0257`): `client`
/// (the caller's own) handles the first digest inline with zero extra
/// threads when there's only one blob to push (or the bound is
/// otherwise 1); every other worker gets its own independent
/// [`Client`] via [`Client::duplicate_for_worker`] (same credentials/
/// insecure-host set, a fresh connection pool). `oci_store::Store`
/// needs no locking (`open_blob` just opens a fresh, independent
/// `File` handle per call — no shared cursor or state two threads
/// could race on). The first real error stops every worker from
/// starting a *new* upload (a shared `AtomicBool`); an already-in-
/// flight upload runs to completion rather than being force-cancelled.
/// Returns the first error encountered, matching the original
/// sequential loop's own fail-on-first-error behavior exactly.
fn push_blobs_concurrently(
    client: &mut Client,
    store: &Store,
    reference: &Reference,
    digests: &[&Digest],
) -> Result<(), PushError> {
    let worker_count = digests.len().min(MAX_CONCURRENT_BLOB_UPLOADS);
    if worker_count <= 1 {
        for digest in digests {
            push_blob_if_missing(client, store, reference, digest)?;
        }
        return Ok(());
    }

    let next_index = AtomicUsize::new(1);
    let failed = AtomicBool::new(false);
    let first_result = push_blob_if_missing(client, store, reference, digests[0]);
    if first_result.is_err() {
        failed.store(true, Ordering::Relaxed);
    }

    let worker = |client: &mut Client| -> Result<(), PushError> {
        loop {
            if failed.load(Ordering::Relaxed) {
                return Ok(());
            }
            let idx = next_index.fetch_add(1, Ordering::Relaxed);
            let Some(digest) = digests.get(idx) else {
                return Ok(());
            };
            if let Err(e) = push_blob_if_missing(client, store, reference, digest) {
                failed.store(true, Ordering::Relaxed);
                return Err(e);
            }
        }
    };

    // Built up front (owned, one per worker thread), the same reason
    // `pull`'s own `fetch_blobs_concurrently` does: `client` is a
    // unique `&mut Client` borrow, so it can't be reborrowed from more
    // than one concurrently-running closure at once.
    let worker_clients: Vec<Client> = (1..worker_count)
        .map(|_| client.duplicate_for_worker())
        .collect();
    let worker_results: Vec<Result<(), PushError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = worker_clients
            .into_iter()
            .map(|mut worker_client| scope.spawn(move || worker(&mut worker_client)))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("blob-push worker thread panicked"))
            .collect()
    });

    // Report the first real error in the same, deterministic order the
    // original sequential loop would have hit it (by digest index).
    first_result?;
    for result in worker_results {
        result?;
    }
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
    use std::time::Duration;

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
            Self::start_with_upload_delay(already_has, Duration::ZERO)
        }

        /// Like [`Self::start`], but every real blob upload `PUT`
        /// (`.../blobs/uploads/...`) sleeps for `delay` before
        /// responding — the artificial "slow upload" this module's own
        /// concurrency test needs to prove real wall-clock overlap
        /// (the exact mirror of `pull`'s own `MockRegistry::
        /// start_with_delays`, `docs/design/0256`/`0257`). Each
        /// accepted connection is handled on its own thread (not one
        /// shared accept-loop thread serially), needed for the same
        /// reason: several real, concurrent connections must actually
        /// be served concurrently, not accidentally serialized by the
        /// mock itself.
        fn start_with_upload_delay(already_has: HashSet<String>, delay: Duration) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let already_has = Arc::new(already_has);
            let uploaded = Arc::new(Mutex::new(HashSet::new()));
            let manifest_puts = Arc::new(Mutex::new(Vec::new()));
            let uploaded_for_accept_loop = Arc::clone(&uploaded);
            let manifest_puts_for_accept_loop = Arc::clone(&manifest_puts);
            thread::spawn(move || {
                while let Ok((stream, _)) = listener.accept() {
                    let already_has = Arc::clone(&already_has);
                    let uploaded = Arc::clone(&uploaded_for_accept_loop);
                    let manifest_puts = Arc::clone(&manifest_puts_for_accept_loop);
                    thread::spawn(move || {
                        Self::handle(stream, &already_has, &uploaded, &manifest_puts, delay)
                    });
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
            upload_delay: Duration,
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
                if !upload_delay.is_zero() {
                    thread::sleep(upload_delay);
                }
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

    /// The real point of `docs/design/0257`: pushing a multi-layer
    /// image's own several independent blobs genuinely overlaps in
    /// wall-clock time rather than serializing them one at a time —
    /// the exact mirror of `pull`'s own equivalent proof
    /// (`docs/design/0256`): five blobs (config + four layers), each
    /// taking a real, deliberate 200ms to upload, over
    /// `MAX_CONCURRENT_BLOB_UPLOADS` (5) simultaneous connections.
    /// Sequential would take at least 5 * 200ms = 1000ms; bounded
    /// concurrency finishes in essentially one real round (~200ms) —
    /// asserting a generous 700ms upper bound leaves ample margin
    /// above the ideal while staying comfortably below what a
    /// sequential push could ever achieve.
    #[test]
    fn push_uploads_multiple_blobs_concurrently_not_sequentially() {
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

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let config_ingested = store.ingest(&config_bytes[..]).unwrap();

        let layer_count = 4;
        let layer_digests: Vec<Digest> = (0..layer_count)
            .map(|i| {
                let bytes = format!("layer contents #{i}").into_bytes();
                store.ingest(&bytes[..]).unwrap().digest
            })
            .collect();

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
            layers: layer_digests
                .iter()
                .map(|digest| Descriptor {
                    media_type: MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
                    digest: digest.clone(),
                    size: 0,
                    urls: vec![],
                    annotations: BTreeMap::new(),
                    platform: None,
                })
                .collect(),
            annotations: BTreeMap::new(),
        };
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_ingested = store.ingest(&manifest_bytes[..]).unwrap();

        let mock =
            MockRegistry::start_with_upload_delay(HashSet::new(), Duration::from_millis(200));
        let reference = Reference::parse(&format!("{}/testrepo:latest", mock.addr)).unwrap();
        let record = ImageRecord {
            reference: reference.to_string(),
            manifest_digest: manifest_ingested.digest,
        };

        let mut client =
            Client::with_options(Credentials::empty(), std::iter::once(mock.addr.to_string()));
        let started = std::time::Instant::now();
        push(&mut client, &store, &reference, &record).unwrap();
        let elapsed = started.elapsed();

        let uploaded = mock.uploaded.lock().unwrap();
        assert!(uploaded.contains(&config_ingested.digest.to_string()));
        for digest in &layer_digests {
            assert!(uploaded.contains(&digest.to_string()), "{digest}");
        }
        assert!(
            elapsed < Duration::from_millis(700),
            "expected real concurrent overlap (~200ms for 5 blobs over 5 workers), \
             got {elapsed:?} -- looks sequential (5 * 200ms = 1000ms)"
        );
    }
}
