//! Build and seal erofs images for immutable deployments.
//!
//! **Scope shipped so far** (milestone 5, see `docs/design/0061`,
//! `docs/design/0062`):
//! - [`builder`] — the [`builder::ErofsBuilder`] trait and
//!   [`builder::MkfsErofs`], the real `mkfs.erofs` CLI backend.
//!   Determinism (same options + same source tree -> bit-identical
//!   image) is verified directly against the real binary, not assumed.
//! - [`verity`] — sealing/verifying a file with fs-verity
//!   ([`verity::enable`]/[`verity::measure`]) via the kernel's own
//!   ioctls directly, no external CLI needed.
//!
//! Planned scope (still ahead):
//! - building directly from streamed OCI layers, not just a
//!   materialized directory tree
//! - deriving `timestamp`/`uuid` from a manifest digest (`ociboot`'s
//!   own policy, layered on top of this crate rather than baked in)
//! - a feature-gated pure-Rust writer implementing the same
//!   [`builder::ErofsBuilder`] trait, as an alternative backend
//! - a detached dm-verity hash tree as a fallback for state
//!   filesystems that lack fs-verity support (`veritysetup`, one of
//!   `docs/HACKING.md`'s own sanctioned shellouts)
//! - verification helpers shared by `ociboot` (host side) and
//!   `ociboot-init` (initramfs side)

pub mod builder;
pub mod verity;

pub use builder::{BuildOptions, ErofsBuilder, MkfsErofs};
