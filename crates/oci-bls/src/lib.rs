//! Boot Loader Specification entries, grubenv, and boot counting.
//!
//! **Scope shipped so far** (milestone 5, see `docs/design/0064`,
//! `docs/design/0065`):
//! - [`grubenv`] — read/write/atomic-write for the GRUB environment
//!   block, byte-for-byte compatible with the real `grub-editenv`
//!   binary for real, well-formed files (verified directly, not
//!   assumed — there is no written spec for this format).
//! - [`entry`] — read/write for Type #1 BLS entries
//!   (`title`/`version`/`linux`/`initrd`/`options`/...), verified
//!   against the real, authoritative, versioned uapi-group
//!   specification's own worked example.
//!
//! Planned scope (still ahead):
//! - scanning `/loader/entries/*.conf` as a directory (this crate so
//!   far only handles a single entry's own file content) and boot-
//!   counting's `+tries_left-tries_done` filename-suffix convention
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

pub mod entry;
pub mod grubenv;

pub use entry::Entry;
pub use grubenv::GrubEnv;
