# Design note 0480: `ociman image history`/`image rm` aliases

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_history.rs`,
`tests/tests/ociman_rmi.rs`.

## What this closes

Continuing the `ociman image` alias family `0478`/`0479` started:
`history` and `rm` were still missing, both explicitly named in
`0478`'s own "still out of scope" section.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/images/history.go:23-40`: `imageHistoryCmd`
  (`Parent: imageCmd`) and `historyCmd` (top-level) share `Args`/
  `Use`/`Short`/`Long`/`ValidArgsFunction`/`RunE` verbatim, plus
  `historyFlags` registered on both.
- `~/git/podman/cmd/podman/images/rm.go:16-52`: **a real, checked-
  directly naming reversal from what "nested vs. top-level" would
  otherwise suggest** — `rmCmd` (`Use: "rm [options] IMAGE
  [IMAGE...]"`) is the one registered with `Parent: imageCmd` (giving
  `podman image rm`), while `rmiCmd` (`Use: "rmi ..."`, sharing
  `Short`/`Long`/`RunE`/`ValidArgsFunction` with `rmCmd` verbatim) is
  the *separate, top-level* `podman rmi`. So `podman image rm` maps
  onto this project's own already-existing `ociman rmi`, not a
  hypothetical `ociman rm` (which doesn't exist for images at all —
  `ociman rm` is already `Command::Rm`, container removal).

## Implementation

A pure CLI-surface dispatch-reuse increment, the exact same shape
`0478`/`0479` already established:

- `ImageCommand` gains `History { reference: String, format:
  Option<String>, no_trunc: bool }` and `Rm { references: Vec<String>,
  force: bool, all: bool, ignore: bool }` — field-for-field identical
  to the already-existing `Command::History`/`Command::Rmi`.
- Two new dispatch arms delegating to the exact same free functions
  (`cmd_history`/`cmd_rmi`) the top-level commands already call.
- `Command::Image`'s own top-level doc comment updated again, this
  time also explicitly flagging the real `rm`/`rmi` naming reversal
  so a future reader isn't misled by the "nested is `rm`, top-level
  is `rmi`" asymmetry.

## Tests

Two new integration tests: `image_history_is_a_byte_identical_alias_
for_history` (`tests/tests/ociman_history.rs`, including the `--no-
trunc` flag working identically through the alias, matching `0430`'s
own established "flag set works through the alias too" convention),
`image_rm_is_a_byte_identical_alias_for_rmi` (`tests/tests/
ociman_rmi.rs`). All 7 tests in `ociman_history.rs` pass (6 prior + 1
new); all 22 in `ociman_rmi.rs` pass (21 prior + 1 new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (121 test-result
blocks, 0 failures on the second attempt — the first attempt hit one
transient, already-documented flaky failure in `ocicri_container.rs`,
confirmed unrelated and passing instantly in isolation), `python3
ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (one transient, already-documented flaky failure on
the first attempt, same file, clean 121/121 on the second), `bash
ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r` round
trip on the first attempt). No benchmark re-run needed: neither
`ociman image history` nor `image rm` is exercised by `ci/bench.sh`,
and this is a pure dispatch-reuse addition touching no existing
function's body at all.

## Deliberately still out of scope

The rest of the same real `podman image` alias family: `push`,
`pull`, `save`, `load`, `import`, `inspect`, `mount`/`unmount`,
`diff` — each its own equally small future increment, following the
identical shared-`RunE` pattern.
