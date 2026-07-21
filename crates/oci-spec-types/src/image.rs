//! OCI image-spec types: descriptors, manifests, indexes, and image config.
//!
//! Field sets follow the [image-spec `v1.1.1`][spec]. Unknown fields are
//! preserved by serde's default "ignore unknown" behavior on `deserialize`
//! (we never round-trip through a `#[serde(deny_unknown_fields)]` struct),
//! and every optional field that the spec marks optional is `Option`/
//! `#[serde(default)]` here so partial documents from lenient registries
//! still parse.
//!
//! [spec]: https://github.com/opencontainers/image-spec/blob/main/spec.md

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::digest::Digest;

/// `application/vnd.oci.image.manifest.v1+json`
pub const MEDIA_TYPE_IMAGE_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
/// `application/vnd.oci.image.index.v1+json`
pub const MEDIA_TYPE_IMAGE_INDEX: &str = "application/vnd.oci.image.index.v1+json";
/// `application/vnd.oci.image.config.v1+json`
pub const MEDIA_TYPE_IMAGE_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
/// `application/vnd.oci.image.layer.v1.tar+gzip`
pub const MEDIA_TYPE_IMAGE_LAYER_GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
/// `application/vnd.oci.image.layer.v1.tar`
pub const MEDIA_TYPE_IMAGE_LAYER: &str = "application/vnd.oci.image.layer.v1.tar";
/// `application/vnd.oci.image.layer.v1.tar+zstd`
pub const MEDIA_TYPE_IMAGE_LAYER_ZSTD: &str = "application/vnd.oci.image.layer.v1.tar+zstd";

/// `application/vnd.docker.distribution.manifest.v2+json` — the legacy
/// Docker manifest media type. Still the most common single-platform
/// manifest type in the wild; oci-tools reads it but never writes it.
pub const MEDIA_TYPE_DOCKER_MANIFEST_V2: &str =
    "application/vnd.docker.distribution.manifest.v2+json";
/// `application/vnd.docker.distribution.manifest.list.v2+json` — the legacy
/// Docker multi-platform manifest list media type.
pub const MEDIA_TYPE_DOCKER_MANIFEST_LIST: &str =
    "application/vnd.docker.distribution.manifest.list.v2+json";
/// `application/vnd.docker.container.image.v1+json` — the legacy Docker
/// image config media type.
pub const MEDIA_TYPE_DOCKER_CONFIG: &str = "application/vnd.docker.container.image.v1+json";
/// `application/vnd.docker.image.rootfs.diff.tar.gzip` — the legacy
/// Docker gzip-compressed layer media type (the layer-level analog of
/// [`MEDIA_TYPE_DOCKER_MANIFEST_V2`]; still common, since many
/// registries serve Docker-v2 manifests by default).
pub const MEDIA_TYPE_DOCKER_LAYER_GZIP: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";

/// Deserializes a JSON `null` the same as an absent field — into `T`'s
/// own [`Default`] — instead of the hard type-mismatch error serde
/// otherwise gives a non-`Option` field for an explicit `null` (only
/// a genuinely *missing* field falls back to `#[serde(default)]` on
/// its own; a field that's *present* but `null` is a different case
/// serde doesn't treat the same way). Real Docker-built image
/// configs routinely emit exactly this shape for an unset map/array
/// `ContainerConfig` field (Go's own `encoding/json` marshals a `nil`
/// map/slice as `null`, not `{}`/`[]`) — caught directly, not
/// theoretically: a real, current `docker.io/library/ubuntu:24.04`
/// pull's own config blob has a literal `"Volumes": null`, which
/// failed every `ociman` command touching that image's config
/// (`run`/`inspect`/...) with "invalid type: null, expected a map"
/// before every [`ContainerConfig`] field this affects
/// (`exposed_ports`/`volumes`/`labels`) started using this instead of
/// a bare `#[serde(default)]`.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// A content descriptor: identifies content by digest, media type, and size.
/// The unit that every manifest/index/config reference is built from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Descriptor {
    /// The MIME-style media type of the referenced content.
    #[serde(rename = "mediaType")]
    pub media_type: String,
    /// The content digest of the referenced content.
    pub digest: Digest,
    /// The byte size of the referenced content.
    pub size: u64,
    /// URLs from which the content may alternatively be fetched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
    /// Arbitrary metadata for the descriptor.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
    /// Platform this descriptor's content targets (only meaningful on
    /// entries inside an [`ImageIndex`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
}

/// A target platform: OS + architecture (+ optional variant), as used in
/// [`ImageIndex`] manifest entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Platform {
    /// GOARCH-style architecture, e.g. `amd64`, `arm64`.
    pub architecture: String,
    /// GOOS-style operating system, e.g. `linux`.
    pub os: String,
    /// CPU variant, e.g. `v8` for `arm64`, `v7` for `arm`.
    #[serde(rename = "variant", default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// OS version (rarely used outside Windows images).
    #[serde(
        rename = "os.version",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub os_version: Option<String>,
}

impl Platform {
    /// The platform oci-tools is running on (`GOARCH`/`GOOS` naming), for
    /// selecting a manifest out of an [`ImageIndex`].
    pub fn host() -> Self {
        Platform {
            architecture: host_arch().to_string(),
            os: "linux".to_string(),
            variant: host_variant().map(str::to_string),
            os_version: None,
        }
    }

    /// Whether `self` (as a selection criterion) matches a candidate
    /// platform from a manifest list. `os`/`architecture` must match
    /// exactly; `variant`, when we require one, must match too.
    pub fn matches(&self, candidate: &Platform) -> bool {
        self.os == candidate.os
            && self.architecture == candidate.architecture
            && (self.variant.is_none() || self.variant == candidate.variant)
    }
}

/// `GOARCH`-style name for the architecture we are running on.
const fn host_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "amd64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "arm") {
        "arm"
    } else if cfg!(target_arch = "x86") {
        "386"
    } else if cfg!(target_arch = "powerpc64") {
        "ppc64le"
    } else if cfg!(target_arch = "s390x") {
        "s390x"
    } else if cfg!(target_arch = "riscv64") {
        "riscv64"
    } else {
        "unknown"
    }
}

/// Default variant for the host architecture, when the ecosystem
/// conventionally tags one (only `arm64` -> `v8` in practice today).
const fn host_variant() -> Option<&'static str> {
    if cfg!(target_arch = "aarch64") {
        Some("v8")
    } else {
        None
    }
}

/// `application/vnd.oci.image.manifest.v1+json`: a single platform's
/// image (one config blob, an ordered list of layer blobs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageManifest {
    /// Manifest schema version; always `2` for both OCI and Docker v2
    /// manifests.
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// This manifest's own media type (may be absent on older Docker
    /// manifests; the registry's `Content-Type` response header is the
    /// authoritative source when this is `None`).
    #[serde(rename = "mediaType", default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Descriptor of the image config blob.
    pub config: Descriptor,
    /// Descriptors of the image's layers, in application order (bottom
    /// first).
    pub layers: Vec<Descriptor>,
    /// Arbitrary metadata for the manifest.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

/// `application/vnd.oci.image.index.v1+json`: a multi-platform "fat
/// manifest" — a list of [`Descriptor`]s pointing at per-platform
/// [`ImageManifest`]s (or nested indexes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageIndex {
    /// Manifest schema version; always `2`.
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// This index's own media type.
    #[serde(rename = "mediaType", default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// One descriptor per platform (or nested index).
    pub manifests: Vec<Descriptor>,
    /// Arbitrary metadata for the index.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

impl ImageIndex {
    /// Find the manifest descriptor matching `platform`, preferring an
    /// exact variant match when the index has one.
    pub fn select(&self, platform: &Platform) -> Option<&Descriptor> {
        self.manifests
            .iter()
            .filter(|d| {
                d.media_type == MEDIA_TYPE_IMAGE_MANIFEST
                    || d.media_type == MEDIA_TYPE_DOCKER_MANIFEST_V2
                    || d.media_type == MEDIA_TYPE_IMAGE_INDEX
                    || d.media_type == MEDIA_TYPE_DOCKER_MANIFEST_LIST
            })
            .find(|d| d.platform.as_ref().is_some_and(|p| platform.matches(p)))
    }
}

/// Either a single-platform manifest or a multi-platform index, as returned
/// by a registry's manifest GET (the media type in the `Content-Type`
/// response header disambiguates before we know which to deserialize as).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Manifest {
    /// Single-platform image manifest.
    Image(Box<ImageManifest>),
    /// Multi-platform manifest index / manifest list.
    Index(ImageIndex),
}

impl Manifest {
    /// Parse `bytes` as either an [`ImageManifest`] or [`ImageIndex`],
    /// using `content_type` (typically the registry's `Content-Type`
    /// response header, falling back to the document's own `mediaType`
    /// field) to disambiguate.
    pub fn parse(bytes: &[u8], content_type: Option<&str>) -> serde_json::Result<Self> {
        let is_index = match content_type {
            Some(MEDIA_TYPE_IMAGE_INDEX) | Some(MEDIA_TYPE_DOCKER_MANIFEST_LIST) => true,
            Some(MEDIA_TYPE_IMAGE_MANIFEST) | Some(MEDIA_TYPE_DOCKER_MANIFEST_V2) => false,
            _ => {
                // No usable Content-Type: sniff the document itself.
                let probe: serde_json::Value = serde_json::from_slice(bytes)?;
                probe.get("manifests").is_some()
            }
        };
        if is_index {
            Ok(Manifest::Index(serde_json::from_slice(bytes)?))
        } else {
            Ok(Manifest::Image(Box::new(serde_json::from_slice(bytes)?)))
        }
    }

    /// The manifest's own declared media type, if present.
    pub fn media_type(&self) -> Option<&str> {
        match self {
            Manifest::Image(m) => m.media_type.as_deref(),
            Manifest::Index(i) => i.media_type.as_deref(),
        }
    }
}

/// `application/vnd.oci.image.config.v1+json`: the image config blob
/// (`docker inspect`'s `.Config`/`.RootFS`/`.History` come from here).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageConfig {
    /// Architecture (`GOARCH`-style).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    /// Operating system (`GOOS`-style).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// RFC 3339 creation timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// Author/maintainer string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Container runtime configuration defaults (entrypoint, cmd, env, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<ContainerConfig>,
    /// Layer application order and diff-ID chain.
    pub rootfs: RootFs,
    /// Per-layer build history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryEntry>,
}

/// Runtime defaults baked into an image (the `Config` object inside
/// [`ImageConfig`]): entrypoint, cmd, env, working dir, exposed ports, etc.
///
/// **Every field here is `PascalCase` on the wire** (`Cmd`, `Entrypoint`,
/// `WorkingDir`, ...), unlike every other struct in this module — a real,
/// deliberate quirk of the image-spec (inherited from Docker's own
/// original Go struct field names, serialized with no `json` tag
/// override), not a typo to "fix" into `camelCase`. Verified against a
/// real pulled image's own config blob (`docker.io/library/busybox`, its
/// `"config"` object is exactly `{"Cmd": ["sh"], "Env": [...]}`) after
/// `ociman run` silently produced an empty command for every real image
/// — this struct had no `rename_all` at all before, so every field
/// always deserialized to its default regardless of what a real image's
/// config actually said.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerConfig {
    /// `NAME=value` environment variables to set by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// Default entrypoint (exec form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    /// Default command, appended to `entrypoint` unless overridden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<Vec<String>>,
    /// Default working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// Default user (`name`, `uid`, `name:group`, or `uid:gid`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Declared exposed ports (`"80/tcp"` -> `{}`), informational only.
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub exposed_ports: BTreeMap<String, serde_json::Value>,
    /// Declared anonymous-volume mount points.
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub volumes: BTreeMap<String, serde_json::Value>,
    /// Free-form image labels (`org.opencontainers.image.*` and others).
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub labels: BTreeMap<String, String>,
    /// `STOPSIGNAL` default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_signal: Option<String>,
    /// `HEALTHCHECK` default, if the image (or a later stage building
    /// on it) ever sets one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<HealthcheckConfig>,
}

/// A `HEALTHCHECK` instruction's own effect on the image config —
/// matches real Docker's own wire representation exactly (`Test`,
/// `Interval`/`Timeout`/`StartPeriod`/`StartInterval` as nanosecond
/// counts, `Retries`), so a real pulled image's own `Healthcheck`
/// object (or one `ociman build` writes) round-trips byte for byte.
/// `0` means "not set"/"inherit" for every numeric field here — the
/// same convention real Docker's own `HealthcheckConfig` uses (a
/// `HEALTHCHECK` instruction that never mentions `--interval=`, for
/// instance, leaves it at its zero value, not some other sentinel).
///
/// **Executing a healthcheck periodically is out of scope for this
/// project so far** — matches this project's own already-established
/// "narrow first increment" pattern (e.g. `ociman top`'s own
/// deliberately-narrower-than-real-podman scope): this struct is only
/// ever parsed (`oci-dockerfile`'s own `HealthcheckCommand`), stored,
/// and round-tripped (`ociman inspect`/`ociman history`/a later
/// `FROM`), never actually run against a live container.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct HealthcheckConfig {
    /// `["NONE"]` (explicitly disables any inherited healthcheck),
    /// `["CMD", ...]` (exec form), or `["CMD-SHELL", "<command>"]`
    /// (shell form) — matches real Docker's own `Test` field exactly,
    /// the same three shapes `parse_healthcheck` already produces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test: Vec<String>,
    /// Nanoseconds between checks; `0` means inherit/unset.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub interval: i64,
    /// Nanoseconds before a single check is considered hung; `0`
    /// means inherit/unset.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub timeout: i64,
    /// Nanoseconds the container gets to initialize before failures
    /// count against `retries`; `0` means inherit/unset.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub start_period: i64,
    /// Nanoseconds between checks during the start period; `0` means
    /// inherit/unset.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub start_interval: i64,
    /// Consecutive failures needed to consider the container
    /// unhealthy; `0` means inherit/unset.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub retries: i64,
}

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

/// The `rootfs` object: how to reconstruct the container filesystem from
/// layers (`diff_ids`, bottom layer first, are digests of the
/// *uncompressed* layer content, distinct from the compressed blob digests
/// in the manifest).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootFs {
    /// Always `"layers"` today; the spec reserves the field for future
    /// rootfs construction schemes.
    #[serde(rename = "type")]
    pub kind: String,
    /// Uncompressed-layer digests, bottom first.
    pub diff_ids: Vec<Digest>,
}

/// One entry in the image config's build history.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// RFC 3339 timestamp this layer/no-op instruction was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// Free-form description of the build instruction that produced this
    /// entry (`created_by` in the spec).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// Author override for this entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Free-text comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// `true` when this history entry corresponds to no layer in `rootfs`
    /// (e.g. `ENV`, `LABEL`, `CMD`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub empty_layer: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_descriptor() -> Descriptor {
        Descriptor {
            media_type: MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
            digest: crate::digest::sha256(b"layer"),
            size: 1234,
            urls: vec![],
            annotations: BTreeMap::new(),
            platform: None,
        }
    }

    #[test]
    fn descriptor_round_trips() {
        let d = sample_descriptor();
        let json = serde_json::to_string(&d).unwrap();
        let back: Descriptor = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
        // camelCase on the wire, not snake_case.
        assert!(json.contains("\"mediaType\""));
    }

    #[test]
    fn parses_real_oci_manifest() {
        let raw = r#"{
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "size": 0
            },
            "layers": [
                {
                    "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                    "digest": "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
                    "size": 100
                }
            ]
        }"#;
        let manifest = Manifest::parse(raw.as_bytes(), Some(MEDIA_TYPE_IMAGE_MANIFEST)).unwrap();
        let Manifest::Image(m) = manifest else {
            panic!("expected an image manifest");
        };
        assert_eq!(m.layers.len(), 1);
        assert_eq!(m.config.size, 0);
    }

    #[test]
    fn parses_real_busybox_image_config_including_pascal_case_container_config() {
        // Captured verbatim from `docker.io/library/busybox`'s real
        // config blob (`skopeo`/`podman pull`, then `podman inspect`'s
        // own digest matched this exact blob) — not hand-written.
        // `config.Cmd`/`config.Env` are `PascalCase` in the raw JSON,
        // exactly the quirk `ContainerConfig`'s own doc comment
        // describes; before it had a `rename_all`, this parsed as an
        // entirely empty `ContainerConfig` regardless of what the real
        // image said, which `ociman run` (0020) caught by actually
        // running a real image, not by inspecting this JSON by eye.
        let raw = include_str!("../tests/fixtures/busybox-image-config.json");
        let config: ImageConfig = serde_json::from_str(raw).unwrap();
        let container_config = config.config.expect("busybox sets a config object");
        assert_eq!(container_config.cmd, Some(vec!["sh".to_string()]));
        assert_eq!(
            container_config.env,
            vec!["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string()]
        );
        assert_eq!(container_config.entrypoint, None);
        assert_eq!(config.architecture.as_deref(), Some("arm64"));
        assert_eq!(config.rootfs.diff_ids.len(), 1);
    }

    #[test]
    fn parses_real_ubuntu_image_config_with_an_explicit_null_volumes_field() {
        // Captured verbatim from a real `docker.io/library/ubuntu:
        // 24.04` pull's own config blob — not hand-written. Its own
        // `config.Volumes` is a literal JSON `null` (Go's own
        // `encoding/json` marshals a `nil` map as `null`, not `{}`),
        // which every `ociman` command touching this image's config
        // (`run`/`inspect`/...) failed on with "invalid type: null,
        // expected a map" before `exposed_ports`/`volumes`/`labels`
        // started using `null_as_default` instead of a bare
        // `#[serde(default)]` (which only ever covers a field missing
        // entirely, not one present-but-`null`) — a real, current
        // compatibility gap against one of the most common base
        // images on Docker Hub, not a hypothetical edge case.
        let raw = include_str!("../tests/fixtures/ubuntu-24.04-image-config.json");
        let config: ImageConfig = serde_json::from_str(raw).unwrap();
        let container_config = config.config.expect("ubuntu sets a config object");
        assert_eq!(container_config.cmd, Some(vec!["/bin/bash".to_string()]));
        assert!(container_config.volumes.is_empty());
        assert!(container_config.entrypoint.is_none());
        assert_eq!(
            container_config
                .labels
                .get("org.opencontainers.image.version"),
            Some(&"24.04".to_string())
        );
    }

    #[test]
    fn sniffs_index_without_content_type() {
        let raw = r#"{
            "schemaVersion": 2,
            "manifests": []
        }"#;
        let manifest = Manifest::parse(raw.as_bytes(), None).unwrap();
        assert!(matches!(manifest, Manifest::Index(_)));
    }

    #[test]
    fn selects_matching_platform_from_index() {
        let mut index = ImageIndex {
            schema_version: 2,
            media_type: Some(MEDIA_TYPE_IMAGE_INDEX.to_string()),
            manifests: vec![],
            annotations: BTreeMap::new(),
        };
        let mut amd64 = sample_descriptor();
        amd64.media_type = MEDIA_TYPE_IMAGE_MANIFEST.to_string();
        amd64.platform = Some(Platform {
            architecture: "amd64".to_string(),
            os: "linux".to_string(),
            variant: None,
            os_version: None,
        });
        let mut arm64 = sample_descriptor();
        arm64.media_type = MEDIA_TYPE_IMAGE_MANIFEST.to_string();
        arm64.digest = crate::digest::sha256(b"arm64-layer");
        arm64.platform = Some(Platform {
            architecture: "arm64".to_string(),
            os: "linux".to_string(),
            variant: Some("v8".to_string()),
            os_version: None,
        });
        index.manifests.push(amd64.clone());
        index.manifests.push(arm64.clone());

        let want = Platform {
            architecture: "arm64".to_string(),
            os: "linux".to_string(),
            variant: Some("v8".to_string()),
            os_version: None,
        };
        assert_eq!(index.select(&want), Some(&arm64));

        let want_amd64 = Platform {
            architecture: "amd64".to_string(),
            os: "linux".to_string(),
            variant: None,
            os_version: None,
        };
        assert_eq!(index.select(&want_amd64), Some(&amd64));
    }

    #[test]
    fn image_config_round_trips_with_defaults() {
        let raw = r#"{
            "architecture": "arm64",
            "os": "linux",
            "rootfs": { "type": "layers", "diff_ids": [] }
        }"#;
        let cfg: ImageConfig = serde_json::from_str(raw).unwrap();
        assert_eq!(cfg.rootfs.kind, "layers");
        assert!(cfg.config.is_none());
        assert!(cfg.history.is_empty());
    }
}
