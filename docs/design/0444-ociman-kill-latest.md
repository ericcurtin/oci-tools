# Design note 0444: `ociman kill --latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_kill.rs`.

## What this closes

`ociman kill` had no `--latest`/`-l` flag at all — every other sibling
in this same real podman command family (`rm`/`stop`/`restart`/
`pause`/`unpause`/`exec`, `0434`-`0437`, `0443`) already got it,
`kill` alone had been skipped.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/kill.go`: `validate.
AddLatestFlag(killCommand, &killOpts.Latest)` — the exact same flag/
validation `ociman rm --latest` (`0434`) already ports, reused
verbatim (see `0434`'s own design note for the full citation).

## Implementation

- `Command::Kill` gains `latest: bool` (`#[arg(short = 'l', long)]`).
- `cmd_kill` gains a `latest: bool` parameter and the same mutual-
  exclusivity check its `--all`/`--cidfile` siblings already have,
  checked against the *original* `ids`/`cidfiles` slices (before any
  merging), matching the exact style `0435`/`0436` already
  established. Like `stop`/`restart` (`0435`/`0436`), and unlike
  `pause`/`unpause` (`0437`), `kill` merges the resolved single id
  straight into the same `ids: Vec<String>` its own `--cidfile`
  handling already builds — right after the cidfile merge, before the
  existing `all` mutual-exclusivity check — so `--latest` needs no
  separate selection logic, or `pause`/`unpause`'s own separate non-
  tolerant branch, of its own at all: `kill`'s own single/multi-target
  path (`kill_one`, used by neither the `--all` branch nor any
  special-cased tolerance) was already never tolerant of an
  ineligible container before `--latest` existed — only its own
  separate `--all` branch silently skips a not-currently-killable
  container — so simply riding that same pre-existing path already
  gives `--latest` the correct, real, reported-error behavior for a
  not-currently-killable latest container, with zero new branching.
  Reuses `resolve_latest_container` (`0434`) unchanged.

## Tests

Three new tests in `tests/tests/ociman_kill.rs`: `kill_latest_kills_
only_the_most_recently_created_running_container` (two named,
genuinely running containers with a real, distinguishable creation-
time gap; `kill --latest` stops only the newer one, the older stays
running, verified directly), `kill_latest_on_an_empty_store_is_a_
clear_error`, and `kill_latest_combined_with_anything_else_is_a_
clear_error` (against `--all`, an explicit id, and `--cidfile`). All
13 prior tests in the file pass unmodified (16/16 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
120/120, clean on the first run too), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). Touches only `ociman
kill`'s own selection logic, not any hot path at all — no benchmark
re-run needed.

## Deliberately still out of scope

Real podman offers `--latest` on a large further family of commands
this project hasn't matched yet at all: `attach`, `diff`, `inspect`,
`logs`, `top`, `stats`, `wait`, `start`, `port`, `mount`/`unmount`
(the last two already have a different, dedicated shape here, `0361`/
`0362`), and `checkpoint`/`restore` (CRIU-based, a much larger,
separately-scoped gap this project has never attempted at all) — each
a natural, separate future increment, continuing this same one-
command-per-note rollout.
