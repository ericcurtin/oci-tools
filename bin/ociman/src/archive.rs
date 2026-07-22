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
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use oci_spec_types::Digest;
use oci_spec_types::image::{Descriptor, ImageIndex, ImageManifest, MEDIA_TYPE_IMAGE_MANIFEST};
use oci_store::{ImageRecord, Store};

const ANNOTATION_REF_NAME: &str = "org.opencontainers.image.ref.name";

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

#[cfg(test)]
mod tests {
    use super::*;
    use oci_spec_types::image::{ContainerConfig, ImageConfig, MEDIA_TYPE_IMAGE_CONFIG};

    /// Build a tiny one-layer image directly in a fresh store (no
    /// real registry/build machinery needed), then save it and confirm
    /// the resulting tar has exactly the real oci-archive shape: an
    /// `oci-layout` file, an `index.json` naming the manifest digest
    /// and the reference annotation, and every blob the manifest
    /// names present under `blobs/sha256/<hex>` with the exact right
    /// content.
    #[test]
    fn save_oci_archive_produces_the_real_oci_archive_shape() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

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
                digest: config_digest.clone(),
                size: config_bytes.len() as u64,
                urls: Vec::new(),
                annotations: BTreeMap::new(),
                platform: None,
            },
            layers: vec![Descriptor {
                media_type: oci_spec_types::image::MEDIA_TYPE_IMAGE_LAYER.to_string(),
                digest: layer_digest.clone(),
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
            reference: "example.com/foo:latest".to_string(),
            manifest_digest: manifest_digest.clone(),
        };
        store.put_image(&record).unwrap();

        let mut out = Vec::new();
        save_oci_archive(&store, &record, &mut out).unwrap();

        let mut archive = tar::Archive::new(&out[..]);
        let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_str().unwrap().to_string();
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut buf).unwrap();
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
        assert_eq!(index.manifests[0].digest, manifest_digest);
        assert_eq!(
            index.manifests[0]
                .annotations
                .get(ANNOTATION_REF_NAME)
                .unwrap(),
            "example.com/foo:latest"
        );

        assert_eq!(
            entries
                .get(&format!("blobs/sha256/{}", manifest_digest.hex()))
                .unwrap(),
            &manifest_bytes
        );
        assert_eq!(
            entries
                .get(&format!("blobs/sha256/{}", config_digest.hex()))
                .unwrap(),
            &config_bytes
        );
        assert_eq!(
            entries
                .get(&format!("blobs/sha256/{}", layer_digest.hex()))
                .unwrap(),
            layer_content
        );
    }
}
