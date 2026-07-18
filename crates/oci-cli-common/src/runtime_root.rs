//! Default state-directory resolution for OCI runtime binaries (`ocirun`
//! today; `ocicri`'s runtime-facing state root later), mirroring runc's
//! `--root` default exactly (including its quirks) rather than inventing a
//! "nicer" default that would silently disagree with the tool oci-tools
//! must be a drop-in replacement for.
//!
//! runc's rule (`shouldHonorXDGRuntimeDir` in `rootless_linux.go`): use
//! `$XDG_RUNTIME_DIR/runc` when `$XDG_RUNTIME_DIR` is set *and* the caller
//! isn't "real" root (euid 0 outside any user namespace); otherwise
//! `/run/runc`. Note this means a rootless invocation with
//! `$XDG_RUNTIME_DIR` unset still defaults to `/run/<name>` (which a
//! rootless user typically cannot write to) — that is runc's real
//! behavior, not a bug introduced here; every real deployment either has
//! `XDG_RUNTIME_DIR` set (systemd-logind sessions always set it) or passes
//! `--root` explicitly.
//!
//! The one simplification made here: runc additionally special-cases
//! "euid 0 but inside a user namespace with `$USER` != root" as rootless
//! too. Detecting "inside a user namespace" needs more than
//! `/proc/self/status` (parsing `/proc/self/uid_map`), and the case only
//! matters for the fairly exotic "rootful container running as mapped
//! root" setup; plain `euid == 0` is treated as root here.

use std::path::PathBuf;

/// Resolve the default state-directory root for a runtime binary named
/// `name` (e.g. `"ocirun"`): `/run/<name>` for root, `$XDG_RUNTIME_DIR/
/// <name>` rootless when `$XDG_RUNTIME_DIR` is set.
pub fn default_root(name: &str) -> PathBuf {
    let (euid, _) = crate::identity::effective_uid_gid();
    resolve_root(name, euid == 0, std::env::var_os("XDG_RUNTIME_DIR"))
}

/// The pure decision logic behind [`default_root`], taking every input
/// explicitly so it can be unit-tested without mutating process-global
/// environment state.
fn resolve_root(
    name: &str,
    running_as_root: bool,
    xdg_runtime_dir: Option<std::ffi::OsString>,
) -> PathBuf {
    if !running_as_root && let Some(dir) = xdg_runtime_dir.filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join(name);
    }
    PathBuf::from("/run").join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_uses_run_name() {
        assert_eq!(
            resolve_root("ocirun", true, Some("/run/user/1000".into())),
            PathBuf::from("/run/ocirun")
        );
    }

    #[test]
    fn rootless_prefers_xdg_runtime_dir() {
        assert_eq!(
            resolve_root("ocirun", false, Some("/run/user/1000".into())),
            PathBuf::from("/run/user/1000/ocirun")
        );
    }

    #[test]
    fn rootless_without_xdg_falls_back_to_run_name_like_runc_does() {
        assert_eq!(
            resolve_root("ocirun", false, None),
            PathBuf::from("/run/ocirun")
        );
    }

    #[test]
    fn empty_xdg_runtime_dir_is_treated_as_unset() {
        assert_eq!(
            resolve_root("ocirun", false, Some("".into())),
            PathBuf::from("/run/ocirun")
        );
    }
}
