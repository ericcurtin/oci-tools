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
//! - [`scan`] — scanning `/loader/entries/` as a real directory,
//!   tolerating whatever else the real spec allows to coexist in it.
//! - [`boot_count`] — the real spec's own `+tries_left-tries_done`
//!   filename-suffix boot-counting convention (parse/format/
//!   decrement/increment), verified against its own worked examples.
//!
//! Planned scope (still ahead):
//! - the real spec's own "Sorting" section (`sort-key`/`machine-id`/
//!   version-order comparison) — [`scan_entries`] discovers entries
//!   but doesn't order them yet
//! - atomic default-entry flips built on [`grubenv`] (upgrade keeps
//!   the previous deployment's entry for rollback)
//! - the `boot_success`/`boot_indeterminate_count` *grubenv* protocol
//!   ([`boot_count`] only covers the filename-suffix convention so
//!   far) so a deployment that repeatedly fails to reach
//!   `boot-complete.target` auto-falls-back to the previous deployment
//! - kernel argument (kargs) editing shared by `ociboot kargs` and install
//!
//! External tools (`grub2-mkconfig`, `grub-install`) will be wrapped
//! behind traits here so pure-Rust replacements can be swapped in
//! later, matching `oci-erofs::builder`'s own `ErofsBuilder` shape.

pub mod boot_count;
pub mod entry;
pub mod grubenv;
pub mod scan;

pub use boot_count::{BootCount, parse_suffix};
pub use entry::Entry;
pub use grubenv::GrubEnv;
pub use scan::{DiscoveredEntry, scan_entries};
