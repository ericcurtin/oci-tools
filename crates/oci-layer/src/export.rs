//! Turning a [`Change`] list (see [`crate::diff`]) into a real OCI
//! layer tar stream — the write side of [`crate::apply`], and the
//! piece a future `ociman build`'s own "commit this `RUN` step's
//! changes into a new layer" will need.
//!
//! Ported directly from real moby's own `ExportChanges`
//! (`~/git/moby/vendor/github.com/moby/go-archive/changes.go`), not
//! re-derived from the OCI image-spec's prose alone:
//!
//! * A [`ChangeKind::Deleted`] change becomes a whiteout entry: an
//!   empty file named `.wh.<basename>` in the same directory as the
//!   original path (see [`crate::apply`]'s own doc comment for the
//!   whiteout convention this mirrors on the read side). No opaque-
//!   directory marker (`.wh..wh..opq`) is ever emitted here —
//!   [`crate::diff`]'s naive algorithm (see its own doc comment) never
//!   needs one: it always emits one `Deleted` change per individually
//!   removed entry rather than "this whole directory's pre-existing
//!   lower-layer content is now gone", so an opaque marker would have
//!   nothing to represent that ordinary whiteouts don't already cover.
//! * An [`ChangeKind::Added`] or [`ChangeKind::Modified`] change
//!   becomes a real entry, read live from `root.join(&change.path)` —
//!   its actual current file/directory/symlink content and metadata
//!   (mode, uid, gid, mtime), for spec-compliant interoperability with
//!   any *other* tool that might later consume this layer (even though
//!   [`crate::apply`] doesn't restore every one of those fields on the
//!   read side yet — that's `apply`'s own separate, already-documented
//!   gap; this writer doesn't need to compensate for it).
//!
//! # What isn't handled yet
//!
//! Matches [`crate::apply`]'s and [`crate::diff`]'s own documented
//! scope limits: device nodes, FIFOs, and sockets are skipped outright
//! (not written, not failing the whole export) rather than archived,
//! since nothing in this project's own rootless containers can create
//! one to begin with (no `CAP_MKNOD`), and `apply` could never restore
//! one either; extended attributes are not preserved.
//!
//! # A vanished source file is skipped, not a hard error
//!
//! Real moby's own `ExportChanges` explicitly tolerates this
//! (`changes.go`'s own comment: "during e.g. a diff operation the
//! container can continue mutating the filesystem and we can see
//! transient errors from this") by logging and continuing past a
//! single failed `addTarFile` rather than aborting the whole export.
//! This crate has no equivalent background logger, so [`export`]
//! mirrors the same tolerance narrowly instead: a path that no longer
//! exists at all by the time it's this path's own turn to be written
//! (a real TOCTOU race between a [`crate::diff::changes`] call and
//! this one, if the two aren't called back-to-back against an
//! otherwise-quiescent tree) is skipped; any other I/O error
//! (permission denied, ...) still fails the whole export, since that's
//! not the specific transient race this exception is for.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::diff::{Change, ChangeKind};
use crate::{LayerError, Result, WHITEOUT_PREFIX};

/// Write a real OCI layer tar stream to `writer`, representing exactly
/// `changes` (as computed by [`crate::diff::changes`]) read live from
/// `root`. See this module's own doc comment for the whiteout
/// convention and scope limits.
pub fn export(root: &Path, changes: &[Change], writer: impl Write) -> Result<()> {
    let mut builder = tar::Builder::new(writer);
    // Every entry is read fresh via `lstat`/`readlink` below (a
    // symlink is archived *as* a symlink, matching `crate::diff`'s own
    // "compared by target string, not followed" stance) rather than
    // the `tar` crate's own default of dereferencing symlinks it's
    // asked to add.
    builder.follow_symlinks(false);
    let mut seen_inodes = std::collections::HashMap::new();

    for change in changes {
        match change.kind {
            ChangeKind::Deleted => write_whiteout(&mut builder, &change.path)?,
            ChangeKind::Added | ChangeKind::Modified => {
                write_entry(&mut builder, root, &change.path, &mut seen_inodes)?
            }
        }
    }

    let mut writer = builder.into_inner().map_err(LayerError::Io)?;
    writer.flush().map_err(LayerError::Io)
}

/// Write a real, flat tar of `root`'s entire current directory tree —
/// every file/directory/symlink it contains right now, no whiteouts,
/// no "only what changed" filtering at all. Unlike [`export`] (a
/// *layer diff*, this crate's own core concern), this is `ociman
/// export`'s own single caller: real `podman export`'s own "the whole
/// current filesystem, verbatim" semantics, which has nothing to do
/// with OCI layers or a base image at all. Entries are visited in a
/// deterministic order (parent directories always before their own
/// children, lexicographic by filename within a directory, matching
/// [`crate::diff::Snapshot::capture`]'s own walk) so the same
/// directory tree always produces byte-identical archives.
///
/// # Never crosses a mount-point boundary
///
/// A real, manually-reproduced bug this project's own `ociman export`
/// hit directly: exporting a *still-running* container's rootfs (whose
/// `/proc`/`/sys`/`/dev/pts` are actively bind-mounted onto that same
/// directory tree for the container's own lifetime) walked straight
/// into those live pseudo-filesystems too, producing a many-hundred-
/// megabyte archive (`/proc`'s own synthetic, effectively unbounded
/// content) instead of the real, few-megabyte image it should have
/// been — the walk never actually hung, it was just doing real,
/// enormous, wrong work. Fixed the same way real `tar --one-file-
/// system`/`rsync -x` both already do it: a subdirectory whose own
/// `st_dev` differs from `root`'s is still archived as an entry itself
/// (an empty directory, exactly what a real storage-driver-level
/// export would also show for a mount point it doesn't otherwise
/// track), but never recursed into.
pub fn export_tree(root: &Path, writer: impl Write) -> Result<()> {
    let mut builder = tar::Builder::new(writer);
    builder.follow_symlinks(false);

    let root_dev = fs::metadata(root)?.dev();
    let mut paths = Vec::new();
    collect_paths(root, Path::new(""), root_dev, &mut paths)?;
    let mut seen_inodes = std::collections::HashMap::new();
    for path in &paths {
        write_entry(&mut builder, root, path, &mut seen_inodes)?;
    }

    let mut writer = builder.into_inner().map_err(LayerError::Io)?;
    writer.flush().map_err(LayerError::Io)
}

fn collect_paths(
    root: &Path,
    relative: &Path,
    root_dev: u64,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    let dir = root.join(relative);
    let mut entries: Vec<_> = fs::read_dir(&dir)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let entry_relative = relative.join(entry.file_name());
        let file_type = entry.file_type()?;
        out.push(entry_relative.clone());
        if file_type.is_dir() {
            // `DirEntry::metadata` is `lstat`, matching this module's
            // own `write_entry`'s use of `symlink_metadata` elsewhere
            // -- a symlink can never reach here anyway (`file_type`
            // above already excludes it), so this is always the real
            // directory's own device, not a followed target's.
            let dev = entry.metadata()?.dev();
            if dev == root_dev {
                collect_paths(root, &entry_relative, root_dev, out)?;
            }
        }
    }
    Ok(())
}

fn write_whiteout(builder: &mut tar::Builder<impl Write>, path: &Path) -> Result<()> {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        // `crate::diff` never emits a `Deleted` change for the tree's
        // own root (which has no file name of its own) or for a
        // non-UTF-8 path (this crate's own walk only ever compares
        // real path components read back via `read_dir`, which are
        // already valid UTF-8 on every real image this project has
        // ever pulled) -- nothing to do if it ever somehow did.
        return Ok(());
    };
    let whiteout_name = format!("{WHITEOUT_PREFIX}{name}");
    let whiteout_path: PathBuf = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(whiteout_name),
        _ => PathBuf::from(whiteout_name),
    };

    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o000);
    header.set_size(0);
    builder
        .append_data(&mut header, whiteout_path, io::empty())
        .map_err(LayerError::Io)
}

/// `seen_inodes` maps a real `(dev, ino)` pair already written once in
/// *this* archive to the path it was first written under -- a
/// same-archive regular file sharing that pair (real `nlink() > 1`,
/// e.g. every one of a real busybox image's own ~380 applets, all
/// hardlinked to the one real binary) is written as a real tar
/// hardlink entry pointing back at that first path instead of a full
/// second copy of the same content. A real, measured difference: a
/// busybox rootfs exported without this is ~490MB (every applet's
/// ~1.2MB content duplicated ~380 times); with it, correctly a few
/// megabytes, matching what real `docker export`/`tar` themselves
/// produce for the identical real content.
fn write_entry(
    builder: &mut tar::Builder<impl Write>,
    root: &Path,
    path: &Path,
    seen_inodes: &mut std::collections::HashMap<(u64, u64), PathBuf>,
) -> Result<()> {
    let full = root.join(path);
    let metadata = match fs::symlink_metadata(&full) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let file_type = metadata.file_type();
    if !(file_type.is_file() || file_type.is_dir() || file_type.is_symlink()) {
        // Device node, FIFO, or socket -- see this module's own doc
        // comment.
        return Ok(());
    }

    if file_type.is_file() && metadata.nlink() > 1 {
        let key = (metadata.dev(), metadata.ino());
        if let Some(first_path) = seen_inodes.get(&key) {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Link);
            header.set_mode(metadata.mode() & 0o7777);
            header.set_size(0);
            return match builder.append_link(&mut header, path, first_path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            };
        }
        seen_inodes.insert(key, path.to_path_buf());
    }

    match builder.append_path_with_name(&full, path) {
        Ok(()) => Ok(()),
        // The narrower race between the `symlink_metadata` call just
        // above and `tar`'s own internal re-read of the same path --
        // see this module's own doc comment on vanished source files.
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{Snapshot, changes};
    use crate::{Compression, apply};
    use std::os::unix::fs::PermissionsExt;

    fn write_file(path: &Path, content: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn export_bytes(root: &Path, changes: &[Change]) -> Vec<u8> {
        let mut out = Vec::new();
        export(root, changes, &mut out).unwrap();
        out
    }

    /// The most convincing check: capture a real "before" state, seed
    /// a *separate* destination with that same "before" state, mutate
    /// the source into an "after" state, diff, export a layer tar of
    /// exactly that diff, apply it onto the destination, and confirm
    /// the destination now matches the source's own "after" state
    /// exactly -- a full, real diff/export/apply round trip.
    #[test]
    fn round_trips_a_mixed_change_set_through_diff_export_and_apply() {
        let source = tempfile::tempdir().unwrap();
        write_file(&source.path().join("keep.txt"), b"unchanged");
        write_file(&source.path().join("edit.txt"), b"original");
        write_file(&source.path().join("gone/deep.txt"), b"will be deleted");
        std::os::unix::fs::symlink("keep.txt", source.path().join("a-link")).unwrap();
        let before = Snapshot::capture(source.path()).unwrap();

        write_file(&source.path().join("edit.txt"), b"edited, now longer");
        let new_mtime = fs::metadata(source.path().join("edit.txt"))
            .unwrap()
            .modified()
            .unwrap()
            + std::time::Duration::from_secs(5);
        fs::File::open(source.path().join("edit.txt"))
            .unwrap()
            .set_modified(new_mtime)
            .unwrap();
        fs::remove_dir_all(source.path().join("gone")).unwrap();
        write_file(&source.path().join("added/new.txt"), b"brand new");

        let changes = changes(source.path(), &before).unwrap();
        let layer = export_bytes(source.path(), &changes);

        // Seed the destination with the same "before" state as the
        // source, then apply the freshly exported layer onto it.
        let dest = tempfile::tempdir().unwrap();
        write_file(&dest.path().join("keep.txt"), b"unchanged");
        write_file(&dest.path().join("edit.txt"), b"original");
        write_file(&dest.path().join("gone/deep.txt"), b"will be deleted");
        std::os::unix::fs::symlink("keep.txt", dest.path().join("a-link")).unwrap();

        apply(layer.as_slice(), Compression::None, dest.path()).unwrap();

        assert_eq!(
            fs::read(dest.path().join("keep.txt")).unwrap(),
            b"unchanged"
        );
        assert_eq!(
            fs::read(dest.path().join("edit.txt")).unwrap(),
            b"edited, now longer"
        );
        assert!(!dest.path().join("gone").exists());
        assert_eq!(
            fs::read(dest.path().join("added/new.txt")).unwrap(),
            b"brand new"
        );
        assert_eq!(
            fs::read_link(dest.path().join("a-link")).unwrap(),
            PathBuf::from("keep.txt")
        );
    }

    #[test]
    fn a_deleted_file_becomes_a_whiteout_entry_in_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("sub/gone.txt"), b"gone");
        let before = Snapshot::capture(dir.path()).unwrap();
        fs::remove_file(dir.path().join("sub/gone.txt")).unwrap();
        let changes = changes(dir.path(), &before).unwrap();

        let layer = export_bytes(dir.path(), &changes);
        let mut archive = tar::Archive::new(layer.as_slice());
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"sub/.wh.gone.txt".to_string()), "{names:?}");
        assert!(
            !names
                .iter()
                .any(|n| n.contains("gone.txt") && !n.contains(".wh.")),
            "{names:?}"
        );
    }

    #[test]
    fn a_top_level_deleted_file_becomes_a_top_level_whiteout() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("gone.txt"), b"gone");
        let before = Snapshot::capture(dir.path()).unwrap();
        fs::remove_file(dir.path().join("gone.txt")).unwrap();
        let changes = changes(dir.path(), &before).unwrap();

        let layer = export_bytes(dir.path(), &changes);
        let mut archive = tar::Archive::new(layer.as_slice());
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec![".wh.gone.txt".to_string()]);
    }

    #[test]
    fn an_added_directory_is_archived_as_a_directory_entry() {
        let dir = tempfile::tempdir().unwrap();
        let before = Snapshot::capture(dir.path()).unwrap();
        fs::create_dir(dir.path().join("newdir")).unwrap();
        let changes = changes(dir.path(), &before).unwrap();

        let layer = export_bytes(dir.path(), &changes);
        let mut archive = tar::Archive::new(layer.as_slice());
        let entries: Vec<_> = archive.entries().unwrap().map(|e| e.unwrap()).collect();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].header().entry_type().is_dir());
        assert_eq!(entries[0].path().unwrap().to_str().unwrap(), "newdir");
    }

    #[test]
    fn an_added_symlink_is_archived_with_its_own_target_not_dereferenced() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("real.txt"), b"actual content");
        let before = Snapshot::capture(dir.path()).unwrap();
        std::os::unix::fs::symlink("real.txt", dir.path().join("link")).unwrap();
        let changes = changes(dir.path(), &before).unwrap();

        let layer = export_bytes(dir.path(), &changes);
        let mut archive = tar::Archive::new(layer.as_slice());
        let entries: Vec<_> = archive.entries().unwrap().map(|e| e.unwrap()).collect();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].header().entry_type().is_symlink());
        assert_eq!(
            entries[0].link_name().unwrap().unwrap(),
            PathBuf::from("real.txt")
        );
    }

    #[test]
    fn a_modified_files_permissions_are_carried_into_the_archived_entry() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("f.txt"), b"content");
        let before = Snapshot::capture(dir.path()).unwrap();

        let mut perms = fs::metadata(dir.path().join("f.txt"))
            .unwrap()
            .permissions();
        perms.set_mode(0o600);
        fs::set_permissions(dir.path().join("f.txt"), perms).unwrap();
        let changes = changes(dir.path(), &before).unwrap();

        let layer = export_bytes(dir.path(), &changes);
        let mut archive = tar::Archive::new(layer.as_slice());
        let entries: Vec<_> = archive.entries().unwrap().map(|e| e.unwrap()).collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].header().mode().unwrap() & 0o777, 0o600);
    }

    #[test]
    fn an_empty_change_list_produces_a_valid_empty_archive() {
        let dir = tempfile::tempdir().unwrap();
        let layer = export_bytes(dir.path(), &[]);
        let mut archive = tar::Archive::new(layer.as_slice());
        assert_eq!(archive.entries().unwrap().count(), 0);
    }

    #[test]
    fn a_source_path_that_vanishes_before_export_is_skipped_not_a_hard_error() {
        // Simulates the TOCTOU race documented on `export` itself: a
        // `Change` that claims `Added`/`Modified` but whose live path
        // no longer exists by the time `export` gets to it.
        let dir = tempfile::tempdir().unwrap();
        let changes = vec![Change {
            path: PathBuf::from("never-existed.txt"),
            kind: ChangeKind::Added,
        }];
        let layer = export_bytes(dir.path(), &changes);
        let mut archive = tar::Archive::new(layer.as_slice());
        assert_eq!(archive.entries().unwrap().count(), 0);
    }

    fn export_tree_bytes(root: &Path) -> Vec<u8> {
        let mut out = Vec::new();
        export_tree(root, &mut out).unwrap();
        out
    }

    /// The real, manually-reproduced bug this fixes: a real busybox-
    /// shaped directory (several names all hardlinked to the exact
    /// same real inode, the same shape a real busybox image's own
    /// ~380 applets have) must archive only the *first* name's real
    /// content; every other name sharing that inode becomes a real
    /// tar hardlink entry, not a second full copy -- checked both by
    /// the archive's own total size (would be ~3x one file's content
    /// without this) and by re-extracting it and confirming every
    /// name still has the right content afterward.
    #[test]
    fn export_tree_writes_a_real_hardlink_entry_instead_of_duplicating_content() {
        let dir = tempfile::tempdir().unwrap();
        let big_content = vec![b'x'; 64 * 1024];
        write_file(&dir.path().join("real-binary"), &big_content);
        fs::hard_link(
            dir.path().join("real-binary"),
            dir.path().join("applet-one"),
        )
        .unwrap();
        fs::hard_link(
            dir.path().join("real-binary"),
            dir.path().join("applet-two"),
        )
        .unwrap();

        let archive_bytes = export_tree_bytes(dir.path());
        // Three ~64KB hardlinked files duplicated naively would be
        // ~192KB of file content alone; written correctly, only one
        // real copy plus two small (zero-size) hardlink header
        // entries -- well under half that.
        assert!(
            archive_bytes.len() < 2 * big_content.len(),
            "archive was {} bytes, expected well under {}",
            archive_bytes.len(),
            2 * big_content.len()
        );

        let mut archive = tar::Archive::new(archive_bytes.as_slice());
        let mut link_entries = 0;
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            if entry.header().entry_type() == tar::EntryType::Link {
                link_entries += 1;
            }
        }
        assert_eq!(link_entries, 2);

        let dest = tempfile::tempdir().unwrap();
        apply(archive_bytes.as_slice(), Compression::None, dest.path()).unwrap();
        assert_eq!(
            fs::read(dest.path().join("real-binary")).unwrap(),
            big_content
        );
        assert_eq!(
            fs::read(dest.path().join("applet-one")).unwrap(),
            big_content
        );
        assert_eq!(
            fs::read(dest.path().join("applet-two")).unwrap(),
            big_content
        );
    }

    /// Every real file/directory/symlink currently in `root`, not just
    /// what's changed relative to some earlier snapshot -- the real
    /// difference from `export` itself.
    #[test]
    fn export_tree_includes_every_current_path_with_real_content() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/b.txt"), b"hello from b");
        write_file(&dir.path().join("a/c/d.txt"), b"hello from d");
        std::os::unix::fs::symlink("b.txt", dir.path().join("a/link-to-b")).unwrap();

        let archive_bytes = export_tree_bytes(dir.path());
        let mut archive = tar::Archive::new(archive_bytes.as_slice());
        let mut paths: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "a".to_string(),
                "a/b.txt".to_string(),
                "a/c".to_string(),
                "a/c/d.txt".to_string(),
                "a/link-to-b".to_string(),
            ]
        );
    }

    /// Re-extracting an `export_tree` archive onto a fresh, empty
    /// directory reproduces the exact same file content and directory
    /// structure -- a full write/read round trip via this crate's own
    /// [`crate::apply`], not just an isolated tar-content check.
    #[test]
    fn export_tree_round_trips_through_apply_onto_an_empty_destination() {
        let source = tempfile::tempdir().unwrap();
        write_file(&source.path().join("a/b.txt"), b"real content");
        write_file(&source.path().join("top.txt"), b"top level file");

        let archive_bytes = export_tree_bytes(source.path());

        let dest = tempfile::tempdir().unwrap();
        apply(archive_bytes.as_slice(), Compression::None, dest.path()).unwrap();

        assert_eq!(
            fs::read(dest.path().join("a/b.txt")).unwrap(),
            b"real content"
        );
        assert_eq!(
            fs::read(dest.path().join("top.txt")).unwrap(),
            b"top level file"
        );
    }

    #[test]
    fn export_tree_of_an_empty_directory_is_a_valid_empty_archive() {
        let dir = tempfile::tempdir().unwrap();
        let archive_bytes = export_tree_bytes(dir.path());
        let mut archive = tar::Archive::new(archive_bytes.as_slice());
        assert_eq!(archive.entries().unwrap().count(), 0);
    }
}
