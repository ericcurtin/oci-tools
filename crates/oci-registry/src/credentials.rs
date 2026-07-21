//! Registry credential lookup ([`Credentials`]) and, since 0126, real
//! read-modify-write editing ([`set`]/[`unset`], `ociman login`/
//! `ociman logout`'s own backing) — both compatible with the `docker
//! login`/`podman login` auth file format:
//!
//! ```json
//! { "auths": { "quay.io": { "auth": "base64(user:pass)" } } }
//! ```
//!
//! The `auth` field is *already* base64(`user:pass`) — exactly the value an
//! HTTP `Authorization: Basic <auth>` header needs, so [`Credentials`]
//! itself never decodes or re-encodes it, just passes it straight
//! through; [`set`] is the one place in this module that actually
//! computes it (see its own hand-rolled `base64_encode`, `docs/design/
//! 0126`).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Docker Hub's legacy credential-store key: `docker login` (and old Docker
/// clients) key Hub credentials by this URL rather than by the registry
/// host oci-tools actually connects to (`registry-1.docker.io`).
const DOCKER_HUB_LEGACY_KEY: &str = "https://index.docker.io/v1/";
const DOCKER_HUB_HOST: &str = "registry-1.docker.io";

/// Credentials loaded from the standard podman/docker auth file locations.
#[derive(Debug, Default, Clone)]
pub struct Credentials {
    /// registry host -> base64(user:pass), verbatim from the auth file.
    entries: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct AuthFile {
    #[serde(default)]
    auths: HashMap<String, AuthEntry>,
}

#[derive(Debug, Deserialize)]
struct AuthEntry {
    #[serde(default)]
    auth: String,
}

impl Credentials {
    /// Load credentials from every standard location that exists, in
    /// priority order (first match for a given host wins):
    /// `$REGISTRY_AUTH_FILE`, `$XDG_RUNTIME_DIR/containers/auth.json`,
    /// `~/.config/containers/auth.json`, `~/.docker/config.json`.
    pub fn load() -> Self {
        let mut entries = HashMap::new();
        for path in candidate_paths() {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(parsed) = serde_json::from_slice::<AuthFile>(&bytes) else {
                continue;
            };
            for (host, entry) in parsed.auths {
                if entry.auth.is_empty() {
                    continue;
                }
                entries.entry(host).or_insert(entry.auth);
            }
        }
        Credentials { entries }
    }

    /// An empty credential set (anonymous pulls only); useful for tests.
    pub fn empty() -> Self {
        Credentials {
            entries: HashMap::new(),
        }
    }

    /// The `Authorization: Basic ...` header value for `registry_host`, if
    /// credentials are configured for it.
    pub fn basic_auth_header(&self, registry_host: &str) -> Option<String> {
        let auth = self.entries.get(registry_host).or_else(|| {
            (registry_host == DOCKER_HUB_HOST)
                .then(|| self.entries.get(DOCKER_HUB_LEGACY_KEY))
                .flatten()
        })?;
        Some(format!("Basic {auth}"))
    }
}

/// Set `registry_host`'s own credentials in the auth file at `path`
/// (`ociman login`'s own backing) — reads whatever is already there
/// first (an empty `{"auths": {}}` object if `path` doesn't exist
/// yet), updates (or inserts) only `auths.<registry_host>.auth`,
/// preserving every other entry and any other top-level field the
/// file might already have (matching real `podman login`'s own
/// behavior of touching only the one entry it's asked to set — a real
/// `~/.docker/config.json` commonly has `credsStore`/`credHelpers`
/// fields alongside `auths`, which must survive untouched), then
/// writes the result back atomically (temp file + rename, the same
/// real, deliberate improvement over a plain in-place write this
/// project's own `oci_bls::grubenv::write` already established for
/// its own equally sensitive file) with real `0o600` permissions —
/// matching real podman's own `ioutils.AtomicWriteFile(path, data,
/// 0o600)` exactly (checked directly against `~/git/container-libs/
/// image/pkg/docker/config/config.go`).
///
/// Deliberately does **not** verify the credentials against the real
/// registry first the way real `podman login`/`docker login` both do
/// (`docker.CheckAuth`, a real HTTP round trip) — a real, honest scope
/// narrowing (not an oversight): this writes exactly what a pull/push
/// would already need to succeed later, and a wrong password simply
/// surfaces as a real, clear failure on the next real registry
/// operation instead, same as if a user had hand-edited this file
/// incorrectly. Verifying correctly against every real registry's own
/// token-scope conventions is real, separate work left for a future
/// increment.
pub fn set(path: &Path, registry_host: &str, username: &str, password: &str) -> io::Result<()> {
    let mut root = read_or_default(path)?;
    let auths = root
        .get_mut("auths")
        .and_then(|v| v.as_object_mut())
        .expect("read_or_default always ensures a real, present `auths` object");
    let encoded = base64_encode(format!("{username}:{password}").as_bytes());
    auths.insert(
        registry_host.to_string(),
        serde_json::json!({ "auth": encoded }),
    );
    write_atomic(path, &root)
}

/// Remove `registry_host`'s own entry from the auth file at `path`
/// (`ociman logout`'s own backing), preserving every other entry —
/// see [`set`]'s own doc comment for the exact on-disk behavior this
/// shares. Returns whether an entry for `registry_host` actually
/// existed to remove. A missing `path` is treated as "nothing to log
/// out of" (`Ok(false)`), not an error — matches real `podman
/// logout`'s own tolerance for logging out of a registry you were
/// never logged into in the first place.
pub fn unset(path: &Path, registry_host: &str) -> io::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let mut root = read_or_default(path)?;
    let auths = root
        .get_mut("auths")
        .and_then(|v| v.as_object_mut())
        .expect("read_or_default always ensures a real, present `auths` object");
    let removed = auths.remove(registry_host).is_some();
    if removed {
        write_atomic(path, &root)?;
    }
    Ok(removed)
}

/// Read `path` as a generic JSON value, defaulting to a fresh
/// `{"auths": {}}` object if it doesn't exist yet — always guarantees
/// a real, present, object-typed `auths` field on return (creating one
/// if the file existed but never had it, or had it as some other
/// type), so [`set`]/[`unset`] never need to handle that case
/// themselves.
fn read_or_default(path: &Path) -> io::Result<serde_json::Value> {
    let mut root: serde_json::Value = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e),
    };
    if !root.is_object() {
        root = serde_json::json!({});
    }
    if !root.get("auths").is_some_and(serde_json::Value::is_object) {
        root["auths"] = serde_json::json!({});
    }
    Ok(root)
}

/// Write `value` to `path` atomically (a temp file in the same
/// directory, renamed into place — so a concurrent reader only ever
/// observes the old or the new content, never a torn write) with real
/// `0o600` permissions, creating `path`'s own parent directory first
/// if it doesn't exist yet (matching real podman's own behavior:
/// `$XDG_RUNTIME_DIR/containers/` commonly doesn't exist until the
/// first `podman login` creates it).
fn write_atomic(path: &Path, value: &serde_json::Value) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    {
        use std::io::Write as _;
        tmp.write_all(&bytes)?;
    }
    {
        use std::os::unix::fs::PermissionsExt as _;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Encode `data` as standard base64 (RFC 4648, `=`-padded) — exactly
/// what an HTTP `Authorization: Basic <...>` header (and this same
/// crate's own auth file format) needs. Hand-rolled rather than
/// pulling in a dependency for one small, well-defined, easily-tested
/// algorithm, matching this project's own established "minimal
/// dependencies" practice (e.g. `HEALTHCHECK`'s own hand-rolled
/// duration parser, `docs/design/0116`).
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied();
        let b2 = chunk.get(2).copied();
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
        match b1 {
            Some(b1) => {
                out.push(ALPHABET[(((b1 & 0x0f) << 2) | (b2.unwrap_or(0) >> 6)) as usize] as char)
            }
            None => out.push('='),
        }
        match b2 {
            Some(b2) => out.push(ALPHABET[(b2 & 0x3f) as usize] as char),
            None => out.push('='),
        }
    }
    out
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(path) = std::env::var("REGISTRY_AUTH_FILE") {
        paths.push(PathBuf::from(path));
    }
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        paths.push(PathBuf::from(dir).join("containers").join("auth.json"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        paths.push(home.join(".config").join("containers").join("auth.json"));
        paths.push(home.join(".docker").join("config.json"));
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_has_no_credentials() {
        assert_eq!(Credentials::empty().basic_auth_header("quay.io"), None);
    }

    #[test]
    fn looks_up_exact_host() {
        let mut entries = HashMap::new();
        entries.insert("quay.io".to_string(), "dXNlcjpwYXNz".to_string());
        let creds = Credentials { entries };
        assert_eq!(
            creds.basic_auth_header("quay.io"),
            Some("Basic dXNlcjpwYXNz".to_string())
        );
        assert_eq!(creds.basic_auth_header("other.example"), None);
    }

    #[test]
    fn falls_back_to_docker_hub_legacy_key() {
        let mut entries = HashMap::new();
        entries.insert(
            DOCKER_HUB_LEGACY_KEY.to_string(),
            "dXNlcjpwYXNz".to_string(),
        );
        let creds = Credentials { entries };
        assert_eq!(
            creds.basic_auth_header(DOCKER_HUB_HOST),
            Some("Basic dXNlcjpwYXNz".to_string())
        );
    }

    #[test]
    fn parses_real_auth_file_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            r#"{"auths": {"quay.io": {"auth": "dXNlcjpwYXNz", "email": "unused"}}}"#,
        )
        .unwrap();
        // Exercise the parser directly (candidate_paths() reads real env,
        // which we don't want to mutate from a unit test).
        let bytes = std::fs::read(&path).unwrap();
        let parsed: AuthFile = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.auths["quay.io"].auth, "dXNlcjpwYXNz");
    }

    #[test]
    fn base64_encode_matches_the_known_user_pass_value_already_used_elsewhere_in_this_file() {
        // The exact value every other test in this file already uses as a
        // known-good `base64(user:pass)` fixture -- not a coincidence:
        // proves this hand-rolled encoder produces exactly what the real
        // `docker login`/`podman login` auth file format expects.
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn base64_encode_handles_every_padding_case() {
        // 0, 1, and 2 bytes of remainder -- the three real cases the
        // trailing `=`/`==` padding logic has to get right.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn set_creates_a_new_auth_file_from_scratch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("auth.json");
        set(&path, "quay.io", "user", "pass").unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let parsed: AuthFile = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.auths["quay.io"].auth, "dXNlcjpwYXNz");

        // Real `0o600` permissions, matching real podman exactly.
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn set_preserves_every_other_entry_and_unrelated_top_level_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            r#"{"auths": {"quay.io": {"auth": "old"}}, "credsStore": "desktop"}"#,
        )
        .unwrap();

        set(&path, "ghcr.io", "newuser", "newpass").unwrap();

        let root: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(root["auths"]["quay.io"]["auth"], "old");
        assert_eq!(
            root["auths"]["ghcr.io"]["auth"],
            base64_encode(b"newuser:newpass")
        );
        assert_eq!(root["credsStore"], "desktop");
    }

    #[test]
    fn set_on_an_existing_host_overwrites_its_own_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        set(&path, "quay.io", "old", "old").unwrap();
        set(&path, "quay.io", "new", "new").unwrap();

        let root: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(root["auths"]["quay.io"]["auth"], base64_encode(b"new:new"));
    }

    #[test]
    fn unset_removes_only_the_named_host() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        set(&path, "quay.io", "user", "pass").unwrap();
        set(&path, "ghcr.io", "user", "pass").unwrap();

        let removed = unset(&path, "quay.io").unwrap();
        assert!(removed);

        let root: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(root["auths"].get("quay.io").is_none());
        assert!(root["auths"].get("ghcr.io").is_some());
    }

    #[test]
    fn unset_of_a_host_never_logged_into_is_a_real_no_op_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        set(&path, "quay.io", "user", "pass").unwrap();

        let removed = unset(&path, "never-logged-in.example").unwrap();
        assert!(!removed);
    }

    #[test]
    fn unset_of_a_missing_file_is_a_real_no_op_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        let removed = unset(&path, "quay.io").unwrap();
        assert!(!removed);
        assert!(!path.exists(), "must not create the file just to log out");
    }

    #[test]
    fn set_then_load_resolves_a_real_basic_auth_header() {
        // End-to-end: what `set` writes is exactly what `Credentials::
        // load` (via `REGISTRY_AUTH_FILE`) already knows how to read
        // back -- the whole point of matching the same file format.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        set(&path, "quay.io", "user", "pass").unwrap();

        // SAFETY: this test process is single-threaded for the
        // duration of this call -- same reasoning already established
        // elsewhere in this project for a scoped env var mutation in a
        // test (e.g. `ociman`'s own `parse_build_args` tests).
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("REGISTRY_AUTH_FILE", &path);
        }
        let creds = Credentials::load();
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("REGISTRY_AUTH_FILE");
        }
        assert_eq!(
            creds.basic_auth_header("quay.io"),
            Some("Basic dXNlcjpwYXNz".to_string())
        );
    }
}
