# Design note 0477: `ocibox ephemeral --clone`/`-c`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_ephemeral.rs`.

## What this closes

`0476`'s own "still out of scope" section flagged this directly:
`ocibox ephemeral` didn't inherit `--clone` the way real `distrobox
ephemeral` inherits every one of `distrobox create`'s own flags.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/ephemeral.go:60,85`: the file's own
  comment, verbatim: *"inherited create flags (e.g. -c/--clone)"* —
  `ContainerClone: cmd.String("clone")` reads the identical flag
  `distrobox create` itself registers.

## Implementation

A pure, mechanical wiring increment — the entire real behavior
(recursive rootfs copy, source-record carry-forward, mutual
exclusivity with `--image`) already exists in full from `0476`,
reused verbatim:

- `Command::Ephemeral::image`: `String` → `Option<String>`; new
  `clone: Option<String>` (`-c`/`--clone`, same flag shape as
  `Command::Create::clone`).
- `cmd_ephemeral` gains a `clone: Option<&str>` parameter, threaded
  straight into the exact same `create_box`/`clone_box` call
  `ocibox create --clone` already established — no new logic of any
  kind.

## Tests

Two new integration tests in `tests/tests/ocibox_ephemeral.rs`:
`ephemeral_clone_sees_the_source_boxs_own_current_state_and_still_
cleans_up` (a real, already-existing box's own current rootfs write
is visible from inside the ephemeral clone; the clone is fully
removed afterward while the real, persistent source box survives
completely untouched — confirmed by listing `boxes/` afterward and
finding only the source), `ephemeral_requires_exactly_one_of_image_
or_clone` (both directions, matching `ocibox create`'s own identical
validation). All 9 tests in the file pass (7 prior + 2 new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (121 test-result
blocks, 0 failures on the first attempt), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (one
transient, already-documented flaky failure in `ociman_logs.rs`'s own
follow test on the first attempt, confirmed unrelated and passing
instantly in isolation; second attempt clean 121/121), `bash
ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r` round
trip on the first attempt). No benchmark re-run needed: `ocibox
ephemeral` is not exercised by `ci/bench.sh` at all.

## Deliberately still out of scope

Nothing left open from `0476`'s own "still out of scope" section for
`--clone` specifically — both real call sites (`create`, `ephemeral`)
now support it, matching real distrobox exactly.
