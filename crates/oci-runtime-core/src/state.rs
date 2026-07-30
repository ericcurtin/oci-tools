//! Container state tracking: the on-disk record every runtime keeps per
//! container (`<root>/<id>/state.json`), and the directory-of-containers
//! abstraction (`StateStore`) that `create`/`start`/`kill`/`delete`/
//! `state`/`list` all operate on.
//!
//! This module is deliberately self-contained: it has no idea how to
//! actually create a container process (namespaces/cgroups/pivot_root
//! land with `create`, next), only how to durably record that one exists,
//! at what pid, in what state — the same bookkeeping runc's `state.json`
//! and libcontainer's `State` do. Building and testing this in isolation
//! first means `create` can focus entirely on process/namespace setup
//! against a state model that is already proven correct.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use oci_spec_types::format_rfc3339_utc;

/// The runtime-spec version this crate reports in [`PersistedState::oci_version`]
/// (matches `oci_spec_types::runtime::VERSION`).
pub const OCI_VERSION: &str = oci_spec_types::runtime::VERSION;

/// A container's lifecycle status, per the OCI runtime spec's
/// `ContainerState` (`creating`/`created`/`running`/`stopped`; the spec
/// also allows implementation-defined additional states) plus one such
/// additional state, [`Status::Paused`], matching real runc/crun's own
/// (both report a real `"paused"` via `state`/`list` once a container's
/// own cgroup freezer reports frozen — checked directly against real
/// runc's own `isPaused()`, `~/git/runc/libcontainer/container_linux.
/// go`).
///
/// [`Status::Paused`] is deliberately never *persisted* to a real
/// `state.json` the way the other four variants are — real runc's own
/// doesn't either, computing it dynamically at query time from the
/// real, current cgroup freezer state instead (see `docs/design/0144`)
/// — [`PersistedState::status`]'s own on-disk field never holds this
/// value; only [`PersistedState::to_view_with_frozen`]'s own returned
/// [`StateView`] ever does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The runtime is still setting the container up (namespaces, mounts,
    /// `create` hooks); nothing has execve'd yet.
    Creating,
    /// `create` finished: namespaces and mounts are set up, the container
    /// process exists and is blocked waiting for `start`.
    Created,
    /// `start` has run the user-specified process.
    Running,
    /// The container process has exited.
    Stopped,
    /// `start` has run the user-specified process, and its own cgroup
    /// freezer currently reports it frozen (`ocirun pause`/`ociman
    /// pause`) — see this enum's own doc comment for why this is never
    /// persisted, only ever computed at display time.
    Paused,
}

impl Status {
    /// The wire/CLI string for this status (`"creating"`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Creating => "creating",
            Status::Created => "created",
            Status::Running => "running",
            Status::Stopped => "stopped",
            Status::Paused => "paused",
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The persisted record for one container, stored as
/// `<root>/<id>/state.json`. Field set matches what `runc state`/`runc
/// list` report (`ociVersion`/`id`/`pid`/`status`/`bundle`/`rootfs`/
/// `created`/`annotations`), so tooling that shells out to an OCI runtime
/// and parses its `state` JSON output sees the shape it expects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedState {
    /// Runtime-spec version of the bundle this container was created from.
    #[serde(rename = "ociVersion")]
    pub oci_version: String,
    /// The container ID (unique within a `StateStore` root).
    pub id: String,
    /// Last-recorded lifecycle status. Use
    /// [`PersistedState::effective_status`] rather than this field
    /// directly: it is stale once the container process has exited on its
    /// own (no `delete` needed to notice that, same as runc/crun).
    pub status: Status,
    /// PID of the container's init process, in the runtime's own PID
    /// namespace view (i.e. as seen from outside the container). `None`
    /// before `create`'s clone/fork has happened.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    /// Absolute path to the bundle directory (containing `config.json`).
    pub bundle: String,
    /// Absolute path to the container's root filesystem.
    pub rootfs: String,
    /// RFC 3339 UTC creation timestamp.
    pub created: String,
    /// RFC 3339 UTC timestamp of this container's own most recent
    /// real start (overwritten on every subsequent `start`, matching
    /// real podman's own identical `StartedTime` field — checked
    /// directly, `~/git/podman/libpod/runtime_ctr.go`'s own
    /// unconditional `ctr.state.StartedTime = time.Now()`, not just
    /// set once at the very first start). `None` for a container that
    /// has never actually started at all yet (still `Creating`/
    /// `Created`), or one recorded by a version of this project
    /// predating this field (`#[serde(default)]`, the same forward-
    /// compatible-record convention `owner`'s own doc comment already
    /// established).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// RFC 3339 UTC timestamp of the moment this container's own
    /// process was last observed to have exited (real podman's own
    /// `FinishedTime`, `~/git/podman/libpod/runtime_ctr.go`) — set
    /// alongside the same real reap moment `ociman`'s own
    /// `run_and_finalize` already records `ANNOTATION_EXIT_CODE` at,
    /// the one real place a container's own process is actually
    /// waited on. `None` for a container that has never stopped at
    /// all (or that predates this field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Annotations copied from `config.json`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
    /// The login name of the effective user that created this
    /// container (`ocirun list`'s own `OWNER` column) — captured once
    /// here, matching real crun's own identical choice
    /// (`~/git/crun/src/libcrun/container.c`'s own `get_user_name
    /// (geteuid())`, persisted into `status.json`) rather than
    /// re-resolved at `list` time the way real runc's own separate
    /// `user.LookupId` approach does (`~/git/runc/list.go`): a
    /// persisted value survives the creating user later being removed
    /// from `/etc/passwd`, matching crun's own choice. A real, honest
    /// empty string when the effective uid has no resolvable
    /// `/etc/passwd` entry at all, matching crun's own identical
    /// `get_user_name` fallback (`return xstrdup("")`) rather than a
    /// fabricated placeholder. `#[serde(default)]` so a `state.json`
    /// written before this field existed deserializes it as an empty
    /// string too — the same forward-compatible-record convention
    /// `docs/design/0344`'s `BoxRecord.hostname` already established.
    #[serde(default)]
    pub owner: String,
}

impl PersistedState {
    /// The status a fresh lookup should report: [`Status::Stopped`] once
    /// the recorded pid is no longer alive, regardless of what was last
    /// written to disk (matches runc/crun re-deriving status from the
    /// live process rather than trusting a cached field). A container
    /// that never got a pid recorded (nothing beyond `create` has run
    /// yet) reports its stored status as-is.
    ///
    /// Liveness is checked via `/proc/<pid>` existence only (no start-time
    /// cross-check against a recorded value yet); a wrapped PID could in
    /// principle be reused by an unrelated process between the container
    /// exiting and this check running. `create`/`start` will record
    /// `/proc/<pid>/stat`'s start-time field alongside the pid so this can
    /// be tightened once there is a real pid to record.
    pub fn effective_status(&self) -> Status {
        match (self.status, self.pid) {
            (Status::Stopped, _) => Status::Stopped,
            (_, Some(pid)) if !process_alive(pid) => Status::Stopped,
            (status, _) => status,
        }
    }

    /// The runc-compatible view of this state, as `ocirun state`/`ocirun
    /// list --format json` render it: [`Self::effective_status`] instead
    /// of the possibly-stale stored status, and `pid` forced to `0`
    /// (rather than omitted) once the container has stopped — matching
    /// `runc state`'s `pid := ...; if status == Stopped { pid = 0 }`.
    pub fn to_view(&self) -> StateView {
        let status = self.effective_status();
        StateView {
            oci_version: self.oci_version.clone(),
            id: self.id.clone(),
            pid: if status == Status::Stopped {
                0
            } else {
                self.pid.unwrap_or(0)
            },
            status,
            bundle: self.bundle.clone(),
            rootfs: self.rootfs.clone(),
            created: self.created.clone(),
            annotations: self.annotations.clone(),
            owner: self.owner.clone(),
        }
    }

    /// Like [`Self::to_view`], but reports [`Status::Paused`] instead
    /// of [`Status::Running`] when `frozen` is `true` — matching real
    /// runc's own `isPaused()`-computed status exactly (see this
    /// module's own `Status::Paused` doc comment). A caller determines
    /// `frozen` itself (by checking the container's own real, current
    /// cgroup — this module has no cgroup access of its own, deliberately,
    /// see its own top doc comment) and passes it in; `false` for a
    /// container this project doesn't even try to check (no cgroup at
    /// all, or the check itself failed) is always a safe default, never
    /// worse than what [`Self::to_view`] alone already reports.
    pub fn to_view_with_frozen(&self, frozen: bool) -> StateView {
        let mut view = self.to_view();
        if frozen && view.status == Status::Running {
            view.status = Status::Paused;
        }
        view
    }
}

/// The runc-compatible JSON shape for `ocirun state`/`ocirun list
/// --format json`: unlike [`PersistedState`] (this crate's own on-disk
/// storage format), `pid` is always present (`0` once stopped, never
/// omitted) and `status` is always freshly computed, never the
/// possibly-stale stored value. Build via [`PersistedState::to_view`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StateView {
    /// Runtime-spec version of the bundle this container was created from.
    #[serde(rename = "ociVersion")]
    pub oci_version: String,
    /// The container ID.
    pub id: String,
    /// PID of the container's init process; `0` once stopped.
    pub pid: i32,
    /// Freshly computed lifecycle status.
    pub status: Status,
    /// Absolute path to the bundle directory.
    pub bundle: String,
    /// Absolute path to the container's root filesystem.
    pub rootfs: String,
    /// RFC 3339 UTC creation timestamp.
    pub created: String,
    /// Annotations copied from `config.json`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
    /// See [`PersistedState::owner`]'s own doc comment — the same
    /// value, always present (a real, possibly-empty string, never
    /// omitted), matching real crun's own `status->owner` always
    /// being serialized (a non-NULL `xstrdup`, even of `""`) rather
    /// than an optional field.
    pub owner: String,
}

/// Whether a process with this PID currently exists.
fn process_alive(pid: i32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// The real, current effective user's own login name — matching real
/// crun's own `get_user_name(geteuid())` exactly (`~/git/crun/src/
/// libcrun/utils.c`, `~/git/crun/src/libcrun/container.c`'s own call
/// site), called once here at container-creation time (see
/// [`PersistedState::owner`]'s own doc comment for why persisting it
/// rather than re-resolving at `list` time, matching crun's own
/// choice over runc's).
///
/// A real, honest empty string on lookup failure (the effective uid
/// has no resolvable `/etc/passwd`/NSS entry at all — a real, if
/// unusual, possibility for a container engine running as some
/// synthetic/dynamic uid) — matching crun's own identical fallback
/// exactly (`get_user_name`'s own `return xstrdup("")` branch when
/// `getpwuid_r` doesn't find a match), never a fabricated placeholder
/// like `runc`'s own `"#<uid>"` string.
fn current_user_name() -> String {
    // `getpwuid_r`'s own buffer must be large enough for every field
    // NSS might return (name, password hash placeholder, gecos, home,
    // shell); `sysconf(_SC_GETPW_R_SIZE_MAX)` is the portable way to
    // ask libc for its own real recommended size rather than guessing
    // a fixed constant (real crun's own fixed 200-byte buffer, `~/git/
    // crun/src/libcrun/utils.c`, works in practice but isn't a
    // documented-safe bound) — falling back to a generous 16KiB (a
    // real, commonly-cited glibc default) on the rare platform where
    // `sysconf` itself returns "no defined limit" (`-1`).
    //
    // SAFETY: `pwd`/`result` are valid, correctly-sized out-params for
    // `getpwuid_r`; `buf` is a real, live, correctly-sized buffer.
    // `getpwuid_r` never retains any of these pointers past its own
    // return, and `pwd.pw_name`, when non-null, points into `buf`
    // itself, which is still alive when read just below.
    #[allow(unsafe_code)]
    unsafe {
        let buf_size = match libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) {
            n if n > 0 => n as usize,
            _ => 16384,
        };
        let mut buf = vec![0_u8; buf_size];
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let ret = libc::getpwuid_r(
            libc::geteuid(),
            &mut pwd,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        );
        if ret == 0 && !result.is_null() && !pwd.pw_name.is_null() {
            std::ffi::CStr::from_ptr(pwd.pw_name)
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        }
    }
}

/// Errors from [`StateStore`] operations.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// Filesystem I/O failure.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// [`StateStore::create`] was called with an ID that already has a
    /// state directory.
    #[error("container with id {0:?} already exists")]
    AlreadyExists(String),
    /// The container ID doesn't exist in this store.
    #[error("container {0:?} does not exist")]
    NotFound(String),
    /// The ID contains characters that would escape the state root
    /// (path separators, `..`, or is otherwise not a safe single path
    /// component) or is empty.
    #[error("invalid container id {0:?}: {1}")]
    InvalidId(String, &'static str),
    /// A `state.json` failed to parse (corrupt, or written by an
    /// incompatible version).
    #[error("corrupt state file for {id:?}: {source}")]
    Corrupt {
        /// The container ID whose state file failed to parse.
        id: String,
        /// The JSON parse error.
        #[source]
        source: serde_json::Error,
    },
}

/// Result alias for [`StateError`].
pub type Result<T> = std::result::Result<T, StateError>;

/// A directory of container state records, rooted at `<root>` (one
/// subdirectory per container ID, holding `state.json`).
pub struct StateStore {
    root: PathBuf,
}

impl StateStore {
    /// Open (creating if necessary) a state store rooted at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(StateStore { root })
    }

    /// The store's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory a container's own `state.json` (and anything else
    /// per-container, like [`crate::exec_fifo`]) lives in. Does not
    /// validate `id` or check the container actually exists — callers
    /// needing that should go through [`StateStore::load`] first.
    pub fn container_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    fn state_path(&self, id: &str) -> PathBuf {
        self.container_dir(id).join("state.json")
    }

    /// Create a new container state record. Fails with
    /// [`StateError::AlreadyExists`] if `id` is already present — the
    /// state directory is created with `create_dir` (not `create_dir_all`),
    /// which is atomic with respect to concurrent creators of the same ID.
    pub fn create(
        &self,
        id: &str,
        bundle: &Path,
        rootfs: &Path,
        annotations: BTreeMap<String, String>,
    ) -> Result<PersistedState> {
        validate_id(id)?;
        let dir = self.container_dir(id);
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(StateError::AlreadyExists(id.to_string()));
            }
            Err(e) => return Err(e.into()),
        }

        let state = PersistedState {
            oci_version: OCI_VERSION.to_string(),
            id: id.to_string(),
            status: Status::Creating,
            pid: None,
            bundle: bundle.to_string_lossy().into_owned(),
            rootfs: rootfs.to_string_lossy().into_owned(),
            created: format_rfc3339_utc(SystemTime::now()),
            started_at: None,
            finished_at: None,
            annotations,
            owner: current_user_name(),
        };

        // Best-effort cleanup on write failure: don't leave a
        // directory-with-no-state.json behind for `list` to trip over.
        if let Err(e) = self.write(&state) {
            let _ = fs::remove_dir(&dir);
            return Err(e);
        }
        Ok(state)
    }

    /// Persist (overwrite) `state`. Atomic: written to a temp file in the
    /// container's own directory, then renamed into place.
    pub fn write(&self, state: &PersistedState) -> Result<()> {
        let dir = self.container_dir(&state.id);
        let json = serde_json::to_vec_pretty(state).expect("PersistedState serializes");
        let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
        io::Write::write_all(&mut tmp, &json)?;
        tmp.persist(self.state_path(&state.id))
            .map_err(|e| e.error)?;
        Ok(())
    }

    /// Load a container's state record.
    pub fn load(&self, id: &str) -> Result<PersistedState> {
        validate_id(id)?;
        let bytes = match fs::read(self.state_path(id)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(StateError::NotFound(id.to_string()));
            }
            Err(e) => return Err(e.into()),
        };
        serde_json::from_slice(&bytes).map_err(|source| StateError::Corrupt {
            id: id.to_string(),
            source,
        })
    }

    /// Remove a container's state record entirely.
    pub fn remove(&self, id: &str) -> Result<()> {
        validate_id(id)?;
        let dir = self.container_dir(id);
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err(StateError::NotFound(id.to_string()))
            }
            Err(_) => {
                // A real rootless overlay-mounted container (`ociman
                // run`'s own `rootfs_setup` module, `docs/design/
                // 0110`) leaves its own `workdir/work` subdirectory
                // locked to mode `0000` behind once it exits — the
                // same real kernel-level bookkeeping `crate::overlay`'s
                // own feasibility probe already found and worked
                // around for its own scratch directories (`docs/
                // design/0108`). Retried once, after resetting
                // permissions everywhere under `dir`, rather than
                // paying that reset's own real (if small) per-
                // directory cost on every ordinary removal that would
                // already have succeeded on the first attempt.
                crate::overlay::reset_permissions_for_removal(&dir);
                fs::remove_dir_all(&dir).map_err(|e| {
                    if e.kind() == io::ErrorKind::NotFound {
                        StateError::NotFound(id.to_string())
                    } else {
                        e.into()
                    }
                })
            }
        }
    }

    /// List every container's state record, sorted by ID. Entries with a
    /// corrupt or missing `state.json` are skipped rather than failing the
    /// whole listing (matches runc's `list`, which logs and continues).
    pub fn list(&self) -> Result<Vec<PersistedState>> {
        let mut out = Vec::new();
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if let Ok(state) = self.load(&id) {
                out.push(state);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }
}

/// A container ID must be a single, non-empty path component: no `/`, no
/// `..`, not `.`. Keeps IDs confined to their state directory (an ID like
/// `../../etc` must never be usable to escape `<root>`).
fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(StateError::InvalidId(id.to_string(), "must not be empty"));
    }
    if id == "." || id == ".." {
        return Err(StateError::InvalidId(
            id.to_string(),
            "must not be \".\" or \"..\"",
        ));
    }
    if id.contains('/') || id.contains('\0') {
        return Err(StateError::InvalidId(
            id.to_string(),
            "must not contain '/' or NUL",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, StateStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(dir.path().join("state-root")).unwrap();
        (dir, store)
    }

    #[test]
    fn create_then_load_round_trips() {
        let (_dir, store) = store();
        let created = store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap();
        assert_eq!(created.status, Status::Creating);
        assert_eq!(created.pid, None);
        assert_eq!(created.oci_version, OCI_VERSION);
        assert_eq!(created.started_at, None);
        assert_eq!(created.finished_at, None);

        let loaded = store.load("c1").unwrap();
        assert_eq!(loaded, created);
    }

    #[test]
    fn create_rejects_duplicate_id() {
        let (_dir, store) = store();
        store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap();
        let err = store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap_err();
        assert!(matches!(err, StateError::AlreadyExists(id) if id == "c1"));
    }

    #[test]
    fn load_missing_container_is_not_found() {
        let (_dir, store) = store();
        let err = store.load("nope").unwrap_err();
        assert!(matches!(err, StateError::NotFound(id) if id == "nope"));
    }

    #[test]
    fn remove_then_load_is_not_found() {
        let (_dir, store) = store();
        store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap();
        store.remove("c1").unwrap();
        assert!(matches!(store.load("c1"), Err(StateError::NotFound(_))));
    }

    #[test]
    fn remove_missing_container_is_not_found() {
        let (_dir, store) = store();
        assert!(matches!(store.remove("nope"), Err(StateError::NotFound(_))));
    }

    #[test]
    fn rejects_unsafe_ids() {
        let (_dir, store) = store();
        for bad in ["", ".", "..", "a/b", "../escape"] {
            assert!(
                matches!(
                    store.create(bad, Path::new("/b"), Path::new("/b/r"), BTreeMap::new()),
                    Err(StateError::InvalidId(_, _))
                ),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn list_is_sorted_and_skips_nothing_valid() {
        let (_dir, store) = store();
        for id in ["zebra", "apple", "mango"] {
            store
                .create(id, Path::new("/b"), Path::new("/b/r"), BTreeMap::new())
                .unwrap();
        }
        let ids: Vec<_> = store.list().unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["apple", "mango", "zebra"]);
    }

    #[test]
    fn list_on_nonexistent_root_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(dir.path().join("nonexistent")).unwrap();
        fs::remove_dir(store.root()).unwrap();
        assert_eq!(store.list().unwrap(), vec![]);
    }

    #[test]
    fn write_updates_persisted_state() {
        let (_dir, store) = store();
        let mut state = store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap();
        state.status = Status::Created;
        state.pid = Some(std::process::id() as i32);
        store.write(&state).unwrap();

        let loaded = store.load("c1").unwrap();
        assert_eq!(loaded.status, Status::Created);
        assert_eq!(loaded.pid, state.pid);
    }

    #[test]
    fn effective_status_downgrades_dead_pid_to_stopped() {
        let (_dir, store) = store();
        let mut state = store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap();
        // PID 1 always exists on a real Linux system (init/systemd), and
        // this test process is very unlikely to be it.
        state.status = Status::Running;
        state.pid = Some(1);
        assert_eq!(state.effective_status(), Status::Running);

        // A pid that (almost certainly) doesn't exist.
        state.pid = Some(i32::MAX - 1);
        assert_eq!(state.effective_status(), Status::Stopped);
    }

    #[test]
    fn effective_status_of_own_pid_is_running() {
        let (_dir, store) = store();
        let mut state = store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap();
        state.status = Status::Running;
        state.pid = Some(std::process::id() as i32);
        assert_eq!(state.effective_status(), Status::Running);
    }

    #[test]
    fn effective_status_without_pid_reports_stored_status() {
        let (_dir, store) = store();
        let state = store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap();
        assert_eq!(state.effective_status(), Status::Creating);
    }

    #[test]
    fn to_view_forces_pid_zero_when_stopped() {
        let (_dir, store) = store();
        let mut state = store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap();
        state.status = Status::Stopped;
        state.pid = Some(4242);
        let view = state.to_view();
        assert_eq!(view.pid, 0);
        assert_eq!(view.status, Status::Stopped);
    }

    #[test]
    fn to_view_reports_live_pid() {
        let (_dir, store) = store();
        let mut state = store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap();
        state.status = Status::Running;
        state.pid = Some(std::process::id() as i32);
        let view = state.to_view();
        assert_eq!(view.pid, std::process::id() as i32);
        assert_eq!(view.status, Status::Running);
    }

    #[test]
    fn to_view_with_frozen_upgrades_running_to_paused() {
        let (_dir, store) = store();
        let mut state = store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap();
        state.status = Status::Running;
        state.pid = Some(std::process::id() as i32);

        assert_eq!(state.to_view_with_frozen(false).status, Status::Running);
        assert_eq!(state.to_view_with_frozen(true).status, Status::Paused);
    }

    #[test]
    fn to_view_with_frozen_never_upgrades_a_non_running_status() {
        let (_dir, store) = store();
        let state = store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap();
        // Still `Creating` (no pid recorded yet) -- `frozen = true`
        // must never turn a non-`Running` status into `Paused`, even
        // if a caller somehow got that wrong.
        assert_eq!(state.to_view_with_frozen(true).status, Status::Creating);
    }

    #[test]
    fn json_field_names_match_runc() {
        let (_dir, store) = store();
        let mut state = store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap();
        state.pid = Some(1234);
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["ociVersion"], OCI_VERSION);
        assert_eq!(json["id"], "c1");
        assert_eq!(json["status"], "creating");
        assert_eq!(json["pid"], 1234);
        assert_eq!(json["bundle"], "/bundle");
        assert_eq!(json["rootfs"], "/bundle/rootfs");
        assert!(json["created"].as_str().unwrap().ends_with('Z'));
    }

    /// `owner` (0345) matches the real, current effective user's own
    /// login name -- checked the most direct way available: against a
    /// real `whoami` subprocess, not just this crate's own
    /// `current_user_name` called a second time (which would only
    /// prove self-consistency, not correctness against the real
    /// system NSS database).
    #[test]
    fn create_records_the_real_effective_users_own_login_name_as_owner() {
        let (_dir, store) = store();
        let state = store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap();
        let whoami = std::process::Command::new("whoami")
            .output()
            .expect("failed to spawn whoami");
        assert!(whoami.status.success());
        let expected = String::from_utf8_lossy(&whoami.stdout).trim().to_string();
        assert_eq!(state.owner, expected);
    }

    /// `to_view`/`to_view_with_frozen` both carry `owner` through
    /// unchanged -- a real, previously-easy-to-miss field to forget
    /// wiring into the view conversion (unlike `pid`/`status`, it
    /// needs no transformation at all, just a plain copy).
    #[test]
    fn to_view_carries_owner_through_unchanged() {
        let (_dir, store) = store();
        let state = store
            .create(
                "c1",
                Path::new("/bundle"),
                Path::new("/bundle/rootfs"),
                BTreeMap::new(),
            )
            .unwrap();
        assert_eq!(state.to_view().owner, state.owner);
        assert_eq!(state.to_view_with_frozen(false).owner, state.owner);
    }
}
