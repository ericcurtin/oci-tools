//! Sniffing whether a chunk of bytes is a tar archive `ociman build`'s
//! own `ADD` instruction should auto-extract, rather than copy
//! verbatim like `COPY` — real `docker`'s own documented `ADD`
//! behavior, checked directly against the currently-vendored
//! `~/git/moby/vendor/github.com/moby/go-archive/archive.go`'s own
//! `IsArchivePath` (which opens the file, tries each recognized
//! compression, and only calls it an archive if the result also
//! parses as a real tar stream — not stopping at magic-byte detection
//! alone) and `~/git/moby/vendor/github.com/containerd/containerd/
//! v2/pkg/archive/compression/compression.go`'s own `DetectCompression`
//! (which, in this exact vendored version, only recognizes gzip and
//! zstd — not bzip2/xz, which some older real `docker` releases did
//! support; matching the version actually vendored in this workspace's
//! own `~/git/moby` checkout, not assumed from memory or older docs).

use std::io::Read;

use crate::Compression;

/// Gzip's own two-byte magic number (`RFC 1952`).
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
/// Zstandard's own four-byte frame magic number (`RFC 8878`).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// If `data` is a real archive `ADD` should auto-extract, return the
/// [`Compression`] to pass to [`crate::apply`]; `None` if it should be
/// copied verbatim instead (not a recognized compression at all, or
/// recognized but the decompressed result doesn't actually parse as a
/// tar stream — a real, common false-positive plain magic-byte
/// sniffing alone would miss: not every gzip-compressed file is
/// secretly a tar archive).
pub fn detect_archive(data: &[u8]) -> Option<Compression> {
    [Compression::Gzip, Compression::Zstd, Compression::None]
        .into_iter()
        .find(|&compression| is_valid_archive(data, compression))
}

fn is_valid_archive(data: &[u8], compression: Compression) -> bool {
    match compression {
        // Real magic-number check first (cheap, and matches real
        // docker's own order) before attempting the real, more
        // expensive decompress-then-parse-a-tar-header check below --
        // avoids wastefully trying to gzip/zstd-decode data that
        // obviously isn't either to begin with.
        Compression::Gzip if data.starts_with(&GZIP_MAGIC) => {
            first_tar_entry_parses(flate2::read::GzDecoder::new(data))
        }
        Compression::Zstd if data.starts_with(&ZSTD_MAGIC) => {
            match ruzstd::decoding::StreamingDecoder::new(data) {
                Ok(decoder) => first_tar_entry_parses(decoder),
                Err(_) => false,
            }
        }
        Compression::None => first_tar_entry_parses(data),
        Compression::Gzip | Compression::Zstd => false,
    }
}

/// Whether `reader`'s content parses as a real tar stream with at
/// least one entry — checked by actually reading the first header,
/// not merely by not-immediately-erroring, matching real docker's own
/// `tar.NewReader(rdr); _, err = r.Next()` exactly (an entirely empty
/// tar archive, real but content-free, is *not* considered an archive
/// either way, on both sides: Go's own tar reader returns `io.EOF` for
/// that case, exactly like this one does).
fn first_tar_entry_parses(reader: impl Read) -> bool {
    let mut archive = tar::Archive::new(reader);
    match archive.entries() {
        Ok(mut entries) => entries.next().is_some_and(|entry| entry.is_ok()),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn make_tar(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *content).unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn detects_a_plain_uncompressed_tar() {
        let data = make_tar(&[("hello.txt", b"hi\n")]);
        assert_eq!(detect_archive(&data), Some(Compression::None));
    }

    #[test]
    fn detects_a_real_gzip_compressed_tar() {
        let tar_bytes = make_tar(&[("hello.txt", b"hi\n")]);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_bytes).unwrap();
        let gzipped = encoder.finish().unwrap();
        assert_eq!(detect_archive(&gzipped), Some(Compression::Gzip));
    }

    #[test]
    fn a_plain_text_file_is_not_an_archive() {
        assert_eq!(detect_archive(b"just some ordinary text\n"), None);
    }

    #[test]
    fn a_gzip_compressed_non_tar_file_is_not_an_archive() {
        // A real gzip stream (correct magic bytes, really
        // decompresses) whose content is *not* a tar archive --
        // exactly the false-positive plain magic-byte sniffing alone
        // would miss.
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(b"just some ordinary text, gzipped\n")
            .unwrap();
        let gzipped = encoder.finish().unwrap();
        assert_eq!(detect_archive(&gzipped), None);
    }

    #[test]
    fn an_empty_tar_archive_is_not_considered_an_archive_either() {
        // An entirely empty (zero-entry) tar is technically valid but
        // has no first entry to parse -- matching real docker's own
        // behavior exactly (confirmed directly against the vendored
        // source's own `r.Next()` check, which sees `io.EOF` for this
        // same case).
        let builder = tar::Builder::new(Vec::new());
        let empty_tar = builder.into_inner().unwrap();
        assert_eq!(detect_archive(&empty_tar), None);
    }

    #[test]
    fn garbage_that_merely_starts_with_the_gzip_magic_bytes_is_not_an_archive() {
        let mut garbage = vec![0x1f, 0x8b];
        garbage.extend_from_slice(b"not actually valid gzip content at all");
        assert_eq!(detect_archive(&garbage), None);
    }
}
