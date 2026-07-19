# Design note 0045: filesystem diffing for a future build's own layer commits (milestone 4)

Status: implemented (the diff algorithm only — see "What's still not
here"; not yet wired into anything)
Scope: `crates/oci-layer/src/diff.rs`.

`oci-layer` has only ever gone one direction: a layer tar *in*, applied
onto a rootfs. An eventual `ociman build` needs the reverse for real:
after a `RUN` step finishes, it needs to know *what changed* in the
rootfs so it can commit that as a brand new layer. This is the
concrete missing piece every one of the Dockerfile-parsing increments
(0039-0043) already flagged as "actual build execution ... still all
future work" without saying *how* — this increment is that "how", for
the diffing half specifically (turning the diff into an actual layer
tarball is a separate, later increment; see "What's still not here").

## The "naive diff" approach, matching real `moby`'s own `vfs` storage driver — for the same reason

This project has no copy-on-write rootfs (no `overlay2` — the
top-level README's own filesystem-policy pillar rules that out
entirely) to read a ready-made diff back out of. Real `moby` hits
exactly the same wall for its own `vfs` storage driver (and any other
driver without native layer diffing), and solves it with a "naive"
algorithm: walk both directory trees and compare — checked directly
against `~/git/moby/vendor/github.com/moby/go-archive/{changes,
changes_unix}.go` (the code `daemon/graphdriver/fsdiff.go`'s
`NaiveDiffDriver.Diff` actually calls), not re-derived from
documentation or guessed from first principles. `containerd`'s own
independent, architecturally-equivalent "generic, works with any
filesystem" differ (`continuity/fs/diff.go`) was cross-checked too, as
a second reference for the trickier edge cases (see below).

## What counts as "changed" — every rule checked directly, several surprising enough to be worth writing down

* **A directory's own mtime/size are never compared at all.** Real
  moby's own code comment cites two real upstream bugs (moby#9874, PR
  #11422) for exactly this: a directory's mtime changes just from
  adding or removing an entry inside it, which would otherwise make
  *every* ancestor of *any* change look independently modified for a
  reason that has nothing to do with the directory's own content.
* **A changed directory still gets reported, via "bubbling up", if any
  descendant of it changed at all** — even though its own fields are
  identical — needed so a later layer-writing step can still emit (and
  a layer-*applying* step can still restore) that directory's own
  permissions. Bubbling stops the moment an ancestor no longer exists
  at all in the "after" snapshot: that ancestor's own `Deleted` entry
  (produced by the ordinary before/after comparison) already covers
  everything below it, so there's nothing further to bubble into.
* **A whole subtree being deleted produces exactly *one* `Deleted`
  entry, not one per descendant.** This module's own first version got
  this wrong — it iterated every path in the "before" snapshot
  independently and flagged every one absent from "after" as deleted,
  which (since the flattened snapshot map has a separate entry for
  every path at every depth) reported a deleted directory's *entire
  subtree*, one entry per file, instead of the single top-level
  `Deleted` real moby's own algorithm produces (its own recursive
  `addChanges` only ever emits one `Delete` for an orphaned child, with
  no further recursion into that child's own children at all once
  there's no corresponding node on the "after" side to pair it with).
  Caught by a dedicated test (`deleting_a_whole_directory_does_not_
  also_report_stale_ancestors_beyond_it`) before this was ever wired
  into anything, not after — fixed by only ever emitting `Deleted` for
  a path whose own immediate parent is *still present* in "after".
* **A symlink is compared by its actual target string (`readlink`), not
  left to `lstat` alone.** Real moby's own naive differ, by its own
  authors' account, can miss a changed symlink target if the mtime
  happens not to differ (an `lstat`-only comparison can't see a
  symlink's own content at all) — this module closes that gap outright
  rather than reproducing it, matching real `containerd`'s own more
  careful equivalent (`continuity/fs`'s `sameFile`, which does compare
  symlink targets explicitly) instead of moby's simpler one.
* **No `sameFsTime` "tar truncates mtimes to whole seconds" workaround
  is needed at all** — that quirk in real moby only matters when
  comparing a live filesystem's mtime against one *restored from a tar
  extraction*; `oci_layer::apply` never restores original mtimes in
  the first place (see its own doc comment), so every file this module
  ever compares was genuinely either untouched or genuinely rewritten
  between the two snapshots it's given — an exact, full-precision
  comparison is simpler *and* more precise here than replicating real
  moby's own workaround would be.

## Scoped narrower than real moby in one place, and it's a real, not merely cosmetic, gap

Extended attributes (including `security.capability`, which real
moby's own naive differ does specifically check) aren't compared at
all. Accepted for now since nothing in this project's own layer-
*application* path (`oci_layer::apply`) extracts or restores extended
attributes to begin with (already documented there) — comparing
something `apply` can never actually produce a difference in would be
dead code, not extra correctness.

## API

`Snapshot::capture(root) -> Snapshot`: walks `root` recursively
(`lstat`, never following symlinks) and records each path's own
type, permission bits, uid, gid, size/mtime (regular files and
symlinks only — never directories), and symlink target — cheap enough
to hold in memory for the duration of, say, a build's own `RUN` step,
since no file *contents* are ever copied, only metadata. `changes(root,
&before) -> Vec<Change>`: re-captures `root`'s own current, live state
and diffs it against an earlier `Snapshot`, in path order (parents
always sort before their own children — directly useful for a future
tar-writing step, which needs to create a directory before writing
anything inside it).

Deliberately designed around "capture once before, compare against the
live filesystem after" rather than "diff two separate directories on
disk" — the natural, efficient shape for the real intended use (record
a snapshot immediately before a `RUN` step, then diff against the same
rootfs's own live state immediately after it finishes) that avoids
ever needing a second, wasteful full copy of the rootfs just to have
"two directories" to compare.

## Real, automated tests

10 unit tests: no changes between identical snapshots; an added file
with its new parent directory correctly bubbled up; a modified file's
content detected via mtime/size; a deleted file with its parent
bubbled up; the real bug above (a whole deleted directory produces
exactly one `Deleted` entry, not one per descendant); a directory's
own mtime alone never counting as a change; permission/ownership
changes; a changed symlink target with no other field different; a
file replaced by a directory of the same name (a full file-type
change); and a combined scenario deliberately shaped to match real
moby's own `TestChangesWithChanges` fixture (one deletion, one
modification, one new file in a new subfolder, all at once) — checked
against real moby's own documented expected output shape, not
invented.

## Performance

Not called from anywhere yet, so zero runtime impact on any existing
hot path by construction, same reasoning every earlier increment in
this Dockerfile/build pipeline (0039-0044) has used for its own
not-yet-wired-in primitive.

## What's still not here

* Turning a `Vec<Change>` into an actual layer tarball (writing real
  file/directory/symlink entries for `Added`/`Modified`, and
  `.wh.<name>` whiteout entries for `Deleted` — real moby's own
  separate `ExportChanges` step, checked directly: whiteout-file
  *naming* is entirely a job of this future tar-writing step, not the
  diff algorithm itself, which only ever produces the abstract
  `Add`/`Modify`/`Delete` list this increment ships).
* Device nodes, FIFOs, and sockets are only compared as an opaque
  "other" file type (not by their own type-specific fields like a
  device's major/minor numbers) — matches `oci_layer::apply`'s own
  existing scope limit (nothing this project extracts ever creates one
  in the first place, rootless `CAP_MKNOD` restrictions), not a new gap
  introduced here.
* Extended attributes, as covered above.
* Everything else 0039-0044 already listed: `ONBUILD`/`HEALTHCHECK`,
  `--build-arg`, the build cache, dependency-ordered execution actually
  running anything, and `ociman build` itself.
