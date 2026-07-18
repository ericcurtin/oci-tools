//! Serde types for the OCI image, runtime, and distribution specifications.
//!
//! **Status: stub** — implemented in milestone 2 (see `docs/design/`).
//!
//! Planned scope:
//! - image-spec: descriptors, manifests, image indexes, image config,
//!   media types, annotations, digest/reference parsing and validation
//! - runtime-spec: `config.json` (process, mounts, namespaces, cgroups,
//!   seccomp, hooks) shared by `oci-runtime-core` and `ocirun`
//! - distribution-spec: tag lists, error payloads, auth challenge parsing
//!
//! This crate is pure data: serde types plus validation, no I/O. Every other
//! crate that touches OCI objects (`oci-registry`, `oci-store`,
//! `oci-runtime-core`, `oci-dockerfile`, `ociboot`) consumes these types so
//! there is exactly one definition of each spec structure in the workspace.
