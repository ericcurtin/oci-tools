//! `ociman save`: writing an already-stored image out as a real,
//! self-contained archive file — matching real `podman save`/`docker
//! save`'s own `oci-archive:` transport, checked directly against
//! `go.podman.io/image/v5/oci/archive/oci_dest.go` +
//! `go.podman.io/image/v5/oci/layout/oci_dest.go` (not guessed at):
//!
//! ```text
//! oci-layout                    {"imageLayoutVersion":"1.0.0"}
//! index.json                    an OCI image index, one manifest entry
//! blobs/sha256/<hex>             every blob (manifest, config, each
//!                                layer), content-addressed, verbatim
//! ```
//!
//! Every blob this project's own [`Store`] holds is *already* laid out
//! exactly this way on disk (`blobs/sha256/<hex>`, no re-encoding), so
//! `oci-archive` output is close to a direct copy: only `oci-layout`
//! and `index.json` are synthesized, and every blob is streamed
//! through unchanged (whatever compression it already has, gzip in
//! every real case this project produces or pulls).
//!
//! # `docker-archive` (real `podman save`'s own default format)
//!
//! See [`save_docker_archive`]'s own doc comment for the exact format
//! (`manifest.json` + flat, decompressed layer/config files) and what
//! remains deliberately out of scope (a legacy `repositories` file and
//! per-layer legacy-chain-ID subdirectories, neither of which real
//! `docker load`'s own loader path actually reads — see `docs/design/
//! 0167`). This is `ociman save`'s own default format, matching real
//! `podman save`/`docker save`'s own default exactly.

use std::collections::BTreeMap;
use std::io::{Read, Seek, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use oci_spec_types::Digest;
use oci_spec_types::Reference;
use oci_spec_types::image::{Descriptor, ImageIndex, ImageManifest, MEDIA_TYPE_IMAGE_MANIFEST};
use oci_store::{ImageRecord, Store};

const ANNOTATION_REF_NAME: &str = "org.opencontainers.image.ref.name";
/// A real, non-OCI-spec annotation `buildkit`/`containerd` (and
/// therefore modern `docker save`, which is `buildkit`-based) set to
/// the *full* reference (`docker.io/library/busybox:latest`), unlike
/// [`ANNOTATION_REF_NAME`] itself, which the OCI image-spec only ever
/// defines as "the name of the reference" — real modern `docker save`
/// sets it to just the bare tag (`latest`), not a full reference at
/// all (confirmed directly: `docker save`'s own real `index.json`
/// output; `podman save`, by contrast, puts the *full* reference under
/// [`ANNOTATION_REF_NAME`] itself). [`load_archive`] checks this one
/// first, exactly matching real podman's own identical precedence —
/// checked directly against `~/git/container-libs/common/libimage/
/// pull.go`'s own `nameFromAnnotations`, itself citing a real upstream
/// bug this exact mismatch caused (`containers/podman/issues/12560`).
const ANNOTATION_CONTAINERD_IMAGE_NAME: &str = "io.containerd.image.name";
const OCI_LAYOUT_VERSION: &str = "1.0.0";

/// Write `record`'s image (manifest, config, every layer) to `writer`
/// as a real `oci-archive:`-format tar, per this module's own doc
/// comment.
pub(crate) fn save_oci_archive(
    store: &Store,
    record: &ImageRecord,
    writer: impl Write,
) -> anyhow::Result<()> {
    let manifest_bytes = store
        .read_blob(&record.manifest_digest)
        .with_context(|| format!("reading manifest blob {}", record.manifest_digest))?;
    let manifest: ImageManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parsing manifest blob {}", record.manifest_digest))?;

    let mtime = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut builder = tar::Builder::new(writer);

    append_dir(&mut builder, "blobs", mtime)?;
    append_dir(&mut builder, "blobs/sha256", mtime)?;
    append_regular(
        &mut builder,
        "oci-layout",
        mtime,
        0o644,
        br#"{"imageLayoutVersion":"1.0.0"}"#,
    )?;

    let mut written = std::collections::HashSet::new();
    append_blob_bytes(
        &mut builder,
        &record.manifest_digest,
        mtime,
        &manifest_bytes,
        &mut written,
    )?;
    append_blob_from_store(
        store,
        &mut builder,
        &manifest.config.digest,
        mtime,
        &mut written,
    )
    .with_context(|| format!("writing config blob {}", manifest.config.digest))?;
    for layer in &manifest.layers {
        append_blob_from_store(store, &mut builder, &layer.digest, mtime, &mut written)
            .with_context(|| format!("writing layer blob {}", layer.digest))?;
    }

    let mut annotations = BTreeMap::new();
    annotations.insert(ANNOTATION_REF_NAME.to_string(), record.reference.clone());
    let index = ImageIndex {
        schema_version: 2,
        // Real `oci-archive` output has no top-level `mediaType` on
        // the index itself (checked directly against a real `podman
        // save --format oci-archive`'s own `index.json`) — only the
        // one manifest entry inside `manifests` carries a media type.
        media_type: None,
        manifests: vec![Descriptor {
            media_type: MEDIA_TYPE_IMAGE_MANIFEST.to_string(),
            digest: record.manifest_digest.clone(),
            size: manifest_bytes.len() as u64,
            urls: Vec::new(),
            annotations,
            platform: None,
        }],
        annotations: BTreeMap::new(),
    };
    let index_bytes =
        serde_json::to_vec(&index).context("serializing index.json for oci-archive")?;
    append_regular(&mut builder, "index.json", mtime, 0o644, &index_bytes)?;

    let mut writer = builder.into_inner().context("finishing oci-archive tar")?;
    writer.flush().context("flushing oci-archive tar")
}

/// `docker-archive`'s own single top-level `manifest.json` entry —
/// checked directly against `go.podman.io/image/v5/docker/internal/
/// tarfile/types.go`'s own `ManifestItem`: `Config`/`RepoTags`/
/// `Layers` are the load-critical fields real `docker load`/`podman
/// load`'s own `ChooseManifestItem` actually reads (`reader.go`);
/// `Parent`/`LayerSources` (both `omitempty` there) are never written
/// here at all, matching what an image saved standalone (not as part
/// of a build's own parent chain) already looks like from real
/// `podman save` too. Also the type [`load_archive`] deserializes a
/// real `manifest.json`'s own entries into on the read side — real
/// `podman load` also tolerates a missing/absent `RepoTags` (an
/// untagged/by-digest save), so `#[serde(default)]` there rather than
/// a hard parse failure.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct DockerArchiveManifestItem {
    #[serde(rename = "Config")]
    config: String,
    #[serde(rename = "RepoTags", default)]
    repo_tags: Vec<String>,
    #[serde(rename = "Layers")]
    layers: Vec<String>,
}

/// Write `record`'s image to `writer` as a real `docker-archive:`-
/// format tar — checked directly against `go.podman.io/image/v5/
/// docker/internal/tarfile/writer.go`+`dest.go`:
///
/// ```text
/// manifest.json                 [ {"Config": "<hex>.json", "RepoTags": [...], "Layers": [...]} ]
/// <config-digest-hex>.json      the image config blob, verbatim (unchanged: the OCI and Docker
///                                config schemas are the same wire format for this blob)
/// <layer-digest-hex>.tar        each layer, DECOMPRESSED (unlike oci-archive, this format wants
///                                plain tar), named by its own real uncompressed digest
/// ```
///
/// Deliberately narrower than real `podman save --format docker-
/// archive`'s own output: no `repositories` file and no per-layer
/// legacy-chain-ID subdirectories (`<id>/VERSION`, `<id>/json`,
/// `<id>/layer.tar`) — checked directly, real `docker load`'s own
/// `ChooseManifestItem` (`docker/internal/tarfile/reader.go`) never
/// reads either of those at all, only `manifest.json` and the flat
/// files it names, so this is a real, load-critical-only subset, not
/// a partial/broken implementation (see this module's own top-level
/// doc comment for the rest of what's deferred).
pub(crate) fn save_docker_archive(
    store: &Store,
    record: &ImageRecord,
    writer: impl Write,
) -> anyhow::Result<()> {
    let manifest_bytes = store
        .read_blob(&record.manifest_digest)
        .with_context(|| format!("reading manifest blob {}", record.manifest_digest))?;
    let manifest: ImageManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parsing manifest blob {}", record.manifest_digest))?;
    let config_bytes = store
        .read_blob(&manifest.config.digest)
        .with_context(|| format!("reading config blob {}", manifest.config.digest))?;

    // Real `docker save`'s own tar entries all carry a fixed epoch
    // mtime and read-only mode (checked directly: `tar -tvf` on a
    // real `podman save`-produced `docker-archive` shows every entry
    // as `-r--r--r-- 0/0 ... 1970-01-01`) -- matched here too, rather
    // than `save_oci_archive`'s own current-time/0644 choice (real
    // `oci-archive` output uses those instead — the two real formats
    // genuinely differ here, not a project-specific inconsistency).
    const MTIME: u64 = 0;
    const MODE: u32 = 0o444;

    let mut builder = tar::Builder::new(writer);

    let config_name = format!("{}.json", manifest.config.digest.hex());
    append_regular(&mut builder, &config_name, MTIME, MODE, &config_bytes)?;

    let mut layer_names = Vec::with_capacity(manifest.layers.len());
    for layer in &manifest.layers {
        let name = append_layer_decompressed(store, &mut builder, layer, MTIME, MODE)
            .with_context(|| format!("writing layer {}", layer.digest))?;
        layer_names.push(name);
    }

    let manifest_item = DockerArchiveManifestItem {
        config: config_name,
        repo_tags: vec![record.reference.clone()],
        layers: layer_names,
    };
    let manifest_json = serde_json::to_vec(&vec![manifest_item])
        .context("serializing manifest.json for docker-archive")?;
    append_regular(&mut builder, "manifest.json", MTIME, MODE, &manifest_json)?;

    let mut writer = builder
        .into_inner()
        .context("finishing docker-archive tar")?;
    writer.flush().context("flushing docker-archive tar")
}

/// Decompress one already-stored layer blob into a real scratch file
/// (so its true size is known before the tar entry's own header is
/// written, and so a large layer is never held fully in memory),
/// computing its own real uncompressed digest as it goes (never
/// trusting the config's own `rootfs.diff_ids` blindly — see
/// [`oci_layer::decompress_verifying`]'s own doc comment), then
/// streams that scratch file into the archive as `<digest-hex>.tar`.
/// Returns the filename written, for `manifest.json`'s own `Layers`
/// list.
fn append_layer_decompressed(
    store: &Store,
    builder: &mut tar::Builder<impl Write>,
    layer: &Descriptor,
    mtime: u64,
    mode: u32,
) -> anyhow::Result<String> {
    let compression =
        oci_layer::compression_for_media_type(&layer.media_type).with_context(|| {
            format!(
                "layer has an unrecognized media type {:?}",
                layer.media_type
            )
        })?;
    let source = store
        .open_blob(&layer.digest)
        .with_context(|| format!("opening blob {}", layer.digest))?;

    let mut scratch =
        tempfile::NamedTempFile::new().context("creating a scratch file to decompress a layer")?;
    let digest = oci_layer::decompress_verifying(source, compression, scratch.as_file_mut())
        .context("decompressing layer")?;
    let size = scratch
        .as_file()
        .metadata()
        .context("statting decompressed layer scratch file")?
        .len();
    scratch
        .as_file_mut()
        .seek(std::io::SeekFrom::Start(0))
        .context("rewinding decompressed layer scratch file")?;

    let name = format!("{}.tar", digest.hex());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(mode);
    header.set_size(size);
    header.set_mtime(mtime);
    header.set_uid(0);
    header.set_gid(0);
    builder
        .append_data(&mut header, &name, scratch.as_file_mut())
        .with_context(|| format!("writing layer entry {name:?}"))?;
    Ok(name)
}

fn append_dir(
    builder: &mut tar::Builder<impl Write>,
    path: &str,
    mtime: u64,
) -> anyhow::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_mode(0o755);
    header.set_size(0);
    header.set_mtime(mtime);
    header.set_uid(0);
    header.set_gid(0);
    builder
        .append_data(&mut header, path, std::io::empty())
        .with_context(|| format!("writing directory entry {path:?}"))
}

fn append_regular(
    builder: &mut tar::Builder<impl Write>,
    path: &str,
    mtime: u64,
    mode: u32,
    content: &[u8],
) -> anyhow::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(mode);
    header.set_size(content.len() as u64);
    header.set_mtime(mtime);
    header.set_uid(0);
    header.set_gid(0);
    builder
        .append_data(&mut header, path, content)
        .with_context(|| format!("writing file entry {path:?}"))
}

/// Write one already-in-memory blob (the manifest itself, read once by
/// the caller already) under `blobs/sha256/<hex>`, skipping it if
/// `written` already has that digest (defensive dedup — never
/// exercised by a real single-platform image today, but cheap to get
/// right).
fn append_blob_bytes(
    builder: &mut tar::Builder<impl Write>,
    digest: &Digest,
    mtime: u64,
    content: &[u8],
    written: &mut std::collections::HashSet<String>,
) -> anyhow::Result<()> {
    if !written.insert(digest.hex().to_string()) {
        return Ok(());
    }
    append_regular(
        builder,
        &format!("blobs/sha256/{}", digest.hex()),
        mtime,
        0o644,
        content,
    )
}

/// Stream an already-stored blob (a config or layer, both potentially
/// too large to read fully into memory) straight from the store into
/// the archive, verbatim — no re-encoding, matching this module's own
/// doc comment.
fn append_blob_from_store(
    store: &Store,
    builder: &mut tar::Builder<impl Write>,
    digest: &Digest,
    mtime: u64,
    written: &mut std::collections::HashSet<String>,
) -> anyhow::Result<()> {
    if !written.insert(digest.hex().to_string()) {
        return Ok(());
    }
    let size = store
        .blob_size(digest)
        .with_context(|| format!("statting blob {digest}"))?;
    let file = store
        .open_blob(digest)
        .with_context(|| format!("opening blob {digest}"))?;

    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o644);
    header.set_size(size);
    header.set_mtime(mtime);
    header.set_uid(0);
    header.set_gid(0);
    let path = format!("blobs/sha256/{}", digest.hex());
    builder
        .append_data(&mut header, &path, file)
        .with_context(|| format!("writing blob entry {path:?}"))
}

/// What [`load_archive`] actually did — mirrors real `podman load`'s
/// own "Loaded image: ..." line(s): zero references for an untagged/
/// by-digest-only archive (this project's own [`Store`] happily holds
/// a manifest with no reference pointing at it at all, so this is a
/// real, supported case, not an error), one for the overwhelmingly
/// common case, or more than one for a real `docker-archive` whose own
/// `manifest.json` named several `RepoTags` for the same image (real
/// `docker load`'s own identical "tag it under every one of them"
/// behavior — an `oci-archive`'s own `index.json` can only ever name
/// at most one, since [`save_oci_archive`] never writes more than one
/// `org.opencontainers.image.ref.name` annotation).
#[derive(Debug)]
pub(crate) struct LoadedImage {
    pub(crate) references: Vec<String>,
    pub(crate) manifest_digest: Digest,
}

/// Read a real archive from `reader` (a real file or standard input,
/// either way just anything [`Read`]) and load the image it contains
/// into `store` — auto-detecting the format, matching real `podman
/// load`/`docker load`'s own identical auto-detection (no `--format`
/// flag on load, only on save): the presence of `index.json` means
/// `oci-archive`; the presence of `manifest.json` (with no
/// `index.json`) means `docker-archive`. Neither present at all is a
/// clear, named error.
///
/// A single linear pass over the tar stream regardless of which
/// format it turns out to be (never needs to seek, so this works
/// directly against standard input too, unlike real `containers/
/// image`'s own `oci-archive` reader, which extracts to a temp
/// directory first): every blob-shaped entry (`blobs/sha256/<hex>`
/// for `oci-archive`; a top-level `<hex>.json`/`<hex>.tar` for
/// `docker-archive`) is ingested as it's encountered; the small,
/// format-defining files (`index.json`/`oci-layout`/`manifest.json`)
/// are buffered in memory and only interpreted once the whole stream
/// has been consumed, so entry order within the archive never
/// matters.
///
/// For `oci-archive`, every blob is ingested verbatim, verified
/// against the exact digest its own filename claims (the same defense
/// a registry pull already applies via [`Store::ingest_verified`] — a
/// malicious or corrupt archive can never poison local storage with
/// content under the wrong digest). For `docker-archive`, each
/// top-level `<hex>.tar` is a **plain, uncompressed** layer (the
/// format's own convention — see [`save_docker_archive`]'s own doc
/// comment): it's gzip-compressed while streaming straight into the
/// store via the same [`oci_layer::compress_for_storage`] `ociman
/// build`/`commit` already use, which also yields that layer's own
/// real, independently-computed uncompressed digest (the `diff_id`) —
/// cross-checked against the config's own `rootfs.diff_ids` afterward
/// (never assumed to already match just because the archive claims
/// so), and a fresh, real OCI [`ImageManifest`] is synthesized to wrap
/// the (unchanged) config blob and the freshly re-compressed layers,
/// since `docker-archive` itself never stores a manifest blob at all,
/// only `manifest.json`'s own flatter `Config`/`RepoTags`/`Layers`
/// description of one.
///
/// Only ever accepts a single-manifest archive for either format —
/// matching the only shape [`save_oci_archive`]/[`save_docker_archive`]
/// themselves ever produce; more than one manifest (a real multi-
/// platform/multi-image archive saved by some other tool, or real
/// `podman save -m`) is a clear, named error rather than a silent
/// "picks whichever one" guess.
pub(crate) fn load_archive(store: &Store, reader: impl Read) -> anyhow::Result<LoadedImage> {
    let mut archive = tar::Archive::new(reader);
    let mut index_bytes: Option<Vec<u8>> = None;
    let mut oci_layout_bytes: Option<Vec<u8>> = None;
    let mut docker_manifest_bytes: Option<Vec<u8>> = None;
    // docker-archive: a top-level `<hex>.json` (real digest of its own
    // content) -> that digest, once ingested; a top-level `<hex>.tar`
    // (the *name* `manifest.json` itself claims, not necessarily a
    // real digest of anything -- verified independently below) -> the
    // freshly gzip-compressed layer's own real store descriptor plus
    // its own real, independently-computed uncompressed `diff_id`.
    let mut docker_configs: std::collections::HashMap<String, Digest> =
        std::collections::HashMap::new();
    let mut docker_layers: std::collections::HashMap<String, (Descriptor, Digest)> =
        std::collections::HashMap::new();

    for entry in archive.entries().context("reading archive")? {
        let mut entry = entry.context("reading archive entry")?;
        if entry.header().entry_type() == tar::EntryType::Directory {
            continue;
        }
        let path = entry
            .path()
            .context("reading archive entry path")?
            .to_string_lossy()
            .into_owned();

        if let Some(hex) = path.strip_prefix("blobs/sha256/") {
            if hex.is_empty() || hex.contains('/') {
                anyhow::bail!("archive entry {path:?} is not a flat blob file");
            }
            let expected = Digest::parse(&format!("sha256:{hex}")).with_context(|| {
                format!("archive entry {path:?} is not named after a valid sha256 digest")
            })?;
            store
                .ingest_verified(&mut entry, &expected)
                .with_context(|| format!("ingesting blob {path}"))?;
        } else if path == "index.json" {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).context("reading index.json")?;
            index_bytes = Some(buf);
        } else if path == "oci-layout" {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).context("reading oci-layout")?;
            oci_layout_bytes = Some(buf);
        } else if path == "manifest.json" {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .context("reading manifest.json")?;
            docker_manifest_bytes = Some(buf);
        } else if !path.contains('/') && path.ends_with(".json") {
            // A docker-archive config blob: real content, ingested
            // (and digested) exactly as given -- never assumed to
            // already equal its own filename's claimed hex.
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .with_context(|| format!("reading {path}"))?;
            let ingested = store
                .ingest(&buf[..])
                .with_context(|| format!("ingesting config blob {path}"))?;
            docker_configs.insert(path, ingested.digest);
        } else if !path.contains('/') && path.ends_with(".tar") {
            let (descriptor, diff_id) = ingest_docker_archive_layer(store, &mut entry)
                .with_context(|| format!("ingesting layer {path}"))?;
            docker_layers.insert(path, (descriptor, diff_id));
        }
        // Anything else (an unrecognized top-level entry, a legacy
        // `repositories` file, a legacy per-layer subdirectory's own
        // `VERSION`/`json`/`layer.tar` -- see `save_docker_archive`'s
        // own doc comment for why those are never written, but a real
        // archive from some *other* tool might still have them) is
        // ignored rather than rejected outright -- forward-compatible,
        // and never load-critical either way (checked directly: real
        // `docker load` doesn't read them either).
    }

    if let Some(index_bytes) = index_bytes {
        load_oci_archive_index(store, index_bytes, oci_layout_bytes)
    } else if let Some(docker_manifest_bytes) = docker_manifest_bytes {
        load_docker_archive_manifest(store, docker_manifest_bytes, docker_configs, docker_layers)
    } else {
        anyhow::bail!(
            "not a valid archive: missing both index.json (oci-archive) and manifest.json \
             (docker-archive)"
        )
    }
}

/// Finish an `oci-archive` load: interpret the already-buffered
/// `index.json`/`oci-layout` bytes now that every `blobs/sha256/<hex>`
/// entry has already been ingested. Split out of [`load_archive`]
/// purely so that function's own body reads as "accumulate, then
/// decide" rather than one long branch.
fn load_oci_archive_index(
    store: &Store,
    index_bytes: Vec<u8>,
    oci_layout_bytes: Option<Vec<u8>>,
) -> anyhow::Result<LoadedImage> {
    let oci_layout_bytes =
        oci_layout_bytes.context("not a valid oci-archive: missing the oci-layout marker file")?;
    let oci_layout: serde_json::Value =
        serde_json::from_slice(&oci_layout_bytes).context("parsing oci-layout")?;
    let version = oci_layout
        .get("imageLayoutVersion")
        .and_then(serde_json::Value::as_str);
    if version != Some(OCI_LAYOUT_VERSION) {
        anyhow::bail!(
            "unsupported oci-layout imageLayoutVersion {version:?}, expected {OCI_LAYOUT_VERSION:?}"
        );
    }

    let index: ImageIndex = serde_json::from_slice(&index_bytes).context("parsing index.json")?;
    match index.manifests.len() {
        0 => anyhow::bail!("index.json names no manifests at all"),
        1 => {}
        n => anyhow::bail!(
            "index.json names {n} manifests -- multi-manifest (multi-platform) archives are \
             not supported yet, only a single-platform archive"
        ),
    }
    let descriptor = &index.manifests[0];
    if descriptor.media_type != MEDIA_TYPE_IMAGE_MANIFEST {
        let media_type = &descriptor.media_type;
        anyhow::bail!(
            "index.json's one manifest entry has media type {media_type:?}, expected a \
             single-platform image manifest ({MEDIA_TYPE_IMAGE_MANIFEST:?}) -- a manifest \
             list/image index entry is not supported yet"
        );
    }
    if !store.has_blob(&descriptor.digest) {
        anyhow::bail!(
            "index.json names manifest {} but the archive never included that blob",
            descriptor.digest
        );
    }

    let manifest_bytes = store
        .read_blob(&descriptor.digest)
        .with_context(|| format!("reading manifest blob {}", descriptor.digest))?;
    let manifest: ImageManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parsing manifest blob {}", descriptor.digest))?;
    if !store.has_blob(&manifest.config.digest) {
        anyhow::bail!(
            "manifest names config blob {} but the archive never included it",
            manifest.config.digest
        );
    }
    for layer in &manifest.layers {
        if !store.has_blob(&layer.digest) {
            anyhow::bail!(
                "manifest names layer blob {} but the archive never included it",
                layer.digest
            );
        }
    }

    // `io.containerd.image.name` (the real, non-spec, full-reference
    // annotation `buildkit`/modern `docker save` actually sets) takes
    // priority over the OCI spec's own `org.opencontainers.image.
    // ref.name` -- see `ANNOTATION_CONTAINERD_IMAGE_NAME`'s own doc
    // comment for exactly why, matching real podman's own identical
    // precedence.
    let raw_reference = descriptor
        .annotations
        .get(ANNOTATION_CONTAINERD_IMAGE_NAME)
        .or_else(|| descriptor.annotations.get(ANNOTATION_REF_NAME));
    let references = match raw_reference {
        Some(raw) => {
            let parsed = Reference::parse(raw)
                .with_context(|| format!("parsing image reference annotation {raw:?}"))?;
            let normalized = parsed.to_string();
            store
                .put_image(&ImageRecord {
                    reference: normalized.clone(),
                    manifest_digest: descriptor.digest.clone(),
                })
                .context("recording loaded image's tag")?;
            vec![normalized]
        }
        None => Vec::new(),
    };

    Ok(LoadedImage {
        references,
        manifest_digest: descriptor.digest.clone(),
    })
}

/// Finish a `docker-archive` load: interpret the already-buffered
/// `manifest.json` bytes now that every config/layer file has already
/// been ingested (`docker_configs`/`docker_layers`, keyed by their own
/// real archive filename). Synthesizes a fresh, real OCI
/// [`ImageManifest`] wrapping the unchanged config and the freshly
/// re-compressed layers, since `docker-archive` never stores a
/// manifest blob of its own at all.
fn load_docker_archive_manifest(
    store: &Store,
    docker_manifest_bytes: Vec<u8>,
    docker_configs: std::collections::HashMap<String, Digest>,
    docker_layers: std::collections::HashMap<String, (Descriptor, Digest)>,
) -> anyhow::Result<LoadedImage> {
    let items: Vec<DockerArchiveManifestItem> =
        serde_json::from_slice(&docker_manifest_bytes).context("parsing manifest.json")?;
    match items.len() {
        0 => anyhow::bail!("manifest.json names no images at all"),
        1 => {}
        n => anyhow::bail!(
            "manifest.json names {n} images -- multi-image docker-archive archives are not \
             supported yet, only a single image"
        ),
    }
    let item = &items[0];

    let config_digest = docker_configs.get(&item.config).with_context(|| {
        format!(
            "manifest.json names config {:?} but the archive never included it",
            item.config
        )
    })?;
    let config_bytes = store
        .read_blob(config_digest)
        .with_context(|| format!("reading config blob {config_digest}"))?;
    let config: oci_spec_types::image::ImageConfig =
        serde_json::from_slice(&config_bytes).context("parsing config blob")?;

    let mut layers = Vec::with_capacity(item.layers.len());
    let mut diff_ids = Vec::with_capacity(item.layers.len());
    for name in &item.layers {
        let (descriptor, diff_id) = docker_layers.get(name).with_context(|| {
            format!("manifest.json names layer {name:?} but the archive never included it")
        })?;
        layers.push(descriptor.clone());
        diff_ids.push(diff_id.clone());
    }
    if config.rootfs.diff_ids != diff_ids {
        anyhow::bail!(
            "the config's own rootfs.diff_ids does not match this archive's own layers' real, \
             independently-computed uncompressed digests -- refusing to load a manifest that \
             would not describe what's actually in the archive"
        );
    }

    let manifest = ImageManifest {
        schema_version: 2,
        media_type: Some(MEDIA_TYPE_IMAGE_MANIFEST.to_string()),
        config: Descriptor {
            media_type: oci_spec_types::image::MEDIA_TYPE_IMAGE_CONFIG.to_string(),
            digest: config_digest.clone(),
            size: config_bytes.len() as u64,
            urls: Vec::new(),
            annotations: BTreeMap::new(),
            platform: None,
        },
        layers,
        annotations: BTreeMap::new(),
    };
    let manifest_bytes =
        serde_json::to_vec(&manifest).context("serializing a fresh manifest for this image")?;
    let manifest_digest = store
        .ingest(&manifest_bytes[..])
        .context("ingesting a fresh manifest for this image")?
        .digest;

    let mut references = Vec::with_capacity(item.repo_tags.len());
    for tag in &item.repo_tags {
        let parsed =
            Reference::parse(tag).with_context(|| format!("parsing RepoTags entry {tag:?}"))?;
        let normalized = parsed.to_string();
        store
            .put_image(&ImageRecord {
                reference: normalized.clone(),
                manifest_digest: manifest_digest.clone(),
            })
            .context("recording loaded image's tag")?;
        references.push(normalized);
    }

    Ok(LoadedImage {
        references,
        manifest_digest,
    })
}

/// Gzip-compress a `docker-archive` layer entry (a plain, uncompressed
/// tar) while streaming it straight into the store, via a real scratch
/// file (never held fully in memory) — the read-side mirror of
/// [`append_layer_decompressed`]. Returns the freshly stored layer's
/// own real descriptor and its own real, independently-computed
/// uncompressed digest (the `diff_id`).
fn ingest_docker_archive_layer(
    store: &Store,
    entry: &mut (impl Read + ?Sized),
) -> anyhow::Result<(Descriptor, Digest)> {
    let mut scratch =
        tempfile::NamedTempFile::new().context("creating a scratch file to compress a layer")?;
    let diff_id = oci_layer::compress_for_storage(entry, scratch.as_file_mut())
        .context("compressing layer")?;
    scratch
        .as_file_mut()
        .seek(std::io::SeekFrom::Start(0))
        .context("rewinding compressed layer scratch file")?;
    let ingested = store
        .ingest(scratch.as_file_mut())
        .context("ingesting compressed layer")?;
    let descriptor = Descriptor {
        media_type: oci_spec_types::image::MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
        digest: ingested.digest,
        size: ingested.size,
        urls: Vec::new(),
        annotations: BTreeMap::new(),
        platform: None,
    };
    Ok((descriptor, diff_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_spec_types::image::{ContainerConfig, ImageConfig, MEDIA_TYPE_IMAGE_CONFIG};

    /// Build a tiny one-layer image directly in a fresh store (no real
    /// registry/build machinery needed) and record it under
    /// `reference`. Shared by every test in this module that needs a
    /// real, storable image to save/load.
    fn seed_sample_image(store: &Store, reference: &str) -> ImageRecord {
        let layer_content = b"hello from a fake layer\n";
        let layer_digest = store.ingest(&layer_content[..]).unwrap().digest;

        let config = ImageConfig {
            architecture: Some("arm64".to_string()),
            os: Some("linux".to_string()),
            config: Some(ContainerConfig::default()),
            rootfs: oci_spec_types::image::RootFs {
                kind: "layers".to_string(),
                diff_ids: vec![layer_digest.clone()],
            },
            history: Vec::new(),
            created: None,
            author: None,
        };
        let config_bytes = serde_json::to_vec(&config).unwrap();
        let config_digest = store.ingest(&config_bytes[..]).unwrap().digest;

        let manifest = ImageManifest {
            schema_version: 2,
            media_type: Some(MEDIA_TYPE_IMAGE_MANIFEST.to_string()),
            config: Descriptor {
                media_type: MEDIA_TYPE_IMAGE_CONFIG.to_string(),
                digest: config_digest,
                size: config_bytes.len() as u64,
                urls: Vec::new(),
                annotations: BTreeMap::new(),
                platform: None,
            },
            layers: vec![Descriptor {
                media_type: oci_spec_types::image::MEDIA_TYPE_IMAGE_LAYER.to_string(),
                digest: layer_digest,
                size: layer_content.len() as u64,
                urls: Vec::new(),
                annotations: BTreeMap::new(),
                platform: None,
            }],
            annotations: BTreeMap::new(),
        };
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_digest = store.ingest(&manifest_bytes[..]).unwrap().digest;

        let record = ImageRecord {
            reference: reference.to_string(),
            manifest_digest,
        };
        store.put_image(&record).unwrap();
        record
    }

    /// Save it and confirm the resulting tar has exactly the real
    /// oci-archive shape: an `oci-layout` file, an `index.json` naming
    /// the manifest digest and the reference annotation, and every
    /// blob the manifest names present under `blobs/sha256/<hex>` with
    /// the exact right content.
    #[test]
    fn save_oci_archive_produces_the_real_oci_archive_shape() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let record = seed_sample_image(&store, "example.com/foo:latest");
        let manifest = store.image_manifest(&record).unwrap();
        let manifest_bytes = store.read_blob(&record.manifest_digest).unwrap();
        let config_bytes = store.read_blob(&manifest.config.digest).unwrap();
        let layer_bytes = store.read_blob(&manifest.layers[0].digest).unwrap();

        let mut out = Vec::new();
        save_oci_archive(&store, &record, &mut out).unwrap();

        let mut archive = tar::Archive::new(&out[..]);
        let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_str().unwrap().to_string();
            let mut buf = Vec::new();
            Read::read_to_end(&mut entry, &mut buf).unwrap();
            entries.insert(path, buf);
        }

        assert_eq!(
            entries.get("oci-layout").unwrap(),
            br#"{"imageLayoutVersion":"1.0.0"}"#
        );

        let index: ImageIndex = serde_json::from_slice(entries.get("index.json").unwrap()).unwrap();
        assert_eq!(index.schema_version, 2);
        assert!(index.media_type.is_none());
        assert_eq!(index.manifests.len(), 1);
        assert_eq!(index.manifests[0].digest, record.manifest_digest);
        assert_eq!(
            index.manifests[0]
                .annotations
                .get(ANNOTATION_REF_NAME)
                .unwrap(),
            "example.com/foo:latest"
        );

        assert_eq!(
            entries
                .get(&format!("blobs/sha256/{}", record.manifest_digest.hex()))
                .unwrap(),
            &manifest_bytes
        );
        assert_eq!(
            entries
                .get(&format!("blobs/sha256/{}", manifest.config.digest.hex()))
                .unwrap(),
            &config_bytes
        );
        assert_eq!(
            entries
                .get(&format!("blobs/sha256/{}", manifest.layers[0].digest.hex()))
                .unwrap(),
            &layer_bytes
        );
    }

    /// The most convincing check for `load_archive`: save a real
    /// image out of one store, load the resulting bytes into a
    /// completely separate, fresh store, and confirm every blob
    /// (manifest, config, layer) made it across byte for byte and the
    /// tag was recorded correctly -- a full save/load round trip, not
    /// just each half tested in isolation.
    #[test]
    fn save_then_load_round_trips_into_a_fresh_store() {
        let source_dir = tempfile::tempdir().unwrap();
        let source_store = Store::open(source_dir.path()).unwrap();
        let record = seed_sample_image(&source_store, "example.com/roundtrip:v1");

        let mut archive_bytes = Vec::new();
        save_oci_archive(&source_store, &record, &mut archive_bytes).unwrap();

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_store = Store::open(dest_dir.path()).unwrap();
        let loaded = load_archive(&dest_store, &archive_bytes[..]).unwrap();

        assert_eq!(loaded.references, vec!["example.com/roundtrip:v1"]);
        assert_eq!(loaded.manifest_digest, record.manifest_digest);

        let dest_record = dest_store
            .resolve_image("example.com/roundtrip:v1")
            .unwrap()
            .unwrap();
        assert_eq!(dest_record.manifest_digest, record.manifest_digest);

        let source_manifest = source_store.image_manifest(&record).unwrap();
        let dest_manifest = dest_store.image_manifest(&dest_record).unwrap();
        assert_eq!(source_manifest, dest_manifest);
        assert_eq!(
            source_store
                .read_blob(&source_manifest.config.digest)
                .unwrap(),
            dest_store.read_blob(&dest_manifest.config.digest).unwrap()
        );
        assert_eq!(
            source_store
                .read_blob(&source_manifest.layers[0].digest)
                .unwrap(),
            dest_store
                .read_blob(&dest_manifest.layers[0].digest)
                .unwrap()
        );
    }

    /// An archive with no `org.opencontainers.image.ref.name`
    /// annotation still loads successfully -- every blob is ingested
    /// and the manifest digest is returned -- it just doesn't record
    /// any tag pointer, matching real `podman load`'s own handling of
    /// an untagged/by-digest-only archive.
    #[test]
    fn load_with_no_ref_name_annotation_ingests_everything_but_records_no_tag() {
        let source_dir = tempfile::tempdir().unwrap();
        let source_store = Store::open(source_dir.path()).unwrap();
        let record = seed_sample_image(&source_store, "example.com/untagged:latest");
        let mut archive_bytes = Vec::new();
        save_oci_archive(&source_store, &record, &mut archive_bytes).unwrap();

        // Strip the ref.name annotation out of the index.json this
        // archive already has, rebuilding the tar with everything
        // else identical -- simulating an archive some other tool
        // saved without a tag.
        let mut entries: Vec<(tar::Header, Vec<u8>)> = Vec::new();
        {
            let mut archive = tar::Archive::new(&archive_bytes[..]);
            for entry in archive.entries().unwrap() {
                let mut entry = entry.unwrap();
                let header = entry.header().clone();
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf).unwrap();
                entries.push((header, buf));
            }
        }
        let mut rebuilt = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut rebuilt);
            for (mut header, content) in entries {
                let path = header.path().unwrap().to_string_lossy().into_owned();
                if path == "index.json" {
                    let mut index: ImageIndex = serde_json::from_slice(&content).unwrap();
                    index.manifests[0].annotations.clear();
                    let new_content = serde_json::to_vec(&index).unwrap();
                    header.set_size(new_content.len() as u64);
                    builder
                        .append_data(&mut header, &path, &new_content[..])
                        .unwrap();
                } else if header.entry_type() != tar::EntryType::Directory {
                    builder
                        .append_data(&mut header, &path, &content[..])
                        .unwrap();
                }
            }
            builder.finish().unwrap();
        }

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_store = Store::open(dest_dir.path()).unwrap();
        let loaded = load_archive(&dest_store, &rebuilt[..]).unwrap();

        assert!(loaded.references.is_empty());
        assert_eq!(loaded.manifest_digest, record.manifest_digest);
        assert!(dest_store.has_blob(&record.manifest_digest));
        assert!(dest_store.list_images().unwrap().is_empty());
    }

    /// A real, previously-caught bug (found via manual interop testing
    /// against a real, modern `docker save`, not written from a
    /// hypothesis): real `buildkit`-based `docker save` sets
    /// `org.opencontainers.image.ref.name` to just the bare tag
    /// (`"latest"`), not a full reference, with the *actual* full
    /// reference under a separate `io.containerd.image.name`
    /// annotation instead. Loading such an archive using
    /// `ref.name` alone previously mis-resolved `docker.io/library/
    /// busybox:latest` down to the nonsensical `docker.io/library/
    /// latest:latest` (treating the bare tag `"latest"` as if it were
    /// itself a bare image name). Real `podman load` handles the exact
    /// same real archive correctly (verified directly) precisely
    /// because it prefers `io.containerd.image.name` first — see
    /// `ANNOTATION_CONTAINERD_IMAGE_NAME`'s own doc comment.
    #[test]
    fn load_prefers_the_real_containerd_image_name_annotation_over_the_bare_oci_ref_name_one() {
        let source_dir = tempfile::tempdir().unwrap();
        let source_store = Store::open(source_dir.path()).unwrap();
        let record = seed_sample_image(&source_store, "docker.io/library/busybox:latest");
        let mut archive_bytes = Vec::new();
        save_oci_archive(&source_store, &record, &mut archive_bytes).unwrap();

        // Rewrite index.json to look exactly like a real modern
        // `docker save`'s own output: `ref.name` is just the bare tag,
        // `io.containerd.image.name` carries the real, full reference.
        let mut entries: Vec<(tar::Header, Vec<u8>)> = Vec::new();
        {
            let mut archive = tar::Archive::new(&archive_bytes[..]);
            for entry in archive.entries().unwrap() {
                let mut entry = entry.unwrap();
                let header = entry.header().clone();
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf).unwrap();
                entries.push((header, buf));
            }
        }
        let mut rebuilt = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut rebuilt);
            for (mut header, content) in entries {
                let path = header.path().unwrap().to_string_lossy().into_owned();
                if path == "index.json" {
                    let mut index: ImageIndex = serde_json::from_slice(&content).unwrap();
                    index.manifests[0]
                        .annotations
                        .insert(ANNOTATION_REF_NAME.to_string(), "latest".to_string());
                    index.manifests[0].annotations.insert(
                        ANNOTATION_CONTAINERD_IMAGE_NAME.to_string(),
                        "docker.io/library/busybox:latest".to_string(),
                    );
                    let new_content = serde_json::to_vec(&index).unwrap();
                    header.set_size(new_content.len() as u64);
                    builder
                        .append_data(&mut header, &path, &new_content[..])
                        .unwrap();
                } else if header.entry_type() != tar::EntryType::Directory {
                    builder
                        .append_data(&mut header, &path, &content[..])
                        .unwrap();
                }
            }
            builder.finish().unwrap();
        }

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_store = Store::open(dest_dir.path()).unwrap();
        let loaded = load_archive(&dest_store, &rebuilt[..]).unwrap();

        assert_eq!(loaded.references, vec!["docker.io/library/busybox:latest"]);
        assert_eq!(loaded.manifest_digest, record.manifest_digest);
    }

    #[test]
    fn load_rejects_an_archive_with_neither_index_json_nor_manifest_json() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            append_regular(
                &mut builder,
                "oci-layout",
                0,
                0o644,
                br#"{"imageLayoutVersion":"1.0.0"}"#,
            )
            .unwrap();
            builder.finish().unwrap();
        }
        let err = load_archive(&store, &bytes[..]).unwrap_err();
        assert!(
            format!("{err:#}").contains("missing both index.json")
                && format!("{err:#}").contains("manifest.json"),
            "{err:#}"
        );
    }

    /// A real `oci-archive` with `index.json` present but its own
    /// required `oci-layout` marker missing -- a real, distinct error
    /// from the "neither format detected" case above.
    #[test]
    fn load_rejects_an_oci_archive_missing_its_own_oci_layout_marker() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let index = ImageIndex {
            schema_version: 2,
            media_type: None,
            manifests: Vec::new(),
            annotations: BTreeMap::new(),
        };
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            append_regular(
                &mut builder,
                "index.json",
                0,
                0o644,
                &serde_json::to_vec(&index).unwrap(),
            )
            .unwrap();
            builder.finish().unwrap();
        }
        let err = load_archive(&store, &bytes[..]).unwrap_err();
        assert!(format!("{err:#}").contains("oci-layout"), "{err:#}");
    }

    /// The docker-archive mirror of `save_then_load_round_trips_into_a
    /// _fresh_store`: build a docker-archive tar directly (not via
    /// `save_docker_archive`, to keep this test focused purely on the
    /// read side), load it, and confirm the freshly synthesized
    /// manifest/config/layer all round-trip correctly, including
    /// tagging under every one of several `RepoTags`.
    #[test]
    fn load_docker_archive_synthesizes_a_real_manifest_and_tags_every_repo_tag() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        let layer_plaintext = b"a real, plain, uncompressed docker-archive layer tar\n";
        let layer_diff_id = oci_spec_types::digest::sha256(layer_plaintext);

        let config = ImageConfig {
            architecture: Some("arm64".to_string()),
            os: Some("linux".to_string()),
            config: Some(ContainerConfig::default()),
            rootfs: oci_spec_types::image::RootFs {
                kind: "layers".to_string(),
                diff_ids: vec![layer_diff_id.clone()],
            },
            history: Vec::new(),
            created: None,
            author: None,
        };
        let config_bytes = serde_json::to_vec(&config).unwrap();
        let config_digest = oci_spec_types::digest::sha256(&config_bytes);
        let config_name = format!("{}.json", config_digest.hex());
        let layer_name = format!("{}.tar", layer_diff_id.hex());

        let manifest_item = DockerArchiveManifestItem {
            config: config_name.clone(),
            repo_tags: vec![
                "example.com/multi-tag:v1".to_string(),
                "example.com/multi-tag:latest".to_string(),
            ],
            layers: vec![layer_name.clone()],
        };
        let manifest_json = serde_json::to_vec(&vec![manifest_item]).unwrap();

        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            append_regular(&mut builder, &config_name, 0, 0o444, &config_bytes).unwrap();
            append_regular(&mut builder, &layer_name, 0, 0o444, layer_plaintext).unwrap();
            append_regular(&mut builder, "manifest.json", 0, 0o444, &manifest_json).unwrap();
            builder.finish().unwrap();
        }

        let loaded = load_archive(&store, &bytes[..]).unwrap();
        assert_eq!(
            loaded.references,
            vec![
                "example.com/multi-tag:v1".to_string(),
                "example.com/multi-tag:latest".to_string()
            ]
        );

        for reference in &loaded.references {
            let record = store.resolve_image(reference).unwrap().unwrap();
            assert_eq!(record.manifest_digest, loaded.manifest_digest);
        }

        let manifest = store
            .image_manifest(
                &store
                    .resolve_image("example.com/multi-tag:v1")
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(manifest.layers.len(), 1);
        assert_eq!(manifest.config.digest, config_digest);

        // The stored layer must be real, valid gzip whose decompressed
        // content is exactly the original plaintext -- not a raw copy
        // of the (uncompressed) archive entry.
        let stored_layer_bytes = store.read_blob(&manifest.layers[0].digest).unwrap();
        let mut decoder = flate2::read::GzDecoder::new(&stored_layer_bytes[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).unwrap();
        assert_eq!(decompressed, layer_plaintext);
    }

    #[test]
    fn load_docker_archive_rejects_a_diff_id_mismatch_between_config_and_layer_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        let layer_plaintext = b"real layer content";
        // Deliberately wrong: the config claims a diff_id that doesn't
        // match this layer's own real, independently-computed digest.
        let wrong_diff_id = oci_spec_types::digest::sha256(b"not the real layer content");

        let config = ImageConfig {
            architecture: Some("arm64".to_string()),
            os: Some("linux".to_string()),
            config: Some(ContainerConfig::default()),
            rootfs: oci_spec_types::image::RootFs {
                kind: "layers".to_string(),
                diff_ids: vec![wrong_diff_id],
            },
            history: Vec::new(),
            created: None,
            author: None,
        };
        let config_bytes = serde_json::to_vec(&config).unwrap();
        let config_digest = oci_spec_types::digest::sha256(&config_bytes);
        let config_name = format!("{}.json", config_digest.hex());
        // Named after the *real* diff_id, matching what a real save
        // would do -- the config is the one lying here, not the file
        // name, exercising the cross-check against the config
        // specifically.
        let real_diff_id = oci_spec_types::digest::sha256(layer_plaintext);
        let layer_name = format!("{}.tar", real_diff_id.hex());

        let manifest_item = DockerArchiveManifestItem {
            config: config_name.clone(),
            repo_tags: Vec::new(),
            layers: vec![layer_name.clone()],
        };
        let manifest_json = serde_json::to_vec(&vec![manifest_item]).unwrap();

        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            append_regular(&mut builder, &config_name, 0, 0o444, &config_bytes).unwrap();
            append_regular(&mut builder, &layer_name, 0, 0o444, layer_plaintext).unwrap();
            append_regular(&mut builder, "manifest.json", 0, 0o444, &manifest_json).unwrap();
            builder.finish().unwrap();
        }

        let err = load_archive(&store, &bytes[..]).unwrap_err();
        assert!(format!("{err:#}").contains("diff_ids"), "{err:#}");
    }

    #[test]
    fn load_rejects_a_blob_whose_content_does_not_match_its_own_filename_digest() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let fake_digest =
            oci_spec_types::digest::sha256(b"the real content, not what's actually in the tar");
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            append_regular(
                &mut builder,
                &format!("blobs/sha256/{}", fake_digest.hex()),
                0,
                0o644,
                b"not the real content at all",
            )
            .unwrap();
            builder.finish().unwrap();
        }
        let err = load_archive(&store, &bytes[..]).unwrap_err();
        assert!(format!("{err:#}").contains("ingesting blob"), "{err:#}");
    }
}
