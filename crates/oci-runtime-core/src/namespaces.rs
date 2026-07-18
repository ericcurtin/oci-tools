//! Linux namespace flag computation and creation (`unshare(2)`), and the
//! rootless user-namespace ID-mapping dance every unprivileged namespace
//! setup needs.
//!
//! **Manually verified against the real kernel** (not just read about):
//! a scratch program calling [`unshare`] with `NEWUSER | NEWUTS`, then
//! [`write_id_mappings`], then `sethostname` succeeded as an unprivileged
//! user, and left the host's own hostname untouched — the same
//! create-userns-then-map-then-do-privileged-things-inside-it sequence
//! rootless `runc`/`crun`/bubblewrap use. That scratch verification is
//! not itself a `cargo test` (see "Why no automated syscall test" below).
//!
//! # Why no automated syscall test
//!
//! `unshare(2)` with `CLONE_NEWUSER` fails with `EINVAL` when the calling
//! process has more than one thread (`unshare(2)`'s "NOTES" section). The
//! default `cargo test` harness runs every test body on its own spawned
//! thread even when filtered down to one test — so calling `unshare`
//! with `NEWUSER` from inside a `#[test]` reliably fails for a reason
//! that has nothing to do with whether the code is correct. Testing this
//! for real needs a genuinely fresh, single-threaded process (i.e. a
//! freshly exec'd child, the same shape `create` will fork next); that
//! lands as part of `create`'s own integration tests, which spawn the
//! real `ocirun` binary as a subprocess exactly like `tests/tests/
//! ocirun_state.rs` already does.

use std::io;
use std::path::Path;

use oci_spec_types::runtime::{LinuxIdMapping, LinuxNamespace, NamespaceType};
use rustix::thread::UnshareFlags;

/// Compute the `unshare(2)`/`clone(2)` flag bitmask for a runtime-spec
/// namespace list (the union of each entry's flag; order and duplicates
/// don't matter to a bitmask).
pub fn clone_flags_for(namespaces: &[LinuxNamespace]) -> UnshareFlags {
    namespaces
        .iter()
        .fold(UnshareFlags::empty(), |flags, ns| flags | flag_for(ns.kind))
}

/// The single `unshare(2)` flag for one namespace type.
fn flag_for(kind: NamespaceType) -> UnshareFlags {
    match kind {
        NamespaceType::Pid => UnshareFlags::NEWPID,
        NamespaceType::Network => UnshareFlags::NEWNET,
        NamespaceType::Mount => UnshareFlags::NEWNS,
        NamespaceType::Ipc => UnshareFlags::NEWIPC,
        NamespaceType::Uts => UnshareFlags::NEWUTS,
        NamespaceType::User => UnshareFlags::NEWUSER,
        NamespaceType::Cgroup => UnshareFlags::NEWCGROUP,
        NamespaceType::Time => UnshareFlags::NEWTIME,
    }
}

/// Create new namespaces (or, for flags not set, keep sharing the current
/// ones) for the calling process, per `unshare(2)`.
///
/// # Notes for callers
/// - `UnshareFlags::NEWUSER` fails with `EINVAL` unless the calling
///   process is single-threaded — call this as close to process start as
///   possible, before spawning any other threads.
/// - After unsharing `NEWUSER`, the process has no valid uid/gid mapping
///   yet (every id resolves to the "overflow" id, typically 65534); nothing
///   that depends on a specific uid — `sethostname` included — works until
///   [`write_id_mappings`] establishes one.
pub fn unshare(flags: UnshareFlags) -> io::Result<()> {
    // SAFETY: `unshare_unsafe`'s only documented hazard is `CLONE_FILES`
    // (a thread using it could observe file descriptors created by
    // another thread after this call); `flag_for` never produces that
    // flag, so there is no such hazard here.
    #[allow(unsafe_code)]
    unsafe { rustix::thread::unshare_unsafe(flags) }.map_err(io::Error::from)
}

/// Write `/proc/<pid_dir>/{setgroups,uid_map,gid_map}` for a process that
/// has just unshared (or been cloned into) a new user namespace.
///
/// `proc_root` is the `/proc`-like directory to operate under (always
/// `Path::new("/proc")` in production; tests substitute a temp directory
/// so nothing here ever touches a real `/proc` entry). `pid_dir` is the
/// path component identifying the target process — `"self"` for the
/// calling process immediately after its own `unshare(NEWUSER)`, or a
/// numeric pid when a parent is mapping a child it just created.
///
/// Writes `setgroups` as `deny` whenever any GID mapping is given (the
/// kernel refuses to accept `gid_map` otherwise unless the writer has
/// `CAP_SETGID` in the parent namespace — never true for the rootless
/// case this exists for); a missing `setgroups` file (kernels older than
/// 3.19) is tolerated, matching every other rootless implementation.
pub fn write_id_mappings(
    proc_root: &Path,
    pid_dir: &str,
    uid_mappings: &[LinuxIdMapping],
    gid_mappings: &[LinuxIdMapping],
) -> io::Result<()> {
    let dir = proc_root.join(pid_dir);

    if !gid_mappings.is_empty() {
        match std::fs::write(dir.join("setgroups"), b"deny") {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    if !uid_mappings.is_empty() {
        std::fs::write(dir.join("uid_map"), format_id_map(uid_mappings))?;
    }
    if !gid_mappings.is_empty() {
        std::fs::write(dir.join("gid_map"), format_id_map(gid_mappings))?;
    }
    Ok(())
}

/// Render the `/proc/<pid>/{uid,gid}_map` file content for a set of ID
/// mappings: one `<container_id> <host_id> <size>` line per entry.
fn format_id_map(mappings: &[LinuxIdMapping]) -> String {
    let mut out = String::new();
    for m in mappings {
        out.push_str(&format!("{} {} {}\n", m.container_id, m.host_id, m.size));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_for_maps_every_namespace_type() {
        assert_eq!(flag_for(NamespaceType::Pid), UnshareFlags::NEWPID);
        assert_eq!(flag_for(NamespaceType::Network), UnshareFlags::NEWNET);
        assert_eq!(flag_for(NamespaceType::Mount), UnshareFlags::NEWNS);
        assert_eq!(flag_for(NamespaceType::Ipc), UnshareFlags::NEWIPC);
        assert_eq!(flag_for(NamespaceType::Uts), UnshareFlags::NEWUTS);
        assert_eq!(flag_for(NamespaceType::User), UnshareFlags::NEWUSER);
        assert_eq!(flag_for(NamespaceType::Cgroup), UnshareFlags::NEWCGROUP);
        assert_eq!(flag_for(NamespaceType::Time), UnshareFlags::NEWTIME);
    }

    #[test]
    fn clone_flags_for_default_spec_matches_expected_bits() {
        let spec = oci_spec_types::runtime::Spec::example();
        let namespaces = &spec.linux.unwrap().namespaces;
        let flags = clone_flags_for(namespaces);
        assert_eq!(
            flags,
            UnshareFlags::NEWPID
                | UnshareFlags::NEWNET
                | UnshareFlags::NEWIPC
                | UnshareFlags::NEWUTS
                | UnshareFlags::NEWNS
                | UnshareFlags::NEWCGROUP
        );
    }

    #[test]
    fn clone_flags_for_rootless_spec_swaps_network_for_user() {
        let spec = oci_spec_types::runtime::Spec::example().into_rootless(1000, 1000);
        let namespaces = &spec.linux.unwrap().namespaces;
        let flags = clone_flags_for(namespaces);
        assert!(flags.contains(UnshareFlags::NEWUSER));
        assert!(!flags.contains(UnshareFlags::NEWNET));
    }

    #[test]
    fn clone_flags_for_empty_list_is_empty() {
        assert_eq!(clone_flags_for(&[]), UnshareFlags::empty());
    }

    #[test]
    fn duplicate_namespace_entries_dont_change_the_result() {
        let once = clone_flags_for(&[LinuxNamespace::new(NamespaceType::Pid)]);
        let twice = clone_flags_for(&[
            LinuxNamespace::new(NamespaceType::Pid),
            LinuxNamespace::new(NamespaceType::Pid),
        ]);
        assert_eq!(once, twice);
    }

    fn mapping(container_id: u32, host_id: u32, size: u32) -> LinuxIdMapping {
        LinuxIdMapping {
            host_id,
            container_id,
            size,
        }
    }

    #[test]
    fn write_id_mappings_writes_expected_file_contents() {
        let proc_root = tempfile::tempdir().unwrap();
        std::fs::create_dir(proc_root.path().join("self")).unwrap();

        write_id_mappings(
            proc_root.path(),
            "self",
            &[mapping(0, 1000, 1)],
            &[mapping(0, 1000, 1)],
        )
        .unwrap();

        let dir = proc_root.path().join("self");
        assert_eq!(
            std::fs::read_to_string(dir.join("uid_map")).unwrap(),
            "0 1000 1\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("gid_map")).unwrap(),
            "0 1000 1\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("setgroups")).unwrap(),
            "deny"
        );
    }

    #[test]
    fn write_id_mappings_supports_multiple_ranges() {
        let proc_root = tempfile::tempdir().unwrap();
        std::fs::create_dir(proc_root.path().join("self")).unwrap();

        write_id_mappings(
            proc_root.path(),
            "self",
            &[mapping(0, 100000, 1), mapping(1, 200000, 65536)],
            &[],
        )
        .unwrap();

        let dir = proc_root.path().join("self");
        assert_eq!(
            std::fs::read_to_string(dir.join("uid_map")).unwrap(),
            "0 100000 1\n1 200000 65536\n"
        );
    }

    #[test]
    fn write_id_mappings_without_gid_mappings_leaves_setgroups_and_gid_map_untouched() {
        let proc_root = tempfile::tempdir().unwrap();
        std::fs::create_dir(proc_root.path().join("self")).unwrap();

        write_id_mappings(proc_root.path(), "self", &[mapping(0, 1000, 1)], &[]).unwrap();

        let dir = proc_root.path().join("self");
        assert!(dir.join("uid_map").exists());
        assert!(!dir.join("gid_map").exists());
        assert!(!dir.join("setgroups").exists());
    }

    #[test]
    fn write_id_mappings_with_no_mappings_at_all_writes_nothing() {
        let proc_root = tempfile::tempdir().unwrap();
        std::fs::create_dir(proc_root.path().join("self")).unwrap();

        write_id_mappings(proc_root.path(), "self", &[], &[]).unwrap();

        let dir = proc_root.path().join("self");
        assert!(!dir.join("uid_map").exists());
        assert!(!dir.join("gid_map").exists());
        assert!(!dir.join("setgroups").exists());
    }
}
