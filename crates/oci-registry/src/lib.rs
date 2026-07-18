//! OCI distribution (registry) client.
//!
//! **Status: stub** — implemented in milestone 2 (see `docs/design/`).
//!
//! Planned scope:
//! - pull and push against OCI distribution-spec registries
//! - auth: WWW-Authenticate token flow (Docker/OAuth2 style) and basic auth,
//!   credential storage compatible with `docker login` config files
//! - registry mirrors and fallback, retry with backoff, resumable blob
//!   downloads, bounded-concurrency layer fetch
//!
//! This is the **only** registry client in the workspace: `ociman pull`,
//! `ocicri`'s ImageService, and `ociboot upgrade/switch` all fetch through
//! this crate into `oci-store`.
