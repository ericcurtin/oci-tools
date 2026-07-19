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

    for change in changes {
        match change.kind {
            ChangeKind::Deleted => write_whiteout(&mut builder, &change.path)?,
            ChangeKind::Added | ChangeKind::Modified => {
                write_entry(&mut builder, root, &change.path)?
            }
        }
    }

    let mut writer = builder.into_inner().map_err(LayerError::Io)?;
    writer.flush().map_err(LayerError::Io)
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

fn write_entry(builder: &mut tar::Builder<impl Write>, root: &Path, path: &Path) -> Result<()> {
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
}
