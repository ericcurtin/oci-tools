//! Docker/OCI image reference parsing and normalization.
//!
//! Mirrors the normalization rules of `github.com/distribution/reference`
//! (the Go library every Docker-ecosystem tool uses) closely enough that
//! familiar references parse the same way podman/docker do:
//!
//! * `ubuntu` -> registry `docker.io`, repository `library/ubuntu`, tag `latest`
//! * `ubuntu:24.04` -> tag `24.04`
//! * `myuser/myrepo` -> registry `docker.io`, repository `myuser/myrepo`
//! * `quay.io/foo/bar@sha256:...` -> digest reference, no tag
//! * `localhost/foo`, `example.com:5000/foo` -> explicit registry (contains
//!   a `.`, `:`, or is exactly `localhost`)
//!
//! Only the normalization users actually rely on is implemented; the full
//! grammar's edge cases (nested-namespace `library/` handling, IPv6 literal
//! hosts) are deliberately out of scope until something needs them.

use std::fmt;

use crate::digest::{Digest, DigestParseError};

const DEFAULT_REGISTRY: &str = "docker.io";
/// The registry host actually spoken to for Docker Hub; `docker.io` is the
/// user-facing/normalized name.
pub const DOCKER_HUB_REGISTRY_HOST: &str = "registry-1.docker.io";
const LEGACY_DEFAULT_REGISTRY: &str = "index.docker.io";
const OFFICIAL_REPO_PREFIX: &str = "library/";
const DEFAULT_TAG: &str = "latest";

/// A fully parsed, normalized image reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    registry: String,
    repository: String,
    tag: Option<String>,
    digest: Option<Digest>,
}

/// Error returned by [`Reference::parse`].
#[derive(Debug, thiserror::Error)]
pub enum ReferenceParseError {
    /// The string was empty.
    #[error("image reference is empty")]
    Empty,
    /// The repository path contains uppercase characters (not allowed;
    /// registries are case-sensitive-lowercase by convention).
    #[error("invalid reference {0:?}: repository name must be lowercase")]
    NotLowercase(String),
    /// The digest part (after `@`) failed to parse.
    #[error("invalid reference {0:?}: {1}")]
    BadDigest(String, #[source] DigestParseError),
    /// The repository path is empty (e.g. `"myregistry.com:5000"` alone).
    #[error("invalid reference {0:?}: empty repository name")]
    EmptyRepository(String),
}

impl Reference {
    /// Parse and normalize an image reference string.
    pub fn parse(s: &str) -> Result<Self, ReferenceParseError> {
        if s.is_empty() {
            return Err(ReferenceParseError::Empty);
        }

        // Split off an `@sha256:...` digest suffix first (it may coexist
        // with a tag on the wire; Docker's convention is that digest wins
        // and the tag is dropped, matching `ParseDockerRef`).
        let (before_digest, digest) = match s.split_once('@') {
            Some((before, digest_str)) => {
                let digest = Digest::parse(digest_str)
                    .map_err(|e| ReferenceParseError::BadDigest(s.to_string(), e))?;
                (before, Some(digest))
            }
            None => (s, None),
        };

        let (domain, remainder) = split_domain(before_digest);

        // A tag, if any, is the part after the last ':' in `remainder`,
        // provided that colon comes after the last '/' (so port-less
        // registry names already consumed by `split_domain` never leak
        // in here).
        let (repo_path, tag) = match remainder.rfind(':') {
            Some(idx) if !remainder[idx + 1..].contains('/') => {
                (&remainder[..idx], Some(remainder[idx + 1..].to_string()))
            }
            _ => (remainder, None),
        };

        if repo_path.is_empty() {
            return Err(ReferenceParseError::EmptyRepository(s.to_string()));
        }
        if repo_path.to_lowercase() != repo_path {
            return Err(ReferenceParseError::NotLowercase(s.to_string()));
        }

        let (registry, repository) = normalize_domain(domain, repo_path);

        // Digest present: drop any tag, matching `ParseDockerRef`.
        let tag = if digest.is_some() { None } else { tag };

        Ok(Reference {
            registry,
            repository,
            tag: tag.or_else(|| digest.is_none().then(|| DEFAULT_TAG.to_string())),
            digest,
        })
    }

    /// The registry host to connect to, e.g. `docker.io` (callers resolve
    /// this to the actual endpoint, `registry-1.docker.io`, via
    /// [`Reference::registry_host`]).
    pub fn registry(&self) -> &str {
        &self.registry
    }

    /// The registry host to actually open a connection to (Docker Hub's
    /// user-facing name `docker.io` maps to `registry-1.docker.io`; every
    /// other registry is used as-is).
    pub fn registry_host(&self) -> &str {
        if self.registry == DEFAULT_REGISTRY {
            DOCKER_HUB_REGISTRY_HOST
        } else {
            &self.registry
        }
    }

    /// The repository path, e.g. `library/ubuntu`.
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// The tag, if the reference is tag-addressed (`None` for digest
    /// references).
    pub fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }

    /// The digest, if the reference is digest-addressed.
    pub fn digest(&self) -> Option<&Digest> {
        self.digest.as_ref()
    }

    /// The manifest-endpoint path segment: the digest if present, else the
    /// tag (`GET /v2/<repository>/manifests/<this>`).
    pub fn manifest_ref(&self) -> String {
        match &self.digest {
            Some(d) => d.to_string(),
            None => self.tag.clone().unwrap_or_else(|| DEFAULT_TAG.to_string()),
        }
    }

    /// The shortened, user-familiar form (drops `docker.io/library/` and
    /// `docker.io/` where they were implied), e.g. `ubuntu:24.04` or
    /// `myuser/myrepo:latest`.
    pub fn familiar(&self) -> String {
        let repo = if self.registry == DEFAULT_REGISTRY {
            self.repository
                .strip_prefix(OFFICIAL_REPO_PREFIX)
                .filter(|rest| !rest.contains('/'))
                .unwrap_or(&self.repository)
                .to_string()
        } else {
            format!("{}/{}", self.registry, self.repository)
        };
        match &self.digest {
            Some(d) => format!("{repo}@{d}"),
            None => format!("{repo}:{}", self.tag.as_deref().unwrap_or(DEFAULT_TAG)),
        }
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.registry, self.repository)?;
        match &self.digest {
            Some(d) => write!(f, "@{d}"),
            None => write!(f, ":{}", self.tag.as_deref().unwrap_or(DEFAULT_TAG)),
        }
    }
}

/// Split `s` into `(domain, remainder)`. `domain` is `None` when `s` has no
/// `/`, or when the first path segment does not look like a registry host.
fn split_domain(s: &str) -> (Option<&str>, &str) {
    match s.split_once('/') {
        None => (None, s),
        Some((maybe_domain, rest)) => {
            let looks_like_domain = maybe_domain == "localhost"
                || maybe_domain.contains('.')
                || maybe_domain.contains(':')
                || maybe_domain.to_lowercase() != maybe_domain;
            if looks_like_domain {
                (Some(maybe_domain), rest)
            } else {
                (None, s)
            }
        }
    }
}

/// Apply Docker Hub normalization: implicit `docker.io` registry, implicit
/// `library/` namespace for single-segment repository paths, and the
/// legacy `index.docker.io` alias.
fn normalize_domain(domain: Option<&str>, repo_path: &str) -> (String, String) {
    match domain {
        None => {
            let repository = if repo_path.contains('/') {
                repo_path.to_string()
            } else {
                format!("{OFFICIAL_REPO_PREFIX}{repo_path}")
            };
            (DEFAULT_REGISTRY.to_string(), repository)
        }
        Some(LEGACY_DEFAULT_REGISTRY) => {
            let repository = if repo_path.contains('/') {
                repo_path.to_string()
            } else {
                format!("{OFFICIAL_REPO_PREFIX}{repo_path}")
            };
            (DEFAULT_REGISTRY.to_string(), repository)
        }
        Some(DEFAULT_REGISTRY) => {
            let repository = if repo_path.contains('/') {
                repo_path.to_string()
            } else {
                format!("{OFFICIAL_REPO_PREFIX}{repo_path}")
            };
            (DEFAULT_REGISTRY.to_string(), repository)
        }
        Some(other) => (other.to_string(), repo_path.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_name_normalizes_to_docker_hub_library() {
        let r = Reference::parse("ubuntu").unwrap();
        assert_eq!(r.registry(), "docker.io");
        assert_eq!(r.registry_host(), "registry-1.docker.io");
        assert_eq!(r.repository(), "library/ubuntu");
        assert_eq!(r.tag(), Some("latest"));
        assert_eq!(r.digest(), None);
        assert_eq!(r.to_string(), "docker.io/library/ubuntu:latest");
        assert_eq!(r.familiar(), "ubuntu:latest");
    }

    #[test]
    fn bare_name_with_tag() {
        let r = Reference::parse("ubuntu:24.04").unwrap();
        assert_eq!(r.repository(), "library/ubuntu");
        assert_eq!(r.tag(), Some("24.04"));
        assert_eq!(r.familiar(), "ubuntu:24.04");
    }

    #[test]
    fn user_repo_without_registry() {
        let r = Reference::parse("myuser/myrepo").unwrap();
        assert_eq!(r.registry(), "docker.io");
        assert_eq!(r.repository(), "myuser/myrepo");
        assert_eq!(r.familiar(), "myuser/myrepo:latest");
    }

    #[test]
    fn explicit_registry_with_port() {
        let r = Reference::parse("example.com:5000/foo/bar:v1").unwrap();
        assert_eq!(r.registry(), "example.com:5000");
        assert_eq!(r.registry_host(), "example.com:5000");
        assert_eq!(r.repository(), "foo/bar");
        assert_eq!(r.tag(), Some("v1"));
    }

    #[test]
    fn localhost_is_a_registry_not_a_namespace() {
        let r = Reference::parse("localhost/foo").unwrap();
        assert_eq!(r.registry(), "localhost");
        assert_eq!(r.repository(), "foo");
    }

    #[test]
    fn explicit_quay_with_digest_drops_default_tag() {
        let r = Reference::parse(
            "quay.io/foo/bar@sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        )
        .unwrap();
        assert_eq!(r.registry(), "quay.io");
        assert_eq!(r.repository(), "foo/bar");
        assert_eq!(r.tag(), None);
        assert!(r.digest().is_some());
        assert_eq!(r.manifest_ref(), r.digest().unwrap().to_string());
        assert_eq!(
            r.familiar(),
            "quay.io/foo/bar@sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn tag_and_digest_together_keeps_only_digest() {
        let r = Reference::parse(
            "docker.io/library/busybox:latest@sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        )
        .unwrap();
        assert_eq!(r.tag(), None);
        assert!(r.digest().is_some());
    }

    #[test]
    fn legacy_index_docker_io_normalizes() {
        let r = Reference::parse("index.docker.io/library/ubuntu").unwrap();
        assert_eq!(r.registry(), "docker.io");
        assert_eq!(r.repository(), "library/ubuntu");
    }

    #[test]
    fn official_repo_prefix_is_not_duplicated() {
        let r = Reference::parse("docker.io/ubuntu").unwrap();
        assert_eq!(r.repository(), "library/ubuntu");
    }

    #[test]
    fn rejects_uppercase_repository() {
        // A leading segment containing uppercase is *itself* treated as the
        // registry domain (matching upstream `distribution/reference`), so
        // the unambiguous way to trigger this is an uppercase path under an
        // already-explicit domain, or a bare uppercase name.
        assert!(matches!(
            Reference::parse("docker.io/MyRepo"),
            Err(ReferenceParseError::NotLowercase(_))
        ));
        assert!(matches!(
            Reference::parse("MyRepo"),
            Err(ReferenceParseError::NotLowercase(_))
        ));
    }

    #[test]
    fn rejects_empty() {
        assert!(matches!(
            Reference::parse(""),
            Err(ReferenceParseError::Empty)
        ));
    }

    #[test]
    fn manifest_ref_defaults_to_latest() {
        let r = Reference::parse("ubuntu").unwrap();
        assert_eq!(r.manifest_ref(), "latest");
    }
}
