//! Version strings with the embedded git hash.

/// Short git commit hash embedded at build time by `oci-build-info`
/// (`"unknown"` when built outside a git checkout without
/// `OCI_TOOLS_GIT_HASH` set).
pub const GIT_HASH: &str = env!("OCI_TOOLS_GIT_HASH");

/// Build a `<pkg_version> (git <hash>)` string for clap:
///
/// ```ignore
/// #[command(version = oci_cli_common::version::long(env!("CARGO_PKG_VERSION")))]
/// ```
pub fn long(pkg_version: &str) -> String {
    format!("{pkg_version} (git {GIT_HASH})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_is_stable() {
        let v = long("1.2.3");
        assert!(v.starts_with("1.2.3 (git "), "got {v:?}");
        assert!(v.ends_with(')'), "got {v:?}");
    }

    #[test]
    fn git_hash_is_sane() {
        // Either a real 12-char hex hash or the documented fallback.
        assert!(
            GIT_HASH == "unknown"
                || (GIT_HASH.len() == 12 && GIT_HASH.bytes().all(|b| b.is_ascii_hexdigit())),
            "unexpected GIT_HASH: {GIT_HASH:?}"
        );
    }
}
