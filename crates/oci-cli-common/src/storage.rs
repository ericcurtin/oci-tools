//! Default storage-root resolution, shared by every binary that opens an
//! `oci_store::Store` (`ociman` today; `ocicri` and `ociboot` later).

use std::ffi::OsString;
use std::path::PathBuf;

/// Resolve the default oci-tools storage root:
///
/// * `$OCI_TOOLS_STORAGE_ROOT`, if set, always wins (tests and packaging
///   both rely on this).
/// * Running as root: `/var/lib/oci-tools/storage`, matching the
///   `/var/lib/containers/storage` convention.
/// * Rootless: `$XDG_DATA_HOME/oci-tools/storage`, defaulting
///   `XDG_DATA_HOME` to `~/.local/share` per the XDG base directory spec.
pub fn default_root() -> PathBuf {
    let (euid, _) = crate::identity::effective_uid_gid();
    resolve_root(
        std::env::var_os("OCI_TOOLS_STORAGE_ROOT"),
        euid == 0,
        std::env::var_os("XDG_DATA_HOME"),
        std::env::var_os("HOME"),
    )
}

/// The pure decision logic behind [`default_root`], taking every input
/// explicitly so it can be unit-tested without mutating process-global
/// environment state.
fn resolve_root(
    storage_root_override: Option<OsString>,
    running_as_root: bool,
    xdg_data_home: Option<OsString>,
    home: Option<OsString>,
) -> PathBuf {
    if let Some(dir) = storage_root_override {
        return PathBuf::from(dir);
    }
    if running_as_root {
        return PathBuf::from("/var/lib/oci-tools/storage");
    }
    let data_home = xdg_data_home
        .map(PathBuf::from)
        .or_else(|| home.map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from(".local/share"));
    data_home.join("oci-tools").join("storage")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_wins_regardless_of_uid_or_xdg() {
        let root = resolve_root(
            Some(OsString::from("/tmp/example-root")),
            true,
            Some(OsString::from("/ignored")),
            None,
        );
        assert_eq!(root, PathBuf::from("/tmp/example-root"));
    }

    #[test]
    fn root_uses_var_lib() {
        let root = resolve_root(None, true, None, Some(OsString::from("/home/x")));
        assert_eq!(root, PathBuf::from("/var/lib/oci-tools/storage"));
    }

    #[test]
    fn rootless_prefers_xdg_data_home() {
        let root = resolve_root(
            None,
            false,
            Some(OsString::from("/custom/data")),
            Some(OsString::from("/home/x")),
        );
        assert_eq!(root, PathBuf::from("/custom/data/oci-tools/storage"));
    }

    #[test]
    fn rootless_falls_back_to_home_local_share() {
        let root = resolve_root(None, false, None, Some(OsString::from("/home/x")));
        assert_eq!(
            root,
            PathBuf::from("/home/x/.local/share/oci-tools/storage")
        );
    }

    #[test]
    fn default_root_agrees_with_real_euid() {
        // End-to-end sanity check that default_root() actually consults
        // the real process identity (identity::tests covers the
        // /proc/self/status parsing itself in detail).
        let (euid, _) = crate::identity::effective_uid_gid();
        let root = default_root();
        if euid == 0 {
            assert_eq!(root, PathBuf::from("/var/lib/oci-tools/storage"));
        } else {
            assert_ne!(root, PathBuf::from("/var/lib/oci-tools/storage"));
        }
    }
}
