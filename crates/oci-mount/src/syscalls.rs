//! Low-level `mount(2)`/`pivot_root(2)` syscall wrappers.
//!
//! [`plan_mount`] is pure logic: given an [`ParsedMountOptions`], decide
//! which `mount(2)` *mode* it calls for (plain, remount, or move) and
//! what flags/data that mode needs. [`mount`] executes that plan via
//! `rustix`. Splitting the decision from the syscall the same way
//! `oci_runtime_core::namespaces` splits `clone_flags_for` from `unshare`
//! means the interesting logic (which of three very different `mount(2)`
//! shapes applies) is unit-testable without touching the kernel at all.
//!
//! **Building the *sequence* of calls a real mount entry needs is not
//! this module's job.** The kernel does not accept most flags atomically
//! together with `MS_BIND` in a single call — a read-only bind mount is
//! two real `mount(2)` calls (bind, then remount-readonly), confirmed by
//! the same `strace` trace 0007 used to verify mount option parsing.
//! Deciding *when* a mount entry needs that two-step dance belongs to
//! `create` (next), which has the full picture (a mount entry, not just
//! one already-resolved set of flags); this module only knows how to
//! make one such call correctly once told to.

use std::ffi::{CStr, CString};
use std::io;
use std::path::Path;

use rustix::mount::MountFlags;

use crate::options::{ParsedMountOptions, flags};

/// Which `mount(2)` mode a [`ParsedMountOptions`] calls for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountPlan {
    /// `mount(source, target, file_system_type, flags, data)`.
    Plain {
        /// Flags to pass (already excludes `REMOUNT`/`MOVE`, which are
        /// not ordinary flags on Linux — see the module docs on why).
        flags: u64,
        /// Filesystem-specific data (`mode=0755`, ...).
        data: String,
    },
    /// `mount(NULL, target, NULL, MS_REMOUNT | flags, data)` — the
    /// `"remount"` option was present.
    Remount {
        /// Flags to pass alongside the implied `REMOUNT` bit.
        flags: u64,
        /// Filesystem-specific data.
        data: String,
    },
    /// `mount(source, target, NULL, MS_MOVE, NULL)`. Not currently
    /// produced by [`crate::options::parse_mount_options`] (no OCI mount
    /// option maps to it), but part of the flag space it computes over,
    /// so it's handled here for completeness rather than silently
    /// mis-dispatched if a caller ever sets the bit directly.
    Move,
}

/// Decide which [`MountPlan`] `parsed` calls for.
pub fn plan_mount(parsed: &ParsedMountOptions) -> MountPlan {
    if parsed.set_flags & flags::MOVE != 0 {
        return MountPlan::Move;
    }
    let public_flags = parsed.set_flags & !(flags::REMOUNT | flags::MOVE);
    if parsed.set_flags & flags::REMOUNT != 0 {
        MountPlan::Remount {
            flags: public_flags,
            data: parsed.data.clone(),
        }
    } else {
        MountPlan::Plain {
            flags: public_flags,
            data: parsed.data.clone(),
        }
    }
}

/// Perform one `mount(2)` call for an OCI mount entry, given its
/// already-parsed options ([`plan_mount`] decides which of the three
/// shapes below applies).
///
/// `source`/`file_system_type` default to `""` when not given (the
/// conventional value the kernel ignores for bind mounts and most
/// pseudo-filesystems); a move mount requires a real `source`.
pub fn mount(
    source: Option<&str>,
    target: &Path,
    file_system_type: Option<&str>,
    parsed: &ParsedMountOptions,
) -> io::Result<()> {
    match plan_mount(parsed) {
        MountPlan::Move => {
            let source = source.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a move mount requires a source",
                )
            })?;
            rustix::mount::mount_move(source, target).map_err(io::Error::from)
        }
        MountPlan::Remount { flags, data } => {
            let mount_flags = MountFlags::from_bits_truncate(flags as u32);
            rustix::mount::mount_remount(target, mount_flags, data.as_str())
                .map_err(io::Error::from)
        }
        MountPlan::Plain { flags, data } => {
            let mount_flags = MountFlags::from_bits_truncate(flags as u32);
            let source = source.unwrap_or("");
            let file_system_type = file_system_type.unwrap_or("");
            let data_cstring = (!data.is_empty())
                .then(|| CString::new(data))
                .transpose()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            let data_ref: Option<&CStr> = data_cstring.as_deref();
            rustix::mount::mount(source, target, file_system_type, mount_flags, data_ref)
                .map_err(io::Error::from)
        }
    }
}

/// `pivot_root(2)`: make `new_root` the process's root filesystem,
/// moving the previous root to `put_old` (a directory at or under
/// `new_root`; both must be mount points, and on the same filesystem
/// `new_root` is being pivoted to — see `pivot_root(2)`).
pub fn pivot_root(new_root: &Path, put_old: &Path) -> io::Result<()> {
    rustix::process::pivot_root(new_root, put_old).map_err(io::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(options: &[&str]) -> ParsedMountOptions {
        crate::options::parse_mount_options(options)
    }

    #[test]
    fn plain_mount_plans_flags_and_data() {
        let plan = plan_mount(&parsed(&[
            "nosuid",
            "strictatime",
            "mode=755",
            "size=65536k",
        ]));
        assert_eq!(
            plan,
            MountPlan::Plain {
                flags: flags::NOSUID | flags::STRICTATIME,
                data: "mode=755,size=65536k".to_string(),
            }
        );
    }

    #[test]
    fn remount_plan_excludes_the_remount_bit_from_flags() {
        let plan = plan_mount(&parsed(&["remount", "ro", "bind"]));
        assert_eq!(
            plan,
            MountPlan::Remount {
                flags: flags::RDONLY | flags::BIND,
                data: String::new()
            }
        );
    }

    #[test]
    fn rbind_readonly_plans_as_a_single_plain_bind_mount() {
        // The two-call bind-then-remount dance (confirmed via strace in
        // 0007) is `create`'s sequencing job, not this module's: given
        // only "rbind,ro" as one parsed option set (no explicit
        // "remount"), this is a single plain mount with both bits set.
        let plan = plan_mount(&parsed(&["rbind", "ro"]));
        assert_eq!(
            plan,
            MountPlan::Plain {
                flags: flags::BIND | flags::REC | flags::RDONLY,
                data: String::new()
            }
        );
    }

    #[test]
    fn move_flag_plans_as_move_regardless_of_other_flags() {
        let options = ParsedMountOptions {
            set_flags: flags::MOVE | flags::NOSUID,
            ..Default::default()
        };
        assert_eq!(plan_mount(&options), MountPlan::Move);
    }

    #[test]
    fn default_spec_dev_mount_plans_correctly() {
        let spec = oci_spec_types::runtime::Spec::example();
        let dev = spec
            .mounts
            .iter()
            .find(|m| m.destination == "/dev")
            .unwrap();
        let plan = plan_mount(&parsed(
            &dev.options.iter().map(String::as_str).collect::<Vec<_>>(),
        ));
        assert_eq!(
            plan,
            MountPlan::Plain {
                flags: flags::NOSUID | flags::STRICTATIME,
                data: "mode=755,size=65536k".to_string(),
            }
        );
    }
}
