# Design note 0573: `ociman image diff`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_diff.rs`,
`tests/tests/ociman_image_diff.rs`.

## What this closes

`Command::Image`'s own doc comment explicitly named this as still
open: *"`image diff` computes a real, new 'image vs. its own parent
layer' comparison this project has no equivalent logic for at all…
real, deliberately deferred gaps for a future increment, not yet
ported."* `docs/design/0572` named it as the recommended follow-up.
This closes the single-positional first slice: `ociman image diff
IMAGE`.

Along the way, that same doc comment turned out to be stale in a
second respect too, corrected here transparently: `image mount`/
`unmount` are **not** still-deferred gaps at all — `0519`/`0520`
already implemented both, genuinely, as their own real, non-alias
subcommands. Only `diff` genuinely remained.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/images/diff.go:16-24`: `Use: "diff
  [options] IMAGE [IMAGE]"`, `Args: cobra.RangeArgs(1, 2)` — "The
  image will be compared to its parent layer or the second argument
  when given."
- `~/git/podman/pkg/domain/infra/abi/containers.go:1159-1177`
  (`ContainerEngine.Diff`, shared by both container and image diff):
  with no second positional, `parent` stays empty.
- `~/git/podman/libpod/diff.go:29-49` (`GetDiff`): `toLayer` is the
  image's own real `TopLayer()`; with `from == ""`,
  `r.store.Changes(fromLayer="", toLayer)` diffs the top layer against
  nothing but its own **direct parent layer** — never the whole
  image's cumulative content from scratch. Confirmed live: for a
  multi-layer `python:3.12-slim`, real `podman image diff` reports
  only a handful of lines (the last layer's own real additions), not
  every file the image contains.

## Why extraction, not a stored layer graph

This project's own content-addressed `oci_store` has no per-layer
storage graph to query a "parent layer" out of directly, unlike real
`container-libs/storage`. The equivalent result is computed instead:
every layer but the last is extracted into one scratch directory, a
[`oci_layer::Snapshot`] captured of *that* state, then the last layer
extracted on top of the exact same directory before diffing — the
same "same directory, two points in time" shape [`cmd_diff`]'s own
doc comment already establishes as the one that sidesteps
`oci_layer::apply`'s own deliberate no-mtime-restoration behavior (two
*separate* extractions of identical content would otherwise make
every regular file spuriously look "changed" — the exact bug `cmd_diff`
itself was already written to avoid, for the same underlying reason).
A single-layer image therefore diffs against a genuinely empty
directory, matching real podman's own identical single-layer
behavior.

## A real, previously-unnoticed bug found and fixed in the already-shipped `cmd_diff`

Live-verifying the new image-diff output byte-for-byte against a real
installed `podman 4.9.3` (`docker.io/library/busybox:latest`) surfaced
a one-line mismatch: this project's own extraction reported `A /dev`,
real podman didn't. Tracing why led directly to `~/git/podman/libpod/
diff.go`'s own `initInodes` map — a fixed set of paths (`/dev`,
`/etc/hostname`, `/etc/hosts`, `/etc/resolv.conf`, `/etc/mtab`,
`/proc`, `/run`, `/run/notify`, `/run/.containerenv`, `/run/secrets`,
`/run/podman-init`, `/sys`) `GetDiff` unconditionally filters out of
**every** diff, container or image alike — not an image-specific
quirk. This meant the already-shipped `cmd_diff` (container diff,
`0146`-`0149`) had the exact same latent divergence, just never
triggered by anything a single-layer image extraction would show —
confirmed directly: `sudo podman diff` on a real, started `busybox`
container shows nothing at all (not even `/dev`/`/proc`/`/sys`), while
this project's own `ociman diff` on the equivalent container showed
all three as `Added`. A pre-existing test (`ociman_diff.rs`'s own
`diff_with_no_deliberate_changes_at_all_reports_no_base_image_files_
as_changed`) had explicitly asserted the *opposite* — a comment
claiming "real docker/podman's own `diff` shows these too" without
ever actually checking directly — now corrected to assert a genuinely
empty diff, matching the live-verified reality.

Both `cmd_diff` and `cmd_image_diff` now share one `DIFF_EXCLUDED_
PATHS` filter (`filter_diff_changes`), applied right after each
computes its own raw `oci_layer::changes` and before printing —
ported verbatim, including three paths (`/run/notify`, `/run/.
containerenv`, `/run/podman-init`) this project has no equivalent
concept of at all yet (harmless to include regardless: nothing here
ever creates them, so they can never spuriously match).

## Why this is narrow and safe

No new architecture: [`cmd_image_diff`] reuses the exact same
`resolve_image_by_reference_or_id`/`compression_for_media_type`/
`oci_layer::apply` primitives `cmd_clone` (`0571`) and every other
image-resolving command already rely on, plus the exact same
`oci_layer::Snapshot::capture`/`changes` pair `cmd_diff` already uses.
`--format`/output-rendering logic is now shared via two small,
extracted helpers (`resolve_diff_format`/`print_diff_changes`) rather
than duplicated. No cgroup, namespace, capability, systemd, or mount
code is anywhere near this change. Real podman's own second, explicit
`IMAGE` positional (diff against a genuinely different, named image)
remains a real, deliberately deferred gap — a separate, future
increment, not this narrower first slice.

## Tests

New file `tests/tests/ociman_image_diff.rs` (6 tests): single-layer
(every path added), a real multi-layer image built via `ociman build`
(only the last layer's own real change shown, never the base image's
own content or a metadata-only `ENV`), `--format json`'s exact
three-array shape, `--format` rejecting anything but `json`, an
unknown image, and the global `--json` flag matching `--format json`.
`tests/tests/ociman_diff.rs`'s own `diff_with_no_deliberate_changes_
at_all_reports_no_base_image_files_as_changed` corrected as described
above.

Manually verified end to end beyond the automated tests: real `pull`
of both a single-layer (`busybox`) and multi-layer (`python:3.12-
slim`) public image, `ociman image diff` on each, and a direct,
byte-for-byte `diff`(1) comparison against the equivalent real
installed `podman 4.9.3` output for both — identical in every case
after the `initInodes` fix.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (130
test-result blocks — one more than `0572`'s `129`, the new
`ociman_image_diff.rs` file — all passing on the first attempt with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (clean on the first attempt,
`RUST_TEST_THREADS=2` set from the start given this host's own
repeatedly-observed concurrent-session CPU contention this same
week), `bash ci/build-deb.sh` (clean on the first attempt, real
`dpkg -i`/`--version`/`dpkg -r` round trip). No `ci/bench.sh` rerun
needed: `ociman diff`/`image diff` are read-only filesystem-comparison
commands, never exercised by it and nowhere near any container
startup/exec/destroy hot path.

## Deliberately still out of scope

Real podman's own explicit two-`IMAGE` form (diff against a genuinely
different, named image rather than this same image's own immediate
parent) — a separate, future increment. `image mount`/`unmount`'s own
already-known, separate `--format`/bare-mode-listing gaps (`0519`'s
own doc comment) are untouched and unrelated to this note.
