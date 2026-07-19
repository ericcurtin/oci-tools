//! Applying one OCI image layer (a tar stream, optionally gzip-
//! compressed) onto a root filesystem directory — the "tar/whiteout
//! applier" the top-level README's design pillars already name as one
//! of the workspace's one-implementation-per-function components.
//!
//! # Whiteouts
//!
//! Ported from the OCI image-spec's own definition (the same convention
//! `moby`/`containerd` implement — checked against `moby`'s own
//! reference implementation, `vendor/github.com/moby/go-archive/
//! diff.go`'s `UnpackLayer`, not re-derived from the spec prose alone,
//! since the exact "what counts as already-unpacked-this-layer" rule
//! for opaque directories is easy to get subtly wrong):
//!
//! * A `.wh.<name>` entry means `<name>` (a file or a whole directory
//!   tree) was deleted in this layer relative to the layers below it:
//!   remove it entirely, don't extract a `.wh.<name>` entry itself.
//! * A `.wh..wh..opq` entry, found directly inside some directory `D`,
//!   marks `D` **opaque**: every pre-existing entry under `D` that
//!   came from a *lower* layer (not one this same [`apply`] call has
//!   already written) is removed. Entries this layer has already
//!   written under `D` earlier in the same tar stream are kept — the
//!   marker can appear before or after the layer's own real entries
//!   for that directory in the stream, so this tracks "written this
//!   call" explicitly rather than assuming an order.
//!
//! Legacy AUFS-specific artifacts some older exported archives still
//! contain (`.wh..wh.plnk`, a hardlink-redirect directory purely for
//! the long-obsolete AUFS graph driver) are not handled — this project
//! never targets AUFS, and no other current graph driver ecosystem
//! (`overlay2`, `fuse-overlayfs`, `containerd`'s own snapshotters)
//! needs it either.
//!
//! # Ownership
//!
//! Extracted files keep the tar entry's **permission bits** but are
//! *not* `chown`ed to the entry's `uid`/`gid`: doing that for real
//! (matching what a file was actually built as inside its image)
//! needs either running as real root or a subordinate-uid-range
//! rootless remap (`/etc/subuid`), neither of which this increment
//! sets up. Every file this crate extracts ends up owned by the
//! calling process's own real uid/gid — correct for the common case
//! (an image's files are overwhelmingly owned by `root`, which is
//! exactly what a rootless container's own id-mapping already
//! resolves the calling user to *inside* the container — see
//! `oci_runtime_core::namespaces`), wrong for the rarer case of a
//! file intentionally owned by some other uid in the image. A real
//! gap, not silently "close enough" — flagged here for exactly that
//! reason.
//!
//! # `zstd` layers
//!
//! Decompressed via `ruzstd` (pure Rust, MIT, no libzstd dependency —
//! matches this project's own gzip choice, `flate2`'s Rust backend,
//! for the same reason). `ruzstd::decoding::StreamingDecoder` expects
//! its input to be a *single* zstd frame; the format itself allows an
//! archive to concatenate several, which this crate doesn't handle
//! (real registries' own zstd layer blobs are, in every real image
//! this project has pulled so far, a single frame — the overwhelmingly
//! common shape most encoders produce by default).
//!
//! # What isn't handled yet
//!
//! * Device nodes (`mknod`) and FIFOs: skipped rather than attempted,
//!   since creating a real device node needs `CAP_MKNOD`, which a
//!   rootless caller never has on the host (this is a real, standing
//!   rootless-container-tooling limitation, not specific to this
//!   crate — real `podman`/`buildah` hit the identical wall).
//! * Extended attributes (SELinux labels, capabilities stored as
//!   `security.capability`, ...).

use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

/// How a layer's tar stream is compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// A plain (uncompressed) tar stream.
    None,
    /// `tar+gzip` — the overwhelmingly common real-world case
    /// (`application/vnd.oci.image.layer.v1.tar+gzip`).
    Gzip,
    /// `tar+zstd`
    /// (`application/vnd.oci.image.layer.v1.tar+zstd`) — see this
    /// module's own doc comment for the single-frame scope limit.
    Zstd,
}

/// Errors from [`apply`].
#[derive(Debug, thiserror::Error)]
pub enum LayerError {
    /// I/O failure reading the layer stream or writing to `dest`.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// A tar entry's path would escape `dest` (a `..` component, or an
    /// absolute path) — rejected outright rather than silently
    /// confined, since this is exactly the kind of input a hostile or
    /// corrupt layer would use to write outside the intended rootfs.
    #[error("layer entry path {0:?} escapes the extraction root")]
    PathEscapesRoot(PathBuf),
    /// The zstd frame header itself was malformed (not a tar/gzip
    /// concern — `ruzstd` validates this eagerly, at construction,
    /// rather than lazily on the first read the way `flate2`'s own
    /// gzip decoder does).
    #[error("invalid zstd stream: {0}")]
    InvalidZstd(String),
}

type Result<T> = std::result::Result<T, LayerError>;

const WHITEOUT_PREFIX: &str = ".wh.";
const WHITEOUT_OPAQUE_MARKER: &str = ".wh..wh..opq";

/// Apply one layer's tar stream (per `compression`) onto `dest`,
/// applying OCI whiteouts along the way (see this module's own doc
/// comment). `dest` must already exist.
pub fn apply(reader: impl Read, compression: Compression, dest: &Path) -> Result<()> {
    match compression {
        Compression::None => apply_tar(reader, dest),
        Compression::Gzip => apply_tar(flate2::read::GzDecoder::new(reader), dest),
        Compression::Zstd => {
            let decoder = ruzstd::decoding::StreamingDecoder::new(reader)
                .map_err(|e| LayerError::InvalidZstd(e.to_string()))?;
            apply_tar(decoder, dest)
        }
    }
}

fn apply_tar(reader: impl Read, dest: &Path) -> Result<()> {
    // Tracks paths this call has already written, so an opaque-
    // directory whiteout (which can appear anywhere relative to this
    // layer's own real entries for the same directory in the stream)
    // only ever removes pre-existing *lower-layer* content, never
    // something this same call just extracted — matching moby's own
    // `unpackedPaths` bookkeeping in `UnpackLayer`.
    let mut written: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    // Entries are extracted by hand below (create_dir_all/File::create/
    // symlink/hard_link), not via `tar`'s own `Entry::unpack`, so there
    // is no built-in ownership/xattr-preservation behavior to disable —
    // this crate's own `extract_entry` already only ever sets
    // permission bits (see this module's doc comment on ownership).
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?.into_owned();
        let target = safe_join(dest, &entry_path)?;

        let file_name = entry_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();

        if file_name == WHITEOUT_OPAQUE_MARKER {
            if let Some(dir) = target.parent() {
                make_opaque(dir, &written)?;
            }
            continue;
        }
        if let Some(removed_name) = file_name.strip_prefix(WHITEOUT_PREFIX) {
            let removed = target
                .parent()
                .map(|p| p.join(removed_name))
                .unwrap_or_else(|| dest.join(removed_name));
            remove_if_exists(&removed)?;
            continue;
        }

        extract_entry(&mut entry, &target, dest, &written)?;
        written.insert(target);
    }
    Ok(())
}

/// Join `dest` and a tar entry's path, rejecting anything that would
/// escape `dest` (an absolute path, or any `..` component) rather than
/// silently `Path::join`ing (which lets an absolute entry path replace
/// `dest` outright) or lexically stripping the offending components
/// (which would just as silently accept a hostile/corrupt entry).
fn safe_join(dest: &Path, entry_path: &Path) -> Result<PathBuf> {
    let mut out = dest.to_path_buf();
    for component in entry_path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(LayerError::PathEscapesRoot(entry_path.to_path_buf()));
            }
        }
    }
    Ok(out)
}

/// Remove every pre-existing entry directly and transitively under
/// `dir` that isn't in `written` (i.e. came from a lower layer, not
/// this same [`apply`] call) — the opaque-whiteout semantics.
fn make_opaque(dir: &Path, written: &std::collections::HashSet<PathBuf>) -> io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let path = entry?.path();
        if written.contains(&path) {
            continue;
        }
        remove_if_exists(&path)?;
    }
    Ok(())
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => match std::fs::remove_dir_all(path) {
            Ok(()) | Err(_) if !path.exists() => Ok(()),
            other => other,
        },
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Extract one non-whiteout tar entry to `target`, replacing whatever
/// (if anything) a lower layer already left there — matching moby's
/// own rule: an existing entry is removed first unless *both* the
/// existing entry and the new one are directories (those merge: only
/// the new directory's own metadata is applied).
fn extract_entry(
    entry: &mut tar::Entry<'_, impl Read>,
    target: &Path,
    dest: &Path,
    written: &std::collections::HashSet<PathBuf>,
) -> Result<()> {
    let header = entry.header().clone();
    let entry_type = header.entry_type();

    if let Ok(existing) = std::fs::symlink_metadata(target) {
        let both_dirs = existing.is_dir() && entry_type.is_dir();
        if !both_dirs {
            remove_if_exists(target)?;
        }
    }

    match entry_type {
        tar::EntryType::Directory => {
            std::fs::create_dir_all(target)?;
            set_mode(target, header.mode()?)?;
        }
        tar::EntryType::Regular | tar::EntryType::Continuous => {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(target)?;
            io::copy(entry, &mut out)?;
            set_mode(target, header.mode()?)?;
        }
        tar::EntryType::Symlink => {
            let link_target = entry.link_name()?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "symlink with no target")
            })?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let _ = std::fs::remove_file(target);
            #[cfg(unix)]
            std::os::unix::fs::symlink(link_target, target)?;
        }
        tar::EntryType::Link => {
            let link_name = entry.link_name()?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "hard link with no target")
            })?;
            let link_source = safe_join(dest, &link_name)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let _ = std::fs::remove_file(target);
            match std::fs::hard_link(&link_source, target) {
                Ok(()) => {}
                // The link target might be something this same layer
                // already wrote earlier in the stream (fine, already
                // handled above) or, rarely, missing entirely (a
                // malformed/unsupported layer) — copy its content
                // instead of failing the whole extraction over one
                // ordering quirk, if it exists at all as a real file.
                Err(_) if link_source.exists() => {
                    std::fs::copy(&link_source, target)?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        tar::EntryType::Fifo | tar::EntryType::Char | tar::EntryType::Block => {
            // See this module's own doc comment: device nodes/FIFOs
            // need privilege a rootless caller doesn't have. Skipped,
            // not failed.
            let _ = written;
        }
        _ => {
            // XGlobalHeader/XHeader/GNU longname-longlink entries: the
            // `tar` crate already resolves these into the *next* real
            // entry's path/link-name transparently, so nothing reaches
            // here for them in practice; anything else genuinely
            // unrecognized is skipped rather than failing the whole
            // extraction.
        }
    }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Build an in-memory (uncompressed) tar stream from `(path, kind)`
    /// pairs for the simple cases (`Dir`/`File(content)`), used across
    /// most tests below.
    enum Entry<'a> {
        Dir(&'a str),
        File(&'a str, &'a [u8]),
        Symlink(&'a str, &'a str),
    }

    fn build_tar(entries: &[Entry]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for entry in entries {
            match entry {
                Entry::Dir(path) => {
                    let mut header = tar::Header::new_gnu();
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_mode(0o755);
                    header.set_size(0);
                    builder.append_data(&mut header, path, io::empty()).unwrap();
                }
                Entry::File(path, content) => {
                    let mut header = tar::Header::new_gnu();
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_mode(0o644);
                    header.set_size(content.len() as u64);
                    builder.append_data(&mut header, path, *content).unwrap();
                }
                Entry::Symlink(path, target) => {
                    let mut header = tar::Header::new_gnu();
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_mode(0o777);
                    header.set_size(0);
                    builder.append_link(&mut header, path, target).unwrap();
                }
            }
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn extracts_a_plain_file() {
        let dir = tempfile::tempdir().unwrap();
        let data = build_tar(&[Entry::File("hello.txt", b"hi there")]);
        apply(data.as_slice(), Compression::None, dir.path()).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("hello.txt")).unwrap(),
            b"hi there"
        );
    }

    #[test]
    fn extracts_nested_directories_and_preserves_mode() {
        let dir = tempfile::tempdir().unwrap();
        let data = build_tar(&[
            Entry::Dir("a"),
            Entry::Dir("a/b"),
            Entry::File("a/b/c.txt", b"nested"),
        ]);
        apply(data.as_slice(), Compression::None, dir.path()).unwrap();
        assert!(dir.path().join("a/b").is_dir());
        assert_eq!(
            std::fs::read(dir.path().join("a/b/c.txt")).unwrap(),
            b"nested"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join("a/b/c.txt"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o644);
        }
    }

    #[test]
    fn extracts_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let data = build_tar(&[
            Entry::File("real.txt", b"x"),
            Entry::Symlink("link.txt", "real.txt"),
        ]);
        apply(data.as_slice(), Compression::None, dir.path()).unwrap();
        let link = dir.path().join("link.txt");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("real.txt"));
    }

    #[test]
    fn extracts_hard_links() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(7);
        builder
            .append_data(&mut header, "orig.bin", &b"payload"[..])
            .unwrap();

        let mut link_header = tar::Header::new_gnu();
        link_header.set_entry_type(tar::EntryType::Link);
        link_header.set_mode(0o644);
        link_header.set_size(0);
        builder
            .append_link(&mut link_header, "linked.bin", "orig.bin")
            .unwrap();
        let data = builder.into_inner().unwrap();

        apply(data.as_slice(), Compression::None, dir.path()).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("linked.bin")).unwrap(),
            b"payload"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let a = std::fs::metadata(dir.path().join("orig.bin")).unwrap();
            let b = std::fs::metadata(dir.path().join("linked.bin")).unwrap();
            assert_eq!(a.ino(), b.ino(), "hard link should share an inode");
        }
    }

    #[test]
    fn whiteout_removes_a_file_from_a_lower_layer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gone.txt"), b"from a lower layer").unwrap();

        let data = build_tar(&[Entry::File(".wh.gone.txt", b"")]);
        apply(data.as_slice(), Compression::None, dir.path()).unwrap();

        assert!(!dir.path().join("gone.txt").exists());
        assert!(!dir.path().join(".wh.gone.txt").exists());
    }

    #[test]
    fn whiteout_removes_a_directory_tree_from_a_lower_layer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("olddir/sub")).unwrap();
        std::fs::write(dir.path().join("olddir/sub/f.txt"), b"x").unwrap();

        let data = build_tar(&[Entry::File(".wh.olddir", b"")]);
        apply(data.as_slice(), Compression::None, dir.path()).unwrap();

        assert!(!dir.path().join("olddir").exists());
    }

    #[test]
    fn opaque_whiteout_removes_lower_layer_siblings_but_keeps_this_layers_own_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("d")).unwrap();
        std::fs::write(dir.path().join("d/old.txt"), b"from below").unwrap();

        // This layer re-creates `d` as opaque and adds its own file —
        // the opaque marker's position in the stream (before its own
        // sibling entry here) must not matter.
        let data = build_tar(&[
            Entry::Dir("d"),
            Entry::File("d/.wh..wh..opq", b""),
            Entry::File("d/new.txt", b"from this layer"),
        ]);
        apply(data.as_slice(), Compression::None, dir.path()).unwrap();

        assert!(!dir.path().join("d/old.txt").exists());
        assert_eq!(
            std::fs::read(dir.path().join("d/new.txt")).unwrap(),
            b"from this layer"
        );
    }

    #[test]
    fn opaque_whiteout_after_the_new_entries_still_keeps_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("d")).unwrap();
        std::fs::write(dir.path().join("d/old.txt"), b"from below").unwrap();

        let data = build_tar(&[
            Entry::Dir("d"),
            Entry::File("d/new.txt", b"from this layer"),
            Entry::File("d/.wh..wh..opq", b""),
        ]);
        apply(data.as_slice(), Compression::None, dir.path()).unwrap();

        assert!(!dir.path().join("d/old.txt").exists());
        assert_eq!(
            std::fs::read(dir.path().join("d/new.txt")).unwrap(),
            b"from this layer"
        );
    }

    #[test]
    fn a_new_entry_replaces_a_lower_layers_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), b"old content").unwrap();

        let data = build_tar(&[Entry::File("f.txt", b"new content")]);
        apply(data.as_slice(), Compression::None, dir.path()).unwrap();

        assert_eq!(
            std::fs::read(dir.path().join("f.txt")).unwrap(),
            b"new content"
        );
    }

    #[test]
    fn a_directory_replacing_a_file_removes_the_file_first() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x"), b"was a file").unwrap();

        let data = build_tar(&[Entry::Dir("x")]);
        apply(data.as_slice(), Compression::None, dir.path()).unwrap();

        assert!(dir.path().join("x").is_dir());
    }

    #[test]
    fn rejects_a_path_traversal_entry() {
        let dir = tempfile::tempdir().unwrap();
        // `tar::Builder::append_data` refuses to build an archive
        // containing a `..` path at all (its own, separate safety
        // check), so a hostile entry has to be raw-constructed here to
        // prove `apply` itself would also reject one that somehow made
        // it into a hand-crafted/corrupt archive.
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(7);
        header.as_gnu_mut().unwrap().name[..15].copy_from_slice(b"../escape.txt\0\0");
        header.set_cksum();
        builder.append(&header, &b"hostile"[..]).unwrap();
        let data = builder.into_inner().unwrap();

        let err = apply(data.as_slice(), Compression::None, dir.path()).unwrap_err();
        assert!(matches!(err, LayerError::PathEscapesRoot(_)));
    }

    #[test]
    fn rejects_an_absolute_path_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(4);
        // The `tar` crate's own `Header::set_path` rejects absolute
        // paths outright, so this constructs the raw header field
        // directly to prove `apply` itself would reject one that
        // somehow got through (a hand-crafted/corrupt archive, not
        // something this crate's own `Builder` use would ever
        // produce).
        header.as_gnu_mut().unwrap().name[..12].copy_from_slice(b"/etc/passwd\0");
        header.set_cksum();
        builder.append(&header, &b"evil"[..]).unwrap();
        let data = builder.into_inner().unwrap();

        let err = apply(data.as_slice(), Compression::None, dir.path()).unwrap_err();
        assert!(matches!(err, LayerError::PathEscapesRoot(_)));
    }

    #[test]
    fn gzip_compressed_layer_extracts_the_same_as_uncompressed() {
        let dir = tempfile::tempdir().unwrap();
        let data = build_tar(&[Entry::File("g.txt", b"gzipped")]);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&data).unwrap();
        let gzipped = encoder.finish().unwrap();

        apply(gzipped.as_slice(), Compression::Gzip, dir.path()).unwrap();
        assert_eq!(std::fs::read(dir.path().join("g.txt")).unwrap(), b"gzipped");
    }

    #[test]
    fn zstd_compressed_layer_extracts_the_same_as_uncompressed() {
        let dir = tempfile::tempdir().unwrap();
        let data = build_tar(&[Entry::File("z.txt", b"zstd-compressed")]);
        let zstd_bytes = ruzstd::encoding::compress_to_vec(
            data.as_slice(),
            ruzstd::encoding::CompressionLevel::Fastest,
        );

        apply(zstd_bytes.as_slice(), Compression::Zstd, dir.path()).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("z.txt")).unwrap(),
            b"zstd-compressed"
        );
    }

    #[test]
    fn a_malformed_zstd_stream_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = apply(
            b"this is not a zstd frame at all".as_slice(),
            Compression::Zstd,
            dir.path(),
        )
        .unwrap_err();
        assert!(matches!(err, LayerError::InvalidZstd(_)), "{err:?}");
    }
}
