//! Dropping the container process from "root in the freshly created
//! namespaces" down to the runtime-spec's declared `process.user` and
//! capability sets, in the exact order the kernel requires.
//!
//! **Verified against real `crun` source** (`libcrun_set_caps`/
//! `set_required_caps`/`libcrun_container_setgroups` in
//! `src/libcrun/linux.c`), not just the man pages, since the order here
//! is unforgiving: get it wrong and either the wrong capabilities end up
//! effective, or a later step fails because an earlier one already
//! dropped the privilege it needed.
//!
//! 0. `umask(2)` to `process.user.umask` if given, else the same real
//!    `0o022` default crun itself always falls back to
//!    (`~/git/crun/src/libcrun/container.c:1447`) — unprivileged, no
//!    ordering dependency on anything below, but applied first anyway,
//!    matching crun's own placement.
//! 1. `setgroups(2)` for `process.user.additionalGids` — but *only* if
//!    `/proc/self/setgroups` doesn't already say `deny` (the rootless ID
//!    mapping dance in [`crate::namespaces::write_id_mappings`] writes
//!    that whenever a GID mapping is present, and the kernel refuses
//!    `setgroups(2)` outright once it does — matching crun's own
//!    `can_setgroups` check, not a guess).
//! 2. Drop every bounding-set capability *not* requested, via repeated
//!    `prctl(PR_CAPBSET_DROP)` — while still privileged enough to do so
//!    (dropping bounding capabilities needs `CAP_SETPCAP`, which is only
//!    guaranteed before the `setresuid`/`setresgid` below).
//! 3. `prctl(PR_SET_KEEPCAPS, 1)` so a `0 -> non-zero` UID transition
//!    next doesn't wipe the capability sets the `capset(2)` call right
//!    after is about to set explicitly anyway.
//! 4. `setresgid(2)` then `setresuid(2)` to the spec's `process.user`.
//! 5. `capset(2)` for the effective/permitted/inheritable sets.
//! 6. Clear the ambient set, then raise exactly the requested ambient
//!    capabilities (each is a separate `prctl(PR_CAP_AMBIENT_RAISE)`
//!    call — the kernel has no "set the whole ambient set at once").
//! 7. `prctl(PR_SET_NO_NEW_PRIVS, 1)` if the spec asks for it.
//!
//! # Why no automated syscall test
//!
//! Same reason as [`crate::namespaces`]: `setresuid(2)`/`setresgid(2)`
//! change the *calling thread's* credentials, and Linux does not
//! propagate that to sibling threads the way glibc's `setuid()` wrapper
//! fakes by signalling every thread in the process — so exercising this
//! for real needs a genuinely single-threaded process, which `cargo
//! test`'s per-test-thread harness never provides. Covered instead by
//! `tests/tests/ocirun_run.rs`, which spawns the real `ocirun` binary.

use std::io;
use std::path::Path;

use oci_spec_types::runtime::{LinuxCapabilities, User};
use rustix::thread::{CapabilitySet, CapabilitySets};

/// Drop from the calling (still-privileged, single-threaded) process down
/// to `user`'s uid/gid/supplementary groups and `capabilities`' capability
/// sets, then apply `no_new_privileges` last. `proc_root` is `/proc` in
/// production; tests substitute a temp directory.
pub fn apply(
    proc_root: &Path,
    user: &User,
    capabilities: Option<&LinuxCapabilities>,
    no_new_privileges: bool,
) -> io::Result<()> {
    // `umask(2)` — always called, matching real crun's own
    // unconditional `def->process && def->process->user` branch (a
    // container's own process/user is always present here): a caller
    // that happened to have a stricter/looser umask than `0o022`
    // before invoking this project's own binaries (a real, plausible
    // operational scenario -- many hardened shells/systemd units set
    // their own `umask`/`UMask=`) must not leak that into every
    // container's own file-creation permissions, matching what real
    // `runc`/`crun` themselves already guarantee unconditionally.
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(user.umask.unwrap_or(0o022)));

    apply_supplementary_groups(proc_root, &user.additional_gids)?;

    let empty = LinuxCapabilities::default();
    let caps = capabilities.unwrap_or(&empty);
    drop_bounding_capabilities(&parse_set(&caps.bounding))?;

    rustix::thread::set_keep_capabilities(true).map_err(io::Error::from)?;
    setresgid(user.gid)?;
    setresuid(user.uid)?;

    rustix::thread::set_capabilities(
        None,
        CapabilitySets {
            effective: parse_set(&caps.effective),
            permitted: parse_set(&caps.permitted),
            inheritable: parse_set(&caps.inheritable),
        },
    )
    .map_err(io::Error::from)?;
    apply_ambient_capabilities(&parse_set(&caps.ambient))?;

    if no_new_privileges {
        rustix::thread::set_no_new_privs(true).map_err(io::Error::from)?;
    }
    Ok(())
}

/// `setgroups(2)`, skipped entirely when `/proc/<proc_root>/self/
/// setgroups` already reads `deny` — attempting it anyway would always
/// fail `EPERM` once that's set, exactly matching crun's own
/// `can_setgroups` check.
fn apply_supplementary_groups(proc_root: &Path, additional_gids: &[u32]) -> io::Result<()> {
    if setgroups_denied(proc_root)? {
        return Ok(());
    }
    let gids: Vec<libc::gid_t> = additional_gids.to_vec();
    // SAFETY: `setgroups(2)` with a valid pointer/length pair we just
    // built from an owned `Vec`; changes only the calling (single-
    // threaded) process's supplementary groups.
    #[allow(unsafe_code)]
    let ret = unsafe { libc::setgroups(gids.len(), gids.as_ptr()) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn setgroups_denied(proc_root: &Path) -> io::Result<bool> {
    match std::fs::read_to_string(proc_root.join("self/setgroups")) {
        Ok(content) => Ok(content.trim() == "deny"),
        // Kernels without /proc/<pid>/setgroups (older than 3.19) never
        // restrict setgroups(2) in the first place.
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn drop_bounding_capabilities(keep: &CapabilitySet) -> io::Result<()> {
    for &bit in ALL_CAPABILITIES {
        if !keep.contains(bit) {
            match rustix::thread::remove_capability_from_bounding_set(bit) {
                Ok(()) => {}
                // The kernel returns EINVAL for a capability number it
                // doesn't know about (an older kernel than this binary
                // was built against expects) — matching crun's own
                // tolerance for the same case.
                Err(rustix::io::Errno::INVAL) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

fn apply_ambient_capabilities(ambient: &CapabilitySet) -> io::Result<()> {
    match rustix::thread::clear_ambient_capability_set() {
        Ok(()) => {}
        // No ambient-capability support (pre-4.3 kernel) or not
        // permitted (e.g. the capability isn't in the permitted set) —
        // tolerated exactly like crun tolerates EINVAL/EPERM here.
        Err(rustix::io::Errno::INVAL | rustix::io::Errno::PERM) => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    for &bit in ALL_CAPABILITIES {
        if ambient.contains(bit) {
            match rustix::thread::configure_capability_in_ambient_set(bit, true) {
                Ok(()) => {}
                Err(rustix::io::Errno::INVAL | rustix::io::Errno::PERM) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

fn setresgid(gid: u32) -> io::Result<()> {
    // SAFETY: plain `setresgid(2)` call with no pointers.
    #[allow(unsafe_code)]
    let ret = unsafe { libc::setresgid(gid, gid, gid) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn setresuid(uid: u32) -> io::Result<()> {
    // SAFETY: plain `setresuid(2)` call with no pointers.
    #[allow(unsafe_code)]
    let ret = unsafe { libc::setresuid(uid, uid, uid) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Every `CAP_*` name the runtime-spec may list, in the same order as
/// `capabilities(7)`. Unknown names are ignored (matching crun's own
/// `cap_from_name` failure handling: warn conceptually, don't fail the
/// whole container over a typo or a name a newer kernel added).
fn parse_set(names: &[String]) -> CapabilitySet {
    let mut set = CapabilitySet::empty();
    for name in names {
        if let Some(bit) = capability_named(name) {
            set |= bit;
        }
    }
    set
}

/// Every capability name this build recognizes, in the OCI runtime-
/// spec's own `"CAP_..."` string form — the exact same set
/// [`capability_named`] accepts, kept as data here (rather than only
/// implicitly, one match arm at a time) so other crates in this
/// workspace needing the full name list (`ociman run --cap-add`/
/// `--cap-drop`'s own validation, in particular) have a single, real
/// source of truth to check user-supplied names against instead of
/// re-deriving or duplicating this same list.
pub const ALL_CAPABILITY_NAMES: &[&str] = &[
    "CAP_CHOWN",
    "CAP_DAC_OVERRIDE",
    "CAP_DAC_READ_SEARCH",
    "CAP_FOWNER",
    "CAP_FSETID",
    "CAP_KILL",
    "CAP_SETGID",
    "CAP_SETUID",
    "CAP_SETPCAP",
    "CAP_LINUX_IMMUTABLE",
    "CAP_NET_BIND_SERVICE",
    "CAP_NET_BROADCAST",
    "CAP_NET_ADMIN",
    "CAP_NET_RAW",
    "CAP_IPC_LOCK",
    "CAP_IPC_OWNER",
    "CAP_SYS_MODULE",
    "CAP_SYS_RAWIO",
    "CAP_SYS_CHROOT",
    "CAP_SYS_PTRACE",
    "CAP_SYS_PACCT",
    "CAP_SYS_ADMIN",
    "CAP_SYS_BOOT",
    "CAP_SYS_NICE",
    "CAP_SYS_RESOURCE",
    "CAP_SYS_TIME",
    "CAP_SYS_TTY_CONFIG",
    "CAP_MKNOD",
    "CAP_LEASE",
    "CAP_AUDIT_WRITE",
    "CAP_AUDIT_CONTROL",
    "CAP_SETFCAP",
    "CAP_MAC_OVERRIDE",
    "CAP_MAC_ADMIN",
    "CAP_SYSLOG",
    "CAP_WAKE_ALARM",
    "CAP_BLOCK_SUSPEND",
    "CAP_AUDIT_READ",
    "CAP_PERFMON",
    "CAP_BPF",
    "CAP_CHECKPOINT_RESTORE",
];

/// The special `--cap-add`/`--cap-drop` (`ociman`) / `add_
/// capabilities`/`drop_capabilities` (`ocicri`, `0392`) value meaning
/// "every capability this build recognizes" — matching real `docker`/
/// `podman`'s own `capabilities.All` (`"ALL"`, compared
/// case-insensitively on the way in, like every other name here).
pub const CAP_ALL: &str = "ALL";

/// Normalize one capability name the same way real `docker`/`podman`
/// do (checked directly against `~/git/container-libs/common/pkg/
/// capabilities/capabilities.go`'s own `NormalizeCapabilities`):
/// upper-cased, `CAP_` prefixed if not already, and validated against
/// every capability name this build actually recognizes
/// ([`ALL_CAPABILITY_NAMES`] — the same list [`capability_named`]
/// accepts, so a name this normalizes successfully is guaranteed to
/// also be one the runtime itself can actually apply). [`CAP_ALL`]/
/// `"all"`/`"ALL"` is left as the literal `"ALL"` marker, un-prefixed
/// and unvalidated against the name list — it's a merge-time
/// instruction, not a real capability name.
///
/// Shared by `ociman run --cap-add`/`--cap-drop` and `ocicri
/// CreateContainer`'s own `security_context.capabilities.add_
/// capabilities`/`drop_capabilities` (`0392`) — moved here, out of
/// `ociman`-private code, the moment a second, unrelated caller needed
/// the identical primitive (the same "shared prerequisite" reasoning
/// `oci_runtime_core::etc_hosts`/`resolv_conf` already established for
/// their own second callers, `0296`/`0297`).
pub fn normalize_capability(name: &str) -> io::Result<String> {
    let upper = name.to_ascii_uppercase();
    if upper == CAP_ALL {
        return Ok(upper);
    }
    let prefixed = if upper.starts_with("CAP_") {
        upper
    } else {
        format!("CAP_{upper}")
    };
    if !ALL_CAPABILITY_NAMES.contains(&prefixed.as_str()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown capability {name:?}"),
        ));
    }
    Ok(prefixed)
}

/// [`normalize_capability`] applied to every name in `names`.
pub fn normalize_capabilities(names: &[String]) -> io::Result<Vec<String>> {
    names
        .iter()
        .map(|name| normalize_capability(name))
        .collect()
}

/// Compute a container's own final capability set from `base` (the
/// default set before any add/drop override) plus add/drop overrides
/// — a direct, checked-against-the-real-source port of real `docker`/
/// `podman`'s own `MergeCapabilities` (`~/git/container-libs/common/
/// pkg/capabilities/capabilities.go`), not an independently invented
/// algorithm:
///
/// * `drop=["ALL"]` (in any case) discards `base` entirely and keeps
///   only whatever `add` separately grants — real `docker`/`podman`'s
///   own documented behavior, not "drop everything and ignore `add`
///   too".
/// * `drop=["ALL"]` together with `add=["ALL"]` is a real, refused
///   error (`"adding all capabilities and removing all capabilities
///   not allowed"`), matching the real source exactly, not silently
///   resolved either way.
/// * `add=["ALL"]` (without `drop=["ALL"]`) replaces `base` with every
///   capability this build recognizes ([`ALL_CAPABILITY_NAMES`]) —
///   real `docker`/`podman` use the *calling process's own real
///   bounding set* here instead, which has no equivalent meaning for a
///   runtime-spec's own `bounding`/`effective`/`permitted` arrays (a
///   declaration of what the *container* should have, independent of
///   whatever privilege the invoking process itself happens to hold)
///   — using the full recognized-name list is the more literal,
///   correct reading of "grant every capability" for that context.
///   `add=["ALL"]` combined with an individual `drop` (e.g. `drop =
///   ["CHOWN"]`) still applies that drop on top of the full set —
///   matching real cri-o's own identical, checked-directly
///   `SpecSetupCapabilities` behavior for this exact combination
///   (`~/git/cri-o/internal/factory/container/container.go`,
///   `kubernetes/kubernetes#51980`).
/// * The same capability appearing in both `add` and `drop` (after
///   `all`-handling above) is a real, surfaced error, never silently
///   resolved one way or the other.
///
/// Deliberately does *not* replicate real cri-o's own `toCAPPrefixed`
/// quirk (`container.go`): a name already starting with a
/// case-insensitive `cap_` prefix is returned *verbatim*, un-
/// uppercased, there — so a request literally spelled `"cap_chown"`
/// ends up as that exact lowercase string in cri-o's own generated
/// spec, not the canonical `"CAP_CHOWN"` every real runtime's own
/// capability-name matching actually expects. [`normalize_capability`]
/// always uppercases first, so this can never happen here — the same
/// "diverge from a real tool's own bug, keep the more correct
/// behavior" precedent this project already established elsewhere
/// (e.g. `0376`'s rootless hard-limit fix).
pub fn merge_capabilities(
    base: &[String],
    adds: &[String],
    drops: &[String],
) -> io::Result<Vec<String>> {
    if adds.is_empty() && drops.is_empty() {
        return Ok(base.to_vec());
    }
    let adds = normalize_capabilities(adds)?;
    let drops = normalize_capabilities(drops)?;

    if drops.iter().any(|c| c == CAP_ALL) {
        if adds.iter().any(|c| c == CAP_ALL) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "adding all capabilities and removing all capabilities not allowed",
            ));
        }
        let mut result = adds;
        result.sort();
        result.dedup();
        return Ok(result);
    }

    let (base, adds): (Vec<String>, Vec<String>) = if adds.iter().any(|c| c == CAP_ALL) {
        (
            ALL_CAPABILITY_NAMES.iter().map(|s| s.to_string()).collect(),
            Vec::new(),
        )
    } else {
        (base.to_vec(), adds)
    };

    for add in &adds {
        if drops.contains(add) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("capability {add:?} cannot be dropped and added"),
            ));
        }
    }

    let mut result: Vec<String> = base
        .into_iter()
        .filter(|cap| !drops.contains(cap))
        .collect();
    for add in adds {
        if !result.contains(&add) {
            result.push(add);
        }
    }
    result.sort();
    result.dedup();
    Ok(result)
}

const ALL_CAPABILITIES: &[CapabilitySet] = &[
    CapabilitySet::CHOWN,
    CapabilitySet::DAC_OVERRIDE,
    CapabilitySet::DAC_READ_SEARCH,
    CapabilitySet::FOWNER,
    CapabilitySet::FSETID,
    CapabilitySet::KILL,
    CapabilitySet::SETGID,
    CapabilitySet::SETUID,
    CapabilitySet::SETPCAP,
    CapabilitySet::LINUX_IMMUTABLE,
    CapabilitySet::NET_BIND_SERVICE,
    CapabilitySet::NET_BROADCAST,
    CapabilitySet::NET_ADMIN,
    CapabilitySet::NET_RAW,
    CapabilitySet::IPC_LOCK,
    CapabilitySet::IPC_OWNER,
    CapabilitySet::SYS_MODULE,
    CapabilitySet::SYS_RAWIO,
    CapabilitySet::SYS_CHROOT,
    CapabilitySet::SYS_PTRACE,
    CapabilitySet::SYS_PACCT,
    CapabilitySet::SYS_ADMIN,
    CapabilitySet::SYS_BOOT,
    CapabilitySet::SYS_NICE,
    CapabilitySet::SYS_RESOURCE,
    CapabilitySet::SYS_TIME,
    CapabilitySet::SYS_TTY_CONFIG,
    CapabilitySet::MKNOD,
    CapabilitySet::LEASE,
    CapabilitySet::AUDIT_WRITE,
    CapabilitySet::AUDIT_CONTROL,
    CapabilitySet::SETFCAP,
    CapabilitySet::MAC_OVERRIDE,
    CapabilitySet::MAC_ADMIN,
    CapabilitySet::SYSLOG,
    CapabilitySet::WAKE_ALARM,
    CapabilitySet::BLOCK_SUSPEND,
    CapabilitySet::AUDIT_READ,
    CapabilitySet::PERFMON,
    CapabilitySet::BPF,
    CapabilitySet::CHECKPOINT_RESTORE,
];

fn capability_named(name: &str) -> Option<CapabilitySet> {
    Some(match name {
        "CAP_CHOWN" => CapabilitySet::CHOWN,
        "CAP_DAC_OVERRIDE" => CapabilitySet::DAC_OVERRIDE,
        "CAP_DAC_READ_SEARCH" => CapabilitySet::DAC_READ_SEARCH,
        "CAP_FOWNER" => CapabilitySet::FOWNER,
        "CAP_FSETID" => CapabilitySet::FSETID,
        "CAP_KILL" => CapabilitySet::KILL,
        "CAP_SETGID" => CapabilitySet::SETGID,
        "CAP_SETUID" => CapabilitySet::SETUID,
        "CAP_SETPCAP" => CapabilitySet::SETPCAP,
        "CAP_LINUX_IMMUTABLE" => CapabilitySet::LINUX_IMMUTABLE,
        "CAP_NET_BIND_SERVICE" => CapabilitySet::NET_BIND_SERVICE,
        "CAP_NET_BROADCAST" => CapabilitySet::NET_BROADCAST,
        "CAP_NET_ADMIN" => CapabilitySet::NET_ADMIN,
        "CAP_NET_RAW" => CapabilitySet::NET_RAW,
        "CAP_IPC_LOCK" => CapabilitySet::IPC_LOCK,
        "CAP_IPC_OWNER" => CapabilitySet::IPC_OWNER,
        "CAP_SYS_MODULE" => CapabilitySet::SYS_MODULE,
        "CAP_SYS_RAWIO" => CapabilitySet::SYS_RAWIO,
        "CAP_SYS_CHROOT" => CapabilitySet::SYS_CHROOT,
        "CAP_SYS_PTRACE" => CapabilitySet::SYS_PTRACE,
        "CAP_SYS_PACCT" => CapabilitySet::SYS_PACCT,
        "CAP_SYS_ADMIN" => CapabilitySet::SYS_ADMIN,
        "CAP_SYS_BOOT" => CapabilitySet::SYS_BOOT,
        "CAP_SYS_NICE" => CapabilitySet::SYS_NICE,
        "CAP_SYS_RESOURCE" => CapabilitySet::SYS_RESOURCE,
        "CAP_SYS_TIME" => CapabilitySet::SYS_TIME,
        "CAP_SYS_TTY_CONFIG" => CapabilitySet::SYS_TTY_CONFIG,
        "CAP_MKNOD" => CapabilitySet::MKNOD,
        "CAP_LEASE" => CapabilitySet::LEASE,
        "CAP_AUDIT_WRITE" => CapabilitySet::AUDIT_WRITE,
        "CAP_AUDIT_CONTROL" => CapabilitySet::AUDIT_CONTROL,
        "CAP_SETFCAP" => CapabilitySet::SETFCAP,
        "CAP_MAC_OVERRIDE" => CapabilitySet::MAC_OVERRIDE,
        "CAP_MAC_ADMIN" => CapabilitySet::MAC_ADMIN,
        "CAP_SYSLOG" => CapabilitySet::SYSLOG,
        "CAP_WAKE_ALARM" => CapabilitySet::WAKE_ALARM,
        "CAP_BLOCK_SUSPEND" => CapabilitySet::BLOCK_SUSPEND,
        "CAP_AUDIT_READ" => CapabilitySet::AUDIT_READ,
        "CAP_PERFMON" => CapabilitySet::PERFMON,
        "CAP_BPF" => CapabilitySet::BPF,
        "CAP_CHECKPOINT_RESTORE" => CapabilitySet::CHECKPOINT_RESTORE,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_set_recognizes_every_capability_name() {
        let names: Vec<String> = ALL_CAPABILITY_NAMES.iter().map(|s| s.to_string()).collect();
        let set = parse_set(&names);
        for bit in ALL_CAPABILITIES {
            assert!(set.contains(*bit), "{bit:?} not recognized");
        }
    }

    #[test]
    fn parse_set_ignores_unknown_names() {
        let set = parse_set(&["CAP_NOT_A_REAL_CAPABILITY".to_string()]);
        assert!(set.is_empty());
    }

    #[test]
    fn parse_set_of_empty_list_is_empty() {
        assert!(parse_set(&[]).is_empty());
    }

    #[test]
    fn setgroups_denied_reads_deny_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("self")).unwrap();
        std::fs::write(dir.path().join("self/setgroups"), b"deny").unwrap();
        assert!(setgroups_denied(dir.path()).unwrap());
    }

    #[test]
    fn setgroups_denied_false_when_file_says_allow() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("self")).unwrap();
        std::fs::write(dir.path().join("self/setgroups"), b"allow").unwrap();
        assert!(!setgroups_denied(dir.path()).unwrap());
    }

    #[test]
    fn setgroups_denied_false_when_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("self")).unwrap();
        assert!(!setgroups_denied(dir.path()).unwrap());
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn normalize_capability_adds_the_cap_prefix_and_upper_cases() {
        assert_eq!(normalize_capability("chown").unwrap(), "CAP_CHOWN");
        assert_eq!(normalize_capability("Chown").unwrap(), "CAP_CHOWN");
        assert_eq!(normalize_capability("CAP_CHOWN").unwrap(), "CAP_CHOWN");
        assert_eq!(normalize_capability("cap_chown").unwrap(), "CAP_CHOWN");
    }

    #[test]
    fn normalize_capability_leaves_all_as_the_literal_marker() {
        assert_eq!(normalize_capability("all").unwrap(), "ALL");
        assert_eq!(normalize_capability("ALL").unwrap(), "ALL");
        assert_eq!(normalize_capability("All").unwrap(), "ALL");
    }

    #[test]
    fn normalize_capability_rejects_an_unknown_name() {
        let err = normalize_capability("not_a_real_capability").unwrap_err();
        assert!(err.to_string().contains("not_a_real_capability"), "{err}");
    }

    #[test]
    fn merge_capabilities_is_the_base_untouched_when_nothing_is_given() {
        let base = strings(&["CAP_CHOWN", "CAP_FOWNER"]);
        assert_eq!(merge_capabilities(&base, &[], &[]).unwrap(), base);
    }

    #[test]
    fn merge_capabilities_drops_a_base_capability() {
        let base = strings(&["CAP_CHOWN", "CAP_FOWNER"]);
        let result = merge_capabilities(&base, &[], &strings(&["chown"])).unwrap();
        assert_eq!(result, strings(&["CAP_FOWNER"]));
    }

    #[test]
    fn merge_capabilities_adds_a_capability_not_in_base() {
        let base = strings(&["CAP_CHOWN"]);
        let result = merge_capabilities(&base, &strings(&["net_admin"]), &[]).unwrap();
        assert_eq!(result, strings(&["CAP_CHOWN", "CAP_NET_ADMIN"]));
    }

    #[test]
    fn merge_capabilities_adding_a_capability_already_in_base_does_not_duplicate_it() {
        let base = strings(&["CAP_CHOWN"]);
        let result = merge_capabilities(&base, &strings(&["chown"]), &[]).unwrap();
        assert_eq!(result, strings(&["CAP_CHOWN"]));
    }

    #[test]
    fn merge_capabilities_rejects_the_same_capability_added_and_dropped() {
        let base = strings(&["CAP_CHOWN"]);
        let err = merge_capabilities(&base, &strings(&["net_admin"]), &strings(&["net_admin"]))
            .unwrap_err();
        assert!(err.to_string().contains("CAP_NET_ADMIN"), "{err}");
    }

    #[test]
    fn merge_capabilities_drop_all_keeps_only_what_add_grants_ignoring_base() {
        let base = strings(&["CAP_CHOWN", "CAP_FOWNER"]);
        let result =
            merge_capabilities(&base, &strings(&["net_admin"]), &strings(&["all"])).unwrap();
        assert_eq!(result, strings(&["CAP_NET_ADMIN"]));
    }

    #[test]
    fn merge_capabilities_add_all_replaces_base_with_every_recognized_capability() {
        let base = strings(&["CAP_CHOWN"]);
        let result = merge_capabilities(&base, &strings(&["all"]), &[]).unwrap();
        let mut expected: Vec<String> =
            ALL_CAPABILITY_NAMES.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(result, expected);
    }

    #[test]
    fn merge_capabilities_add_all_and_drop_all_together_is_a_real_error() {
        let base = strings(&["CAP_CHOWN"]);
        let err = merge_capabilities(&base, &strings(&["all"]), &strings(&["all"])).unwrap_err();
        assert!(err.to_string().contains("not allowed"), "{err}");
    }

    #[test]
    fn merge_capabilities_result_is_always_sorted_and_deduplicated() {
        let base = strings(&["CAP_FOWNER", "CAP_CHOWN"]);
        let result = merge_capabilities(&base, &strings(&["chown"]), &[]).unwrap();
        assert_eq!(result, strings(&["CAP_CHOWN", "CAP_FOWNER"]));
    }

    /// `add=["ALL"]` combined with an individual `drop` applies that
    /// drop on top of the full set, matching real cri-o's own
    /// checked-directly behavior (`merge_capabilities`'s own doc
    /// comment, `kubernetes/kubernetes#51980`) -- a case ociman's own
    /// CLI has no way to trigger (`--cap-add=all --cap-drop=chown` is
    /// perfectly expressible there too, so this isn't `ocicri`-only,
    /// just previously untested).
    #[test]
    fn merge_capabilities_add_all_with_an_individual_drop_removes_just_that_one() {
        let base = strings(&["CAP_CHOWN"]);
        let result = merge_capabilities(&base, &strings(&["all"]), &strings(&["chown"])).unwrap();
        assert!(!result.contains(&"CAP_CHOWN".to_string()));
        assert!(result.contains(&"CAP_NET_ADMIN".to_string()));
        assert_eq!(result.len(), ALL_CAPABILITY_NAMES.len() - 1);
    }
}
