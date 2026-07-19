//! Build and seal erofs images for immutable deployments.
//!
//! **Scope shipped so far** (milestone 5, see `docs/design/0061`,
//! `docs/design/0062`, `docs/design/0063`):
//! - [`builder`] — the [`builder::ErofsBuilder`] trait and
//!   [`builder::MkfsErofs`], the real `mkfs.erofs` CLI backend.
//!   Determinism (same options + same source tree -> bit-identical
//!   image) is verified directly against the real binary, not assumed.
//! - [`verity`] — sealing/verifying a file with fs-verity
//!   ([`verity::enable`]/[`verity::measure`]) via the kernel's own
//!   ioctls directly, no external CLI needed.
//! - [`dmverity`] — a detached dm-verity hash tree
//!   ([`dmverity::format`]/[`dmverity::verify`]) via `veritysetup`, the
//!   fallback for state filesystems that lack fs-verity support at
//!   all -- entirely at the plain-file level, no loop devices or
//!   device-mapper activation needed for sealing/checking.
//!
//! Planned scope (still ahead):
//! - building directly from streamed OCI layers, not just a
//!   materialized directory tree
//! - deriving `timestamp`/`uuid` from a manifest digest (`ociboot`'s
//!   own policy, layered on top of this crate rather than baked in)
//! - a feature-gated pure-Rust writer implementing the same
//!   [`builder::ErofsBuilder`] trait, as an alternative backend
//! - actually mounting a dm-verity-protected image at boot
//!   (`veritysetup open` against loop devices) -- `ociboot-init`'s own
//!   much larger boot-time-flow concern, not this crate's
//! - verification helpers shared by `ociboot` (host side) and
//!   `ociboot-init` (initramfs side)

pub mod builder;
pub mod dmverity;
pub mod verity;

pub use builder::{BuildOptions, ErofsBuilder, MkfsErofs};
