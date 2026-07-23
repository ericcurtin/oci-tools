//! Serde types for the OCI image, runtime, and distribution specifications.
//!
//! Scope shipped so far:
//! - [`digest`]: content digests (`sha256:...`), streaming hashing
//! - [`image`]: descriptors, manifests, image indexes, image config,
//!   media types (milestone 2)
//! - [`reference`]: Docker/OCI image reference parsing and normalization
//!   (milestone 2)
//! - [`runtime`]: runtime-spec `config.json` — currently just what
//!   `ocirun spec` needs (process/root/mounts/namespaces/ID-mappings/
//!   device-cgroup allow-list); full resource limits, seccomp, and hooks
//!   land with actual container creation (milestone 3, in progress)
//! - [`time`]: RFC 3339 UTC timestamp formatting, without a date/time
//!   dependency (shared by `oci-runtime-core`'s own state file and
//!   this crate's own [`image::ImageConfig`]/[`image::HistoryEntry`]
//!   `created` fields)
//!
//! Planned:
//! - distribution-spec: tag lists, error payloads, auth challenge parsing
//!   beyond what `oci-registry` already needs internally
//!
//! This crate is pure data: serde types plus validation, no I/O. Every other
//! crate that touches OCI objects (`oci-registry`, `oci-store`,
//! `oci-runtime-core`, `oci-dockerfile`, `ociboot`) consumes these types so
//! there is exactly one definition of each spec structure in the workspace.

pub mod digest;
pub mod image;
pub mod reference;
pub mod runtime;
pub mod time;

pub use digest::{Algorithm, Digest};
pub use reference::Reference;
pub use time::{format_rfc3339_nanos_utc, format_rfc3339_utc};
