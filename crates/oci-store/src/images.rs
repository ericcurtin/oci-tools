//! Image tag/digest pointer metadata: which manifest digest a reference
//! (e.g. `docker.io/library/ubuntu:latest`) currently resolves to.
//!
//! Each reference gets exactly one file under the store's `images/`
//! directory, named by hashing the reference string (references contain
//! `/` and `:`, which is awkward to lay out as a nested path unambiguously
//! and reversibly — hashing sidesteps that entirely). The reference string
//! itself is stored inside the JSON, so listing never needs to reverse the
//! filename.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use oci_spec_types::Digest;
use oci_spec_types::digest::sha256;
use serde::{Deserialize, Serialize};

/// A stored pointer: `reference` (the full normalized image reference
/// string) currently resolves to `manifest_digest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRecord {
    /// Full normalized reference, e.g. `docker.io/library/ubuntu:latest` or
    /// `quay.io/foo/bar@sha256:...`.
    pub reference: String,
    /// The manifest digest this reference currently points at.
    pub manifest_digest: Digest,
}

/// Errors from the image-pointer metadata store.
#[derive(Debug, thiserror::Error)]
pub enum ImagesError {
    /// Filesystem I/O failure.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// A pointer file's content did not deserialize as [`ImageRecord`]
    /// (corrupt or written by an incompatible version).
    #[error("corrupt image pointer at {path}: {source}")]
    Corrupt {
        /// Path of the offending file.
        path: PathBuf,
        /// The JSON parse error.
        #[source]
        source: serde_json::Error,
    },
}

fn pointer_path(images_dir: &Path, reference: &str) -> PathBuf {
    // sha256 gives a fixed-length, filesystem-safe, collision-resistant
    // name; the human-readable reference lives inside the file.
    images_dir.join(format!("{}.json", sha256(reference.as_bytes()).hex()))
}

pub(crate) fn put(images_dir: &Path, record: &ImageRecord) -> Result<(), ImagesError> {
    let path = pointer_path(images_dir, &record.reference);
    let json = serde_json::to_vec_pretty(record).expect("ImageRecord serializes");
    // Same-directory temp file + rename: a reader never observes a
    // partially written pointer file.
    let mut tmp = tempfile::NamedTempFile::new_in(images_dir)?;
    io::Write::write_all(&mut tmp, &json)?;
    tmp.persist(&path).map_err(|e| e.error)?;
    Ok(())
}

pub(crate) fn get(images_dir: &Path, reference: &str) -> Result<Option<ImageRecord>, ImagesError> {
    let path = pointer_path(images_dir, reference);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|source| ImagesError::Corrupt { path, source }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn remove(images_dir: &Path, reference: &str) -> Result<bool, ImagesError> {
    let path = pointer_path(images_dir, reference);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn list(images_dir: &Path) -> Result<Vec<ImageRecord>, ImagesError> {
    let mut out = Vec::new();
    for entry in fs::read_dir(images_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path)?;
        let record = serde_json::from_slice(&bytes)
            .map_err(|source| ImagesError::Corrupt { path, source })?;
        out.push(record);
    }
    out.sort_by(|a: &ImageRecord, b: &ImageRecord| a.reference.cmp(&b.reference));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_remove_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let record = ImageRecord {
            reference: "docker.io/library/ubuntu:latest".to_string(),
            manifest_digest: sha256(b"manifest"),
        };
        assert!(get(dir.path(), &record.reference).unwrap().is_none());

        put(dir.path(), &record).unwrap();
        assert_eq!(
            get(dir.path(), &record.reference).unwrap(),
            Some(record.clone())
        );

        assert!(remove(dir.path(), &record.reference).unwrap());
        assert!(get(dir.path(), &record.reference).unwrap().is_none());
        assert!(!remove(dir.path(), &record.reference).unwrap());
    }

    #[test]
    fn put_overwrites_existing_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let reference = "docker.io/library/ubuntu:latest".to_string();
        put(
            dir.path(),
            &ImageRecord {
                reference: reference.clone(),
                manifest_digest: sha256(b"old"),
            },
        )
        .unwrap();
        put(
            dir.path(),
            &ImageRecord {
                reference: reference.clone(),
                manifest_digest: sha256(b"new"),
            },
        )
        .unwrap();
        let got = get(dir.path(), &reference).unwrap().unwrap();
        assert_eq!(got.manifest_digest, sha256(b"new"));
    }

    #[test]
    fn list_is_sorted_by_reference() {
        let dir = tempfile::tempdir().unwrap();
        for reference in [
            "z.example/x:latest",
            "a.example/y:latest",
            "m.example/z:latest",
        ] {
            put(
                dir.path(),
                &ImageRecord {
                    reference: reference.to_string(),
                    manifest_digest: sha256(reference.as_bytes()),
                },
            )
            .unwrap();
        }
        let refs: Vec<_> = list(dir.path())
            .unwrap()
            .into_iter()
            .map(|r| r.reference)
            .collect();
        assert_eq!(
            refs,
            vec![
                "a.example/y:latest",
                "m.example/z:latest",
                "z.example/x:latest"
            ]
        );
    }
}
