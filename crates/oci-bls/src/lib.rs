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
//! - [`version`] — version string comparison per the real,
//!   authoritative UAPI.10 Version Format Specification, verified
//!   against every one of its own worked examples and cross-checked
//!   against the real `systemd-analyze compare-versions` binary.
//! - [`sort`] — the real Boot Loader Specification's own "Sorting"
//!   section, built on [`version::compare`]: bad (boot-counted)
//!   entries sort last; entries with `sort-key` sort by
//!   `sort-key`/`machine-id`/version, in that priority order; entries
//!   without fall back to their own file name, decreasing version
//!   order, boot-counting suffix removed.
//! - [`cmdline`] — kernel command-line parsing and editing
//!   ([`Cmdline`]/[`Parameter`]/[`Action`]), a direct, narrower port of
//!   real bootc's own `bootc-kernel-cmdline` crate, cross-checked
//!   against its own real test suite (including its own
//!   "pathological" quoting edge cases) — the primitive a future
//!   `ociboot kargs` subcommand needs; no CLI surface of its own yet.
//!
//! `ociboot grubenv` (`bin/ociboot/src/main.rs`) is the first real CLI
//! surface built on [`grubenv`]: a generic, real, pure-Rust
//! `grub-editenv` equivalent (`create`/`list`/`set`/`unset`, verified
//! byte-for-byte compatible with the real binary — `docs/design/
//! 0125`) — deliberately no BLS-specific policy of its own yet.
//!
//! Planned scope (still ahead):
//! - atomic default-entry flips built on [`grubenv`] (upgrade keeps
//!   the previous deployment's entry for rollback)
//! - the `boot_success`/`boot_indeterminate_count` *grubenv* protocol
//!   ([`boot_count`] only covers the filename-suffix convention so
//!   far) so a deployment that repeatedly fails to reach
//!   `boot-complete.target` auto-falls-back to the previous deployment
//! - an actual `ociboot kargs` subcommand built on [`cmdline`], and a
//!   real image's own declared kargs (a `kargs.d`-shaped config,
//!   matching real bootc's own convention) diffed against a BLS
//!   entry's own current `options` field
//!
//! External tools (`grub2-mkconfig`, `grub-install`) will be wrapped
//! behind traits here so pure-Rust replacements can be swapped in
//! later, matching `oci-erofs::builder`'s own `ErofsBuilder` shape.

pub mod boot_count;
pub mod cmdline;
pub mod entry;
pub mod grubenv;
pub mod scan;
pub mod sort;
pub mod version;

pub use boot_count::{BootCount, parse_suffix};
pub use cmdline::{Action, Cmdline, Parameter};
pub use entry::Entry;
pub use grubenv::GrubEnv;
pub use scan::{DiscoveredEntry, scan_entries};
pub use sort::sort_entries;
