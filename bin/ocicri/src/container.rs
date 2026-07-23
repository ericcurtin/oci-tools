//! The real, persistent container record store behind
//! `RuntimeService`'s container lifecycle RPCs (`docs/design/0236`):
//! the second record family stored via the shared, generic
//! `records.rs` mechanics `sandbox.rs`'s own pod-sandbox records
//! already use (one atomic JSON file per record under
//! `<storage-root>/cri-containers/`, surviving an `ocicri` restart).
//!
//! What a record here deliberately is (and isn't) — see
//! `docs/design/0236` for the full reasoning checked against real
//! `cri-o`: this first slice covers `CreateContainer`/
//! `ContainerStatus`/`ListContainers`/`RemoveContainer` with real CRI
//! state-machine semantics, and every record is honestly
//! `CONTAINER_CREATED` — no process is ever spawned, because
//! `StartContainer` itself is still a real, honest
//! `Status::unimplemented` (actually launching the container process,
//! via the same shared `oci_runtime_core::launch` machinery
//! `ociman`/`ocirun`/`ocibox` already use, is its own bigger, later
//! increment). A container that can never have been started also
//! never transitions to `RUNNING`/`EXITED`, so the one state this
//! slice ever writes is also the only one that can truthfully exist.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use crate::records::LookupError;
use crate::records::{self, Record};

/// The real CRI container states, mirroring the proto's own
/// `ContainerState` — stored by name so an on-disk record stays
/// self-describing. Only [`ContainerState::Created`] is ever written
/// by this first slice (see the module doc comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContainerState {
    /// `CONTAINER_CREATED`.
    Created,
    /// `CONTAINER_RUNNING` (unreachable until `StartContainer` exists).
    Running,
    /// `CONTAINER_EXITED` (unreachable until `StartContainer` exists).
    Exited,
}

/// The container's own CRI metadata (`ContainerMetadata`), stored
/// verbatim from the `CreateContainer` request: the proto requires
/// status/list responses to echo it back identically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerMetadata {
    /// Container name (`metadata.name`).
    pub name: String,
    /// Attempt number (`metadata.attempt`, defaults to 0).
    pub attempt: u32,
}

/// One real, on-disk container record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerRecord {
    /// 64-hex container ID (the shape real cri-o's own
    /// `stringid.GenerateNonCryptoID` produces).
    pub id: String,
    /// The unique container name, real cri-o's own
    /// `k8s_<ctrName>_<podName>_<podNamespace>_<podUid>_<ctrAttempt>`
    /// join (`internal/factory/container`'s own `SetNameAndID`,
    /// checked directly — the pod half comes from the request's own
    /// `sandbox_config`, exactly like real cri-o).
    pub name: String,
    /// The full 64-hex ID of the sandbox this container belongs to.
    pub sandbox_id: String,
    /// CRI metadata, echoed back verbatim.
    pub metadata: ContainerMetadata,
    /// The image reference exactly as the request asked for it
    /// (`ContainerStatus.image`'s own "image name in spec" half).
    pub image: String,
    /// The resolved image's own manifest digest
    /// (`ContainerStatus.image_ref`/`image_id`).
    pub image_ref: String,
    /// Labels, stored verbatim.
    pub labels: std::collections::HashMap<String, String>,
    /// Annotations, stored verbatim (the proto: "MUST NOT be altered
    /// by the runtime").
    pub annotations: std::collections::HashMap<String, String>,
    /// Current lifecycle state.
    pub state: ContainerState,
    /// Creation timestamp in nanoseconds since the epoch.
    pub created_at_nanos: i64,
}

impl Record for ContainerRecord {
    fn id(&self) -> &str {
        &self.id
    }
    fn created_at_nanos(&self) -> i64 {
        self.created_at_nanos
    }
}

/// The container record directory under one storage root.
pub fn container_root(storage_root: &Path) -> PathBuf {
    storage_root.join("cri-containers")
}

/// Persists `record` atomically — see [`crate::records::save`].
pub fn save(root: &Path, record: &ContainerRecord) -> std::io::Result<()> {
    records::save(root, record)
}

/// Loads every stored record, newest first — see
/// [`crate::records::load_all`].
pub fn load_all(root: &Path) -> std::io::Result<Vec<ContainerRecord>> {
    records::load_all(root)
}

/// Resolves one container by ID prefix (real cri-o's own
/// `GetContainerFromShortID` truncindex equivalent) — see
/// [`crate::records::find_by_id_prefix`].
pub fn find_by_id_prefix(
    root: &Path,
    prefix: &str,
) -> Result<Option<ContainerRecord>, LookupError> {
    records::find_by_id_prefix(root, prefix)
}

/// Resolves one container by its unique name — the duplicate-request
/// check `CreateContainer` needs (`docs/design/0236`, the same
/// "duplicate request returns the existing ID" rule `RunPodSandbox`
/// already implements from real cri-o's own identical branch).
pub fn find_by_name(root: &Path, name: &str) -> std::io::Result<Option<ContainerRecord>> {
    Ok(load_all(root)?.into_iter().find(|r| r.name == name))
}

/// Removes one container record by exact ID. Returns whether a record
/// actually existed — see [`crate::records::remove`].
pub fn remove(root: &Path, id: &str) -> std::io::Result<bool> {
    records::remove(root, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, name: &str, sandbox_id: &str, created_at_nanos: i64) -> ContainerRecord {
        ContainerRecord {
            id: id.to_string(),
            name: name.to_string(),
            sandbox_id: sandbox_id.to_string(),
            metadata: ContainerMetadata {
                name: "app".to_string(),
                attempt: 0,
            },
            image: "docker.io/library/busybox:latest".to_string(),
            image_ref: "sha256:abc".to_string(),
            labels: std::collections::HashMap::new(),
            annotations: std::collections::HashMap::new(),
            state: ContainerState::Created,
            created_at_nanos,
        }
    }

    #[test]
    fn save_load_find_and_remove_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let root = container_root(dir.path());
        save(&root, &record("aa11", "k8s_app_pod_ns_uid_0", "sb1", 10)).unwrap();
        save(&root, &record("bb22", "k8s_app2_pod_ns_uid_0", "sb2", 20)).unwrap();

        let all = load_all(&root).unwrap();
        assert_eq!(all.len(), 2);
        // Newest first (shared records.rs ordering).
        assert_eq!(all[0].id, "bb22");
        assert_eq!(all[1].sandbox_id, "sb1");

        assert_eq!(find_by_id_prefix(&root, "aa").unwrap().unwrap().id, "aa11");
        assert!(matches!(
            find_by_id_prefix(&root, ""),
            Ok(None) // The empty prefix is a real None, never a scan.
        ));
        assert_eq!(
            find_by_name(&root, "k8s_app_pod_ns_uid_0")
                .unwrap()
                .unwrap()
                .id,
            "aa11"
        );

        assert!(remove(&root, "aa11").unwrap());
        assert!(!remove(&root, "aa11").unwrap(), "removal is idempotent");
        assert!(
            find_by_name(&root, "k8s_app_pod_ns_uid_0")
                .unwrap()
                .is_none()
        );
    }
}
