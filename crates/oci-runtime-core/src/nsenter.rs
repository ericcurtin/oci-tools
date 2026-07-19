//! Joining an already-running container's namespaces (`setns(2)`) —
//! the primitive `ocirun exec` needs that `create`/`run` never did
//! (they only ever create *new* namespaces, never join existing ones).
//!
//! Ported from real `runc`'s own careful join order
//! (`libcontainer/nsenter/nsexec.c`'s `join_namespaces`), not
//! reinvented: naively `setns`ing into a rootless container's
//! namespaces in the wrong order fails outright. That file's own
//! comment explains why precisely:
//!
//! > We first try to join all non-userns namespaces ... We then join
//! > the user namespace, and then try to join any remaining
//! > namespaces (this last step is needed for rootless containers —
//! > we don't get `setns(2)` permissions until we join the userns and
//! > get `CAP_SYS_ADMIN`).
//!
//! `runc` does that as a 3-phase dance (attempt non-user namespaces
//! first, join user, retry the failures) to also support joining an
//! *externally created* namespace that isn't the container's own
//! userns — this project has no such feature (every namespace `ocirun`
//! ever creates is scoped to one container, created together), so the
//! simpler 2-phase version suffices here: join the user namespace
//! first if the container has one, then everything else.
//!
//! # Why the fds are all opened before joining anything
//!
//! Once the calling process has joined the container's mount or user
//! namespace, the *host's* `/proc/<pid>/ns/*` paths this module reads
//! from may no longer resolve the same way (a mount namespace change
//! can hide the host's own `/proc`; a user namespace change can lose
//! the permission to read another process's `/proc/<pid>/ns/*` at
//! all). `open_all` opens every namespace's file descriptor up front,
//! in the original namespace, before [`join_all`] ever calls
//! `setns(2)` — the same ordering `runc`'s own `__open_namespaces`/
//! `join_namespaces` split enforces.

use std::fs::File;
use std::io;
use std::path::Path;

use oci_spec_types::runtime::NamespaceType;
use rustix::thread::LinkNameSpaceType;

/// One namespace to join: which kind, and an already-open file
/// descriptor for `/proc/<pid>/ns/<name>` (opened by [`open_all`],
/// *before* joining anything — see this module's own doc comment).
pub struct OpenNamespace {
    kind: NamespaceType,
    file: File,
}

/// Open `/proc/<pid>/ns/<name>` for every namespace type in `kinds`
/// (deduplicated), ready to hand to [`join_all`]. Call this before
/// joining *any* of the returned namespaces.
pub fn open_all(pid: i32, kinds: &[NamespaceType]) -> io::Result<Vec<OpenNamespace>> {
    let proc_ns_dir = Path::new("/proc").join(pid.to_string()).join("ns");
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for &kind in kinds {
        if seen.contains(&kind) {
            continue;
        }
        seen.push(kind);
        let file = File::open(proc_ns_dir.join(filename(kind)))?;
        out.push(OpenNamespace { kind, file });
    }
    Ok(out)
}

/// Join every namespace in `namespaces`: the user namespace first (if
/// present), then everything else — see this module's own doc comment
/// on why that specific order, ported from real `runc`.
///
/// The calling process's own PID-namespace membership does *not*
/// change from joining a PID namespace here (the same `unshare(2)`
/// wrinkle `docs/design/0012` already documents for `create`/`run`) —
/// only a *subsequent* `fork(2)` lands a member of it. Callers that
/// need to actually run inside the joined PID namespace (rather than
/// merely being able to see it) must fork again afterward.
pub fn join_all(namespaces: Vec<OpenNamespace>) -> io::Result<()> {
    let (user, rest): (Vec<_>, Vec<_>) = namespaces
        .into_iter()
        .partition(|ns| ns.kind == NamespaceType::User);
    for ns in user {
        join_one(ns)?;
    }
    for ns in rest {
        join_one(ns)?;
    }
    Ok(())
}

fn join_one(ns: OpenNamespace) -> io::Result<()> {
    use std::os::fd::AsFd as _;
    rustix::thread::move_into_link_name_space(ns.file.as_fd(), Some(link_type(ns.kind)))
        .map_err(io::Error::from)
}

/// The `/proc/<pid>/ns/<name>` filename for one namespace type (the
/// kernel's own naming: `mnt`, not `mount`; everything else matches
/// the runtime-spec name).
fn filename(kind: NamespaceType) -> &'static str {
    match kind {
        NamespaceType::Pid => "pid",
        NamespaceType::Network => "net",
        NamespaceType::Mount => "mnt",
        NamespaceType::Ipc => "ipc",
        NamespaceType::Uts => "uts",
        NamespaceType::User => "user",
        NamespaceType::Cgroup => "cgroup",
        NamespaceType::Time => "time",
    }
}

fn link_type(kind: NamespaceType) -> LinkNameSpaceType {
    match kind {
        NamespaceType::Pid => LinkNameSpaceType::ProcessID,
        NamespaceType::Network => LinkNameSpaceType::Network,
        NamespaceType::Mount => LinkNameSpaceType::Mount,
        NamespaceType::Ipc => LinkNameSpaceType::InterProcessCommunication,
        NamespaceType::Uts => LinkNameSpaceType::HostNameAndNISDomainName,
        NamespaceType::User => LinkNameSpaceType::User,
        NamespaceType::Cgroup => LinkNameSpaceType::ControlGroup,
        NamespaceType::Time => LinkNameSpaceType::Time,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_matches_the_kernels_own_proc_ns_names() {
        assert_eq!(filename(NamespaceType::Mount), "mnt");
        assert_eq!(filename(NamespaceType::Network), "net");
        assert_eq!(filename(NamespaceType::Pid), "pid");
        assert_eq!(filename(NamespaceType::Ipc), "ipc");
        assert_eq!(filename(NamespaceType::Uts), "uts");
        assert_eq!(filename(NamespaceType::User), "user");
        assert_eq!(filename(NamespaceType::Cgroup), "cgroup");
        assert_eq!(filename(NamespaceType::Time), "time");
    }

    #[test]
    fn open_all_opens_this_own_processes_namespaces() {
        // A real, if narrow, integration check runnable in `cargo
        // test`'s own harness: every one of the calling process's own
        // `/proc/self/ns/*` entries is always openable by itself, so
        // this proves `open_all` builds the right paths and file
        // descriptors without needing a second process.
        let pid = std::process::id() as i32;
        let opened = open_all(
            pid,
            &[
                NamespaceType::Mount,
                NamespaceType::Uts,
                NamespaceType::Ipc,
                NamespaceType::Pid,
            ],
        )
        .unwrap();
        assert_eq!(opened.len(), 4);
    }

    #[test]
    fn open_all_deduplicates_repeated_kinds() {
        let pid = std::process::id() as i32;
        let opened = open_all(
            pid,
            &[
                NamespaceType::Mount,
                NamespaceType::Mount,
                NamespaceType::Uts,
            ],
        )
        .unwrap();
        assert_eq!(opened.len(), 2);
    }
}
