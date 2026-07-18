//! Boot Loader Specification entries, grubenv, and boot counting.
//!
//! **Status: stub** — implemented in milestones 5 and 6 (see
//! `docs/design/`).
//!
//! Planned scope:
//! - read/write BLS entries under `/boot/loader/entries/` (`title`,
//!   `version`, `linux`, `initrd`, `options`) for `ociboot` deployments
//! - grubenv manipulation with atomic default-entry flips (upgrade keeps the
//!   previous deployment's entry for rollback)
//! - boot counting: `boot_counter` / `boot_success` grubenv protocol so a
//!   deployment that repeatedly fails to reach `boot-complete.target`
//!   auto-falls-back to the previous deployment
//! - kernel argument (kargs) editing shared by `ociboot kargs` and install
//!
//! External tools (`grub2-mkconfig`, `grub-install`) are wrapped behind
//! traits here so pure-Rust replacements can be swapped in later.
