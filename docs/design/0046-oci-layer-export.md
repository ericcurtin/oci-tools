# Design note 0046: turning a `Vec<Change>` into a real layer tarball (milestone 4)

Status: implemented (the tar-writing step only — not yet wired into
anything)
Scope: `crates/oci-layer/src/export.rs`.

0045 shipped the "diff" half of an eventual `ociman build`'s own "what
did this `RUN` step change, and how do I turn that into a new layer"
problem — a `Vec<Change>`, but only the *abstract* `Add`/`Modify`/
`Delete` list, explicitly flagged there as needing a separate step to
become an actual layer tar. This increment is that step: `oci_layer::
export`, the write-side counterpart of the existing `oci_layer::apply`.

## Ported directly from real moby's own `ExportChanges` — not re-derived

Checked directly against `~/git/moby/vendor/github.com/moby/go-archive/
changes.go`'s `ExportChanges`, not guessed from the OCI image-spec's
prose alone:

* A `Deleted` change becomes a whiteout: an empty file named
  `.wh.<basename>` in the *same directory* as the original path (real
  moby: `filepath.Join(filepath.Dir(change.Path), WhiteoutPrefix +
  filepath.Base(change.Path))`, `WhiteoutPrefix = ".wh."` — matches
  `oci_layer::apply`'s own existing whiteout convention exactly, since
  both sides were checked against the same real reference).
* An `Added`/`Modified` change becomes a real entry read live from
  `root.join(&change.path)` — real moby: `ta.addTarFile(filepath.Join(
  dir, change.Path), change.Path[1:])`, which reads the file's actual
  current mode/uid/gid/mtime/size/content (or symlink target) straight
  off the filesystem.
* **No opaque-directory marker (`.wh..wh..opq`) is ever emitted.** Real
  moby's own `ExportChanges` doesn't emit one either — it only exists
  in `oci_layer::apply`'s own *reading* side because some *other* tool's
  layers can contain one. `crate::diff`'s naive algorithm (checked
  directly, see 0045) never produces the situation an opaque marker
  exists to represent ("this whole directory's pre-existing lower-layer
  content should be wiped, even entries not otherwise individually
  changed") — it always emits one ordinary `Deleted` change per
  individually removed entry instead, which ordinary whiteouts already
  cover completely.

## A vanished source file is skipped, not a hard error — grounded directly, not a guess

Real moby's own comment on this exact case (`changes.go`): *"during
e.g. a diff operation the container can continue mutating the
filesystem and we can see transient errors from this"* — real moby logs
and continues past one failed `addTarFile` rather than aborting the
whole export. This crate has no equivalent background logger, so
`export` mirrors the same tolerance at the same narrow granularity
instead of either extreme (neither "silently ignore any error" nor
"abort the whole export over one racy path"): a path that's already
gone by the time it's this path's own turn (checked twice — once via an
explicit `symlink_metadata` before deciding whether to skip a device
node/FIFO/socket, and once by treating a `NotFound` from `tar`'s own
internal read as the same case) is skipped; any *other* I/O error still
fails the whole export.

## `follow_symlinks(false)` — a real, easy-to-miss default to get right

The `tar` crate's own `Builder::follow_symlinks` defaults to `true`
(dereference symlinks when adding them) — checked directly against
`tar-0.4.46`'s own source, not assumed. Left at the default, every
symlink `crate::diff` ever reports as changed (compared by its own
*target string* — see 0045) would silently be archived as a regular
copy of whatever it points at instead of as a symlink, which would both
misrepresent the change and, if the symlink's target doesn't currently
resolve inside `root` at all (a real, common shape — an absolute-path
symlink into a location that only exists once the container is
actually running), fail outright. `export` calls
`builder.follow_symlinks(false)` explicitly before adding any entry.

## Device nodes, FIFOs, sockets — skipped, matching `apply`'s and `diff`'s own stance

Filtered out by an explicit `file_type()` check before ever calling
into `tar`'s own path-adding helper (which would otherwise happily
special-case and archive a FIFO/device node itself, rather than
skipping it) — kept consistent with `oci_layer::apply`, which could
never extract one of these back out anyway (no `CAP_MKNOD` rootless).

## Real, automated tests — including a full diff/export/apply round trip

The most convincing test: capture a real "before" snapshot, seed a
*separate* destination directory with that same "before" state, mutate
the *source* into an "after" state (an edited file, a whole deleted
subtree, a new file in a new directory, an added symlink), diff,
`export` a layer tar of exactly that diff, `apply` it onto the
destination, and assert the destination now matches the source's own
"after" state exactly — a real, end-to-end diff → export → apply round
trip, not diff/export tested only in isolation from the existing
`apply` side. Plus per-kind tests: a deleted file becomes a whiteout in
the right directory; a top-level deletion becomes a top-level whiteout
(no leading directory component); an added directory archives as a
directory entry; an added symlink archives with its own target,
unresolved (not dereferenced into a copy); a permission change is
carried into the archived entry's own mode; an empty change list
produces a valid, readable, empty archive; and the vanished-source-file
tolerance documented above, exercised directly (a `Change` referencing
a path that was never actually created).

## Performance

Not called from anywhere yet (no `ociman build` CLI command exists to
call it) — zero runtime impact on any existing hot path by
construction, same reasoning every earlier not-yet-wired-in increment
in this Dockerfile/build pipeline (0039-0045) has used for itself.

## What's still not here

* An in-memory or on-disk `Vec<Change>` → tar → `oci_store::Store::
  ingest` pipeline actually committing a build step's changes as a real
  content-addressed layer blob (this increment only produces the tar
  bytes; nothing yet streams them into the store).
* Image manifest/config construction for a newly committed layer (the
  layer's own digest, `diff_id`, and history entry).
* Everything else 0039-0045 already listed as future work: `ONBUILD`/
  `HEALTHCHECK`, `--build-arg`, the build cache, dependency-ordered
  execution actually running anything, and `ociman build` itself.
