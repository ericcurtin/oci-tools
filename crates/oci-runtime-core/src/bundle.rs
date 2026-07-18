//! Loading an OCI bundle: reading and parsing `config.json` out of a
//! bundle directory. Pure I/O + parsing; spec-content validation lives in
//! [`crate::validate`] so the two concerns (can we even read the file? vs.
//! is what it says internally consistent?) stay separate and separately
//! testable.

use std::fs;
use std::path::{Path, PathBuf};

use oci_spec_types::runtime::Spec;

/// Filename of the OCI runtime-spec bundle configuration, per the spec.
pub const CONFIG_FILENAME: &str = "config.json";

/// Errors from [`Bundle::load`].
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    /// Filesystem I/O failure other than a missing `config.json`.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// `config.json` does not exist in the bundle directory.
    #[error("no {} in bundle {bundle}", CONFIG_FILENAME)]
    MissingConfig {
        /// The bundle directory that was checked.
        bundle: PathBuf,
    },
    /// `config.json` exists but isn't valid JSON, or doesn't match the
    /// runtime-spec shape this crate understands.
    #[error("invalid {}: {source}", CONFIG_FILENAME)]
    InvalidConfig {
        /// The JSON parse error.
        #[source]
        source: serde_json::Error,
    },
}

/// A loaded (but not yet validated) OCI bundle: the directory it came
/// from, and the parsed `config.json`.
#[derive(Debug, Clone)]
pub struct Bundle {
    /// The bundle directory (exactly as given to [`Bundle::load`]; not
    /// canonicalized).
    pub path: PathBuf,
    /// The parsed `config.json`.
    pub spec: Spec,
}

impl Bundle {
    /// Read and parse `<dir>/config.json`.
    pub fn load(dir: impl Into<PathBuf>) -> Result<Self, BundleError> {
        let path = dir.into();
        let config_path = path.join(CONFIG_FILENAME);
        let bytes = match fs::read(&config_path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(BundleError::MissingConfig { bundle: path });
            }
            Err(e) => return Err(e.into()),
        };
        let spec: Spec = serde_json::from_slice(&bytes)
            .map_err(|source| BundleError::InvalidConfig { source })?;
        Ok(Bundle { path, spec })
    }

    /// The rootfs path `spec.root.path` resolves to: absolute paths are
    /// used as-is, relative paths are resolved against the bundle
    /// directory (per the runtime-spec: "if this property is not
    /// absolute, it MUST be interpreted relative to the bundle
    /// directory"). Returns `None` if the spec has no `root` at all (an
    /// invalid spec; [`crate::validate::validate`] catches that).
    pub fn rootfs_path(&self) -> Option<PathBuf> {
        let root = self.spec.root.as_ref()?;
        let raw = Path::new(&root.path);
        Some(if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.path.join(raw)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_a_valid_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let spec = Spec::example();
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            serde_json::to_vec(&spec).unwrap(),
        )
        .unwrap();

        let bundle = Bundle::load(dir.path()).unwrap();
        assert_eq!(bundle.spec.hostname.as_deref(), Some("ocirun"));
        assert_eq!(bundle.rootfs_path(), Some(dir.path().join("rootfs")));
    }

    #[test]
    fn missing_config_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = Bundle::load(dir.path()).unwrap_err();
        assert!(matches!(err, BundleError::MissingConfig { bundle } if bundle == dir.path()));
    }

    #[test]
    fn invalid_json_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(CONFIG_FILENAME), b"not json").unwrap();
        let err = Bundle::load(dir.path()).unwrap_err();
        assert!(matches!(err, BundleError::InvalidConfig { .. }));
    }

    #[test]
    fn absolute_root_path_is_used_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = Spec::example();
        spec.root.as_mut().unwrap().path = "/some/absolute/rootfs".to_string();
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            serde_json::to_vec(&spec).unwrap(),
        )
        .unwrap();

        let bundle = Bundle::load(dir.path()).unwrap();
        assert_eq!(
            bundle.rootfs_path(),
            Some(PathBuf::from("/some/absolute/rootfs"))
        );
    }

    #[test]
    fn missing_root_resolves_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = Spec::example();
        spec.root = None;
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            serde_json::to_vec(&spec).unwrap(),
        )
        .unwrap();

        let bundle = Bundle::load(dir.path()).unwrap();
        assert_eq!(bundle.rootfs_path(), None);
    }
}
