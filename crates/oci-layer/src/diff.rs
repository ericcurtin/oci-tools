//! Computing which files changed between two points in a directory
//! tree's own history — the reverse of [`crate::apply`]: instead of
//! turning a layer tar into filesystem changes, this turns real
//! filesystem changes into the abstract `Add`/`Modify`/`Delete` list a
//! future layer-*writing* step (an eventual `ociman build`'s own "what
//! did this `RUN` step change") will need to turn into a new layer
//! tar of its own.
//!
//! This project has no copy-on-write rootfs (no `overlay2`, per the
//! top-level README's own filesystem-policy design pillar) to read a
//! ready-made diff back out of, so this is the same "naive", walk-two-
//! trees-and-compare algorithm real `moby`'s own `vfs` storage driver
//! uses for exactly the same reason (checked directly against
//! `~/git/moby/vendor/github.com/moby/go-archive/{changes,
//! changes_unix}.go`, the code `daemon/graphdriver/fsdiff.go`'s
//! `NaiveDiffDriver.Diff` calls, not re-derived from documentation).
//!
//! # What counts as "changed" — checked directly, not guessed
//!
//! Matches real `statDifferent` (`changes_unix.go`) applied to `lstat`
//! (not `stat`) results — a changed path is one whose file *type*
//! (regular/directory/symlink — this crate's own scope, see below),
//! permission bits, uid, or gid differ, **plus**, only for
//! *non-directories*, a different size or modification time. A
//! directory's own mtime/size are deliberately *never* compared (real
//! moby's own code comment cites two real upstream bugs, moby#9874 and
//! PR #11422, for exactly this reason — a directory's mtime changes
//! spuriously just from adding/removing an entry inside it, which
//! would otherwise make *every* ancestor directory of *any* change
//! look independently "modified" for a reason that has nothing to do
//! with the directory's own content).
//!
//! A symlink is compared by its **target string** (via `readlink`),
//! not by `lstat` alone — real moby's own naive differ, by its own
//! authors' account, only compares `lstat` fields and can miss a
//! changed symlink target with an unchanged mtime; this module closes
//! that gap instead of reproducing it (matching real `containerd`'s
//! own more careful equivalent, `continuity/fs`'s `sameFile`, which
//! does compare symlink targets explicitly).
//!
//! Unlike real moby, this module does **not** need the "tar truncates
//! mtimes to whole seconds" workaround (`sameFsTime`): that quirk only
//! matters when comparing a *live* filesystem's mtime against one
//! *restored from a tar extraction*, and [`crate::apply`] doesn't
//! restore original mtimes at all (see its own doc comment) — every
//! file this module ever compares is either genuinely untouched since
//! its own most recent real write, or was genuinely rewritten by
//! something between the two snapshots, so an exact, full-precision
//! mtime comparison is both simpler and more precise here than real
//! moby's own workaround would be.
//!
//! # Directories bubble up; deletions do not cross an already-deleted ancestor
//!
//! A directory none of whose own fields changed is still recorded as
//! [`ChangeKind::Modified`] if *any* descendant of it changed at all
//! (real moby's own `addChanges`, checked directly) — needed so a
//! layer-writing step can still emit (and a layer-*applying* step can
//! still restore) that directory's own permissions even when nothing
//! about the directory *entry* itself differs. Bubbling stops at an
//! ancestor that no longer exists at all in the "after" snapshot
//! (i.e. that ancestor was itself wholly deleted) — that ancestor's
//! own `Deleted` entry, produced independently by the ordinary
//! before/after comparison, already covers everything below it.
//!
//! # What isn't handled yet
//!
//! Matches [`crate::apply`]'s own already-documented scope limits,
//! for the same reasons: device nodes, FIFOs, and sockets are treated
//! as an opaque "other" file type (compared only by that fact, not by
//! any type-specific field like a device's major/minor numbers) since
//! nothing in this project ever creates one on extraction in the
//! first place (no `CAP_MKNOD` rootless); extended attributes
//! (`security.capability` included) aren't compared at all, unlike
//! real moby's own naive differ, which does special-case that one
//! attrible — a real, narrower gap than upstream, accepted for now
//! since nothing in this project's own layer-*application* path
//! extracts or restores extended attributes to begin with (see
//! [`crate::apply`]'s own "what isn't handled yet").

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// What kind of change happened to one path between two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Present after, absent before.
    Added,
    /// Present in both, but differs (see the module's own doc comment
    /// for exactly what's compared) — or a directory whose own
    /// content changed even though its own fields didn't.
    Modified,
    /// Present before, absent after.
    Deleted,
}

/// One changed path, relative to the directory tree's own root (no
/// leading `/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Relative to the diff root, no leading `/`.
    pub path: PathBuf,
    /// What kind of change this is.
    pub kind: ChangeKind,
}

/// The type-relevant part of a path's own metadata this module
/// compares — deliberately not the raw [`fs::Metadata`] itself, so a
/// [`Snapshot`] can be held in memory well after the underlying file
/// might have changed again. `Serialize`/`Deserialize`: a [`Snapshot`]
/// captured now may need comparing against much later — e.g. `ociman
/// diff`, comparing a container's own current filesystem against a
/// snapshot `ociman run` persisted to disk back when it first created
/// that container (see `docs/design/0149`'s own doc comment for why a
/// *persisted* snapshot, rather than a second, fresh extraction of
/// the same base image, is the only correct way to do that).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct EntryMeta {
    kind: EntryKind,
    /// Permission bits only (`mode & 0o7777`), not the file-type bits
    /// packed into the same field — those are already captured by
    /// `kind`.
    permissions: u32,
    uid: u32,
    gid: u32,
    /// Only meaningful for [`EntryKind::Other`] (e.g. a device node);
    /// unused for every other kind.
    rdev: u64,
    /// Only meaningful for [`EntryKind::Regular`]/[`EntryKind::Other`].
    size: u64,
    /// `None` for a directory (never compared); `Some` for every
    /// other kind, including symlinks (a symlink's own mtime, same as
    /// a regular file's).
    mtime: Option<(i64, i64)>,
    /// Only populated for [`EntryKind::Symlink`].
    symlink_target: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum EntryKind {
    Regular,
    Directory,
    Symlink,
    /// Anything else (device node, FIFO, socket) — see the module's
    /// own doc comment for why this project doesn't need to
    /// distinguish these further yet.
    Other,
}

fn entry_kind(metadata: &fs::Metadata) -> EntryKind {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_file() {
        EntryKind::Regular
    } else {
        EntryKind::Other
    }
}

/// A captured, in-memory record of every path under some directory
/// tree, along with the metadata this module needs to later detect
/// what changed — cheap enough to hold onto for the duration of, say,
/// a build's own `RUN` step (no file contents are copied, only
/// `lstat`-shaped metadata for each entry). Also cheap enough to
/// serialize and persist to disk for comparing much later (`ociman
/// diff`) — see [`EntryMeta`]'s own doc comment.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    entries: BTreeMap<PathBuf, EntryMeta>,
}

impl Snapshot {
    /// Walk `root` recursively (following no symlinks — every entry is
    /// `lstat`ed, matching real moby's own naive differ) and capture
    /// every path's own relevant metadata.
    pub fn capture(root: &Path) -> io::Result<Snapshot> {
        let mut entries = BTreeMap::new();
        walk(root, Path::new(""), &mut entries)?;
        Ok(Snapshot { entries })
    }
}

fn walk(root: &Path, relative: &Path, out: &mut BTreeMap<PathBuf, EntryMeta>) -> io::Result<()> {
    let dir = root.join(relative);
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let entry_relative = relative.join(&name);
        let metadata = entry.metadata()?; // `DirEntry::metadata` is `lstat`, not `stat`.
        let kind = entry_kind(&metadata);
        let symlink_target = if kind == EntryKind::Symlink {
            Some(fs::read_link(root.join(&entry_relative))?)
        } else {
            None
        };
        let mtime = if kind == EntryKind::Directory {
            None
        } else {
            Some((metadata.mtime(), metadata.mtime_nsec()))
        };
        out.insert(
            entry_relative.clone(),
            EntryMeta {
                kind,
                permissions: metadata.mode() & 0o7777,
                uid: metadata.uid(),
                gid: metadata.gid(),
                rdev: metadata.rdev(),
                size: metadata.size(),
                mtime,
                symlink_target,
            },
        );
        if kind == EntryKind::Directory {
            walk(root, &entry_relative, out)?;
        }
    }
    Ok(())
}

/// Whether `before` and `after` (the same path, present in both
/// snapshots) count as different — see the module's own doc comment
/// for exactly which fields.
fn entry_differs(before: &EntryMeta, after: &EntryMeta) -> bool {
    if before.kind != after.kind
        || before.permissions != after.permissions
        || before.uid != after.uid
        || before.gid != after.gid
    {
        return true;
    }
    if before.kind == EntryKind::Other && before.rdev != after.rdev {
        return true;
    }
    if before.kind == EntryKind::Symlink {
        return before.symlink_target != after.symlink_target;
    }
    if before.kind != EntryKind::Directory && before.mtime != after.mtime {
        return true;
    }
    if before.kind == EntryKind::Regular && before.size != after.size {
        return true;
    }
    false
}

/// Compute every change in `root`'s own current, live state relative
/// to `before` (a [`Snapshot`] captured at some earlier point — see
/// the module's own doc comment for the exact algorithm). Returned in
/// path order (parent directories always sort before their own
/// children), ready for a future layer-writing step to turn directly
/// into a tar stream.
pub fn changes(root: &Path, before: &Snapshot) -> io::Result<Vec<Change>> {
    let after = Snapshot::capture(root)?;
    let mut result: BTreeMap<PathBuf, ChangeKind> = BTreeMap::new();

    for (path, after_meta) in &after.entries {
        match before.entries.get(path) {
            None => {
                result.insert(path.clone(), ChangeKind::Added);
            }
            Some(before_meta) => {
                if entry_differs(before_meta, after_meta) {
                    result.insert(path.clone(), ChangeKind::Modified);
                }
            }
        }
    }
    for path in before.entries.keys() {
        if after.entries.contains_key(path) {
            continue;
        }
        // Only emit a `Deleted` entry for the *first* level at which a
        // path disappears — a path whose own immediate parent is
        // *also* gone from `after` is just a descendant of an already
        // wholly-removed subtree, already fully implied by that
        // parent's own `Deleted` entry (matches real moby's own
        // `addChanges`: the "leftover = deleted" case emits exactly
        // one `Delete` per orphaned child, without recursing into its
        // own children at all — checked directly, this module's own
        // first version got this wrong by treating every flattened
        // path independently instead).
        let parent_still_present = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => after.entries.contains_key(parent),
            _ => true, // a top-level entry has no ancestor left to imply it
        };
        if parent_still_present {
            result.insert(path.clone(), ChangeKind::Deleted);
        }
    }

    // Bubble up: every ancestor directory of a changed path that still
    // exists in `after` gets recorded as `Modified` too, unless it
    // already has a more specific entry of its own. Walking every
    // initial change's own ancestors all the way up to the root in one
    // pass (rather than a fixed-point/worklist loop) is enough on its
    // own: an ancestor's own bubbled-up `Modified` entry never needs
    // *its own* ancestors bubbled any further than this same walk
    // already reaches.
    let initial_paths: Vec<PathBuf> = result.keys().cloned().collect();
    for path in initial_paths {
        let mut current = path.as_path();
        while let Some(parent) = current.parent() {
            if parent.as_os_str().is_empty() {
                break; // reached the tree's own root
            }
            if !after.entries.contains_key(parent) {
                break; // this ancestor was itself wholly deleted
            }
            if let std::collections::btree_map::Entry::Vacant(entry) =
                result.entry(parent.to_path_buf())
            {
                entry.insert(ChangeKind::Modified);
            }
            current = parent;
        }
    }

    Ok(result
        .into_iter()
        .map(|(path, kind)| Change { path, kind })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write_file(path: &Path, content: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn no_changes_between_identical_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/b.txt"), b"hello");
        let before = Snapshot::capture(dir.path()).unwrap();
        let result = changes(dir.path(), &before).unwrap();
        assert!(result.is_empty(), "{result:?}");
    }

    #[test]
    fn detects_an_added_file_and_bubbles_up_its_new_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("existing.txt"), b"hi");
        let before = Snapshot::capture(dir.path()).unwrap();

        write_file(&dir.path().join("newdir/newfile.txt"), b"new");
        let result = changes(dir.path(), &before).unwrap();

        assert_eq!(
            result,
            vec![
                Change {
                    path: PathBuf::from("newdir"),
                    kind: ChangeKind::Added,
                },
                Change {
                    path: PathBuf::from("newdir/newfile.txt"),
                    kind: ChangeKind::Added,
                },
            ]
        );
    }

    #[test]
    fn detects_a_modified_files_content_via_mtime_or_size() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("f.txt"), b"before");
        let before = Snapshot::capture(dir.path()).unwrap();

        // Force a real, later mtime -- a same-second rewrite could
        // otherwise (rarely) coincide with the original in a fast
        // test, so bump it forward explicitly rather than relying on
        // wall-clock timing alone.
        write_file(&dir.path().join("f.txt"), b"after-longer-content");
        let new_mtime = fs::metadata(dir.path().join("f.txt"))
            .unwrap()
            .modified()
            .unwrap()
            + std::time::Duration::from_secs(5);
        let file = fs::File::open(dir.path().join("f.txt")).unwrap();
        file.set_modified(new_mtime).unwrap();

        let result = changes(dir.path(), &before).unwrap();
        assert_eq!(
            result,
            vec![Change {
                path: PathBuf::from("f.txt"),
                kind: ChangeKind::Modified,
            }]
        );
    }

    #[test]
    fn detects_a_deleted_file_and_bubbles_up_its_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("keep.txt"), b"keep");
        write_file(&dir.path().join("sub/gone.txt"), b"gone");
        let before = Snapshot::capture(dir.path()).unwrap();

        fs::remove_file(dir.path().join("sub/gone.txt")).unwrap();
        let result = changes(dir.path(), &before).unwrap();

        assert_eq!(
            result,
            vec![
                Change {
                    path: PathBuf::from("sub"),
                    kind: ChangeKind::Modified,
                },
                Change {
                    path: PathBuf::from("sub/gone.txt"),
                    kind: ChangeKind::Deleted,
                },
            ]
        );
    }

    #[test]
    fn deleting_a_whole_directory_does_not_also_report_stale_ancestors_beyond_it() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/b/c.txt"), b"deep");
        let before = Snapshot::capture(dir.path()).unwrap();

        fs::remove_dir_all(dir.path().join("a")).unwrap();
        let result = changes(dir.path(), &before).unwrap();

        // Only `a` itself is reported deleted -- its own children
        // don't get separate, redundant entries once their own parent
        // is already gone from `after` entirely (matches real moby's
        // own "stop bubbling at an already-deleted ancestor" rule,
        // applied here from the opposite/deletion direction: nothing
        // for `a/b` or `a/b/c.txt` individually).
        assert_eq!(
            result,
            vec![Change {
                path: PathBuf::from("a"),
                kind: ChangeKind::Deleted,
            }]
        );
    }

    #[test]
    fn directory_mtime_alone_never_counts_as_a_change() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("d")).unwrap();
        let before = Snapshot::capture(dir.path()).unwrap();

        // Touch the directory's own mtime (e.g. by adding then
        // removing a file inside it -- a real, common way a
        // directory's mtime changes without its own *content*, as
        // seen from the outside, actually differing) without leaving
        // any net change behind.
        write_file(&dir.path().join("d/tmp.txt"), b"tmp");
        fs::remove_file(dir.path().join("d/tmp.txt")).unwrap();

        let result = changes(dir.path(), &before).unwrap();
        assert!(result.is_empty(), "{result:?}");
    }

    #[test]
    fn detects_permission_and_ownership_changes() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("f.txt"), b"content");
        let before = Snapshot::capture(dir.path()).unwrap();

        let mut perms = fs::metadata(dir.path().join("f.txt"))
            .unwrap()
            .permissions();
        perms.set_mode(0o600);
        fs::set_permissions(dir.path().join("f.txt"), perms).unwrap();

        let result = changes(dir.path(), &before).unwrap();
        assert_eq!(
            result,
            vec![Change {
                path: PathBuf::from("f.txt"),
                kind: ChangeKind::Modified,
            }]
        );
    }

    #[test]
    fn detects_a_changed_symlink_target_even_with_no_other_field_different() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("original-target", dir.path().join("link")).unwrap();
        let before = Snapshot::capture(dir.path()).unwrap();

        fs::remove_file(dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink("different-target", dir.path().join("link")).unwrap();

        let result = changes(dir.path(), &before).unwrap();
        assert_eq!(
            result,
            vec![Change {
                path: PathBuf::from("link"),
                kind: ChangeKind::Modified,
            }]
        );
    }

    #[test]
    fn a_combined_scenario_matching_real_moby_own_test_shape() {
        // Mirrors the shape of real moby's own `TestChangesWithChanges`
        // (`~/git/moby/vendor/github.com/moby/go-archive`'s upstream
        // test suite, `changes_test.go`): one deleted file, one
        // modified file, and one new file in a brand new subfolder, all
        // in a single snapshot-to-snapshot comparison.
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("dir1/file1-1"), b"original");
        write_file(&dir.path().join("dir1/file1-2"), b"will be deleted");
        let before = Snapshot::capture(dir.path()).unwrap();

        write_file(
            &dir.path().join("dir1/file1-1"),
            b"modified, longer content",
        );
        let new_mtime = fs::metadata(dir.path().join("dir1/file1-1"))
            .unwrap()
            .modified()
            .unwrap()
            + std::time::Duration::from_secs(5);
        fs::File::open(dir.path().join("dir1/file1-1"))
            .unwrap()
            .set_modified(new_mtime)
            .unwrap();
        fs::remove_file(dir.path().join("dir1/file1-2")).unwrap();
        write_file(&dir.path().join("dir1/subfolder/newfile"), b"new");

        let result = changes(dir.path(), &before).unwrap();
        assert_eq!(
            result,
            vec![
                Change {
                    path: PathBuf::from("dir1"),
                    kind: ChangeKind::Modified,
                },
                Change {
                    path: PathBuf::from("dir1/file1-1"),
                    kind: ChangeKind::Modified,
                },
                Change {
                    path: PathBuf::from("dir1/file1-2"),
                    kind: ChangeKind::Deleted,
                },
                Change {
                    path: PathBuf::from("dir1/subfolder"),
                    kind: ChangeKind::Added,
                },
                Change {
                    path: PathBuf::from("dir1/subfolder/newfile"),
                    kind: ChangeKind::Added,
                },
            ]
        );
    }

    #[test]
    fn a_file_replaced_by_a_directory_of_the_same_name_is_modified() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("x"), b"was a file");
        let before = Snapshot::capture(dir.path()).unwrap();

        fs::remove_file(dir.path().join("x")).unwrap();
        fs::create_dir(dir.path().join("x")).unwrap();

        let result = changes(dir.path(), &before).unwrap();
        assert_eq!(
            result,
            vec![Change {
                path: PathBuf::from("x"),
                kind: ChangeKind::Modified,
            }]
        );
    }

    /// `Snapshot` needs to round-trip through JSON exactly (`ociman
    /// diff`, see `docs/design/0149`, persists one to disk with
    /// `serde_json` and loads it back much later, potentially after
    /// this same process has long since exited): a symlink target,
    /// a directory (whose own `mtime` is always `None`), and a plain
    /// regular file all present in the same snapshot, comparing a
    /// freshly re-parsed copy against the current real directory
    /// produces the exact same (empty) diff a direct, in-memory
    /// `Snapshot` would.
    #[test]
    fn snapshot_round_trips_through_json_and_still_diffs_correctly() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/b.txt"), b"hello");
        #[cfg(unix)]
        std::os::unix::fs::symlink("b.txt", dir.path().join("a/link")).unwrap();

        let before = Snapshot::capture(dir.path()).unwrap();
        let json = serde_json::to_vec(&before).unwrap();
        let reloaded: Snapshot = serde_json::from_slice(&json).unwrap();

        let result = changes(dir.path(), &reloaded).unwrap();
        assert!(result.is_empty(), "{result:?}");

        // And a real, genuine change is still detected against the
        // reloaded copy, exactly as it would be against the original
        // (its own parent directory "a" bubbles up too, matching the
        // module's own already-established "any changed descendant
        // marks its ancestors Modified too" rule).
        write_file(&dir.path().join("a/b.txt"), b"changed");
        let result = changes(dir.path(), &reloaded).unwrap();
        assert_eq!(
            result,
            vec![
                Change {
                    path: PathBuf::from("a"),
                    kind: ChangeKind::Modified,
                },
                Change {
                    path: PathBuf::from("a/b.txt"),
                    kind: ChangeKind::Modified,
                },
            ]
        );
    }
}
