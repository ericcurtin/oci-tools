//! Boot Loader Specification entries, grubenv, and boot counting.
//!
//! **Scope shipped so far** (milestone 5, see `docs/design/0064`):
//! - [`grubenv`] — read/write/atomic-write for the GRUB environment
//!   block, byte-for-byte compatible with the real `grub-editenv`
//!   binary for real, well-formed files (verified directly, not
//!   assumed — there is no written spec for this format).
//!
//! Planned scope (still ahead):
//! - read/write BLS entries under `/boot/loader/entries/` (`title`,
//!   `version`, `linux`, `initrd`, `options`) for `ociboot` deployments
//! - atomic default-entry flips built on [`grubenv`] (upgrade keeps
//!   the previous deployment's entry for rollback)
//! - boot counting: `boot_counter` / `boot_success` grubenv protocol so a
//!   deployment that repeatedly fails to reach `boot-complete.target`
//!   auto-falls-back to the previous deployment
//! - kernel argument (kargs) editing shared by `ociboot kargs` and install
//!
//! External tools (`grub2-mkconfig`, `grub-install`) will be wrapped
//! behind traits here so pure-Rust replacements can be swapped in
//! later, matching `oci-erofs::builder`'s own `ErofsBuilder` shape.

pub mod grubenv;

pub use grubenv::GrubEnv;
