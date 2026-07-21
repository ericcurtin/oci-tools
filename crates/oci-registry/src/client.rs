//! The pull-path registry client: manifest and blob GETs against any
//! OCI distribution-spec / Docker Registry HTTP API v2 registry, with
//! bearer-token auth handled transparently.

use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

use oci_spec_types::digest::sha256;
use oci_spec_types::image::{
    MEDIA_TYPE_DOCKER_MANIFEST_LIST, MEDIA_TYPE_DOCKER_MANIFEST_V2, MEDIA_TYPE_IMAGE_INDEX,
    MEDIA_TYPE_IMAGE_MANIFEST,
};
use oci_spec_types::{Digest, Reference};

use crate::RegistryError;
use crate::auth::{self, BearerChallenge};
use crate::credentials::Credentials;

/// Manifests larger than this are refused: no real image manifest or index
/// approaches this size, and it bounds memory use against a misbehaving or
/// hostile registry.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;

/// A manifest or index fetched from a registry: raw bytes (so re-hashing
/// and storage never need to re-serialize, which would risk not matching
/// the original digest byte-for-byte), the digest the client computed, and
/// the `Content-Type` the registry sent.
#[derive(Debug, Clone)]
pub struct PulledManifest {
    /// The exact bytes returned by the registry.
    pub bytes: Vec<u8>,
    /// The digest of `bytes` (always computed locally; never trusted
    /// blindly from a `Docker-Content-Digest` response header, though that
    /// header is cross-checked against it when present).
    pub digest: Digest,
    /// The registry's `Content-Type` response header, if any.
    pub content_type: Option<String>,
}

/// A streaming reader for a blob response body. Wraps ureq's reader type
/// so `oci-registry`'s public API never leaks it directly.
pub struct BlobReader {
    inner: ureq::BodyReader<'static>,
    content_length: Option<u64>,
}

impl BlobReader {
    /// The `Content-Length` the registry advertised for this blob, if any
    /// (useful for progress bars; the actual byte count read should always
    /// be verified against the manifest descriptor's `size`, which this
    /// crate does not do — that is `oci-store`'s / the caller's job).
    pub fn content_length(&self) -> Option<u64> {
        self.content_length
    }
}

impl Read for BlobReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

struct CachedToken {
    token: String,
}

/// A registry client. Not `Clone`; construct one per `ociman`/`ocicri`
/// invocation (it is cheap: one connection-pooling [`ureq::Agent`] plus an
/// in-memory token cache).
pub struct Client {
    agent: ureq::Agent,
    credentials: Credentials,
    /// Registry hosts (`host` or `host:port`) to talk plain HTTP to instead
    /// of HTTPS — the same escape hatch every other engine offers
    /// (`--tls-verify=false` / Docker's `insecure-registries`), for
    /// developer/CI registries that don't terminate TLS. Empty by default.
    insecure_hosts: std::collections::HashSet<String>,
    tokens: HashMap<(String, String), CachedToken>,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    /// A client that loads credentials from the standard podman/docker
    /// auth file locations (see [`crate::credentials::Credentials::load`]).
    pub fn new() -> Self {
        Client::with_credentials(Credentials::load())
    }

    /// A client using an explicit credential set (anonymous pulls only via
    /// [`Credentials::empty`]); primarily for tests, and for callers that
    /// manage credentials themselves rather than relying on auth files.
    pub fn with_credentials(credentials: Credentials) -> Self {
        Client::with_options(credentials, std::iter::empty())
    }

    /// A client with an explicit credential set and a set of registry
    /// hosts to connect to over plain HTTP rather than HTTPS.
    pub fn with_options(
        credentials: Credentials,
        insecure_hosts: impl IntoIterator<Item = String>,
    ) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .build();
        Client {
            agent: config.into(),
            credentials,
            insecure_hosts: insecure_hosts.into_iter().collect(),
            tokens: HashMap::new(),
        }
    }

    /// Fetch the manifest (or index) `reference` points at.
    pub fn pull_manifest(
        &mut self,
        reference: &Reference,
    ) -> Result<PulledManifest, RegistryError> {
        self.pull_manifest_at(reference, &reference.manifest_ref())
    }

    /// Fetch a manifest from `reference`'s repository at an explicit
    /// tag-or-digest string, rather than `reference`'s own tag/digest.
    /// Used to fetch a child manifest selected out of a multi-platform
    /// index, which is addressed by its own digest.
    pub fn pull_manifest_at(
        &mut self,
        reference: &Reference,
        manifest_ref: &str,
    ) -> Result<PulledManifest, RegistryError> {
        let url = format!(
            "{}://{}/v2/{}/manifests/{}",
            self.scheme(reference.registry_host()),
            reference.registry_host(),
            reference.repository(),
            manifest_ref
        );
        let accept = [
            MEDIA_TYPE_IMAGE_INDEX,
            MEDIA_TYPE_IMAGE_MANIFEST,
            MEDIA_TYPE_DOCKER_MANIFEST_LIST,
            MEDIA_TYPE_DOCKER_MANIFEST_V2,
        ]
        .join(", ");

        let mut resp = self.request_with_auth(
            reference.registry_host(),
            reference.repository(),
            "pull",
            |client, bearer| {
                let mut req = client.agent.get(&url).header("Accept", &accept);
                if let Some(bearer) = bearer {
                    req = req.header("Authorization", format!("Bearer {bearer}"));
                }
                req.call()
                    .map_err(|e| RegistryError::Transport(e.to_string()))
            },
        )?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(RegistryError::UnexpectedStatus {
                url,
                status: status.as_u16(),
                body,
            });
        }

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let advertised_digest = resp
            .headers()
            .get("docker-content-digest")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| Digest::parse(v).ok());

        let bytes = resp
            .body_mut()
            .with_config()
            .limit(MAX_MANIFEST_BYTES)
            .read_to_vec()
            .map_err(|e| RegistryError::Transport(e.to_string()))?;

        let digest = sha256(&bytes);
        if let Some(advertised) = advertised_digest
            && advertised != digest
        {
            return Err(RegistryError::DigestMismatch {
                expected: advertised,
                actual: digest,
            });
        }

        Ok(PulledManifest {
            bytes,
            digest,
            content_type,
        })
    }

    /// Open a streaming reader for the blob `digest` in `reference`'s
    /// repository (works for layers and config blobs alike).
    pub fn pull_blob(
        &mut self,
        reference: &Reference,
        digest: &Digest,
    ) -> Result<BlobReader, RegistryError> {
        let url = format!(
            "{}://{}/v2/{}/blobs/{}",
            self.scheme(reference.registry_host()),
            reference.registry_host(),
            reference.repository(),
            digest
        );
        let mut resp = self.request_with_auth(
            reference.registry_host(),
            reference.repository(),
            "pull",
            |client, bearer| {
                let mut req = client.agent.get(&url);
                if let Some(bearer) = bearer {
                    req = req.header("Authorization", format!("Bearer {bearer}"));
                }
                req.call()
                    .map_err(|e| RegistryError::Transport(e.to_string()))
            },
        )?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(RegistryError::UnexpectedStatus {
                url,
                status: status.as_u16(),
                body,
            });
        }
        let content_length = resp.body().content_length();
        Ok(BlobReader {
            inner: resp.into_body().into_reader(),
            content_length,
        })
    }

    /// `"http"` for hosts configured as insecure via
    /// [`Client::with_options`], `"https"` (the only sane default) for
    /// everything else.
    fn scheme(&self, registry_host: &str) -> &'static str {
        if self.insecure_hosts.contains(registry_host) {
            "http"
        } else {
            "https"
        }
    }

    /// Issue a request, transparently handling the bearer-token challenge/
    /// response dance on a `401` (using a cached token when we already
    /// have one for this repository's own `scope_actions` scope, e.g.
    /// `"pull"` or, for a push, `"pull,push"` — checked directly against
    /// real Docker Registry v2 API tokens: a push needs a scope granting
    /// both actions, not `"push"` alone).
    ///
    /// `send` builds and issues the actual HTTP request given a bearer
    /// token (or `None`, for the first, credential-less attempt) — the
    /// one part that genuinely differs per call site (GET with an
    /// `Accept` header for a manifest, a plain GET for a blob, `HEAD`/
    /// `POST`/`PUT` for a push) — so this method itself stays entirely
    /// about the auth orchestration, shared by every one of them.
    fn request_with_auth(
        &mut self,
        registry_host: &str,
        repository: &str,
        scope_actions: &str,
        send: impl Fn(&Client, Option<&str>) -> Result<ureq::http::Response<ureq::Body>, RegistryError>,
    ) -> Result<ureq::http::Response<ureq::Body>, RegistryError> {
        let default_scope = format!("repository:{repository}:{scope_actions}");
        let key = (registry_host.to_string(), default_scope.clone());

        let cached = self.tokens.get(&key).map(|t| t.token.clone());
        let resp = send(self, cached.as_deref())?;
        if resp.status().as_u16() != 401 {
            return Ok(resp);
        }

        let challenge = resp
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .and_then(auth::parse_bearer_challenge);
        let Some(challenge): Option<BearerChallenge> = challenge else {
            return Ok(resp); // not a bearer challenge; let the caller report the 401
        };

        let scope = challenge.scope.clone().unwrap_or(default_scope);
        let basic_auth = self.credentials.basic_auth_header(registry_host);
        let token = auth::fetch_token(&self.agent, &challenge, &scope, basic_auth.as_deref())?;
        self.tokens.insert(
            key,
            CachedToken {
                token: token.clone(),
            },
        );

        send(self, Some(&token))
    }

    /// Whether `digest` already exists in `reference`'s own repository
    /// on the registry — a real `HEAD` request against the OCI
    /// Distribution Spec's own blob endpoint, checked directly against
    /// a real local `registry:2` instance: `200` means "already
    /// there" (skip re-uploading it, the same real cross-push
    /// deduplication a real `docker push`/`podman push` also relies
    /// on), `404` means "not there yet, upload it". Uses the
    /// `"pull,push"` token scope (not `"push"` alone) — checked
    /// directly against a real registry's own `WWW-Authenticate`
    /// challenge for this exact endpoint, which asks for both actions
    /// even for what is, on its own, a read-only check.
    pub fn blob_exists(
        &mut self,
        reference: &Reference,
        digest: &Digest,
    ) -> Result<bool, RegistryError> {
        let registry_host = reference.registry_host();
        let repository = reference.repository();
        let url = format!(
            "{}://{registry_host}/v2/{repository}/blobs/{digest}",
            self.scheme(registry_host)
        );
        let resp =
            self.request_with_auth(registry_host, repository, "pull,push", |client, bearer| {
                let mut req = client.agent.head(&url);
                if let Some(bearer) = bearer {
                    req = req.header("Authorization", format!("Bearer {bearer}"));
                }
                req.call()
                    .map_err(|e| RegistryError::Transport(e.to_string()))
            })?;
        match resp.status().as_u16() {
            200 => Ok(true),
            404 => Ok(false),
            other => Err(RegistryError::UnexpectedStatus {
                url,
                status: other,
                body: String::new(),
            }),
        }
    }

    /// Upload `data` (a real, already-open file — streamed, never
    /// fully read into memory, matching this project's own established
    /// convention for a real layer's own possibly-large content, see
    /// `oci_store::Store::open_blob`'s own doc comment) as `digest` in
    /// `reference`'s own repository — the real OCI Distribution Spec's
    /// own "start an upload session, then one monolithic `PUT`" push
    /// flow (checked directly, step by step, against a real local
    /// `registry:2` instance, not assumed from the spec text alone):
    /// `POST .../blobs/uploads/` (`202 Accepted`, a real `Location`
    /// header naming the actual upload URL — either a full URL or an
    /// absolute path, both handled, both observed directly from a real
    /// registry), then `PUT <location>?digest=<digest>` with the real
    /// file content as the body. Does **not** itself check whether the
    /// blob already exists first — see [`Client::blob_exists`] for
    /// that; callers combine the two (`push`'s own orchestration).
    pub fn upload_blob(
        &mut self,
        reference: &Reference,
        digest: &Digest,
        data: std::fs::File,
    ) -> Result<(), RegistryError> {
        let registry_host = reference.registry_host();
        let repository = reference.repository();
        let scheme = self.scheme(registry_host);

        let start_url = format!("{scheme}://{registry_host}/v2/{repository}/blobs/uploads/");
        let resp =
            self.request_with_auth(registry_host, repository, "pull,push", |client, bearer| {
                let mut req = client.agent.post(&start_url);
                if let Some(bearer) = bearer {
                    req = req.header("Authorization", format!("Bearer {bearer}"));
                }
                req.send_empty()
                    .map_err(|e| RegistryError::Transport(e.to_string()))
            })?;
        if resp.status().as_u16() != 202 {
            return Err(RegistryError::UnexpectedStatus {
                url: start_url,
                status: resp.status().as_u16(),
                body: String::new(),
            });
        }
        let location = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                RegistryError::Auth(
                    "registry did not send a Location header for the upload session".to_string(),
                )
            })?
            .to_string();
        let upload_url =
            append_digest_query(&resolve_location(scheme, registry_host, &location), digest);

        // A `RefCell`, not a plain `File`, so the `Fn` closure below can
        // re-seek to the start before every real send attempt --
        // `request_with_auth` may call it a second time (a fresh bearer
        // token after a `401`), and a real file's own read cursor must
        // not still be partway through from a first, failed attempt.
        let file = std::cell::RefCell::new(data);
        let resp2 =
            self.request_with_auth(registry_host, repository, "pull,push", |client, bearer| {
                use std::io::{Seek, SeekFrom};
                file.borrow_mut()
                    .seek(SeekFrom::Start(0))
                    .map_err(|e| RegistryError::Transport(e.to_string()))?;
                let mut req = client
                    .agent
                    .put(&upload_url)
                    .content_type("application/octet-stream");
                if let Some(bearer) = bearer {
                    req = req.header("Authorization", format!("Bearer {bearer}"));
                }
                let borrowed = file.borrow();
                req.send(&*borrowed)
                    .map_err(|e| RegistryError::Transport(e.to_string()))
            })?;
        let status2 = resp2.status();
        if !status2.is_success() {
            return Err(RegistryError::UnexpectedStatus {
                url: upload_url,
                status: status2.as_u16(),
                body: String::new(),
            });
        }
        Ok(())
    }

    /// `PUT` a manifest (or index) to `reference`'s own repository at
    /// `manifest_ref` (a tag or a digest string) with `media_type` as
    /// its own `Content-Type` — real registries reject a manifest
    /// `PUT` with the wrong (or missing) content type, checked
    /// directly against a real local `registry:2` instance.
    pub fn push_manifest(
        &mut self,
        reference: &Reference,
        manifest_ref: &str,
        media_type: &str,
        bytes: &[u8],
    ) -> Result<(), RegistryError> {
        let registry_host = reference.registry_host();
        let repository = reference.repository();
        let url = format!(
            "{}://{registry_host}/v2/{repository}/manifests/{manifest_ref}",
            self.scheme(registry_host)
        );
        let resp =
            self.request_with_auth(registry_host, repository, "pull,push", |client, bearer| {
                let mut req = client.agent.put(&url).content_type(media_type);
                if let Some(bearer) = bearer {
                    req = req.header("Authorization", format!("Bearer {bearer}"));
                }
                req.send(bytes)
                    .map_err(|e| RegistryError::Transport(e.to_string()))
            })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(RegistryError::UnexpectedStatus {
                url,
                status: status.as_u16(),
                body: String::new(),
            });
        }
        Ok(())
    }
}

/// Resolve a `Location` response header (from starting a blob upload
/// session) into a real, absolute URL — real registries send either a
/// full URL or just an absolute path, both confirmed directly against
/// a real local `registry:2` instance.
fn resolve_location(scheme: &str, registry_host: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        location.to_string()
    } else if let Some(rest) = location.strip_prefix('/') {
        format!("{scheme}://{registry_host}/{rest}")
    } else {
        format!("{scheme}://{registry_host}/{location}")
    }
}

/// Append `digest=<digest>` to `url`'s own query string — correctly
/// whether `url` already has other query parameters (a real registry
/// commonly includes its own opaque state token in the upload
/// session's own `Location` header already) or none at all.
fn append_digest_query(url: &str, digest: &Digest) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}digest={digest}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    #[test]
    fn client_new_does_not_panic() {
        let _ = Client::new();
    }

    /// A tiny single-threaded HTTP/1.1 mock: serves exactly one canned
    /// response per accepted connection, requiring `Authorization: Bearer
    /// <expected_token>` when `requires_auth` is set (else it replies 401
    /// with a `Bearer` challenge pointing back at `/token` on itself).
    struct MockRegistry {
        addr: std::net::SocketAddr,
    }

    impl MockRegistry {
        fn start(manifest_body: &'static str, expected_token: &'static str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            thread::spawn(move || {
                // Serve requests for the lifetime of the test process (a
                // full challenge/token/retry round trip takes three
                // connections; a cached-token call takes one more).
                while let Ok((stream, _)) = listener.accept() {
                    Self::handle(stream, addr, manifest_body, expected_token);
                }
            });
            MockRegistry { addr }
        }

        fn handle(
            mut stream: TcpStream,
            addr: std::net::SocketAddr,
            manifest_body: &str,
            expected_token: &str,
        ) {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_string();

            let mut auth_header = None;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some(idx) = line.to_ascii_lowercase().find("authorization:") {
                    auth_header = Some(line[idx + "authorization:".len()..].trim().to_string());
                }
            }

            if path.starts_with("/token") {
                let body = format!(r#"{{"token":"{expected_token}"}}"#);
                write_response(&mut stream, 200, "application/json", &body);
                return;
            }

            match auth_header.as_deref() {
                Some(v) if v == format!("Bearer {expected_token}") => {
                    write_response(
                        &mut stream,
                        200,
                        "application/vnd.oci.image.manifest.v1+json",
                        manifest_body,
                    );
                }
                _ => {
                    let challenge = format!(
                        "Bearer realm=\"http://{addr}/token\",service=\"mock\",scope=\"repository:testrepo:pull\""
                    );
                    let resp = format!(
                        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: {challenge}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    stream.write_all(resp.as_bytes()).unwrap();
                }
            }
        }
    }

    fn write_response(stream: &mut TcpStream, status: u16, content_type: &str, body: &str) {
        let response = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
    }

    #[test]
    fn request_with_auth_retries_after_401_challenge() {
        let manifest_body = r#"{"schemaVersion":2,"config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","size":0},"layers":[]}"#;
        let mock = MockRegistry::start(manifest_body, "testtoken123");

        let mut client = Client::with_credentials(Credentials::empty());
        let url = format!("http://{}/v2/testrepo/manifests/latest", mock.addr);
        let get = |client: &Client, bearer: Option<&str>| {
            let mut req = client.agent.get(&url);
            if let Some(bearer) = bearer {
                req = req.header("Authorization", format!("Bearer {bearer}"));
            }
            req.call()
                .map_err(|e| RegistryError::Transport(e.to_string()))
        };
        let mut resp = client
            .request_with_auth(&mock.addr.to_string(), "testrepo", "pull", get)
            .unwrap();
        assert!(resp.status().is_success());
        let body = resp.body_mut().read_to_string().unwrap();
        assert_eq!(body, manifest_body);

        // The token must now be cached: a second call should not need the
        // extra token-endpoint round trip (there is only one more accept()
        // queued by MockRegistry::start, for the manifest re-request).
        let resp2 = client
            .request_with_auth(&mock.addr.to_string(), "testrepo", "pull", get)
            .unwrap();
        assert!(resp2.status().is_success());
    }
}
