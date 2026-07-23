//! Planning a container's filesystem setup: the *sequence* of mount/
//! pivot_root/hostname operations a bundle's `config.json` calls for.
//!
//! [`oci_mount::syscalls`] deliberately stops at "make one `mount(2)`
//! call correctly" — building the sequence needs the whole bundle
//! (which mounts, which paths are masked/read-only, whether there's a
//! UTS namespace to set a hostname in) and was explicitly left for this
//! module (see that module's docs). [`plan_rootfs_setup`] is pure logic
//! and touches the filesystem *not at all*; nothing in this module
//! performs a `mount(2)`/`pivot_root(2)`/`stat(2)` call itself, `create`
//! (next) executes the plan via `oci_mount::syscalls`.
//!
//! **Masked-path classification is deliberately deferred to execution
//! time, not decided here.** A masked path needs different treatment
//! depending on whether it is (or would be) a file or a directory — a
//! read-only empty `tmpfs` for a directory (or a path that doesn't exist
//! at all yet), a bind-from-`/dev/null` for a file (confirmed via the
//! `strace` trace `oci_mount`'s design notes, 0007/0008, captured) — but
//! an earlier version of this module tried to answer "file or directory"
//! by `stat`-ing the bundle's rootfs *during planning*, before any of the
//! plan's own earlier mounts had actually happened. That's wrong for any
//! masked path a *later* part of the very same plan brings into
//! existence: `/proc/kcore`, `/proc/keys`, and several other standard
//! `maskedPaths` entries are procfs-provided pseudo-*files* that don't
//! exist at all until `/proc` itself is mounted (an earlier action in the
//! same plan) — so a pre-mount `stat` sees "missing" and (wrongly) plans
//! a directory-shaped `tmpfs` mount, which then fails at execution time
//! with `ENOTDIR` once `/proc` has actually made that path a file. Caught
//! by actually executing a generated plan end-to-end against a real
//! kernel (see the increment's design note) — not a hypothetical. Fixed
//! by making `RootfsAction::MaskPath` carry only the target path, leaving
//! the file-or-directory `stat` to whoever executes the plan (`create`),
//! which by construction only reaches a given masked-path action after
//! every earlier action — including the mounts that might have created
//! it — has already run.

use std::path::{Path, PathBuf};

use oci_mount::ParsedMountOptions;
use oci_spec_types::runtime::NamespaceType;

use crate::bundle::Bundle;

/// One step of a container's filesystem setup, in the order `create`
/// must perform them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootfsAction {
    /// Recursively make the whole mount tree private, so nothing this
    /// process does can propagate back out to the host (or, in a
    /// container, to a shared parent mount namespace).
    MakeMountsPrivate,
    /// Bind-mount `rootfs` onto itself: the standard trick to give it a
    /// distinct mount point, a prerequisite for `pivot_root`.
    BindRootfsOntoItself {
        /// The rootfs directory to bind onto itself.
        rootfs: PathBuf,
    },
    /// One `mount(2)` call.
    Mount {
        /// Mount target (absolute, inside the not-yet-pivoted rootfs).
        target: PathBuf,
        /// Mount source, if any.
        source: Option<String>,
        /// Filesystem type, if any.
        file_system_type: Option<String>,
        /// Already-parsed options.
        parsed: ParsedMountOptions,
    },
    /// A bind mount with no other flags — the first half of the
    /// bind-then-remount-readonly two-step (or, for a masked *file*, the
    /// whole operation: bind `/dev/null` over it, no remount needed).
    BindMount {
        /// Bind source.
        source: PathBuf,
        /// Bind target.
        target: PathBuf,
    },
    /// `mount(NULL, target, NULL, MS_REMOUNT|MS_BIND|MS_RDONLY, NULL)` —
    /// the second half of the bind-then-remount-readonly two-step.
    RemountReadonly {
        /// Target to remount read-only.
        target: PathBuf,
        /// Whether a real `EPERM` here is a known, tolerable rootless
        /// limitation, or a real failure that must never be silently
        /// swallowed.
        ///
        /// `true` only for `linux.readonly_paths`/a read-only root
        /// filesystem (`docs/design/0010`): these are commonly
        /// host-adjacent paths (e.g. `/proc/bus`, or `/sys` reached
        /// through a rootless root's own recursive bind) a
        /// fake-root-in-a-userns doesn't fully own the superblock of,
        /// where remounting read-only can legitimately require
        /// `CAP_SYS_ADMIN` in a namespace this process doesn't have.
        ///
        /// `false` for every real `-v`/`--volume` bind mount a caller
        /// explicitly requested `:ro` for (`bundle.spec.mounts`, e.g.
        /// a named volume this project's own code created and fully
        /// owns) — found and fixed a real, previously-unnoticed,
        /// security-relevant bug here directly (`docs/design/0232`):
        /// this single enum variant used to tolerate `EPERM`
        /// *unconditionally*, so a remount failure for a user's own
        /// explicitly-requested read-only volume was silently
        /// swallowed too, leaving it genuinely writable while
        /// `ociman run` itself reported success — a real, silent lie
        /// about a security-relevant guarantee the caller was relying
        /// on, not a benign, already-understood limitation the way
        /// `/sys` genuinely is.
        tolerate_permission_denied: bool,
    },
    /// Mask `target`: hide whatever is there. Whether that means binding
    /// `/dev/null` over a file or mounting an empty read-only `tmpfs`
    /// over a directory (or a path that doesn't exist yet) can only be
    /// decided once every earlier action in the plan has actually run —
    /// see the module docs — so it isn't decided here; `create` `stat`s
    /// `target` immediately before acting on this step.
    MaskPath {
        /// The path to mask (inside the not-yet-pivoted rootfs).
        target: PathBuf,
    },
    /// `pivot_root(new_root, put_old)`.
    PivotRoot {
        /// The new root (the container's rootfs).
        new_root: PathBuf,
        /// Where the old root gets relocated to (a directory under
        /// `new_root`).
        put_old: PathBuf,
    },
    /// Unmount the relocated old root and remove its now-empty directory.
    UnmountOldRoot {
        /// The relocated old root, as passed to the preceding
        /// [`RootfsAction::PivotRoot`].
        put_old: PathBuf,
    },
    /// `chdir("/")`, required after `pivot_root` (per `pivot_root(2)`:
    /// the caller's current working directory may still refer to the old
    /// root and should be changed).
    ChangeDirectoryToRoot,
    /// `sethostname(name)` — only planned when the bundle both sets a
    /// non-empty hostname and has a UTS namespace (matches
    /// `oci_runtime_core::validate`'s own `HostnameWithoutUts` check: a
    /// validated bundle never has one without the other, but this module
    /// doesn't assume `validate` already ran).
    SetHostname(String),
}

/// The directory name `pivot_root`'s old root gets relocated to, created
/// (and later removed) inside the new rootfs. Deliberately not
/// `.old_root`/`oldroot` (common choices that could collide with
/// something a real image ships at its rootfs top level); an oci-tools-
/// specific name avoids that even if it's cosmetically different from
/// what runc/crun happen to use internally.
const PUT_OLD_DIR_NAME: &str = ".oci-tools-put-old";

/// Plan the ordered filesystem setup `bundle` calls for, given its
/// already-resolved `rootfs` path (see [`crate::bundle::Bundle::rootfs_path`]).
pub fn plan_rootfs_setup(bundle: &Bundle, rootfs: &Path) -> Vec<RootfsAction> {
    let mut actions = vec![
        RootfsAction::MakeMountsPrivate,
        RootfsAction::BindRootfsOntoItself {
            rootfs: rootfs.to_path_buf(),
        },
    ];

    for mount in &bundle.spec.mounts {
        let target = join_under_root(rootfs, &mount.destination);
        let parsed = oci_mount::parse_mount_options(&mount.options);
        // The runtime-spec's mount type vocabulary predates cgroup v2 and
        // still says "cgroup" (a cgroup-v1-style single-hierarchy mount);
        // oci-tools targets cgroup v2 exclusively (no v1 host support to
        // fall back to), so the substitution is unconditional, not an
        // auto-detected-at-runtime choice. Confirmed against a real
        // `strace` of `runc run` on a cgroup-v2-unified host: it issues
        // `mount("cgroup", ..., "cgroup2", ...)` -- the source string is
        // untouched, only the filesystem type changes (see 0007/0010).
        let file_system_type = match mount.kind.as_deref() {
            Some("cgroup") => Some("cgroup2".to_string()),
            _ => mount.kind.clone(),
        };
        plan_one_mount(
            target,
            mount.source.clone(),
            file_system_type,
            parsed,
            // The one, real, already-documented (`docs/design/0010`)
            // rootless exception reaching this function at all: a
            // rootless container can't mount a fresh `sysfs` of its
            // own, so `Spec::into_rootless` bind-mounts the host's
            // real `/sys` read-only instead -- a real host filesystem
            // this process doesn't own the superblock of, where a
            // remount-to-read-only `EPERM` is a known, tolerable
            // limitation. Every *other* mount reaching this function
            // (a user's own `-v`/`--volume` bind mount, or any other
            // real bind entry) is this project's own or the caller's
            // own explicitly-requested mount, matched by its own real
            // OCI-spec-fixed destination the exact same way `Spec::
            // into_rootless` itself identifies this one special case
            // (`docs/design/0232`).
            mount.destination == "/sys",
            &mut actions,
        );
    }

    if let Some(linux) = &bundle.spec.linux {
        for path in &linux.readonly_paths {
            let target = join_under_root(rootfs, path);
            actions.push(RootfsAction::BindMount {
                source: target.clone(),
                target: target.clone(),
            });
            actions.push(RootfsAction::RemountReadonly {
                target,
                tolerate_permission_denied: true,
            });
        }
        for path in &linux.masked_paths {
            let target = join_under_root(rootfs, path);
            // File-vs-directory is decided by whoever executes this
            // step, not here — see the module docs.
            actions.push(RootfsAction::MaskPath { target });
        }
    }

    let put_old = rootfs.join(PUT_OLD_DIR_NAME);
    actions.push(RootfsAction::PivotRoot {
        new_root: rootfs.to_path_buf(),
        put_old: put_old.clone(),
    });
    actions.push(RootfsAction::UnmountOldRoot { put_old });
    actions.push(RootfsAction::ChangeDirectoryToRoot);

    if let Some(root) = &bundle.spec.root
        && root.readonly
    {
        actions.push(RootfsAction::BindMount {
            source: PathBuf::from("/"),
            target: PathBuf::from("/"),
        });
        actions.push(RootfsAction::RemountReadonly {
            target: PathBuf::from("/"),
            tolerate_permission_denied: true,
        });
    }

    if let Some(hostname) = &bundle.spec.hostname
        && !hostname.is_empty()
        && has_uts_namespace(bundle)
    {
        actions.push(RootfsAction::SetHostname(hostname.clone()));
    }

    actions
}

/// Plan one mount entry, splitting a combined bind+readonly request into
/// the two real `mount(2)` calls the kernel requires (see
/// `oci_mount::syscalls`' module docs and the 0007/0008 `strace`
/// verification): a plain bind, then a read-only remount.
fn plan_one_mount(
    target: PathBuf,
    source: Option<String>,
    file_system_type: Option<String>,
    parsed: ParsedMountOptions,
    tolerate_permission_denied: bool,
    actions: &mut Vec<RootfsAction>,
) {
    let is_bind = parsed.set_flags & oci_mount::options::flags::BIND != 0;
    let is_readonly = parsed.set_flags & oci_mount::options::flags::RDONLY != 0;
    let is_remount_or_move = parsed.set_flags
        & (oci_mount::options::flags::REMOUNT | oci_mount::options::flags::MOVE)
        != 0;

    if is_bind && is_readonly && !is_remount_or_move {
        let bind_only = ParsedMountOptions {
            set_flags: parsed.set_flags & !oci_mount::options::flags::RDONLY,
            ..parsed.clone()
        };
        actions.push(RootfsAction::Mount {
            target: target.clone(),
            source,
            file_system_type,
            parsed: bind_only,
        });
        actions.push(RootfsAction::RemountReadonly {
            target,
            tolerate_permission_denied,
        });
    } else {
        actions.push(RootfsAction::Mount {
            target,
            source,
            file_system_type,
            parsed,
        });
    }
}

/// Join a mount destination (as given in `config.json`, always absolute
/// per the runtime-spec) under `rootfs`, the same way
/// [`crate::bundle::Bundle::rootfs_path`] resolves the rootfs itself.
fn join_under_root(rootfs: &Path, destination: &str) -> PathBuf {
    let relative = destination.strip_prefix('/').unwrap_or(destination);
    rootfs.join(relative)
}

/// How a [`RootfsAction::MaskPath`] target should be handled, decided by
/// [`classify_masked_path`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskedPathKind {
    /// A regular file: bind-mount `/dev/null` over it.
    File,
    /// A directory: mount an empty, read-only `tmpfs` over it.
    Directory,
    /// Doesn't exist (or is some other special file type this crate
    /// doesn't have a defined masking strategy for, e.g. a device node
    /// or socket — conservatively treated the same as missing). Matches
    /// runc's own `maskPaths`: "open the target path; skip if it doesn't
    /// exist" — there's nothing to protect if the path was never there,
    /// and (for paths under a virtual filesystem like `/proc`) often
    /// nothing that *could* be created there to mask it even if we
    /// wanted to.
    Missing,
}

/// Classify a [`RootfsAction::MaskPath`] target. Callers executing a plan
/// should call this *immediately before* acting on a `MaskPath` step —
/// not any earlier — so it sees the effect of every action the plan
/// already ran (see the module docs for why that matters: a path can
/// come into existence partway through the very same plan).
pub fn classify_masked_path(target: &Path) -> MaskedPathKind {
    match std::fs::metadata(target) {
        Ok(m) if m.is_file() => MaskedPathKind::File,
        Ok(m) if m.is_dir() => MaskedPathKind::Directory,
        _ => MaskedPathKind::Missing,
    }
}

fn has_uts_namespace(bundle: &Bundle) -> bool {
    bundle
        .spec
        .linux
        .as_ref()
        .is_some_and(|l| l.namespaces.iter().any(|ns| ns.kind == NamespaceType::Uts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_spec_types::runtime::Spec;
    use std::fs;

    fn bundle_with(spec: Spec) -> (tempfile::TempDir, Bundle) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("rootfs")).unwrap();
        fs::write(
            dir.path().join("config.json"),
            serde_json::to_vec(&spec).unwrap(),
        )
        .unwrap();
        let bundle = Bundle::load(dir.path()).unwrap();
        (dir, bundle)
    }

    #[test]
    fn starts_with_private_then_self_bind() {
        let (_dir, bundle) = bundle_with(Spec::example());
        let rootfs = bundle.rootfs_path().unwrap();
        let actions = plan_rootfs_setup(&bundle, &rootfs);
        assert_eq!(actions[0], RootfsAction::MakeMountsPrivate);
        assert_eq!(
            actions[1],
            RootfsAction::BindRootfsOntoItself {
                rootfs: rootfs.clone()
            }
        );
    }

    #[test]
    fn plans_a_mount_per_spec_mount_entry_in_order() {
        let (_dir, bundle) = bundle_with(Spec::example());
        let rootfs = bundle.rootfs_path().unwrap();
        let actions = plan_rootfs_setup(&bundle, &rootfs);

        let mount_targets: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                RootfsAction::Mount { target, .. } => Some(target.clone()),
                _ => None,
            })
            .collect();
        // 7 spec mounts, none of which are masked-path tmpfs mounts (no
        // masked paths exist in a fresh empty rootfs used by this test,
        // so those add `Mount`s too -- check the spec mounts come first,
        // in spec order).
        assert_eq!(mount_targets[0], rootfs.join("proc"));
        assert_eq!(mount_targets[1], rootfs.join("dev"));
        assert_eq!(mount_targets[2], rootfs.join("dev/pts"));
        assert_eq!(mount_targets[3], rootfs.join("dev/shm"));
        assert_eq!(mount_targets[4], rootfs.join("dev/mqueue"));
        assert_eq!(mount_targets[5], rootfs.join("sys"));
        assert_eq!(mount_targets[6], rootfs.join("sys/fs/cgroup"));
    }

    #[test]
    fn cgroup_mount_type_is_substituted_with_cgroup2() {
        // oci-tools targets cgroup v2 exclusively (no v1 host to fall
        // back to), so this is unconditional -- confirmed against a real
        // `strace` of `runc run` on a cgroup-v2-unified host (0007/0010).
        let (_dir, bundle) = bundle_with(Spec::example());
        let rootfs = bundle.rootfs_path().unwrap();
        let actions = plan_rootfs_setup(&bundle, &rootfs);

        let cgroup_mount = actions
            .iter()
            .find(|a| matches!(a, RootfsAction::Mount { target, .. } if *target == rootfs.join("sys/fs/cgroup")))
            .unwrap();
        let RootfsAction::Mount {
            source,
            file_system_type,
            ..
        } = cgroup_mount
        else {
            panic!("expected a Mount action");
        };
        assert_eq!(
            source.as_deref(),
            Some("cgroup"),
            "source string is untouched"
        );
        assert_eq!(file_system_type.as_deref(), Some("cgroup2"));
    }

    #[test]
    fn ends_with_pivot_root_unmount_chdir_then_hostname() {
        let (_dir, bundle) = bundle_with(Spec::example());
        let rootfs = bundle.rootfs_path().unwrap();
        let actions = plan_rootfs_setup(&bundle, &rootfs);

        let put_old = rootfs.join(PUT_OLD_DIR_NAME);
        let pivot_index = actions
            .iter()
            .position(|a| matches!(a, RootfsAction::PivotRoot { .. }))
            .unwrap();
        assert_eq!(
            &actions[pivot_index..pivot_index + 3],
            &[
                RootfsAction::PivotRoot {
                    new_root: rootfs.clone(),
                    put_old: put_old.clone()
                },
                RootfsAction::UnmountOldRoot { put_old },
                RootfsAction::ChangeDirectoryToRoot,
            ]
        );
        // Spec::example()'s root.readonly is true, so a bind+remount on
        // "/" comes between the chdir and the hostname (see
        // `readonly_root_adds_bind_then_remount_on_new_root`); the
        // hostname action must still be last overall.
        assert_eq!(
            actions.last(),
            Some(&RootfsAction::SetHostname("ocirun".to_string()))
        );
    }

    #[test]
    fn no_hostname_action_without_uts_namespace() {
        let mut spec = Spec::example();
        spec.linux
            .as_mut()
            .unwrap()
            .namespaces
            .retain(|ns| ns.kind != NamespaceType::Uts);
        let (_dir, bundle) = bundle_with(spec);
        let rootfs = bundle.rootfs_path().unwrap();
        let actions = plan_rootfs_setup(&bundle, &rootfs);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, RootfsAction::SetHostname(_)))
        );
    }

    #[test]
    fn readonly_root_adds_bind_then_remount_on_new_root() {
        let (_dir, bundle) = bundle_with(Spec::example());
        let rootfs = bundle.rootfs_path().unwrap();
        let actions = plan_rootfs_setup(&bundle, &rootfs);
        // Spec::example()'s root.readonly is true.
        let root_slash = PathBuf::from("/");
        assert!(actions.contains(&RootfsAction::BindMount {
            source: root_slash.clone(),
            target: root_slash.clone()
        }));
        assert!(actions.contains(&RootfsAction::RemountReadonly {
            target: root_slash,
            tolerate_permission_denied: true
        }));
    }

    #[test]
    fn non_readonly_root_skips_the_remount() {
        let mut spec = Spec::example();
        spec.root.as_mut().unwrap().readonly = false;
        let (_dir, bundle) = bundle_with(spec);
        let rootfs = bundle.rootfs_path().unwrap();
        let actions = plan_rootfs_setup(&bundle, &rootfs);
        assert!(!actions.iter().any(
            |a| matches!(a, RootfsAction::RemountReadonly { target, .. } if target == Path::new("/"))
        ));
    }

    #[test]
    fn readonly_bind_mount_splits_into_bind_then_remount() {
        let mut spec = Spec::example();
        spec.mounts.push(oci_spec_types::runtime::Mount {
            destination: "/data".to_string(),
            source: Some("/host/data".to_string()),
            kind: None,
            options: vec!["rbind".to_string(), "ro".to_string()],
        });
        let (_dir, bundle) = bundle_with(spec);
        let rootfs = bundle.rootfs_path().unwrap();
        let actions = plan_rootfs_setup(&bundle, &rootfs);

        let data_target = rootfs.join("data");
        let bind_index = actions
            .iter()
            .position(|a| matches!(a, RootfsAction::Mount { target, .. } if *target == data_target))
            .unwrap();
        let Some(RootfsAction::Mount { parsed, .. }) = actions.get(bind_index) else {
            panic!("expected a Mount action");
        };
        // The split-out bind mount must NOT carry RDONLY (it's applied by
        // the following remount instead -- the kernel rejects it
        // atomically with BIND on a fresh bind mount).
        assert_eq!(parsed.set_flags & oci_mount::options::flags::RDONLY, 0);
        assert_ne!(parsed.set_flags & oci_mount::options::flags::BIND, 0);
        assert_eq!(
            actions[bind_index + 1],
            RootfsAction::RemountReadonly {
                target: data_target,
                // A real, user-requested volume/bind mount --
                // `tolerate_permission_denied` must be `false` (see
                // `docs/design/0232`), never silently swallowing a
                // real remount failure the way `readonly_paths`/root
                // legitimately can.
                tolerate_permission_denied: false,
            }
        );
    }

    /// The one, real, already-documented (`docs/design/0010`) rootless
    /// exception: `/sys`'s own read-only bind mount (`Spec::
    /// into_rootless`'s own doc comment: a rootless container can't
    /// mount a fresh `sysfs`, so it bind-mounts the host's real `/sys`
    /// read-only instead) is a real host filesystem this process
    /// doesn't own the superblock of — a remount-to-read-only `EPERM`
    /// there is a known, tolerable limitation, unlike any other bind
    /// mount reaching the exact same code path (`docs/design/0232`,
    /// found and fixed directly: an earlier version of this code
    /// tolerated `EPERM` for *every* mount here, including a real
    /// user's own explicitly-requested `-v name:/path:ro` volume,
    /// silently leaving it genuinely writable).
    #[test]
    fn sys_bind_mount_remount_tolerates_permission_denied_but_a_real_volume_never_does() {
        let mut spec = Spec::example();
        spec.mounts.push(oci_spec_types::runtime::Mount {
            destination: "/sys".to_string(),
            source: Some("/sys".to_string()),
            kind: Some("none".to_string()),
            options: vec!["rbind".to_string(), "ro".to_string()],
        });
        spec.mounts.push(oci_spec_types::runtime::Mount {
            destination: "/data".to_string(),
            source: Some("/host/data".to_string()),
            kind: Some("bind".to_string()),
            options: vec!["rbind".to_string(), "ro".to_string()],
        });
        let (_dir, bundle) = bundle_with(spec);
        let rootfs = bundle.rootfs_path().unwrap();
        let actions = plan_rootfs_setup(&bundle, &rootfs);

        let sys_target = rootfs.join("sys");
        let data_target = rootfs.join("data");
        let tolerates = |target: &Path| {
            actions.iter().find_map(|a| match a {
                RootfsAction::RemountReadonly {
                    target: t,
                    tolerate_permission_denied,
                } if t == target => Some(*tolerate_permission_denied),
                _ => None,
            })
        };
        assert_eq!(tolerates(&sys_target), Some(true));
        assert_eq!(tolerates(&data_target), Some(false));
    }

    #[test]
    fn plain_bind_mount_without_readonly_is_not_split() {
        let mut spec = Spec::example();
        spec.mounts.push(oci_spec_types::runtime::Mount {
            destination: "/data".to_string(),
            source: Some("/host/data".to_string()),
            kind: None,
            options: vec!["rbind".to_string()],
        });
        let (_dir, bundle) = bundle_with(spec);
        let rootfs = bundle.rootfs_path().unwrap();
        let actions = plan_rootfs_setup(&bundle, &rootfs);

        let data_target = rootfs.join("data");
        assert!(!actions.contains(&RootfsAction::RemountReadonly {
            target: data_target,
            tolerate_permission_denied: false
        }));
    }

    #[test]
    fn masked_paths_plan_as_mask_path_with_no_file_vs_directory_decision() {
        // Planning must not stat anything: it can't know, for a path
        // like `/proc/acpi`, whether a *later* action in this very plan
        // (mounting `/proc`) will make it a file, a directory, or leave
        // it missing -- see the module docs. Every masked path becomes a
        // `MaskPath`, decided only once whoever executes the plan
        // actually gets there.
        let mut spec = Spec::example();
        spec.linux.as_mut().unwrap().masked_paths =
            vec!["/proc/acpi".to_string(), "/etc/secret".to_string()];
        let (_dir, bundle) = bundle_with(spec);
        let rootfs = bundle.rootfs_path().unwrap();
        let actions = plan_rootfs_setup(&bundle, &rootfs);

        assert!(actions.contains(&RootfsAction::MaskPath {
            target: rootfs.join("proc/acpi")
        }));
        assert!(actions.contains(&RootfsAction::MaskPath {
            target: rootfs.join("etc/secret")
        }));
    }

    #[test]
    fn classify_masked_path_reflects_real_filesystem_state() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a-file");
        let subdir = dir.path().join("a-dir");
        let missing = dir.path().join("does-not-exist");
        fs::write(&file, b"x").unwrap();
        fs::create_dir(&subdir).unwrap();

        assert_eq!(classify_masked_path(&file), MaskedPathKind::File);
        assert_eq!(classify_masked_path(&subdir), MaskedPathKind::Directory);
        assert_eq!(classify_masked_path(&missing), MaskedPathKind::Missing);
    }
}
