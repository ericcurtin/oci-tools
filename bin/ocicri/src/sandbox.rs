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

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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

/// The sandbox record directory under one storage root.
pub fn sandbox_root(storage_root: &Path) -> PathBuf {
    storage_root.join("cri-sandboxes")
}

fn record_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.json"))
}

/// A real, random 64-hex sandbox ID — the exact shape real cri-o's
/// own `stringid.GenerateNonCryptoID` produces, generated the same
/// dependency-free way `ociman`'s own `short_id`/`ocibox ephemeral`
/// already do (hashing the real current time and this process's own
/// pid), just untruncated — plus a process-global counter so two
/// calls in the same process can never collide even if the clock's
/// own resolution ever made their timestamps identical (the same
/// role `ocibox`'s own `attempt` input plays).
pub fn generate_id() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seed = format!(
        "{:?}-{}-sandbox-{}",
        std::time::SystemTime::now(),
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    oci_spec_types::digest::sha256(seed.as_bytes())
        .hex()
        .to_string()
}

/// Persists `record` atomically (temp file + rename, the same
/// technique `oci_store`'s own pointer files use, so a crash mid-write
/// can never leave a truncated record behind).
pub fn save(root: &Path, record: &SandboxRecord) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let mut tmp = tempfile::NamedTempFile::new_in(root)?;
    tmp.write_all(&serde_json::to_vec_pretty(record)?)?;
    tmp.persist(record_path(root, &record.id))
        .map_err(|e| e.error)?;
    Ok(())
}

/// Loads every stored record, sorted by creation time (newest first,
/// matching real cri-o's own `ListSandboxes` consumers' expectation of
/// a stable order; the proto itself mandates none).
pub fn load_all(root: &Path) -> std::io::Result<Vec<SandboxRecord>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut records = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let bytes = std::fs::read(&path)?;
        let record: SandboxRecord = serde_json::from_slice(&bytes)?;
        records.push(record);
    }
    records.sort_by(|a, b| {
        b.created_at_nanos
            .cmp(&a.created_at_nanos)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(records)
}

/// Resolves one sandbox by ID prefix — matching real cri-o's own
/// truncindex-backed lookup (`PodIDIndex().Get`, prefix-based):
/// `Ok(None)` for no match at all, an `AmbiguousPrefix` error when the
/// prefix matches more than one distinct sandbox.
pub fn find_by_id_prefix(root: &Path, prefix: &str) -> Result<Option<SandboxRecord>, LookupError> {
    if prefix.is_empty() {
        return Ok(None);
    }
    let mut found: Option<SandboxRecord> = None;
    for record in load_all(root).map_err(LookupError::Io)? {
        if record.id.starts_with(prefix) {
            if found.is_some() {
                return Err(LookupError::AmbiguousPrefix(prefix.to_string()));
            }
            found = Some(record);
        }
    }
    Ok(found)
}

/// Resolves one sandbox by its unique pod name (the
/// `k8s_<name>_<namespace>_<uid>_<attempt>` join) — the duplicate-
/// request check `RunPodSandbox` needs (`docs/design/0233`).
pub fn find_by_name(root: &Path, name: &str) -> std::io::Result<Option<SandboxRecord>> {
    Ok(load_all(root)?.into_iter().find(|r| r.name == name))
}

/// Removes one sandbox record by exact ID. Returns whether a record
/// actually existed.
pub fn remove(root: &Path, id: &str) -> std::io::Result<bool> {
    match std::fs::remove_file(record_path(root, id)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// A sandbox lookup failure — either real I/O trouble or a genuinely
/// ambiguous ID prefix (a client-input problem, reported distinctly so
/// the RPC layer can map it to `InvalidArgument` rather than a generic
/// internal error).
#[derive(Debug)]
pub enum LookupError {
    /// Reading the record directory failed.
    Io(std::io::Error),
    /// The given prefix matches more than one distinct sandbox.
    AmbiguousPrefix(String),
}

impl std::fmt::Display for LookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "reading sandbox records: {e}"),
            Self::AmbiguousPrefix(prefix) => {
                write!(f, "sandbox ID {prefix:?} is ambiguous")
            }
        }
    }
}

impl std::error::Error for LookupError {}

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
