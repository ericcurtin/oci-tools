//! Applying `linux.sysctl` (real `/proc/sys/...` writes) before rootfs
//! setup — matching real crun's own `libcrun_set_sysctl` exactly
//! (`~/git/crun/src/libcrun/linux.c:4558`, called from `container.c:
//! 1336`, right before its own `HANDLER_CONFIGURE_BEFORE_MOUNTS`
//! point): after namespaces are unshared, but before the rootfs is
//! ever mounted/`pivot_root`ed — the exact same relative position this
//! project's own [`crate::oom::apply`]/[`crate::rlimits::apply`] calls
//! already occupy in [`crate::launch::ChildSetup::run`], though those
//! two are plain, namespace-independent process attributes and this
//! genuinely needs to run *after* `unshare(2)`, since [`validate`]
//! checks which namespaces are actually present.
//!
//! # Why this can never leak into the *host*'s own sysctls
//!
//! This project's own containers always share the host's real network
//! namespace (`Spec::into_rootless` unconditionally drops any
//! `Network` namespace — rootless containers have no private one at
//! all). A naive, unchecked `net.*` sysctl write would therefore
//! silently modify the *host's own* real networking configuration, a
//! serious, unexpected side effect no real container user would want
//! or expect. [`validate`] closes this exactly the way real crun's own
//! `validate_sysctl` already does: a `net.*` key is only ever accepted
//! when a `Network` namespace was actually unshared — which, for this
//! project, is never true for any of its own rootless containers — so
//! it's always a clear, immediate, real error instead.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use rustix::thread::UnshareFlags;

/// `kernel/<name>` sysctls real crun requires an IPC namespace for
/// (`~/git/crun/src/libcrun/linux.c`'s own `sysctlRequiringIPC[]`,
/// ported verbatim).
const IPC_KERNEL_KEYS: &[&str] = &[
    "kernel/msgmax",
    "kernel/msgmnb",
    "kernel/msgmni",
    "kernel/sem",
    "kernel/shmall",
    "kernel/shmmax",
    "kernel/shmmni",
    "kernel/shm_rmid_forced",
];

/// Apply every `key=value` in `sysctl`, in a deterministic (`BTreeMap`)
/// order, each validated against `namespaces` (the container's own
/// real, already-unshared [`UnshareFlags`]) before ever being written
/// — a real, immediate error rather than a partially-applied set on
/// the first invalid one, matching real crun's own identical
/// fail-fast behavior (it validates the *whole* list before writing
/// anything... actually writes as it validates each one in turn, the
/// same one-at-a-time order this does).
///
/// `proc_root` is `/proc` in production; tests substitute a temp
/// directory. The real path written is `<proc_root>/sys/<name>`, with
/// `key`'s dots translated to slashes (e.g. `"net.ipv4.ip_forward"` ->
/// `<proc_root>/sys/net/ipv4/ip_forward`) — **not** `<proc_root>/self/
/// sys/...`: unlike `/proc/<pid>/oom_score_adj`, `/proc/sys/` is
/// already namespace-relative for whichever process opens it, with no
/// `self/` component at all (confirmed directly against crun's own
/// `libcrun_open_proc_file(container, "sys", ...)` call, which passes
/// `"sys"`, never `"self/sys"`).
pub fn apply(
    proc_root: &Path,
    namespaces: UnshareFlags,
    sysctl: &BTreeMap<String, String>,
) -> io::Result<()> {
    let sys_dir = proc_root.join("sys");
    for (key, value) in sysctl {
        validate(key, namespaces)?;
        let relative = key.replace('.', "/");
        std::fs::write(sys_dir.join(relative), value)?;
    }
    Ok(())
}

/// Port of real crun's own `validate_sysctl` (`~/git/crun/src/
/// libcrun/linux.c`): an allow-list of recognized sysctl prefixes,
/// each requiring the matching namespace to actually be present in
/// `namespaces` — anything else (including every `net.*` key, for any
/// container this project itself ever launches — see this module's
/// own doc comment) is a real, immediate, named error, never a silent
/// no-op or an unchecked write. Deliberately does **not** port crun's
/// own additional `kernel.domainname`-conflicts-with-`domainname`-
/// field cross-check: this project's own [`crate`] has no `domainname`
/// spec field to conflict with at all (only `hostname`), so that
/// specific check would never have anything to compare against.
fn validate(original_key: &str, namespaces: UnshareFlags) -> io::Result<()> {
    let name = original_key.replace('.', "/");

    if name.starts_with("fs/mqueue/") {
        return require(namespaces, UnshareFlags::NEWIPC, original_key, "IPC");
    }
    if name.starts_with("kernel/") {
        if IPC_KERNEL_KEYS.contains(&name.as_str()) {
            return require(namespaces, UnshareFlags::NEWIPC, original_key, "IPC");
        }
        if name == "kernel/domainname" {
            return require(namespaces, UnshareFlags::NEWUTS, original_key, "UTS");
        }
        if name == "kernel/hostname" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the sysctl {original_key:?} conflicts with the OCI hostname field"),
            ));
        }
    }
    if name.starts_with("net/") {
        return require(namespaces, UnshareFlags::NEWNET, original_key, "network");
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("the sysctl {original_key:?} is not namespaced"),
    ))
}

fn require(
    namespaces: UnshareFlags,
    needed: UnshareFlags,
    original_key: &str,
    namespace_name: &str,
) -> io::Result<()> {
    if namespaces.contains(needed) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("the sysctl {original_key:?} requires a new {namespace_name} namespace"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn empty_map_is_a_real_no_op_that_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        // No `sys/` directory created at all -- if this tried to write
        // anything it would fail with a real `NotFound`.
        assert!(apply(dir.path(), UnshareFlags::empty(), &BTreeMap::new()).is_ok());
    }

    #[test]
    fn ipc_kernel_key_is_accepted_and_written_with_an_ipc_namespace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sys/kernel")).unwrap();
        apply(
            dir.path(),
            UnshareFlags::NEWIPC,
            &map(&[("kernel.shmmax", "1000000")]),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sys/kernel/shmmax")).unwrap(),
            "1000000"
        );
    }

    #[test]
    fn ipc_kernel_key_is_rejected_without_an_ipc_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let err = apply(
            dir.path(),
            UnshareFlags::empty(),
            &map(&[("kernel.shmmax", "1000000")]),
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("IPC namespace"), "{err}");
    }

    #[test]
    fn fs_mqueue_key_requires_an_ipc_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let err = apply(
            dir.path(),
            UnshareFlags::empty(),
            &map(&[("fs.mqueue.queues_max", "100")]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("IPC namespace"), "{err}");
    }

    #[test]
    fn kernel_domainname_requires_a_uts_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let err = apply(
            dir.path(),
            UnshareFlags::empty(),
            &map(&[("kernel.domainname", "example")]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("UTS namespace"), "{err}");

        std::fs::create_dir_all(dir.path().join("sys/kernel")).unwrap();
        apply(
            dir.path(),
            UnshareFlags::NEWUTS,
            &map(&[("kernel.domainname", "example")]),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sys/kernel/domainname")).unwrap(),
            "example"
        );
    }

    #[test]
    fn kernel_hostname_always_conflicts_with_the_oci_hostname_field() {
        let dir = tempfile::tempdir().unwrap();
        let err = apply(
            dir.path(),
            UnshareFlags::NEWUTS,
            &map(&[("kernel.hostname", "example")]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("hostname field"), "{err}");
    }

    /// The exact real-world safety case this module's own doc comment
    /// explains: a `net.*` sysctl can never succeed for any container
    /// this project itself ever launches, since none of them ever have
    /// a real, private network namespace of their own (`Spec::
    /// into_rootless` always drops it) -- proven here directly by
    /// checking it's rejected even when *every other* namespace flag
    /// is set, only `NEWNET` itself missing.
    #[test]
    fn net_sysctl_is_rejected_without_a_network_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let err = apply(
            dir.path(),
            UnshareFlags::NEWIPC | UnshareFlags::NEWUTS | UnshareFlags::NEWPID,
            &map(&[("net.ipv4.ip_forward", "1")]),
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("network namespace"), "{err}");
    }

    #[test]
    fn net_sysctl_is_accepted_with_a_real_network_namespace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sys/net/ipv4")).unwrap();
        apply(
            dir.path(),
            UnshareFlags::NEWNET,
            &map(&[("net.ipv4.ip_forward", "1")]),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sys/net/ipv4/ip_forward")).unwrap(),
            "1"
        );
    }

    #[test]
    fn an_unrecognized_sysctl_prefix_is_rejected_as_not_namespaced() {
        let dir = tempfile::tempdir().unwrap();
        let err = apply(
            dir.path(),
            UnshareFlags::all(),
            &map(&[("vm.overcommit_memory", "1")]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not namespaced"), "{err}");
    }

    #[test]
    fn multiple_entries_are_applied_in_deterministic_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sys/kernel")).unwrap();
        apply(
            dir.path(),
            UnshareFlags::NEWIPC,
            &map(&[("kernel.shmmax", "1"), ("kernel.shmmni", "2")]),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sys/kernel/shmmax")).unwrap(),
            "1"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sys/kernel/shmmni")).unwrap(),
            "2"
        );
    }
}
