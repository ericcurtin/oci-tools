//! The one generic, persistent record store behind both of `ocicri`'s
//! CRI object families — pod sandboxes (`sandbox.rs`, 0233) and
//! containers (`container.rs`, 0236): one JSON file per record under
//! a per-family directory, written atomically via the same
//! temp-file-plus-rename technique `oci_store`'s own pointer files
//! use, so a restarted `ocicri` still knows its state — exactly like
//! real `cri-o` restores its own from `containers/storage` rather
//! than starting amnesiac.
//!
//! Factored out of `sandbox.rs`'s originally sandbox-only versions
//! the moment a second record family (containers) needed the
//! identical save/load/prefix-resolve/remove mechanics, rather than
//! duplicating them — `sandbox.rs`'s own public API (and its tests)
//! are unchanged, now thin delegations to this module.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// What every stored record family provides: a unique ID (the file
/// name, and the prefix-resolution key — real cri-o's own truncindex
/// equivalent) and a creation timestamp (the newest-first sort key
/// `load_all` returns records in).
pub trait Record: Serialize + DeserializeOwned {
    /// The record's own unique 64-hex ID.
    fn id(&self) -> &str;
    /// Creation time in nanoseconds since the epoch.
    fn created_at_nanos(&self) -> i64;
}

fn record_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.json"))
}

/// A real, random 64-hex object ID — the exact shape real cri-o's
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
        "{:?}-{}-record-{}",
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
pub fn save<T: Record>(root: &Path, record: &T) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    let mut tmp = tempfile::NamedTempFile::new_in(root)?;
    tmp.write_all(&serde_json::to_vec_pretty(record)?)?;
    tmp.persist(record_path(root, record.id()))
        .map_err(|e| e.error)?;
    Ok(())
}

/// Loads every stored record, sorted by creation time (newest first,
/// a stable order — the proto itself mandates none).
pub fn load_all<T: Record>(root: &Path) -> std::io::Result<Vec<T>> {
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
        let record: T = serde_json::from_slice(&bytes)?;
        records.push(record);
    }
    records.sort_by(|a, b| {
        b.created_at_nanos()
            .cmp(&a.created_at_nanos())
            .then_with(|| a.id().cmp(b.id()))
    });
    Ok(records)
}

/// Resolves one record by ID prefix — matching real cri-o's own
/// truncindex-backed lookups (prefix-based): `Ok(None)` for no match
/// at all, an `AmbiguousPrefix` error when the prefix matches more
/// than one distinct record.
pub fn find_by_id_prefix<T: Record>(root: &Path, prefix: &str) -> Result<Option<T>, LookupError> {
    if prefix.is_empty() {
        return Ok(None);
    }
    let mut found: Option<T> = None;
    for record in load_all::<T>(root).map_err(LookupError::Io)? {
        if record.id().starts_with(prefix) {
            if found.is_some() {
                return Err(LookupError::AmbiguousPrefix(prefix.to_string()));
            }
            found = Some(record);
        }
    }
    Ok(found)
}

/// Removes one record by exact ID. Returns whether a record actually
/// existed.
pub fn remove(root: &Path, id: &str) -> std::io::Result<bool> {
    match std::fs::remove_file(record_path(root, id)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// A record lookup failure — either real I/O trouble or a genuinely
/// ambiguous ID prefix (a client-input problem, reported distinctly so
/// the RPC layer can map it to `InvalidArgument` rather than a generic
/// internal error).
#[derive(Debug)]
pub enum LookupError {
    /// Reading the record directory failed.
    Io(std::io::Error),
    /// The given prefix matches more than one distinct record.
    AmbiguousPrefix(String),
}

impl std::fmt::Display for LookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "reading records: {e}"),
            Self::AmbiguousPrefix(prefix) => {
                write!(f, "ID {prefix:?} is ambiguous")
            }
        }
    }
}

impl std::error::Error for LookupError {}
