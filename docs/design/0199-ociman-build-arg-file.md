# Design note 0199: `ociman build --build-arg-file`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Build`'s new
`--build-arg-file` flag); `bin/ociman/src/build.rs` (`read_build_arg_file`,
`cmd_build`'s new combined-list construction before `parse_build_args`);
`tests/tests/ociman_build.rs`.

## Continuing milestone 4

0198's own investigation surfaced `--timestamp` as the next named
survey item, but a real reproducibility subtlety changed course this
turn: checked directly, real `podman build --timestamp` doesn't just
rewrite the built image's own config/history `created` fields — it
also normalizes the *file mtimes* of every entry a newly-committed
layer's own tar actually writes (confirmed via `podman save` + a raw
`tar -tv` on the resulting layer blob: a `RUN`-produced file's own
mtime matched the given `--timestamp` value exactly, not the real
wall-clock time the `RUN` step actually ran). Doing this properly would
mean threading a "force this mtime" option through `oci_layer::export`
itself — a crate several *other* commands (`ociman export`, `ociman
commit`, `ociman save`'s own archive formats) also depend on and none
of which want or need this — a real, disproportionately invasive
change for one command's own one flag, deserving its own careful,
separate increment rather than a rushed partial version here. Picked
`--build-arg-file` instead: named in `Command::Build`'s own `--build-
arg` doc comment's implicit "similar tools" neighborhood, genuinely
self-contained (touches only `bin/ociman/src/build.rs`'s own existing
build-arg resolution, no shared-crate plumbing at all), and something
this turn could implement, verify, and ship completely, rather than
half of a bigger feature.

## Real, checked-directly semantics

Verified directly against a real installed `podman build
--build-arg-file`, cross-referenced with `~/git/podman/vendor/
go.podman.io/buildah/pkg/cli/build.go`'s own `readBuildArgFile`/
`readBuildArg`:

* Each line of the file is either `KEY=value` (used verbatim) or a
  bare `KEY` (pulls the value from `ociman`'s own current process
  environment if a variable of that name is set there, or is dropped
  entirely — not an empty-string override — if it isn't), exactly the
  same two shapes `--build-arg` itself already accepts.
* A completely empty line is skipped. A line whose very first
  character is `#` is a comment and skipped too — no leading-
  whitespace tolerance at all (`arg[0] == '#'`, not a trimmed check):
  a line starting with a space before the `#` is *not* treated as a
  comment.
* Multiple `--build-arg-file` values (repeatable) are each read in the
  order given.
* Every `--build-arg-file` entry is applied *before* any `--build-arg`
  value — confirmed directly: `--build-arg-file` naming `FOO=fromfile`
  plus an explicit `--build-arg FOO=fromcli` builds with `fromcli`
  winning.

## The fix

`parse_build_args`'s own existing "later entry for the same key wins"
resolution (already established for repeated `--build-arg` values,
`~/git/oci-tools/bin/ociman/src/build.rs`'s own pre-existing doc
comment) turns out to be *exactly* the ordering `--build-arg-file`
needs too — no new merge logic required at all. `cmd_build` now:

1. Reads every `--build-arg-file` path (in the order given) via a new
   `read_build_arg_file`, which parses each file into the same
   `Vec<String>` shape `--build-arg` values already are (skipping
   blank/comment lines per the rules above).
2. Concatenates those onto the front of a combined list, with the real
   `--build-arg` values appended after.
3. Passes the *one* combined list to the existing, completely
   unchanged `parse_build_args` — its own already-tested "later wins"
   behavior does the rest.

A `--build-arg-file` path that doesn't exist is a clear, immediate
build error (`std::fs::read_to_string`'s own `io::Error`, wrapped with
`.with_context`), matching real buildah's own identical refusal.

## Tests

Five new integration tests in `tests/tests/ociman_build.rs`:
`build_arg_file_sets_declared_arg_values` (basic `KEY=value` from a
file), `build_arg_file_skips_blank_and_comment_lines`,
`build_arg_file_bare_key_pulls_from_the_process_environment`,
`build_arg_file_is_overridden_by_an_explicit_build_arg`, and
`build_arg_file_nonexistent_path_is_a_clear_error`. All 108
pre-existing `ociman build` tests continue to pass unchanged (113
total now).

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 83/83 result blocks)/`cargo fmt --all
--check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check`/`bash
ci/native-ci.sh` all clean. No performance regression (`ociman build
--no-cache`, one `RUN` step, ~19.7ms, consistent with prior
measurements for the same scenario).

## What this doesn't do yet

`--timestamp` (deferred this turn for the reasons above — a real
implementation needs `oci_layer::export`'s own layer-writing path to
support a forced mtime too, not just `ImageConfig`/`HistoryEntry`
timestamps, to actually deliver byte-for-byte reproducible image
digests rather than a metadata-only half-measure) remains open, now a
better-scoped future increment for having investigated its real
full extent here first. `ociman prune --filter reference=<pattern>`
and the larger `RUN --mount=`/heredoc/multi-platform gaps named by
the earlier milestone-4 survey also remain open.
