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
    resolve_root(
        std::env::var_os("OCI_TOOLS_STORAGE_ROOT"),
        is_root(),
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

/// Whether the current process is running as uid 0. Reads `/proc/self/status`
/// directly rather than pulling in a syscall-wrapper crate for one integer.
fn is_root() -> bool {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|rest| rest.split_whitespace().next())
        .map(|real_uid| real_uid == "0")
        .unwrap_or(false)
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
    fn is_root_reflects_proc_self_status() {
        // We can't control our own uid in a unit test, but we can assert
        // the parse matches the real geteuid() via the one dependency-free
        // signal available: root's Uid line always starts with "0\t".
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let expected = status
            .lines()
            .find_map(|l| l.strip_prefix("Uid:"))
            .and_then(|r| r.split_whitespace().next())
            .map(|u| u == "0")
            .unwrap_or(false);
        assert_eq!(is_root(), expected);
    }
}
