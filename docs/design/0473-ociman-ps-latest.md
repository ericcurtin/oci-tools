# Design note 0473: `ociman ps`/`container list` `--latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## What this closes

`ociman ps` (and its `container list`/`container ls` alias) had no
`--latest`/`-l` flag at all — a real, previously-missing gap on a
command the `0469`-`0472` `--latest`/`--all`/multi-id series never
touched (that series covered `update`/`mount`/`unmount`, all
single/multi-container action commands, not the `ps` listing command
itself).

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/list.go:29` /
  `~/git/podman/cmd/podman/containers/ps.go:64,71`:
  `validate.AddLatestFlag(...)` registered on `ps`, `container list`,
  and `container ps` alike — a real `-l`/`--latest` flag on every one.
- `~/git/podman/pkg/domain/infra/abi/containers.go:1147-1150`
  (`ContainerList`): `if options.Latest { options.Last = 1 }` — real
  `podman ps --latest` is *literally* just `--last 1` under another
  name, sharing every one of `--last`'s own real selection/visibility
  behaviors (`0171`'s own already-established `--last` implementation
  here) rather than needing any new selection logic of its own.
- `~/git/podman/cmd/podman/containers/ps.go:112-114` (`checkFlags`):
  `if listOpts.Last >= 0 && listOpts.Latest { return errors.New("last
  and latest are mutually exclusive") }` — note the real, checked-
  directly **stricter threshold** here (`>= 0`) than the selection
  logic elsewhere uses (`> 0`, `~/git/podman/pkg/ps/ps.go:36`): `--last
  0 --latest` together is still a real validation error, even though
  `--last 0` alone is otherwise a documented no-op (`Command::Ps::
  last`'s own doc comment, `0171`).
- `~/git/podman/cmd/podman/containers/ps.go:88-89`: `--last`/`-n`'s
  own real default is the literal `-1` — already exactly matching
  this project's own existing `default_value_t = -1`, so the `>= 0`
  check never spuriously fires against the unset default.

## Implementation

A pure translation shim in front of code that already exists in
full — no new selection/sorting primitive needed at all:

- `Command::Ps`/`ContainerCommand::List` (the latter documented as
  sharing `Ps`'s identical field set, `0332`-era convention) both
  gain `#[arg(short = 'l', long)] latest: bool`.
- `cmd_ps` gains a `latest: bool` parameter. Before any selection
  logic runs: `anyhow::ensure!(!(last >= 0 && latest), "last and
  latest are mutually exclusive")` (matching the real, stricter `>=
  0` threshold above), then `let last = if latest { 1 } else { last
  };` — from that point on, the already-existing `let all = all ||
  last > 0;` and the `--last`-driven `views.split_off(...)` trailing-
  slice logic (`0171`) run completely unmodified, exactly reproducing
  real podman's own `Latest -> Last = 1` translation.
- Both dispatch call sites (`Some(Command::Ps { .. })` and
  `ContainerCommand::List { .. }`) updated to thread the new field
  through.

## Tests

Four new integration tests in `tests/tests/ociman_ps.rs`:
`ps_latest_shows_only_the_most_recently_created_container` (three
never-started containers, `--latest`/`-l` alike keep only the single
newest, overriding the default running-only visibility rule the same
way `--last`/`-n` already does), `ps_latest_combined_with_last_is_a_
clear_error` (both the `>0` and the `>=0`-but-not-`>0` case, i.e.
`--last 0`, are real errors; the implicit `-1` default never
conflicts), `container_list_latest_is_a_byte_identical_alias_for_ps_
latest` (matching the file's own pre-existing `container_list_and_
ls_are_byte_identical_aliases_for_ps` convention). All 57 tests in
`ociman_ps.rs` pass (53 prior + 4 new); all 6 in `ociman_container.rs`
pass unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures on the second attempt — the first attempt hit one
transient, already-documented flaky failure in `ociman_exec.rs`,
confirmed unrelated and passing instantly in isolation), `python3
ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (clean, 120/120 on the first attempt), `bash
ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r` round
trip on the first attempt). No benchmark re-run needed: `ociman ps`
is not exercised by `ci/bench.sh` at all, and this change touches no
hot path (`run`/`create`/`update`/`build` spec construction or launch
mechanism) — a pure listing-command CLI addition.

## Deliberately still out of scope

Real `podman ps`'s own `--watch`/`-w` flag (repeated polling output)
has no equivalent here at all — `ociman ps` has never had a `--watch`
concept, so the corresponding "`--watch` and `--latest` cannot be
used together" validation rule real podman also has doesn't apply.
