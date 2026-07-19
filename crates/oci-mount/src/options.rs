//! Translating OCI runtime-spec mount option strings (`mount(8)` `-o`
//! syntax) into kernel `mount(2)` flags, propagation flags, and leftover
//! filesystem-specific data — ported from runc's `parseMountOptions`
//! (`libcontainer/specconv/spec_linux.go`): the same option-name table,
//! the same "unrecognized option becomes mount data" fallback.
//!
//! Pure logic, no I/O, no syscalls: this module answers "what does
//! `nosuid,noexec,mode=755` mean" so the eventual `mount(2)` call (a
//! later increment) has flags and data ready to use. Flag values are the
//! real kernel `MS_*` constants from `<linux/mount.h>` ([`flags`]),
//! defined here as plain `u64` bits rather than imported from a
//! syscall-wrapper crate's flag type: a couple of them (`REMOUNT`,
//! `MOVE`) aren't part of any safe wrapper's *public* flags type — they're
//! special-cased behind dedicated functions in most wrappers, rustix
//! included, since combining them with an ordinary `mount(2)` call is
//! rarely what a caller wants. Tracking them as plain bits here means
//! this module has no opinion about which crate ends up making the
//! actual syscall, or how.
//!
//! **Not ported yet** (no corresponding oci-tools feature to exercise
//! them against): the `recAttrFlags` table (`rro`/`rnosuid`/... for the
//! newer `mount_setattr(2)` recursive-attribute syscall), `idmap`/`ridmap`
//! (mount ID-mapping), and `tmpcopyup` (a runc-specific extension, not
//! part of the OCI runtime-spec).

/// Real kernel `MS_*` mount flag values, from `<linux/mount.h>`.
pub mod flags {
    /// Mount read-only.
    pub const RDONLY: u64 = 1;
    /// Ignore suid and sgid bits.
    pub const NOSUID: u64 = 2;
    /// Disallow access to device special files.
    pub const NODEV: u64 = 4;
    /// Disallow program execution.
    pub const NOEXEC: u64 = 8;
    /// Writes are synced at once.
    pub const SYNCHRONOUS: u64 = 16;
    /// Alter flags of a mounted filesystem.
    pub const REMOUNT: u64 = 32;
    /// Allow mandatory locks on this filesystem.
    pub const MANDLOCK: u64 = 64;
    /// Directory modifications are synchronous.
    pub const DIRSYNC: u64 = 128;
    /// Do not follow symlinks.
    pub const NOSYMFOLLOW: u64 = 256;
    /// Do not update access times.
    pub const NOATIME: u64 = 1024;
    /// Do not update directory access times.
    pub const NODIRATIME: u64 = 2048;
    /// Bind mount (create a second view of an existing subtree).
    pub const BIND: u64 = 4096;
    /// Move a subtree instead of copying it.
    pub const MOVE: u64 = 8192;
    /// Apply an operation recursively to every mount in a subtree.
    pub const REC: u64 = 16384;
    /// Suppress certain kernel warning messages.
    pub const SILENT: u64 = 32768;
    /// Change the propagation type of a subtree to unbindable.
    pub const UNBINDABLE: u64 = 1 << 17;
    /// Change the propagation type of a subtree to private.
    pub const PRIVATE: u64 = 1 << 18;
    /// Change the propagation type of a subtree to downstream (`MS_SLAVE`).
    pub const SLAVE: u64 = 1 << 19;
    /// Change the propagation type of a subtree to shared.
    pub const SHARED: u64 = 1 << 20;
    /// Update atime relative to mtime/ctime.
    pub const RELATIME: u64 = 1 << 21;
    /// Update the inode `I_version` field.
    pub const I_VERSION: u64 = 1 << 23;
    /// Always perform atime updates.
    pub const STRICTATIME: u64 = 1 << 24;
    /// Update the on-disk access/change/modify times lazily.
    pub const LAZYTIME: u64 = 1 << 25;
}

/// The result of parsing an OCI mount's `options` list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedMountOptions {
    /// `mount(2)` flags to set (bitwise-OR of [`flags`] values).
    pub set_flags: u64,
    /// Flags an option explicitly asked to clear — meaningful mainly on a
    /// remount, where clearing an already-set flag is itself significant;
    /// matches runc's `ClearedFlags`.
    pub cleared_flags: u64,
    /// Mount propagation flags (`private`/`shared`/`slave`/`unbindable`,
    /// each with an optional recursive `r`-prefixed variant), in the
    /// order given — the runtime-spec allows setting more than one on the
    /// same mount, applied in sequence.
    pub propagation: Vec<u64>,
    /// Leftover options that aren't recognized flags: filesystem-specific
    /// data (e.g. `mode=0755`, `size=65536k`), comma-joined in the order
    /// given, exactly as `mount(2)`'s `data` argument expects.
    pub data: String,
}

/// Parse an OCI mount's `options` list the way runc's `parseMountOptions`
/// does: each option is either a known flag (set or cleared), a
/// propagation setting, or — if unrecognized — a fragment of
/// filesystem-specific mount data.
pub fn parse_mount_options<S: AsRef<str>>(options: &[S]) -> ParsedMountOptions {
    let mut result = ParsedMountOptions::default();
    let mut data = Vec::new();
    for option in options {
        let option = option.as_ref();
        if let Some((flag, clear)) = mount_flag(option) {
            if clear {
                result.set_flags &= !flag;
                result.cleared_flags |= flag;
            } else {
                result.set_flags |= flag;
                result.cleared_flags &= !flag;
            }
        } else if let Some(propagation) = propagation_flag(option) {
            result.propagation.push(propagation);
        } else {
            data.push(option);
        }
    }
    result.data = data.join(",");
    result
}

/// Every option name [`mount_flag`] or [`propagation_flag`] recognizes,
/// combined into one list — matching real runc's own
/// `specconv.KnownMountOptions()` (`mountFlags` keys plus
/// `mountPropagationMapping` keys; this project has no counterpart to
/// runc's newer `recAttrFlags` table, see this module's own top doc
/// comment for why). Used by `ocirun features`'s own `mountOptions`
/// list — kept honest by a test asserting every name here really does
/// round-trip through one of the two lookup functions, so this list
/// can never silently drift out of sync with the match tables below.
pub fn known_option_names() -> Vec<&'static str> {
    vec![
        "async",
        "atime",
        "bind",
        "dev",
        "diratime",
        "dirsync",
        "exec",
        "lazytime",
        "loud",
        "mand",
        "noatime",
        "nodev",
        "nodiratime",
        "noexec",
        "nolazytime",
        "nomand",
        "norelatime",
        "nostrictatime",
        "nosuid",
        "nosymfollow",
        "rbind",
        "relatime",
        "remount",
        "ro",
        "rw",
        "silent",
        "strictatime",
        "suid",
        "sync",
        "symfollow",
        "rprivate",
        "private",
        "rslave",
        "slave",
        "rshared",
        "shared",
        "runbindable",
        "unbindable",
    ]
}

/// `(flag, clear)` for one recognized `mount(2)`-flag option name, or
/// `None` if `option` isn't one (it's either a propagation setting or
/// filesystem-specific data). `clear` mirrors runc's table: most options
/// set their flag, but some (`rw`, `exec`, `dev`, `suid`, `atime`, ...)
/// exist specifically to clear the flag their opposite sets.
///
/// `"defaults"` is deliberately *not* mapped here (matching runc): its
/// table entry maps to flag `0`, and runc's own lookup only treats an
/// entry as a recognized flag when `flag != 0` — so in practice
/// `"defaults"` falls through and ends up as a (harmless, ignored by the
/// kernel) fragment of mount data, same as any other unrecognized option.
fn mount_flag(option: &str) -> Option<(u64, bool)> {
    use flags::*;
    Some(match option {
        "async" => (SYNCHRONOUS, true),
        "atime" => (NOATIME, true),
        "bind" => (BIND, false),
        "dev" => (NODEV, true),
        "diratime" => (NODIRATIME, true),
        "dirsync" => (DIRSYNC, false),
        "exec" => (NOEXEC, true),
        "lazytime" => (LAZYTIME, false),
        "loud" => (SILENT, true),
        "mand" => (MANDLOCK, false),
        "noatime" => (NOATIME, false),
        "nodev" => (NODEV, false),
        "nodiratime" => (NODIRATIME, false),
        "noexec" => (NOEXEC, false),
        "nolazytime" => (LAZYTIME, true),
        "nomand" => (MANDLOCK, true),
        "norelatime" => (RELATIME, true),
        "nostrictatime" => (STRICTATIME, true),
        "nosuid" => (NOSUID, false),
        "nosymfollow" => (NOSYMFOLLOW, false),
        "rbind" => (BIND | REC, false),
        "relatime" => (RELATIME, false),
        "remount" => (REMOUNT, false),
        "ro" => (RDONLY, false),
        "rw" => (RDONLY, true),
        "silent" => (SILENT, false),
        "strictatime" => (STRICTATIME, false),
        "suid" => (NOSUID, true),
        "sync" => (SYNCHRONOUS, false),
        "symfollow" => (NOSYMFOLLOW, true),
        _ => return None,
    })
}

/// The propagation flag(s) for one recognized propagation-setting option
/// name, or `None` if `option` isn't one.
fn propagation_flag(option: &str) -> Option<u64> {
    use flags::*;
    Some(match option {
        "rprivate" => PRIVATE | REC,
        "private" => PRIVATE,
        "rslave" => SLAVE | REC,
        "slave" => SLAVE,
        "rshared" => SHARED | REC,
        "shared" => SHARED,
        "runbindable" => UNBINDABLE | REC,
        "unbindable" => UNBINDABLE,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn known_option_names_all_round_trip_through_the_real_lookup_functions() {
        // Guards against known_option_names() silently drifting out of
        // sync with mount_flag/propagation_flag: every name it lists
        // must actually be recognized by one of the two. Same
        // hand-maintained-list-plus-round-trip-test discipline this
        // project already uses for `identity::ALL_CAPABILITY_NAMES`.
        for name in known_option_names() {
            assert!(
                mount_flag(name).is_some() || propagation_flag(name).is_some(),
                "{name:?} is in known_option_names() but neither lookup function recognizes it"
            );
        }
        // No duplicates, and a length sanity check -- catches an
        // accidental copy-paste repeat without needing a second,
        // independently-typed copy of the list.
        let mut names = known_option_names();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), known_option_names().len());
        assert_eq!(known_option_names().len(), 38);
    }

    #[test]
    fn simple_set_flags_combine() {
        let parsed = parse_mount_options(&opts(&["nosuid", "noexec", "nodev"]));
        assert_eq!(
            parsed.set_flags,
            flags::NOSUID | flags::NOEXEC | flags::NODEV
        );
        assert_eq!(parsed.cleared_flags, 0);
        assert_eq!(parsed.data, "");
        assert!(parsed.propagation.is_empty());
    }

    #[test]
    fn clearing_options_track_cleared_flags_separately() {
        // "rw" clears RDONLY rather than setting it.
        let parsed = parse_mount_options(&opts(&["rw"]));
        assert_eq!(parsed.set_flags, 0);
        assert_eq!(parsed.cleared_flags, flags::RDONLY);
    }

    #[test]
    fn later_option_overrides_an_earlier_opposite() {
        // "noatime" (set NOATIME) then "atime" (clear NOATIME): the net
        // effect must be "cleared", matching runc's sequential
        // set/clear-tracking (not just an OR of all flags ever seen).
        let parsed = parse_mount_options(&opts(&["noatime", "atime"]));
        assert_eq!(parsed.set_flags, 0);
        assert_eq!(parsed.cleared_flags, flags::NOATIME);
    }

    #[test]
    fn rbind_sets_bind_and_rec() {
        let parsed = parse_mount_options(&opts(&["rbind"]));
        assert_eq!(parsed.set_flags, flags::BIND | flags::REC);
    }

    #[test]
    fn unrecognized_options_become_data_in_order() {
        let parsed = parse_mount_options(&opts(&["mode=0755", "size=65536k"]));
        assert_eq!(parsed.set_flags, 0);
        assert_eq!(parsed.data, "mode=0755,size=65536k");
    }

    #[test]
    fn defaults_is_not_a_recognized_flag_and_becomes_data() {
        let parsed = parse_mount_options(&opts(&["defaults"]));
        assert_eq!(parsed.set_flags, 0);
        assert_eq!(parsed.data, "defaults");
    }

    #[test]
    fn mixed_flags_and_data_in_realistic_order() {
        let parsed = parse_mount_options(&opts(&[
            "nosuid",
            "noexec",
            "newinstance",
            "ptmxmode=0666",
            "mode=0620",
            "gid=5",
        ]));
        assert_eq!(parsed.set_flags, flags::NOSUID | flags::NOEXEC);
        assert_eq!(parsed.data, "newinstance,ptmxmode=0666,mode=0620,gid=5");
    }

    #[test]
    fn propagation_options_are_collected_separately_from_flags() {
        let parsed = parse_mount_options(&opts(&["rshared", "rprivate"]));
        assert_eq!(
            parsed.propagation,
            vec![flags::SHARED | flags::REC, flags::PRIVATE | flags::REC]
        );
        assert_eq!(parsed.set_flags, 0);
    }

    #[test]
    fn empty_options_parse_to_default() {
        assert_eq!(
            parse_mount_options::<&str>(&[]),
            ParsedMountOptions::default()
        );
    }

    // Every mount entry in `Spec::example()`/`.into_rootless()` — real
    // output already checked byte-for-byte against `runc spec` in
    // oci_spec_types::runtime's own tests — parses to the expected flags
    // and data, so this module is checked against the same real fixture
    // the runtime-spec types were.
    #[test]
    fn parses_every_default_spec_mount_option_set() {
        let spec = oci_spec_types::runtime::Spec::example();
        let by_dest = |dest: &str| {
            spec.mounts
                .iter()
                .find(|m| m.destination == dest)
                .unwrap()
                .options
                .clone()
        };

        let dev = parse_mount_options(&by_dest("/dev"));
        assert_eq!(dev.set_flags, flags::NOSUID | flags::STRICTATIME);
        assert_eq!(dev.data, "mode=755,size=65536k");

        let shm = parse_mount_options(&by_dest("/dev/shm"));
        assert_eq!(shm.set_flags, flags::NOSUID | flags::NOEXEC | flags::NODEV);
        assert_eq!(shm.data, "mode=1777,size=65536k");

        let mqueue = parse_mount_options(&by_dest("/dev/mqueue"));
        assert_eq!(
            mqueue.set_flags,
            flags::NOSUID | flags::NOEXEC | flags::NODEV
        );
        assert_eq!(mqueue.data, "");

        let sys = parse_mount_options(&by_dest("/sys"));
        assert_eq!(
            sys.set_flags,
            flags::NOSUID | flags::NOEXEC | flags::NODEV | flags::RDONLY
        );
        assert_eq!(sys.data, "");

        let cgroup = parse_mount_options(&by_dest("/sys/fs/cgroup"));
        assert_eq!(
            cgroup.set_flags,
            flags::NOSUID | flags::NOEXEC | flags::NODEV | flags::RELATIME | flags::RDONLY
        );
        assert_eq!(cgroup.data, "");
    }

    #[test]
    fn parses_rootless_sys_rbind_mount() {
        let spec = oci_spec_types::runtime::Spec::example().into_rootless(1000, 1000);
        let sys = spec
            .mounts
            .iter()
            .find(|m| m.destination == "/sys")
            .unwrap();
        let parsed = parse_mount_options(&sys.options);
        assert_eq!(
            parsed.set_flags,
            flags::BIND | flags::REC | flags::NOSUID | flags::NOEXEC | flags::NODEV | flags::RDONLY
        );
        assert_eq!(parsed.data, "");
    }
}
