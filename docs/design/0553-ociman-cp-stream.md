# Design note 0553: `ociman cp -` (stdin/stdout tar streaming)

Status: implemented
Scope: `bin/ociman/src/main.rs`, `crates/oci-layer/src/export.rs`,
`crates/oci-layer/src/lib.rs`, `tests/tests/ociman_cp.rs`.

## What this closes

`docs/design/0146`'s own "What this doesn't do yet" section explicitly
listed: *"`-` (stdin/stdout streaming) — real `docker cp`/`podman cp`
support `-` as either path to stream a tar archive from stdin or to
stdout; not implemented here."* This closes it.

## Real, checked-directly confirmation, and the real bug it was masking

- Documented feature: `~/git/podman/cmd/podman/containers/cp.go:29`
  (`cpDescription`): *"If '-' is specified for either the SRC_PATH or
  DEST_PATH, you can also stream a tar archive from STDIN or to
  STDOUT."*
- **Live-verified this project's own prior (buggy) behavior, not
  guessed**: `ociman cp ctr:/bin - > out.tar` silently created a real
  directory literally named `-` in the current working directory (and
  copied `/bin`'s own contents into it), leaving `out.tar` empty —
  `parse_user_input` (already correctly ported from real podman's own
  `parseUserInput`, `~/git/podman/pkg/copy/parse.go`) never special-
  cases `-` at all, matching real podman's own identical parsing
  exactly; the `-`-as-stream-request handling happens entirely
  *later*, inside `copyFromContainer`/`copyToContainer` themselves
  (`hostPath == "-"` string comparisons) — this project simply never
  had that later check at all. `cat in.tar | ociman cp - ctr:/target`
  failed outright trying to `stat` a literal file named `-`.
- **Live-verified the exact real tar shape against a real installed
  `podman 4.9.3`** (not assumed): `podman cp ctr:/etc/passwd -`
  produces a tar with exactly one entry named `passwd` (no parent-
  directory entries at all); `podman cp ctr:/etc -` produces one whose
  top-level entry is `etc` itself, with `etc/group` etc. nested under
  it; `podman cp ctr:/ -` produces one whose top-level entries are the
  rootfs's own direct children (`bin`, `etc`, ...), since `/` has no
  basename of its own to wrap with.
- Container-side `-` is never meaningful: checked directly,
  `~/git/podman/cmd/podman/containers/cp.go:346-348` (`copyToContainer`)
  and `:209-212` (`copyFromContainer`) only ever check `-` against the
  argument that ISN'T `[CONTAINER:]`-prefixed — a container-to-
  container call (`cp.go:89-90`, only entered when *both* sides parsed
  a container) never even reaches those checks; and `parse_user_input`
  never assigns a container to input `-` (it has no `:`), so a bare
  `-` is *always* the plain-host-path side of the operation, never a
  container reference.
- Real, checked-directly **absence** of a terminal guard: `grep -n
  "IsTerminal" ~/git/podman/cmd/podman/containers/cp.go` finds nothing
  at all — unlike `ociman save`/`export` (`0550`/`0552`), real
  `podman cp -` never refuses a real terminal on either side.
  Faithfully *not* added here either, matching real podman's own
  checked-directly quirk rather than "improving" on it.

## Implementation

### `oci-layer`: two new/adjusted primitives, reused directly

- `oci_layer::export_single_path_tree` (new, `export.rs`): tars a
  *single* file/directory/symlink under its own basename as the one
  top-level entry (with the same recursive descent, and the same
  never-cross-a-mount-point safety net, `export_tree` already has) —
  genuinely different from `export_tree`'s own "root's children are
  the top-level entries, root itself is never an entry" shape, which
  is what real `podman cp ctr:/ -` uses instead (matched here too: `cp`
  uses `export_tree` directly when `SRC_PATH` resolves to the
  container's own rootfs root, `export_single_path_tree` otherwise).
- `oci_layer::extract_plain_tar` (new, `lib.rs`): `apply_tar` gained a
  `handle_whiteouts: bool` parameter (`apply` passes `true`,
  unchanged); this new function passes `false`. A **real correctness
  distinction**, not cosmetic: an arbitrary user-supplied tar (`cp`'s
  own stdin side) has no OCI-layer semantics at all, so a literal
  `.wh.<name>` entry inside it must land on disk exactly as given —
  never silently interpreted as a delete marker for some unrelated
  pre-existing `<name>` the way a real image layer's own whiteout
  would be.

### `ociman`: wiring inside `cmd_cp`'s existing two single-container branches

- `(Some(container), None)` (container → host): if `DEST_PATH == "-"`,
  stream a tar to stdout instead of calling `copy_cp_path` — `export_
  tree` when `SRC_PATH` resolved to the container's own rootfs root,
  `export_single_path_tree` otherwise.
- `(None, Some(container))` (host → container): if `SRC_PATH == "-"`,
  read a tar from stdin and extract it via `extract_plain_tar` into
  the already-resolved `DEST_PATH`, which must already exist as a real
  directory — matching real podman's own exact, checked-directly
  wording, `"destination must be a directory when copying from
  stdin"`.
- The container-to-container branch is untouched: by construction (see
  above), neither side can ever be `-` there.

## Tests

Five new tests in `tests/tests/ociman_cp.rs`:

- `cp_stdout_streams_a_single_file_tarred_under_its_own_basename`
- `cp_stdout_streams_a_directory_with_its_own_basename_as_the_top_
  level_entry`
- `cp_stdin_extracts_a_tar_into_an_existing_container_directory`
- `cp_stdin_requires_the_destination_to_already_be_a_real_directory`
- `cp_stdin_of_non_tar_input_is_a_clear_error`

Plus five new low-level unit tests in `crates/oci-layer` (68 total, up
from 63): `export_single_path_tree`'s own file/directory/round-trip
shapes, `extract_plain_tar`'s own literal-`.wh.`-entry preservation,
and its own clear error on genuinely non-tar input.

Manually exercised beyond the automated tests: `ociman cp
ctr:/etc/passwd -`/`ctr:/etc -`/`ctr:/ -` against a real container,
each compared directly against the equivalent real `podman cp`
invocation's own tar shape; `ociman cp - ctr:/existing-dir` (a real
stdin stream), `ociman cp - ctr:/does-not-exist` (real error), `ociman
cp - ctr:/etc/passwd` (dest is a file, real error), and garbage-stdin
(real error) — all via the real `ociman container cp` alias too,
confirming the shared `cmd_cp` fix covers it automatically.

## Verification

`cargo build --workspace --locked` (clean), `cargo fmt --all` (clean,
no changes needed for the new tests), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), `cargo test -p
oci-layer --locked` (68/68), targeted `ociman_cp.rs` run (18/18, 13
pre-existing + 5 new), `python3 ci/guards.py` (clean), `cargo deny
check` (clean). Reuses the already-tested extraction/export machinery
directly (a small, additive `handle_whiteouts` parameter plus one new
tar-building function) — no new hot path, no `ci/bench.sh` rerun
needed.
