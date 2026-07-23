//! The real, persistent pod-sandbox record store behind
//! `RuntimeService`'s pod-sandbox lifecycle RPCs (see
//! `docs/design/0233`): one JSON file per sandbox under
//! `<storage-root>/cri-sandboxes/`, written atomically via the same
//! temp-file-plus-rename technique `oci_store`'s own pointer files
//! use, so a restarted `ocicri` still knows its sandboxes — exactly
//! like real `cri-o` restores its own sandbox state from
//! `containers/storage` rather than starting amnesiac.
//!
//! Deliberately *not* a `crates/` library: the repo rule is that
//! **shared** logic lives in `crates/`, and no other binary in this
//! workspace has any concept of a CRI pod sandbox (the same reasoning
//! `image_service.rs`'s own CRI-specific mapping code already
//! follows).
//!
//! What a record here deliberately is (and isn't) — see
//! `docs/design/0233` for the full reasoning checked against real
//! `cri-o`: real bookkeeping with real CRI state-machine semantics,
//! but no infra ("pause") process and no pinned namespaces yet (real
//! `cri-o`'s own `drop_infra_ctr` default means an ordinary real
//! sandbox has no live process of its own either; the namespace
//! pinning is deferred until this project grows real pod networking
//! and a real `CreateContainer` that could join those namespaces).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use crate::records::LookupError;
use crate::records::{self, Record};

/// The real CRI pod-sandbox states, mirroring the proto's own
/// `PodSandboxState` (`SANDBOX_READY`/`SANDBOX_NOTREADY`) — stored by
/// name (not by protobuf enum integer) so an on-disk record stays
/// self-describing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxState {
    /// `SANDBOX_READY`.
    Ready,
    /// `SANDBOX_NOTREADY` (stopped).
    NotReady,
}

/// The sandbox's own CRI metadata, stored verbatim from the
/// `RunPodSandbox` request (`PodSandboxMetadata`): the proto requires
/// status/list responses to echo it back identically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxMetadata {
    /// Pod name (`metadata.name`).
    pub name: String,
    /// Pod UID (`metadata.uid`).
    pub uid: String,
    /// Pod namespace (`metadata.namespace`).
    pub namespace: String,
    /// Attempt number (`metadata.attempt`, defaults to 0).
    pub attempt: u32,
}

/// The namespace modes the `RunPodSandbox` request itself declared
/// (`NamespaceOption`), echoed back by `PodSandboxStatus` — matching
/// real `cri-o`, whose own status echoes the *requested* config
/// options (`sb.NamespaceOptions()`), not a live probe. Stored as the
/// proto's own raw enum integers: this project doesn't act on them
/// yet (no namespaces are pinned at all — `docs/design/0233`), it
/// just preserves what was asked for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NamespaceOptions {
    /// `NamespaceMode` for the network namespace.
    pub network: i32,
    /// `NamespaceMode` for the PID namespace.
    pub pid: i32,
    /// `NamespaceMode` for the IPC namespace.
    pub ipc: i32,
    /// Target container ID for `NamespaceMode_TARGET`.
    pub target_id: String,
}

/// One real, on-disk pod-sandbox record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxRecord {
    /// 64-hex sandbox ID (the shape real cri-o's own
    /// `stringid.GenerateNonCryptoID` produces).
    pub id: String,
    /// The unique pod name, real cri-o's own
    /// `k8s_<name>_<namespace>_<uid>_<attempt>` join.
    pub name: String,
    /// CRI metadata, echoed back verbatim.
    pub metadata: SandboxMetadata,
    /// Labels (including the kubelet-default `io.kubernetes.pod.*`
    /// entries populated at creation if missing).
    pub labels: std::collections::HashMap<String, String>,
    /// Annotations, stored verbatim (the proto: "MUST NOT be altered
    /// by the runtime").
    pub annotations: std::collections::HashMap<String, String>,
    /// Current lifecycle state.
    pub state: SandboxState,
    /// Creation timestamp in nanoseconds since the epoch (the proto:
    /// "Must be > 0").
    pub created_at_nanos: i64,
    /// The namespace options the request declared, if any.
    pub namespace_options: Option<NamespaceOptions>,
}

impl Record for SandboxRecord {
    fn id(&self) -> &str {
        &self.id
    }
    fn created_at_nanos(&self) -> i64 {
        self.created_at_nanos
    }
}

/// The sandbox record directory under one storage root.
pub fn sandbox_root(storage_root: &Path) -> PathBuf {
    storage_root.join("cri-sandboxes")
}

/// A real, random 64-hex sandbox ID — see
/// [`crate::records::generate_id`].
pub fn generate_id() -> String {
    records::generate_id()
}

/// Persists `record` atomically — see [`crate::records::save`].
pub fn save(root: &Path, record: &SandboxRecord) -> std::io::Result<()> {
    records::save(root, record)
}

/// Loads every stored record, newest first — see
/// [`crate::records::load_all`].
pub fn load_all(root: &Path) -> std::io::Result<Vec<SandboxRecord>> {
    records::load_all(root)
}

/// Resolves one sandbox by ID prefix (real cri-o's own
/// `PodIDIndex().Get` truncindex equivalent) — see
/// [`crate::records::find_by_id_prefix`].
pub fn find_by_id_prefix(root: &Path, prefix: &str) -> Result<Option<SandboxRecord>, LookupError> {
    records::find_by_id_prefix(root, prefix)
}

/// Resolves one sandbox by its unique pod name (the
/// `k8s_<name>_<namespace>_<uid>_<attempt>` join) — the duplicate-
/// request check `RunPodSandbox` needs (`docs/design/0233`).
pub fn find_by_name(root: &Path, name: &str) -> std::io::Result<Option<SandboxRecord>> {
    Ok(load_all(root)?.into_iter().find(|r| r.name == name))
}

/// Removes one sandbox record by exact ID. Returns whether a record
/// actually existed — see [`crate::records::remove`].
pub fn remove(root: &Path, id: &str) -> std::io::Result<bool> {
    records::remove(root, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, name: &str, created_at_nanos: i64) -> SandboxRecord {
        SandboxRecord {
            id: id.to_string(),
            name: name.to_string(),
            metadata: SandboxMetadata {
                name: "pod".to_string(),
                uid: "uid".to_string(),
                namespace: "default".to_string(),
                attempt: 0,
            },
            labels: std::collections::HashMap::new(),
            annotations: std::collections::HashMap::new(),
            state: SandboxState::Ready,
            created_at_nanos,
            namespace_options: None,
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let root = sandbox_root(dir.path());
        save(&root, &record("aa11", "k8s_a_b_c_0", 10)).unwrap();
        save(&root, &record("bb22", "k8s_d_e_f_0", 20)).unwrap();

        let all = load_all(&root).unwrap();
        assert_eq!(all.len(), 2);
        // Newest first.
        assert_eq!(all[0].id, "bb22");
        assert_eq!(all[1].id, "aa11");
        assert_eq!(all[1].name, "k8s_a_b_c_0");
        assert_eq!(all[1].state, SandboxState::Ready);
    }

    #[test]
    fn save_overwrites_an_existing_record_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let root = sandbox_root(dir.path());
        save(&root, &record("aa11", "k8s_a_b_c_0", 10)).unwrap();
        let mut updated = record("aa11", "k8s_a_b_c_0", 10);
        updated.state = SandboxState::NotReady;
        save(&root, &updated).unwrap();

        let all = load_all(&root).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].state, SandboxState::NotReady);
    }

    #[test]
    fn load_all_of_a_missing_root_is_a_real_empty_list_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = sandbox_root(dir.path());
        assert!(load_all(&root).unwrap().is_empty());
    }

    #[test]
    fn find_by_id_prefix_resolves_exact_unique_ambiguous_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let root = sandbox_root(dir.path());
        save(&root, &record("aa11", "k8s_a_b_c_0", 10)).unwrap();
        save(&root, &record("ab22", "k8s_d_e_f_0", 20)).unwrap();

        // Exact.
        assert_eq!(
            find_by_id_prefix(&root, "aa11").unwrap().unwrap().id,
            "aa11"
        );
        // Unique prefix.
        assert_eq!(find_by_id_prefix(&root, "ab").unwrap().unwrap().id, "ab22");
        // Ambiguous prefix.
        assert!(matches!(
            find_by_id_prefix(&root, "a"),
            Err(LookupError::AmbiguousPrefix(_))
        ));
        // No match, and the empty prefix, are both a real None, never
        // an error.
        assert!(find_by_id_prefix(&root, "zz").unwrap().is_none());
        assert!(find_by_id_prefix(&root, "").unwrap().is_none());
    }

    #[test]
    fn find_by_name_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let root = sandbox_root(dir.path());
        save(&root, &record("aa11", "k8s_a_b_c_0", 10)).unwrap();

        assert_eq!(
            find_by_name(&root, "k8s_a_b_c_0").unwrap().unwrap().id,
            "aa11"
        );
        assert!(find_by_name(&root, "k8s_missing_x_y_0").unwrap().is_none());

        assert!(remove(&root, "aa11").unwrap());
        // Idempotent: a second removal is a real, silent false.
        assert!(!remove(&root, "aa11").unwrap());
        assert!(find_by_name(&root, "k8s_a_b_c_0").unwrap().is_none());
    }

    #[test]
    fn generate_id_is_64_hex_and_unique_within_one_process() {
        let a = generate_id();
        let b = generate_id();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, b, "two IDs generated back to back should differ");
    }
}
