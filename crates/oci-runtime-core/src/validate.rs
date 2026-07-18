//! Config sanity checks that must pass before a bundle can be created,
//! ported from (a deliberately partial subset of) runc's
//! `libcontainer/configs/validate`. Pure logic: no privilege, no
//! namespace/process manipulation, just internal-consistency checks on a
//! parsed [`Spec`] plus one filesystem stat (does the rootfs exist).
//!
//! Scope shipped so far: rootfs existence, `process.args` non-empty, UTS
//! (hostname needs a UTS namespace), mount-restriction paths (masked/
//! read-only paths need a mount namespace), namespace-list duplicates,
//! and user-namespace/ID-mapping consistency. Not yet ported: sysctls,
//! Intel RDT, scheduler/IO-priority/memory-policy, SELinux label
//! availability, network device checks, and the "is this namespace type
//! actually enabled in the running kernel" `/proc/self/ns/*` checks
//! (those depend on the host kernel config, which would make validation
//! behavior vary between otherwise-identical test runs; they land when
//! `create` actually needs to join/create those namespaces and can
//! surface the same failure directly from the syscall instead).
//!
//! **Cross-checked against a real `runc create`**, not just its source:
//! [`ValidateError::HostnameWithoutUts`] and
//! [`ValidateError::RestrictedPathsWithoutMountNs`] reproduce verbatim
//! (the latter's wording adjusted to match) by feeding runc a `runc
//! spec`-generated bundle with the relevant namespace stripped out.
//! [`ValidateError::MappingsWithoutUserNamespace`] is source-derived only
//! — runc's `specconv` conversion step appears to silently drop
//! `uidMappings`/`gidMappings` before its own equivalent check ever runs
//! when no user namespace is requested, so real `runc create` does not
//! actually reject that combination in practice, even though the
//! validator source says it should. Rejecting it here anyway is stricter
//! than upstream, not looser: it flags a config that upstream silently
//! ignores rather than silently accepting it too.

use std::path::{Path, PathBuf};

use oci_spec_types::runtime::NamespaceType;

use crate::bundle::Bundle;

/// Errors from [`validate`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidateError {
    /// `config.json` has no `process`.
    #[error("config has no process")]
    NoProcess,
    /// `process.args` is empty.
    #[error("process.args must not be empty")]
    EmptyArgs,
    /// `config.json` has no `root`.
    #[error("config has no root")]
    NoRoot,
    /// The resolved rootfs path doesn't exist (or isn't a directory).
    #[error("rootfs {0:?} does not exist or is not a directory")]
    RootfsMissing(PathBuf),
    /// `hostname` is set without a UTS namespace.
    #[error("unable to set hostname without a private UTS namespace")]
    HostnameWithoutUts,
    /// `maskedPaths`/`readonlyPaths` are set without a mount namespace.
    #[error("unable to restrict sys entries without a private MNT namespace")]
    RestrictedPathsWithoutMountNs,
    /// The same namespace type appears more than once in
    /// `linux.namespaces`.
    #[error("duplicate {0:?} namespace entry")]
    DuplicateNamespace(NamespaceType),
    /// A user namespace is requested but has neither a path to join nor
    /// any ID mappings.
    #[error(
        "user namespace enabled, but no namespace path to join nor mappings to apply specified"
    )]
    UserNamespaceWithoutMappings,
    /// ID mappings are present without a user namespace.
    #[error("uid/gid mappings specified, but no user namespace is enabled")]
    MappingsWithoutUserNamespace,
}

/// Validate `bundle.spec` against the checks listed in the module docs,
/// returning the verified rootfs path on success (so callers don't have
/// to re-derive and re-check it).
pub fn validate(bundle: &Bundle) -> Result<PathBuf, ValidateError> {
    let process = bundle
        .spec
        .process
        .as_ref()
        .ok_or(ValidateError::NoProcess)?;
    if process.args.is_empty() {
        return Err(ValidateError::EmptyArgs);
    }

    bundle.spec.root.as_ref().ok_or(ValidateError::NoRoot)?;
    let rootfs = bundle.rootfs_path().ok_or(ValidateError::NoRoot)?;
    if !is_directory(&rootfs) {
        return Err(ValidateError::RootfsMissing(rootfs));
    }

    let namespaces = bundle
        .spec
        .linux
        .as_ref()
        .map(|l| l.namespaces.as_slice())
        .unwrap_or(&[]);
    let has_ns = |kind: NamespaceType| namespaces.iter().any(|ns| ns.kind == kind);

    if bundle
        .spec
        .hostname
        .as_deref()
        .is_some_and(|h| !h.is_empty())
        && !has_ns(NamespaceType::Uts)
    {
        return Err(ValidateError::HostnameWithoutUts);
    }

    let linux = bundle.spec.linux.as_ref();
    let has_restricted_paths =
        linux.is_some_and(|l| !l.masked_paths.is_empty() || !l.readonly_paths.is_empty());
    if has_restricted_paths && !has_ns(NamespaceType::Mount) {
        return Err(ValidateError::RestrictedPathsWithoutMountNs);
    }

    for (i, ns) in namespaces.iter().enumerate() {
        if namespaces[..i]
            .iter()
            .any(|earlier| earlier.kind == ns.kind)
        {
            return Err(ValidateError::DuplicateNamespace(ns.kind));
        }
    }

    let has_mappings =
        linux.is_some_and(|l| !l.uid_mappings.is_empty() || !l.gid_mappings.is_empty());
    if has_ns(NamespaceType::User) {
        let user_ns_has_path = namespaces
            .iter()
            .any(|ns| ns.kind == NamespaceType::User && ns.path.is_some());
        if !user_ns_has_path && !has_mappings {
            return Err(ValidateError::UserNamespaceWithoutMappings);
        }
    } else if has_mappings {
        return Err(ValidateError::MappingsWithoutUserNamespace);
    }

    Ok(rootfs)
}

fn is_directory(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_spec_types::runtime::{LinuxIdMapping, Spec};
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
    fn accepts_the_default_example_spec() {
        let (_dir, bundle) = bundle_with(Spec::example());
        let rootfs = validate(&bundle).unwrap();
        assert_eq!(rootfs, bundle.path.join("rootfs"));
    }

    #[test]
    fn accepts_the_rootless_example_spec() {
        let (_dir, bundle) = bundle_with(Spec::example().into_rootless(1000, 1000));
        validate(&bundle).unwrap();
    }

    #[test]
    fn rejects_missing_process() {
        let mut spec = Spec::example();
        spec.process = None;
        let (_dir, bundle) = bundle_with(spec);
        assert_eq!(validate(&bundle), Err(ValidateError::NoProcess));
    }

    #[test]
    fn rejects_empty_args() {
        let mut spec = Spec::example();
        spec.process.as_mut().unwrap().args = vec![];
        let (_dir, bundle) = bundle_with(spec);
        assert_eq!(validate(&bundle), Err(ValidateError::EmptyArgs));
    }

    #[test]
    fn rejects_missing_root() {
        let mut spec = Spec::example();
        spec.root = None;
        let (_dir, bundle) = bundle_with(spec);
        assert_eq!(validate(&bundle), Err(ValidateError::NoRoot));
    }

    #[test]
    fn rejects_nonexistent_rootfs() {
        let mut spec = Spec::example();
        spec.root.as_mut().unwrap().path = "does-not-exist".to_string();
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.json"),
            serde_json::to_vec(&spec).unwrap(),
        )
        .unwrap();
        let bundle = Bundle::load(dir.path()).unwrap();
        assert_eq!(
            validate(&bundle),
            Err(ValidateError::RootfsMissing(
                dir.path().join("does-not-exist")
            ))
        );
    }

    #[test]
    fn rejects_hostname_without_uts_namespace() {
        let mut spec = Spec::example();
        spec.linux
            .as_mut()
            .unwrap()
            .namespaces
            .retain(|ns| ns.kind != NamespaceType::Uts);
        let (_dir, bundle) = bundle_with(spec);
        assert_eq!(validate(&bundle), Err(ValidateError::HostnameWithoutUts));
    }

    #[test]
    fn rejects_restricted_paths_without_mount_namespace() {
        let mut spec = Spec::example();
        spec.linux
            .as_mut()
            .unwrap()
            .namespaces
            .retain(|ns| ns.kind != NamespaceType::Mount);
        let (_dir, bundle) = bundle_with(spec);
        assert_eq!(
            validate(&bundle),
            Err(ValidateError::RestrictedPathsWithoutMountNs)
        );
    }

    #[test]
    fn rejects_duplicate_namespace_entries() {
        let mut spec = Spec::example();
        let pid_ns = spec.linux.as_ref().unwrap().namespaces[0].clone();
        spec.linux.as_mut().unwrap().namespaces.push(pid_ns);
        let (_dir, bundle) = bundle_with(spec);
        assert_eq!(
            validate(&bundle),
            Err(ValidateError::DuplicateNamespace(NamespaceType::Pid))
        );
    }

    #[test]
    fn rejects_mappings_without_user_namespace() {
        let mut spec = Spec::example();
        spec.linux.as_mut().unwrap().uid_mappings = vec![LinuxIdMapping {
            host_id: 1000,
            container_id: 0,
            size: 1,
        }];
        let (_dir, bundle) = bundle_with(spec);
        assert_eq!(
            validate(&bundle),
            Err(ValidateError::MappingsWithoutUserNamespace)
        );
    }

    #[test]
    fn rejects_user_namespace_without_mappings_or_path() {
        let mut spec = Spec::example();
        spec.linux
            .as_mut()
            .unwrap()
            .namespaces
            .push(oci_spec_types::runtime::LinuxNamespace::new(
                NamespaceType::User,
            ));
        let (_dir, bundle) = bundle_with(spec);
        assert_eq!(
            validate(&bundle),
            Err(ValidateError::UserNamespaceWithoutMappings)
        );
    }

    #[test]
    fn accepts_user_namespace_with_join_path_and_no_mappings() {
        let mut spec = Spec::example();
        spec.linux
            .as_mut()
            .unwrap()
            .namespaces
            .push(oci_spec_types::runtime::LinuxNamespace {
                kind: NamespaceType::User,
                path: Some("/proc/1234/ns/user".to_string()),
            });
        let (_dir, bundle) = bundle_with(spec);
        validate(&bundle).unwrap();
    }
}
