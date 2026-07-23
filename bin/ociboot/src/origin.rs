//! A real, on-disk provenance record for a deployment image built by
//! `ociboot build-image` — the actual prerequisite milestone 6's own
//! `status`/`upgrade`/`switch`/`rollback` will eventually need
//! ("which OCI image reference/digest did this deployment come
//! from?"), and the one piece a direct research pass against real
//! `bootc`'s own `status` implementation (`bootc_composefs::status`,
//! its own `<digest>.origin` ini file) found completely missing from
//! this project until now: `build-image` wrote only the erofs image
//! itself, nothing recording where it came from.
//!
//! Deliberately not a literal port of bootc's own ini format —
//! `ociboot` has its own design (this project's README says so
//! explicitly), and a plain JSON sidecar matches this workspace's own
//! already-established convention (`oci_store::images`'s own
//! `ImageRecord` pointer files, and every `--json` CLI output
//! elsewhere) rather than inventing a second, ini-flavored format
//! just to mirror bootc's own internal choice.
//!
//! Written silently (no stdout announcement) by `build-image` — this
//! is internal bookkeeping metadata, the same category as `oci_store`'s
//! own pointer-file writes, not a user-facing result the way the
//! `--seal` flag's own `verity:`/`dm-verity:` digest lines are.
//! Nothing reads this file yet (that's `status`'s own future job,
//! still ahead) — this increment is scoped to writing it correctly
//! and deterministically, nothing more.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where a deployment image came from: the exact OCI image reference
/// and resolved digest it was built from, plus the same `built_at`
/// timestamp `oci_erofs::BuildOptions::timestamp` embeds in the erofs
/// image's own superblock (the image's own `created` field, never
/// wall-clock "now" — see [`Command::BuildImage`](crate::Command::BuildImage)'s
/// own doc comment for why), so this record is exactly as
/// deterministic as the image it describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentOrigin {
    /// The fully normalized image reference (`oci_spec_types::Reference`'s
    /// own `Display` form) this deployment was built from.
    pub image_reference: String,
    /// The resolved manifest digest (`"sha256:<hex>"`) — the real,
    /// content-addressed identity of the image, not just its
    /// human-chosen tag.
    pub image_digest: String,
    /// The image's own declared `org.opencontainers.image.version`
    /// label, if it set one — `None` is a real, honest "the image
    /// never declared a version," not a missing-data placeholder.
    pub image_version: Option<String>,
    /// Unix timestamp this deployment's own build time is pinned to —
    /// the image's own `created` field when parseable, `0` otherwise,
    /// exactly mirroring `BuildOptions::timestamp`'s own derivation so
    /// the two never disagree.
    pub built_at: u64,
}

/// The sidecar path for a deployment image at `image_path` — a plain
/// sibling file, `<image_path>.origin.json` (matching
/// `detached_hash_tree_path`'s own `<output>.verity` sibling-file
/// convention right above it in `main.rs`, for the same reason: no
/// real prior-art naming to match, since bootc's own equivalent lives
/// in a directory named *by* the digest rather than beside a single
/// image file at a caller-chosen path).
pub fn origin_path(image_path: &Path) -> PathBuf {
    let mut name = image_path.as_os_str().to_owned();
    name.push(".origin.json");
    PathBuf::from(name)
}

/// Writes `origin` to `origin_path(image_path)`, atomically (a
/// same-directory temp file plus rename, the exact technique
/// `oci_store::images::put`'s own doc comment already established:
/// "a reader never observes a partially written pointer file").
pub fn write(image_path: &Path, origin: &DeploymentOrigin) -> std::io::Result<()> {
    let path = origin_path(image_path);
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let json = serde_json::to_vec_pretty(origin).expect("DeploymentOrigin serializes");
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(&json)?;
    tmp.persist(&path).map_err(|e| e.error)?;
    Ok(())
}

/// Reads back whatever [`write`] last stored for `image_path`, if
/// anything — `Ok(None)` when there's no origin record at all (an
/// image built by an older version of this binary, or not by
/// `ociboot build-image` at all), never an error just for that.
#[cfg(test)]
pub fn read(image_path: &Path) -> std::io::Result<Option<DeploymentOrigin>> {
    let path = origin_path(image_path);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let image_path = dir.path().join("deployment.erofs");
        let origin = DeploymentOrigin {
            image_reference: "docker.io/library/busybox:latest".to_string(),
            image_digest: "sha256:abc123".to_string(),
            image_version: Some("1.2.3".to_string()),
            built_at: 1_700_000_000,
        };
        write(&image_path, &origin).unwrap();
        let path = origin_path(&image_path);
        assert!(path.exists());
        assert_eq!(path.file_name().unwrap(), "deployment.erofs.origin.json");

        let read_back = read(&image_path).unwrap();
        assert_eq!(read_back, Some(origin));
    }

    #[test]
    fn a_missing_origin_file_is_a_real_none_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let image_path = dir.path().join("no-such-deployment.erofs");
        assert_eq!(read(&image_path).unwrap(), None);
    }

    #[test]
    fn overwriting_an_existing_origin_never_leaves_a_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let image_path = dir.path().join("deployment.erofs");
        let first = DeploymentOrigin {
            image_reference: "docker.io/library/busybox:1.0".to_string(),
            image_digest: "sha256:aaa".to_string(),
            image_version: None,
            built_at: 1,
        };
        let second = DeploymentOrigin {
            image_reference: "docker.io/library/busybox:2.0".to_string(),
            image_digest: "sha256:bbb".to_string(),
            image_version: Some("2.0".to_string()),
            built_at: 2,
        };
        write(&image_path, &first).unwrap();
        write(&image_path, &second).unwrap();
        assert_eq!(read(&image_path).unwrap(), Some(second));
    }
}
