# Design note 0499: `ociman container diff` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0498` started: `diff` — the thirteenth member of real
podman's own `podman container <verb>` family closed so far — was
still missing. Explicitly flagged as one of the three deferred
members ("cross-concept aliasing or new comparison logic needed") in
`0482`'s own "still out of scope" note; re-examined here and found to
be simpler than that note assumed.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/diff.go:15-49`: `diffCmd`
  (`Parent: containerCmd`) has its own `diffRun` unconditionally set
  `diffOpts.Type = define.DiffContainer` before calling the shared
  `diff.Diff` — a genuinely *narrower* scope than real top-level
  `podman diff`'s own `diffRun` (`~/git/podman/cmd/podman/diff.go`),
  which instead sets `define.DiffAll` (auto-detecting container-or-
  image).
- This project's own top-level `Command::Diff` was already, from the
  start, scoped to match real `podman container diff`'s own narrower
  container-only behavior exactly (its own doc comment already says
  so directly) — unlike `ImageCommand::Inspect` (`0482`)/
  `ContainerCommand::Inspect` (`0488`), which both genuinely needed a
  "force one specific type" wrapper around a richer, auto-detecting
  top-level sibling. `ociman diff`/`ociman container diff` need no
  such forcing at all: they're already exactly the same scope.

## Implementation

`ContainerCommand::Diff` is a new variant, field-for-field identical
to the already-existing `Command::Diff` (`id`, `latest`, `format`).
Since `Command::Diff`'s own dispatch arm does its explicit-id-wins-
over-latest resolution inline (there's no dedicated `cmd_diff`-
adjacent wrapper that already takes raw, unresolved `id`/`latest`
fields), the new arm replays the identical logic verbatim before
calling the same `cmd_diff(&resolved_id, cli.global.json,
format.as_deref())` — the same "replay the top-level arm's own inline
validation" shape `0488`/`0491`/`0496`/`0497`/`0498` already used.

Genuinely simpler than `0482`'s own deferral originally assumed: that
note grouped `mount`/`unmount`/`diff` together as all needing either
cross-concept aliasing or new comparison logic. `diff` turns out to
need neither — `mount`/`unmount` remain deferred (they alias the
*container* mount/unmount commands from the *image* side, a genuinely
different, still-unverified cross-concept shape), but `container
diff` itself, examined directly this time, is a plain, ordinary
alias of the exact same shape as every other member of this family.

## Tests

One new integration test added to `tests/tests/ociman_container.rs`:

- `container_diff_is_a_byte_identical_alias_for_top_level_diff` —
  proves the alias actually reports a real container's own added/
  deleted paths against its base image, exactly like the top-level
  command (using the same `.rootless-overlay-supported` = `false`
  fixture setup `ociman_diff.rs`'s own tests already establish, since
  `diff` doesn't support this project's own rootless-overlay rootfs
  optimization yet, `docs/design/0146`).

Full `diff` semantics (`--format json`, `--latest`, explicit-id-wins-
over-latest, the rootless-overlay gap itself) are already exhaustively
tested against the top-level command in `ociman_diff.rs` — this
note's own test deliberately only proves the alias itself reaches the
identical function with the identical fields, not re-testing `diff`'s
own semantics a second time.

All 25 tests in `tests/tests/ociman_container.rs` pass (24 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0498`; clean on the first attempt with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (clean on the first attempt,
also with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on
the first attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip).
No benchmark re-run needed: `ociman container diff` is not exercised
by `ci/bench.sh`, and this is a pure dispatch-reuse addition touching
no existing function's body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `cp`, `commit`, `run`, `create`, `exec`,
  `attach`, `export`, `port`, `mount`/`unmount`, `init`, `stats`,
  `runlabel` — each a pure-alias candidate of the same shape as this
  one and `0488`-`0498`, left for future increments to keep each one
  individually small and independently verified.
- `ociman image mount`/`unmount` — still genuinely deferred (`0482`),
  a real cross-concept aliasing shape (real podman's own `podman
  image mount`/`unmount` alias the *container* commands from the
  *image* side) not yet independently verified.
</content>
