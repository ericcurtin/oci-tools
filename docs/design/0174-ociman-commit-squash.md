# Design note 0174: `ociman commit --squash`

Status: implemented
Scope: `crates/oci-dockerfile/src/commit.rs` (new: `squash_layer`, plus
promoting `tempfile` from a dev-dependency to a real one);
`crates/oci-dockerfile/src/lib.rs` (re-export); `bin/ociman/src/
main.rs` (`Command::Commit`'s new `squash` field, `cmd_commit`/
`commit_inner`'s new early branch); `tests/tests/ociman_commit.rs`.

## Closing a real, explicitly-named deferred gap

`Command::Commit`'s own doc comment used to list `--squash` among
"deliberately out of scope for now". This increment closes it.

## Real buildah's own squash semantics, checked directly

Read `~/git/podman/vendor/go.podman.io/buildah/image.go` directly,
then confirmed it against a real `podman commit --squash` run (base
`docker.io/library/busybox:latest`, a container with one added and one
removed file, committed with `--squash`): the resulting image has
**exactly one layer** (no base layers referenced at all — a real
`Parent: ""`) and **exactly one history entry**. This is a
fundamentally different operation from the default commit path (one
new *diff* layer stacked on the base image's own existing layers): a
squash needs no diff against any earlier snapshot at all, only the
container's own current, complete filesystem state.

## Reusing `oci_layer::export_tree`, not `oci_layer::changes`+`export`

`ociman export`'s own `export_tree` (0169) already does exactly what a
squash's own layer capture needs: tar up a directory's *entire current
tree*, not a diff, with real mount-boundary awareness (`st_dev`) and
hardlink deduplication. `squash_layer` (new, in `oci-dockerfile`,
right next to `commit_layer`) is a thin wrapper: `export_tree` into a
scratch file, `compress_for_storage` into a second scratch file (same
`Digest`-returning primitive `commit_layer` already uses), then
`Store::ingest`. It deliberately does **not** take a `changes: &[Change]`
parameter at all — unlike `commit_layer`, it never looks at any earlier
snapshot.

Unlike `commit_layer`'s own in-memory `Vec<u8>` buffers (sized to one
build step's typically-small diff), `squash_layer` streams through two
real `tempfile::NamedTempFile` scratch files, matching `bin/ociman/src/
archive.rs`'s own identical precedent (`ingest_docker_archive_layer`):
a squash's own input is a whole rootfs, which can be arbitrarily large,
so holding it fully in memory would be a real, avoidable regression.
This is the one reason `tempfile` moved from a dev-only to a real
dependency of `oci-dockerfile`.

## A real question investigated and refuted before writing any code:
does `commit`'s existing diff path already have `export_tree`'s
pre-0169 mount-recursion bug?

`export_tree` was written to fix a real bug in `cmd_export`'s own use
of `/proc/<pid>/root` (a running container's live, namespace-visible
rootfs view) — walking into a container's own live-mounted `/proc`/
`/sys` produced a ~490MB archive instead of ~4MB. Before assuming
`commit`'s existing (non-squash) diff path might share that same bug
for a running container, this was checked directly rather than
assumed: `resolve_container_root` (which both the squash and
non-squash commit paths use identically) always resolves to the
*static* bundle rootfs path (`state.rootfs`), never `/proc/<pid>/root`.
From the host's own default mount namespace, a running container's
`/proc`/`/sys` mount targets under that static path are invisible —
confirmed empirically (`ls .../rootfs/proc` on a genuinely running
container's bundle shows an empty directory; `ociman diff` on the same
container returns in single-digit milliseconds, not the multi-second
walk a real recursive `/proc` listing would take). Conclusion: no such
bug exists in `commit`'s existing path, and feeding `squash_layer` the
same static `root` `commit_inner` already resolves is exactly as safe
— `export_tree`'s own `st_dev` check is simply a no-op at this call
site (never crossing a mount boundary in practice), with its real
value here being the hardlink-deduplication logic instead.

## Where `commit_inner` branches, and how little else changes

`commit_inner` now resolves `base_reference`/`base_record`/`config`
first (needed either way, for `architecture`/`os`/`Config` defaults),
then branches:

* **Squash**: `config.rootfs.diff_ids`/`config.history` are cleared
  (both were just inherited whole from the base image's own config,
  and must not survive into a squashed image that references no base
  layers at all); `layers` starts empty rather than cloned from
  `base_manifest.layers`; `squash_layer(&store, root)` replaces
  `commit_layer`+the whole snapshot-read-and-diff step entirely (no
  `BASE_SNAPSHOT_FILENAME` read at all — an older container missing
  that file, which would fail a plain `commit`, can still be
  `commit --squash`ed).
* **Default (unchanged)**: exactly the same code as before this
  increment — reads the snapshot, diffs, `commit_layer`, clones
  `base_manifest.layers`.

Both paths converge back into the same, single `record_layer` call
(now parameterized by a `created_by` string computed per-branch) and
the same message/author/`--change`/store/tag logic below it —
`--message`/`--author`/`--change` all apply identically regardless of
`--squash`, matching real `podman commit --squash --change ...`'s own
combinable flags.

## The one new history entry's own text: this project's own choice

Real `podman commit --squash`'s own single surviving history entry
(`comment: "FROM docker.io/library/busybox:latest"`, `created_by:
"/bin/sh"`) comes from a buildah/podman-internal call path not fully
traced during this investigation (buildah's own lower-level
`image.go` shows `History = []` when squashing; the one entry a real
`podman commit --squash` still produces must come from a wrapper
`commit.go` doesn't fully account for from `image.go` alone). Matching
that exact text was not attempted — this project's own established
convention (e.g. 0164's `--change`) is functional correctness over
exact string content. Instead: `created_by` = `"ociman commit --squash
<container> (was based on <base-reference>)"`, chosen to keep the one
piece of real, useful provenance (what image this container was
originally based on) a fully flattened image would otherwise lose
entirely.

## Verified against real `podman commit --squash`

Real `podman commit --squash` was run directly during design (see
above) to confirm the exact "one layer, `Parent: ""`, one history
entry" shape. This project's own implementation was then run directly
against a real busybox-based container (added a file, removed
`/bin/cat`) and confirmed: `ociman inspect --json` shows exactly one
`rootfs.diff_ids` entry and one `history` entry; the resulting image
runs correctly (`/bin/cat` genuinely absent, the added file genuinely
present); the new layer's own stored blob size stayed close to the
base layer's own (no `/proc`/`/sys` content leaked in, confirming the
mount-boundary investigation above).

## Tests

`crates/oci-dockerfile/src/commit.rs` gained 2 unit tests:
`squash_layer` produces byte-identical output to a direct
`export_tree`+`compress_for_storage` call over the same root, and
ignores any earlier `Snapshot` entirely (unlike `commit_layer`).
`tests/tests/ociman_commit.rs` gained 3 integration tests: squashing a
*multi*-layer stack (base image, plus two separate real commits on top
of it, with an add in each and a deletion in the last) down to exactly
one layer/one history entry, with every surviving change (and the
deletion) verified by actually running the squashed image;
`--squash` on a container with zero filesystem changes and no
recorded base snapshot at all still produces a real, valid, runnable
single-layer image; `--squash` combined with `--author`/`--change`
applies both exactly like the non-squash path. Full `cargo build
--workspace --locked`/`cargo test --workspace --locked` (2 clean
runs)/`cargo fmt --all --check`/`cargo clippy --workspace --all-
targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo deny
check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* `--squash-all` (real buildah/podman's own separate flag for
  multi-stage builds, not `commit`) — not applicable to `commit` at
  all, real podman's own `commit` has no such flag either.
* Exactly matching real podman's own squash history-entry text (see
  above) — a deliberate, documented deviation, not a gap.
* `ociman build --squash` — a related but separate future increment
  (a build executor's own squash would need to fold multiple
  in-progress build stages, not one already-running container's
  filesystem); left for later.
