//! Build and seal erofs images for immutable deployments.
//!
//! **Status: stub** — implemented in milestone 5 (see `docs/design/`).
//!
//! Planned scope:
//! - deterministic erofs image creation from a materialized root tree or
//!   streamed OCI layers: same manifest digest in, bit-identical image out
//!   (fixed mtimes, sorted entries, UUID derived from the manifest digest)
//! - backend trait with an `mkfs.erofs` driver implementation first and a
//!   feature-gated pure-Rust writer later (one of the few sanctioned
//!   external-tool escape hatches, wrapped behind a trait)
//! - sealing: fsverity on the image file, with a detached dm-verity hash
//!   tree as fallback when the state filesystem lacks fsverity support
//! - verification helpers shared by `ociboot` (host side) and
//!   `ociboot-init` (initramfs side)
