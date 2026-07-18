//! Serde types for the OCI image, runtime, and distribution specifications.
//!
//! Scope shipped so far (milestone 2):
//! - [`digest`]: content digests (`sha256:...`), streaming hashing
//! - [`image`]: descriptors, manifests, image indexes, image config,
//!   media types
//! - [`reference`]: Docker/OCI image reference parsing and normalization
//!
//! Planned (milestone 3+):
//! - runtime-spec: `config.json` (process, mounts, namespaces, cgroups,
//!   seccomp, hooks) shared by `oci-runtime-core` and `ocirun`
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

pub use digest::{Algorithm, Digest};
pub use reference::Reference;
