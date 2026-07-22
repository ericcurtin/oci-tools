//! Named volumes: a real, persistent directory this project's own
//! store manages on the caller's behalf, matching real `docker
//! volume`/`podman volume`'s own "local" driver exactly — a plain
//! host directory under a fixed root
//! (`<storage-root>/volumes/<name>/_data`, checked directly against a
//! real `podman volume inspect`'s own `Mountpoint`), nothing more
//! elaborate. Distinct from a plain `--volume /host/path:/container/
//! path` bind mount, whose own host side is the *caller's* directory,
//! never this project's own to create/track/remove.

use std::fs;
use std::io;
use std::path::PathBuf;

use oci_spec_types::time::format_rfc3339_utc;
use serde::{Deserialize, Serialize};

/// A real, on-disk named volume's own persisted metadata —
/// deliberately narrow next to real podman's own much larger volume
/// record (`Labels`, `Options`, `MountCount`, `NeedsCopyUp`,
/// `NeedsChown`, `LockNumber`, ... — a real, separate driver-level
/// bookkeeping concern this project's own single, fixed "local
/// directory" driver has no equivalent need for).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct VolumeRecord {
    pub(crate) name: String,
    pub(crate) created_at: String,
}

/// Real docker/podman's own volume-name validation — checked directly
/// against a real installed `podman volume create`'s own error text
/// (`names must match [a-zA-Z0-9][a-zA-Z0-9_.-]*`): the first
/// character must be alphanumeric, every character after that (zero
/// or more) must be alphanumeric, `.`, `_`, or `-`. Real moby's own
/// `RestrictedNamePattern` looks almost identical but requires a
/// *second* character too (`+`, not `*`) — this project matches real
/// podman's own more permissive rule instead (confirmed directly: a
/// real `podman volume create x`, a single character, succeeds),
/// since podman is `ociman`'s own primary reference implementation.
pub(crate) fn is_valid_volume_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// Where every named volume this project ever creates actually lives
/// on disk, rooted at `root` (the same storage root images/containers
/// already share, via `oci_cli_common::storage::default_root`) — one
/// directory per volume, holding its own real `_data` subdirectory
/// (the container-visible mountpoint) plus a small `metadata.json`
/// (this module's own `VolumeRecord`, serialized).
pub(crate) struct VolumeStore {
    root: PathBuf,
}

impl VolumeStore {
    pub(crate) fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(VolumeStore { root })
    }

    fn volume_dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// The real host directory a container's own bind mount should
    /// point at — matches real podman's own `_data` subdirectory
    /// convention exactly.
    pub(crate) fn data_dir(&self, name: &str) -> PathBuf {
        self.volume_dir(name).join("_data")
    }

    fn metadata_path(&self, name: &str) -> PathBuf {
        self.volume_dir(name).join("metadata.json")
    }

    pub(crate) fn exists(&self, name: &str) -> bool {
        self.metadata_path(name).is_file()
    }

    /// Look up an already-created volume's own record, `None` if no
    /// such volume exists at all.
    pub(crate) fn get(&self, name: &str) -> io::Result<Option<VolumeRecord>> {
        match fs::read(self.metadata_path(name)) {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).map_err(io::Error::from)?,
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Create `name` if it doesn't already exist — idempotent,
    /// matching real `podman volume create`'s own identical "already
    /// exists -> just return the existing one" behavior (checked
    /// directly: a second real `podman volume create` of the same
    /// name succeeds, printing the same name back, rather than
    /// erroring "already exists" the way `ociman run --name` on an
    /// already-used container name does). Also what `--volume
    /// name:/path` itself calls on first use, matching real `docker
    /// run -v name:/path`/`podman run -v name:/path`'s own identical
    /// "auto-create on first reference" convention.
    pub(crate) fn get_or_create(&self, name: &str) -> io::Result<VolumeRecord> {
        if let Some(existing) = self.get(name)? {
            return Ok(existing);
        }
        fs::create_dir_all(self.data_dir(name))?;
        let record = VolumeRecord {
            name: name.to_string(),
            created_at: format_rfc3339_utc(std::time::SystemTime::now()),
        };
        fs::write(
            self.metadata_path(name),
            serde_json::to_vec(&record).map_err(io::Error::from)?,
        )?;
        Ok(record)
    }

    /// Every real, currently-existing volume, sorted by name for a
    /// deterministic `ociman volume ls`.
    pub(crate) fn list(&self) -> io::Result<Vec<VolumeRecord>> {
        let mut result = Vec::new();
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(result),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(record) = self.get(&name)? {
                result.push(record);
            }
        }
        result.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(result)
    }

    /// Remove `name`'s own real on-disk directory (metadata and
    /// `_data` together) entirely. Returns whether it existed at all
    /// (matching `oci_store::Store::remove_image`'s own identical
    /// "did something actually get removed" return convention).
    pub(crate) fn remove(&self, name: &str) -> io::Result<bool> {
        if !self.exists(name) {
            return Ok(false);
        }
        fs::remove_dir_all(self.volume_dir(name))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_volume_name_accepts_a_single_alphanumeric_character() {
        assert!(is_valid_volume_name("x"));
        assert!(is_valid_volume_name("9"));
    }

    #[test]
    fn is_valid_volume_name_accepts_dots_underscores_and_dashes_after_the_first_character() {
        assert!(is_valid_volume_name("a.b_c-d9"));
    }

    #[test]
    fn is_valid_volume_name_rejects_an_empty_string() {
        assert!(!is_valid_volume_name(""));
    }

    #[test]
    fn is_valid_volume_name_rejects_a_non_alphanumeric_first_character() {
        assert!(!is_valid_volume_name(".hidden"));
        assert!(!is_valid_volume_name("-x"));
        assert!(!is_valid_volume_name("_x"));
    }

    #[test]
    fn is_valid_volume_name_rejects_a_space_or_slash_anywhere() {
        assert!(!is_valid_volume_name("bad name"));
        assert!(!is_valid_volume_name("a/b"));
    }

    #[test]
    fn get_or_create_is_idempotent_and_creates_a_real_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = VolumeStore::open(dir.path()).unwrap();
        let first = store.get_or_create("myvol").unwrap();
        assert_eq!(first.name, "myvol");
        assert!(store.data_dir("myvol").is_dir());

        let second = store.get_or_create("myvol").unwrap();
        assert_eq!(second.created_at, first.created_at);
    }

    #[test]
    fn list_returns_every_real_volume_sorted_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = VolumeStore::open(dir.path()).unwrap();
        store.get_or_create("zeta").unwrap();
        store.get_or_create("alpha").unwrap();
        let names: Vec<_> = store.list().unwrap().into_iter().map(|v| v.name).collect();
        assert_eq!(names, vec!["alpha".to_string(), "zeta".to_string()]);
    }

    #[test]
    fn list_on_a_store_with_no_volumes_at_all_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = VolumeStore::open(dir.path()).unwrap();
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn remove_deletes_a_real_volume_and_reports_whether_it_existed() {
        let dir = tempfile::tempdir().unwrap();
        let store = VolumeStore::open(dir.path()).unwrap();
        store.get_or_create("gone-soon").unwrap();
        assert!(store.remove("gone-soon").unwrap());
        assert!(!store.exists("gone-soon"));
        assert!(!store.remove("gone-soon").unwrap());
    }

    #[test]
    fn get_of_an_unknown_volume_is_none_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = VolumeStore::open(dir.path()).unwrap();
        assert!(store.get("never-created").unwrap().is_none());
    }
}
