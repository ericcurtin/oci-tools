//! Pull orchestration: resolve a reference against a registry, follow a
//! multi-platform index down to the platform we run on if necessary, and
//! land the manifest/config/layers in an `oci_store::Store`.
//!
//! Shared by `ociman pull`/`images`/`inspect` today; `ocicri`'s
//! ImageService and `ociboot upgrade`/`switch` reuse it later — every
//! binary that needs to pull an image goes through exactly this code path,
//! never re-implements it. [`resolve_or_pull`] (0204) is the next level
//! up: "the image I'm about to use, resolved against local storage and
//! possibly freshly pulled according to a policy" — `ociman run`/
//! `create`/`build`'s own shared need, moved here from being `ociman`-
//! private so `ocibox create` and `ocicri`'s own ImageService can reuse
//! the exact same policy decision tree without reimplementing it.

use oci_spec_types::image::{ImageManifest, Manifest, Platform};
use oci_spec_types::{Digest, Reference};
use oci_store::{ImageRecord, Store};

use crate::{Client, Credentials, RegistryError};

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
    /// [`resolve_or_pull`]'s own [`PullPolicy::Never`] and `reference`
    /// isn't already in local storage — deliberately generic wording
    /// (never a specific "run `<binary> pull` first" suggestion baked
    /// in here, since that command name differs per caller binary);
    /// a caller that wants one should match this variant specifically
    /// and add its own.
    #[error("{reference}: no such image in local storage")]
    NotFoundLocally {
        /// The reference that was being resolved.
        reference: String,
    },
}

/// Real image-pull policy — matching real `podman run --pull`/`podman
/// build --pull` exactly (checked directly against a real installed
/// `podman`): [`Missing`](Self::Missing) pulls only if the reference
/// isn't already in local storage; [`Always`](Self::Always) pulls
/// unconditionally, even when already present (confirmed directly: a
/// real `podman run --pull always`/`podman build --pull=always`
/// against an already-pulled image still shows a real "Trying to
/// pull..." line); [`Never`](Self::Never) never pulls at all, failing
/// with a clear error if the reference isn't already present;
/// [`Newer`](Self::Newer) pulls only if the registry's own current
/// manifest has a *different digest* than what's already stored
/// locally — never a timestamp comparison, checked directly against
/// real podman/buildah's own current source
/// (`hasDifferentDigestWithSystemContext`, `~/git/podman/vendor/
/// go.podman.io/common/libimage/image.go`) — a real registry request
/// is always made when something is already present (there's no
/// cheaper way to know without one), but never a real blob download
/// unless the digest actually differs. This project's own CLI-facing
/// enums (e.g. `ociman`'s own `PullPolicy`, which additionally derives
/// `clap::ValueEnum`) stay defined per-binary and convert into this
/// one at the CLI boundary — shared library crates deliberately never
/// depend on `clap` at all (this project's own established
/// convention, `oci-cli-common` alone among `crates/` needs it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullPolicy {
    /// Pull unconditionally, even if already present locally.
    Always,
    /// Pull only if not already present locally (the default almost
    /// every caller wants).
    Missing,
    /// Never pull; a clear [`PullError::NotFoundLocally`] if nothing
    /// is already present.
    Never,
    /// Pull only if the registry's own current manifest has a
    /// different digest than what's already stored locally.
    Newer,
}

/// A real registry client for `registry_host`, honoring `tls_verify`
/// the same way `docker pull --tls-verify=false`/`podman pull
/// --tls-verify=false` do — plain HTTP only for that one host when
/// `false` (the escape hatch a local/private development registry
/// commonly needs, scoped to just the one registry actually being
/// talked to, never a blanket "every registry is insecure" toggle),
/// HTTPS-with-credentials otherwise.
pub fn client_for(registry_host: &str, tls_verify: bool) -> Client {
    let credentials = Credentials::load();
    if tls_verify {
        Client::with_credentials(credentials)
    } else {
        Client::with_options(credentials, std::iter::once(registry_host.to_string()))
    }
}

/// [`pull`], but resolving its own [`Client`] from `reference`'s own
/// registry host and `tls_verify` first (via [`client_for`]) — the
/// "just pull it, unconditionally, no policy decision needed" building
/// block both [`resolve_or_pull`] and any caller not going through a
/// policy at all (e.g. `ociman pull` itself) share.
pub fn pull_unconditionally(
    store: &Store,
    reference: &Reference,
    tls_verify: bool,
) -> Result<ImageRecord, PullError> {
    let mut client = client_for(reference.registry_host(), tls_verify);
    pull(&mut client, store, reference, &Platform::host())
}

/// Look `reference` up in `store`, pulling it according to
/// `pull_policy` if needed — every binary that needs "the image I'm
/// about to use, resolved and possibly freshly pulled" goes through
/// this one decision tree (see this module's own doc comment).
///
/// `pull_now` performs the actual unconditional pull whenever the
/// policy decides one is needed (may be called zero, one, or — for
/// [`PullPolicy::Newer`], if the registry check itself fails — zero
/// times) — injected rather than always calling [`pull_unconditionally`]
/// directly so a caller that wants its own progress UI around a real
/// pull (`ociman`'s own spinner, e.g.) can wrap it there; a caller with
/// no such UI can simply pass `|| pull_unconditionally(store,
/// reference, tls_verify)` verbatim.
pub fn resolve_or_pull(
    store: &Store,
    reference: &Reference,
    pull_policy: PullPolicy,
    tls_verify: bool,
    mut pull_now: impl FnMut() -> Result<ImageRecord, PullError>,
) -> Result<ImageRecord, PullError> {
    let local = store.resolve_image(&reference.to_string())?;
    match pull_policy {
        PullPolicy::Never => local.ok_or_else(|| PullError::NotFoundLocally {
            reference: reference.to_string(),
        }),
        PullPolicy::Missing => match local {
            Some(record) => Ok(record),
            None => pull_now(),
        },
        PullPolicy::Always => pull_now(),
        PullPolicy::Newer => {
            let Some(record) = local else {
                return pull_now();
            };
            let mut client = client_for(reference.registry_host(), tls_verify);
            let different = has_different_digest(
                &mut client,
                reference,
                &Platform::host(),
                &record.manifest_digest,
            )?;
            if different { pull_now() } else { Ok(record) }
        }
    }
}

/// Resolve `reference` (for `platform`) down to the single, real
/// manifest that would actually be pulled — following a multi-platform
/// index down to the one matching `platform` if the top-level
/// reference serves one — returning its own real bytes, (always
/// locally computed, per [`PulledManifest`]'s own doc comment) digest,
/// and already-parsed [`ImageManifest`] (so neither caller below ever
/// parses the same bytes twice), without writing anything to a
/// [`Store`] at all. Shared by [`pull`] itself (which pulls exactly
/// this manifest, then also fetches its config/layer blobs) and
/// [`has_different_digest`] (which only ever needs the digest, never
/// a blob).
fn resolve_manifest(
    client: &mut Client,
    reference: &Reference,
    platform: &Platform,
) -> Result<(Vec<u8>, Digest, ImageManifest), PullError> {
    let top = client.pull_manifest(reference)?;
    let parsed = Manifest::parse(&top.bytes, top.content_type.as_deref()).map_err(|source| {
        PullError::InvalidJson {
            what: "manifest",
            source,
        }
    })?;

    match parsed {
        Manifest::Image(image) => Ok((top.bytes, top.digest, *image)),
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
            Ok((child.bytes, child.digest, image))
        }
    }
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
    let (manifest_bytes, _digest, manifest) = resolve_manifest(client, reference, platform)?;

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

/// Whether `reference`'s own real, current manifest on the registry
/// (resolved down to `platform` exactly like [`pull`] itself does, if
/// the top-level reference serves a multi-platform index) has a
/// different digest than `local_digest` — the real check real
/// podman/buildah's own `--pull newer` policy performs
/// (`hasDifferentDigestWithSystemContext`, `~/git/podman/vendor/
/// go.podman.io/common/libimage/image.go`, read directly): comparing
/// digests, never a timestamp — a real registry request is always
/// made (there is no cheaper way to know without one), but never a
/// blob download unless the digest actually turns out to differ (left
/// to a subsequent real [`pull`] call, not performed here).
pub fn has_different_digest(
    client: &mut Client,
    reference: &Reference,
    platform: &Platform,
    local_digest: &Digest,
) -> Result<bool, PullError> {
    let (_, remote_digest, _manifest) = resolve_manifest(client, reference, platform)?;
    Ok(&remote_digest != local_digest)
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

    fn single_layer_manifest_routes(
        marker: &[u8],
    ) -> (HashMap<String, (&'static str, Vec<u8>)>, Digest) {
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
        let layer_bytes = marker.to_vec();
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
        let manifest_digest = sha256(&manifest_bytes);

        let mut routes = HashMap::new();
        routes.insert(
            "/v2/testrepo/manifests/latest".to_string(),
            (MEDIA_TYPE_IMAGE_MANIFEST, manifest_bytes),
        );
        routes.insert(
            format!("/v2/testrepo/blobs/{config_digest}"),
            ("application/octet-stream", config_bytes),
        );
        routes.insert(
            format!("/v2/testrepo/blobs/{layer_digest}"),
            ("application/octet-stream", layer_bytes),
        );
        (routes, manifest_digest)
    }

    #[test]
    fn has_different_digest_is_false_when_the_remote_manifest_matches_local() {
        let (routes, manifest_digest) = single_layer_manifest_routes(b"same content");
        let mock = MockRegistry::start(routes);
        let mut client =
            Client::with_options(Credentials::empty(), std::iter::once(mock.addr.to_string()));
        let reference = Reference::parse(&format!("{}/testrepo:latest", mock.addr)).unwrap();

        let different =
            has_different_digest(&mut client, &reference, &Platform::host(), &manifest_digest)
                .unwrap();
        assert!(!different);
    }

    #[test]
    fn has_different_digest_is_true_when_the_remote_manifest_differs() {
        let (routes, _remote_manifest_digest) = single_layer_manifest_routes(b"new content");
        let mock = MockRegistry::start(routes);
        let mut client =
            Client::with_options(Credentials::empty(), std::iter::once(mock.addr.to_string()));
        let reference = Reference::parse(&format!("{}/testrepo:latest", mock.addr)).unwrap();

        // A digest that plainly doesn't match anything the mock serves --
        // standing in for "whatever this project's own local storage
        // already had from a previous, different pull".
        let stale_local_digest = sha256(b"a completely different, stale local manifest");
        let different = has_different_digest(
            &mut client,
            &reference,
            &Platform::host(),
            &stale_local_digest,
        )
        .unwrap();
        assert!(different);
    }

    #[test]
    fn has_different_digest_never_fetches_a_blob_at_all() {
        // Only the manifest route exists -- if `has_different_digest`
        // ever tried to fetch a blob (it never should), this mock would
        // 404 and the call would fail instead of returning a real,
        // successful `bool`.
        let (mut routes, manifest_digest) = single_layer_manifest_routes(b"irrelevant content");
        routes.retain(|path, _| path.contains("/manifests/"));
        let mock = MockRegistry::start(routes);
        let mut client =
            Client::with_options(Credentials::empty(), std::iter::once(mock.addr.to_string()));
        let reference = Reference::parse(&format!("{}/testrepo:latest", mock.addr)).unwrap();

        let different =
            has_different_digest(&mut client, &reference, &Platform::host(), &manifest_digest)
                .unwrap();
        assert!(!different);
    }

    /// [`resolve_or_pull`]'s own policy decision tree needs no real
    /// registry at all for `Never`/`Missing`/`Always` (only `Newer`
    /// ever makes a real registry request, already covered by
    /// `has_different_digest`'s own tests above) — `pull_now` is a
    /// plain counting closure here, confirming exactly how many times
    /// (zero, or exactly one) each policy actually invokes it.
    fn fake_record(reference: &Reference) -> ImageRecord {
        ImageRecord {
            reference: reference.to_string(),
            manifest_digest: sha256(b"fake"),
        }
    }

    #[test]
    fn resolve_or_pull_never_returns_the_local_record_without_calling_pull_now() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let reference = Reference::parse("example.com/testrepo:latest").unwrap();
        let local = fake_record(&reference);
        store.put_image(&local).unwrap();

        let mut pull_now_calls = 0;
        let record = resolve_or_pull(&store, &reference, PullPolicy::Never, true, || {
            pull_now_calls += 1;
            unreachable!("Never must not pull when already present locally")
        })
        .unwrap();
        assert_eq!(record, local);
        assert_eq!(pull_now_calls, 0);
    }

    #[test]
    fn resolve_or_pull_never_errors_clearly_when_nothing_is_stored_locally() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let reference = Reference::parse("example.com/testrepo:latest").unwrap();

        let err = resolve_or_pull(&store, &reference, PullPolicy::Never, true, || {
            unreachable!("Never must never pull at all")
        })
        .unwrap_err();
        assert!(matches!(err, PullError::NotFoundLocally { .. }));
    }

    #[test]
    fn resolve_or_pull_missing_returns_the_local_record_without_pulling() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let reference = Reference::parse("example.com/testrepo:latest").unwrap();
        let local = fake_record(&reference);
        store.put_image(&local).unwrap();

        let record = resolve_or_pull(&store, &reference, PullPolicy::Missing, true, || {
            unreachable!("Missing must not pull when already present locally")
        })
        .unwrap();
        assert_eq!(record, local);
    }

    #[test]
    fn resolve_or_pull_missing_calls_pull_now_exactly_once_when_nothing_is_stored() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let reference = Reference::parse("example.com/testrepo:latest").unwrap();

        let mut pull_now_calls = 0;
        let record = resolve_or_pull(&store, &reference, PullPolicy::Missing, true, || {
            pull_now_calls += 1;
            Ok(fake_record(&reference))
        })
        .unwrap();
        assert_eq!(record.reference, reference.to_string());
        assert_eq!(pull_now_calls, 1);
    }

    #[test]
    fn resolve_or_pull_always_calls_pull_now_even_when_already_present_locally() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let reference = Reference::parse("example.com/testrepo:latest").unwrap();
        store.put_image(&fake_record(&reference)).unwrap();

        let mut pull_now_calls = 0;
        resolve_or_pull(&store, &reference, PullPolicy::Always, true, || {
            pull_now_calls += 1;
            Ok(fake_record(&reference))
        })
        .unwrap();
        assert_eq!(pull_now_calls, 1);
    }
}
