//! Compressing a freshly [`crate::export`]ed (or otherwise assembled)
//! uncompressed layer tar stream for storage — the last step between
//! this crate's own diff/export pair and a future `ociman build`
//! actually committing a `RUN` step's changes as a real, storable OCI
//! layer blob.
//!
//! # Two digests, one pass — per the OCI image-spec, not invented here
//!
//! An OCI image layer has two distinct digests, checked directly
//! against the image-spec's own [config][config-spec] and
//! [manifest][manifest-spec] definitions:
//!
//! * `diff_id`: the digest of the layer's **uncompressed** tar
//!   content, recorded in the image config's own `rootfs.diff_ids`
//!   (in the same order as the manifest's own layer list) — used to
//!   identify a layer independent of *how* it happens to be
//!   compressed, so the same uncompressed content pushed twice with
//!   two different compression settings is still recognized as the
//!   same layer.
//! * the manifest's own layer descriptor digest: the digest of the
//!   **compressed** bytes actually stored and transferred — what
//!   [`oci_store::Store::ingest`] computes and returns on its own,
//!   from whatever compressed bytes this module hands it.
//!
//! [`compress_for_storage`] computes the first (`diff_id`) while
//! streaming the same input through a gzip encoder to produce the
//! second (indirectly — the caller still has to hand the compressed
//! output to a blob store to learn *its* digest), in one pass over the
//! uncompressed content, matching the same "hash while writing"
//! streaming shape [`oci_store::Store`]'s own `ingest_impl` already
//! uses on the read side of this same pipeline (checked directly
//! against `crates/oci-store/src/lib.rs`, not reinvented separately
//! here).
//!
//! [config-spec]: https://github.com/opencontainers/image-spec/blob/main/config.md#properties
//! [manifest-spec]: https://github.com/opencontainers/image-spec/blob/main/manifest.md#image-manifest-property-descriptions
//!
//! # Compression level
//!
//! `flate2::Compression::default()` (level 6) — the same default real
//! `moby`'s own layer-export path uses (`gzip.NewWriter`, Go's own
//! `compress/gzip` default, also level 6) rather than a project-
//! specific choice, so a layer built by this project compresses to
//! roughly the size (and takes roughly the time) a layer built by real
//! `moby` would for the same content.

use std::io::{Read, Write};

use oci_spec_types::Digest;
use oci_spec_types::digest::Sha256Writer;

use crate::{LayerError, Result};

/// Stream `reader` (an uncompressed tar, e.g. straight from
/// [`crate::export`]) through a gzip encoder into `writer`, computing
/// the *uncompressed* content's own digest (the `diff_id`) as it goes.
/// See this module's own doc comment for what a caller still needs to
/// do with the result (ingest `writer`'s own accumulated bytes into a
/// blob store separately, to learn the *compressed* blob's own
/// digest).
pub fn compress_for_storage(mut reader: impl Read, writer: impl Write) -> Result<Digest> {
    let mut hasher = Sha256Writer::new();
    let mut encoder = flate2::write::GzEncoder::new(writer, flate2::Compression::default());
    let mut buf = [0u8; 128 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.write_all(&buf[..n])?;
        encoder.write_all(&buf[..n])?;
    }
    encoder.finish().map_err(LayerError::Io)?;
    Ok(hasher.finish_digest())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Compression, apply};

    #[test]
    fn diff_id_matches_sha256_of_the_uncompressed_input() {
        let input = b"hello, this is real uncompressed tar-shaped content";
        let mut compressed = Vec::new();
        let diff_id = compress_for_storage(&input[..], &mut compressed).unwrap();
        assert_eq!(diff_id, oci_spec_types::digest::sha256(input));
    }

    #[test]
    fn compressed_output_decompresses_back_to_the_original_bytes() {
        let input = b"some content, repeated repeated repeated for compressibility";
        let mut compressed = Vec::new();
        compress_for_storage(&input[..], &mut compressed).unwrap();

        let mut decoder = flate2::read::GzDecoder::new(compressed.as_slice());
        let mut roundtripped = Vec::new();
        decoder.read_to_end(&mut roundtripped).unwrap();
        assert_eq!(roundtripped, input);
    }

    #[test]
    fn same_input_compressed_twice_yields_the_same_diff_id() {
        let input = b"deterministic content";
        let mut out1 = Vec::new();
        let mut out2 = Vec::new();
        let diff_id1 = compress_for_storage(&input[..], &mut out1).unwrap();
        let diff_id2 = compress_for_storage(&input[..], &mut out2).unwrap();
        assert_eq!(diff_id1, diff_id2);
    }

    #[test]
    fn an_empty_input_produces_the_well_known_empty_diff_id() {
        let mut compressed = Vec::new();
        let diff_id = compress_for_storage(&b""[..], &mut compressed).unwrap();
        assert_eq!(diff_id, Digest::empty_sha256());
    }

    /// The most convincing check: a real layer tar (built via
    /// [`crate::export`]) compressed by this module round-trips all
    /// the way back through this crate's own [`apply`] with
    /// [`Compression::Gzip`] -- exactly the path a real layer pulled
    /// from a registry (or, eventually, committed by `ociman build`)
    /// takes.
    #[test]
    fn a_real_exported_layer_compressed_here_applies_correctly_as_gzip() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("a")).unwrap();
        std::fs::write(source.path().join("a/b.txt"), b"content").unwrap();
        let before = crate::Snapshot::capture(source.path()).unwrap();

        std::fs::write(source.path().join("a/b.txt"), b"more content, now longer").unwrap();
        let changes = crate::changes(source.path(), &before).unwrap();

        let mut tar_bytes = Vec::new();
        crate::export(source.path(), &changes, &mut tar_bytes).unwrap();

        let mut compressed = Vec::new();
        compress_for_storage(tar_bytes.as_slice(), &mut compressed).unwrap();

        let dest = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dest.path().join("a")).unwrap();
        std::fs::write(dest.path().join("a/b.txt"), b"content").unwrap();
        apply(compressed.as_slice(), Compression::Gzip, dest.path()).unwrap();

        assert_eq!(
            std::fs::read(dest.path().join("a/b.txt")).unwrap(),
            b"more content, now longer"
        );
    }
}
