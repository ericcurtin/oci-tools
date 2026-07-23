//! OCI distribution (registry) client.
//!
//! [`Client::pull_manifest`]/[`Client::pull_blob`] cover everything
//! `ociman pull`, `ociman images`/`inspect`, and (later) `ocicri`'s
//! ImageService and `ociboot upgrade/switch` need to fetch content
//! from a registry. [`Client::blob_exists`]/[`Client::upload_blob`]/
//! [`Client::push_manifest`] (`ociman push`'s own backing, `docs/
//! design/0127`) cover the other direction.
//!
//! - Bearer token auth (Docker/OAuth2-style `WWW-Authenticate` challenge),
//!   plus HTTP Basic credentials read from the standard podman/docker auth
//!   file locations ([`credentials::Credentials`]) for the initial token
//!   request — shared by both pull and push (a push needs a `"pull,push"`
//!   token scope, not `"push"` alone, checked directly against a real
//!   registry's own challenge for this exact case).
//! - Manifests are fetched as raw bytes and hashed locally — never
//!   re-serialized — so the digest used for storage always matches
//!   byte-for-byte what the registry actually sent; pushing does the
//!   same in reverse, uploading a stored manifest's own already-ingested
//!   bytes verbatim rather than re-serializing them.
//! - Blob downloads are streamed ([`client::BlobReader`] implements
//!   [`std::io::Read`]); callers pipe them straight into
//!   `oci_store::Store::ingest_verified` without buffering full layers in
//!   memory. Blob uploads are streamed the same way, from a real,
//!   already-open [`std::fs::File`].
//!
//! Planned (later milestones): registry mirrors and fallback, retry with
//! backoff, resumable blob downloads/uploads, bounded-concurrency layer
//! fetch.
//!
//! This is the **only** registry client in the workspace: `ociman
//! pull`/`push`, `ocicri`'s ImageService, and `ociboot upgrade/switch`
//! all fetch (or push) through this crate into `oci-store`.

mod auth;
mod client;
pub mod credentials;
pub mod pull;
pub mod push;

pub use client::{BlobReader, Client, PulledManifest};
pub use credentials::Credentials;
use oci_spec_types::Digest;
pub use pull::{
    PullError, PullPolicy, client_for, has_different_digest, pull as pull_image,
    pull_unconditionally, resolve_or_pull,
};
pub use push::{PushError, push as push_image};

/// Errors from registry operations.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// A transport-level failure (DNS, TCP, TLS, malformed HTTP).
    #[error("registry request failed: {0}")]
    Transport(String),
    /// The registry responded with a non-2xx status this client does not
    /// otherwise handle (`401` is handled internally via the bearer-token
    /// flow and only surfaces here if retried auth also failed).
    #[error("registry request to {url} failed: HTTP {status}: {body}")]
    UnexpectedStatus {
        /// The request URL.
        url: String,
        /// The HTTP status code.
        status: u16,
        /// The response body (best-effort; truncated by nothing today, but
        /// callers should not assume it is bounded for hostile servers).
        body: String,
    },
    /// Authentication with the registry failed (bad/missing challenge, bad
    /// credentials, token endpoint error).
    #[error("registry authentication failed: {0}")]
    Auth(String),
    /// Downloaded content did not hash to the digest the caller expected.
    #[error("digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch {
        /// Expected digest (from a manifest descriptor or response header).
        expected: Digest,
        /// Digest actually computed from the downloaded content.
        actual: Digest,
    },
    /// A JSON response (token, error payload) failed to parse.
    #[error("failed to parse registry JSON response: {0}")]
    Json(#[from] serde_json::Error),
}
