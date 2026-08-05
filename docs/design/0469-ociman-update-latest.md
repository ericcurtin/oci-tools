# Design note 0469: `ociman update --latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_update.rs`.

## What this closes

Every other single-container `ociman` command already supports
`--latest`/`-l` (the `0434`-`0452` series: `rm`/`stop`/`restart`/
`kill`/`pause`/`unpause`/`stats`/`wait`/`top`/`exec`/`logs`/`diff`/
`inspect`/`start`/`attach`), but `update` was never included in that
series and had no `--latest` support at all — a real, previously-
missing flag, not a partial/edge-case gap.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/update.go:27`: `Args:
  validate.IDOrLatestArgs` on `updateCommand` (and its identical
  `containerUpdateCommand` alias for `podman container update`).
  Line 44-45: `ContainerUpdateOptions{ ContainerCreateOptions; Latest
  bool }`. Lines 61/68: `validate.AddLatestFlag(...)` registered on
  both command variants.
- `~/git/podman/cmd/podman/validate/latest.go:8-14`:
  `AddLatestFlag` — registers `-l`/`--latest`, default `false`.
- `~/git/podman/cmd/podman/validate/args.go:35-51`: `IDOrLatestArgs`
  — the exact validation semantics ported here verbatim: at most one
  positional argument; if `--latest` is not given and no argument is
  given either, `"%q requires a name, id, or the \"--latest\" flag"`;
  if `--latest` is given and an argument is also given, `"--latest
  and containers cannot be used together"`.

## Implementation

- `Command::Update`: `id: String` → `id: Option<String>`, new `#[arg(
  short = 'l', long)] latest: bool` field.
- The `Some(Command::Update { .. })` match arm gains the exact same
  two-step resolution block `Command::Wait`'s own arm already
  established (`0434`-`0452`'s own precedent): a mutual-exclusivity
  check first, then either the given `id` or (requiring `latest`)
  `resolve_latest_container(&open_container_store()?)?` — the same
  shared primitive every other `--latest` command already uses, no
  new resolution logic needed. `cmd_update`'s own signature (`id:
  &str`) needed no change at all — a pure CLI-layer/dispatch-layer
  change.

## Tests

Three new integration tests in `tests/tests/ociman_update.rs`:
`update_latest_targets_the_most_recently_created_container` (a real
running container updated by `--latest` alone, verified against the
real live `memory.max` cgroup file, matching the existing explicit-id
test's own verification style), `update_latest_and_an_explicit_id_
together_is_a_clear_error`, `update_with_neither_an_id_nor_latest_is_
a_clear_error` (both checking the exact real podman wording). All 15
tests in the file pass (12 prior + 3 new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures on the confirming rerun — the first two attempts
each hit transient, already-documented flaky failures in
`ocicri_container.rs`, all confirmed unrelated and passing instantly
in isolation, consistent with this dev host's long-running CPU-
spinning background process), `python3 ci/guards.py` (clean), `cargo
deny check` (clean), `bash ci/native-ci.sh` (clean, 120/120 on the
first attempt), `bash ci/build-deb.sh` (clean, real `dpkg -i`/
`--version`/`dpkg -r` round trip on the first attempt). No benchmark
re-run needed: `ociman update` is not exercised by `ci/bench.sh` at
all, and this change is a pure CLI-dispatch-layer addition with no
effect on any hot path (`run`/`create`/`build`) `bench.sh` does
measure.

## Deliberately still out of scope

The secondary candidates flagged during research (`ociman mount`/
`unmount` missing `--latest`/bare-all-listing mode) are a slightly
larger, two-sub-behavior change (an explicit resolution rule plus a
new "list every mounted container" output shape) and are left for
their own future increment.
