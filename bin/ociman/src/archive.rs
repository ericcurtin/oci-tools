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
//! # `docker-archive` (real `podman save`'s own default format) is not
//! implemented yet
//!
//! `docker-archive` needs every layer *decompressed* first (the format
//! wants a plain, uncompressed tar per layer, unlike this project's
//! own gzip'd blobs — checked directly against
//! `go.podman.io/image/v5/docker/internal/tarfile/dest.go`'s own
//! `DesiredLayerCompression: types.Decompress`) plus a synthesized
//! legacy `repositories` file and a `manifest.json`. Real, separate
//! scope for a follow-up increment — see `docs/design/0165` for the
//! rest of what's deferred. `--format` therefore only accepts
//! `oci-archive` for now; `ociman save`'s CLI enum has no
//! `docker-archive` variant to select in the first place, so an
//! attempt to use one is clap's own "invalid value" error, not a
//! silent fallback to the wrong format.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use oci_spec_types::Digest;
use oci_spec_types::Reference;
use oci_spec_types::image::{Descriptor, ImageIndex, ImageManifest, MEDIA_TYPE_IMAGE_MANIFEST};
use oci_store::{ImageRecord, Store};

const ANNOTATION_REF_NAME: &str = "org.opencontainers.image.ref.name";
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
    append_regular(&mut builder, "index.json", mtime, &index_bytes)?;

    let mut writer = builder.into_inner().context("finishing oci-archive tar")?;
    writer.flush().context("flushing oci-archive tar")
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
    content: &[u8],
) -> anyhow::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o644);
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

/// What [`load_oci_archive`] actually did — mirrors real `podman
/// load`'s own "Loaded image: ..." line, which names a tag when the
/// archive's own `index.json` carried one (the
/// `org.opencontainers.image.ref.name` annotation [`save_oci_archive`]
/// itself writes), or falls back to the bare digest when it didn't
/// (an untagged/by-digest-only archive — this project's own [`Store`]
/// happily holds a manifest with no reference pointing at it at all,
/// so this is a real, supported case, not an error).
#[derive(Debug)]
pub(crate) struct LoadedImage {
    pub(crate) reference: Option<String>,
    pub(crate) manifest_digest: Digest,
}

/// Read a real `oci-archive:`-format tar from `reader` (a real file or
/// standard input, either way just anything [`Read`]) and ingest every
/// blob it names into `store`, verifying each blob's content against
/// the exact digest its own `blobs/sha256/<hex>` filename claims (the
/// same defense a registry pull already applies via
/// [`Store::ingest_verified`] — a malicious or corrupt archive can
/// never poison local storage with content under the wrong digest).
///
/// A single linear pass over the tar stream (never needs to seek, so
/// this works directly against standard input too, unlike real
/// `containers/image`'s own oci-archive reader, which extracts to a
/// temp directory first): every `blobs/sha256/<hex>` entry is ingested
/// as it's encountered, `index.json`/`oci-layout` are buffered in
/// memory (both always small) and only interpreted once the whole
/// stream has been consumed, so entry order within the archive
/// (blobs before or after `index.json`, in practice always before —
/// see this module's own `save_oci_archive`) never matters.
///
/// Only ever accepts a single-manifest, single-platform archive —
/// matching the only shape [`save_oci_archive`] itself ever produces;
/// a multi-manifest `index.json` (a real multi-platform image saved by
/// some other tool) is a clear, named error rather than a silent
/// "picks whichever one" guess.
pub(crate) fn load_oci_archive(store: &Store, reader: impl Read) -> anyhow::Result<LoadedImage> {
    let mut archive = tar::Archive::new(reader);
    let mut index_bytes: Option<Vec<u8>> = None;
    let mut oci_layout_bytes: Option<Vec<u8>> = None;

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
        }
        // Anything else (an unrecognized top-level entry) is ignored
        // rather than rejected outright -- a real, forward-compatible
        // choice: a future OCI image-spec revision could add another
        // top-level file this reader doesn't know about yet without
        // every existing archive suddenly failing to load.
    }

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

    let index_bytes = index_bytes.context("not a valid oci-archive: missing index.json")?;
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

    let reference = match descriptor.annotations.get(ANNOTATION_REF_NAME) {
        Some(raw) => {
            let parsed = Reference::parse(raw)
                .with_context(|| format!("parsing {ANNOTATION_REF_NAME} annotation {raw:?}"))?;
            let normalized = parsed.to_string();
            store
                .put_image(&ImageRecord {
                    reference: normalized.clone(),
                    manifest_digest: descriptor.digest.clone(),
                })
                .context("recording loaded image's tag")?;
            Some(normalized)
        }
        None => None,
    };

    Ok(LoadedImage {
        reference,
        manifest_digest: descriptor.digest.clone(),
    })
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

    /// The most convincing check for `load_oci_archive`: save a real
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
        let loaded = load_oci_archive(&dest_store, &archive_bytes[..]).unwrap();

        assert_eq!(
            loaded.reference.as_deref(),
            Some("example.com/roundtrip:v1")
        );
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
        let loaded = load_oci_archive(&dest_store, &rebuilt[..]).unwrap();

        assert_eq!(loaded.reference, None);
        assert_eq!(loaded.manifest_digest, record.manifest_digest);
        assert!(dest_store.has_blob(&record.manifest_digest));
        assert!(dest_store.list_images().unwrap().is_empty());
    }

    #[test]
    fn load_rejects_an_archive_with_no_index_json_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            append_regular(
                &mut builder,
                "oci-layout",
                0,
                br#"{"imageLayoutVersion":"1.0.0"}"#,
            )
            .unwrap();
            builder.finish().unwrap();
        }
        let err = load_oci_archive(&store, &bytes[..]).unwrap_err();
        assert!(format!("{err:#}").contains("missing index.json"), "{err:#}");
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
                b"not the real content at all",
            )
            .unwrap();
            builder.finish().unwrap();
        }
        let err = load_oci_archive(&store, &bytes[..]).unwrap_err();
        assert!(format!("{err:#}").contains("ingesting blob"), "{err:#}");
    }
}
