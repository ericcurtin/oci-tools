//! Registry credential lookup, compatible with the `docker login` /
//! `podman login` auth file format:
//!
//! ```json
//! { "auths": { "quay.io": { "auth": "base64(user:pass)" } } }
//! ```
//!
//! The `auth` field is *already* base64(`user:pass`) — exactly the value an
//! HTTP `Authorization: Basic <auth>` header needs, so no decoding or
//! re-encoding happens here; we pass it straight through.

use std::collections::HashMap;
use std::path::PathBuf;

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
}
