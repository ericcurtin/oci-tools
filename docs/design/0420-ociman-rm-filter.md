# Design note 0420: `ociman rm --filter`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`,
`README.md`.

## What this closes

`ociman rm` had no `--filter` support at all. This closes the
`label=`/`label!=`/`until=` slice — the same deliberate, narrower-
first-slice scope `ociman stop --filter` (0419) and `ociman
container prune --filter` (0418) already established, reusing the
exact same shared `LabelUntilFilters`/`parse_label_until_filters`/
`matches_label_until_filters` those two increments already put in
place. This is the third and most mechanical port of that same
family so far — no new shared code needed at all.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/rm.go:77-79,123-129`: real
`podman rm --filter`, wired through the identical `getContainers`
(`~/git/podman/pkg/domain/infra/abi/containers.go`) real `podman stop
--filter` already goes through — confirmed in `0419`'s own design
note already: when `--filter` is given, it becomes the base
selection, optionally narrowed further by explicit names if also
given. Deliberately not attempting that further-narrowing
interaction here either, for the same reason `0419` gave.

## Implementation

- `Command::Rm` gains `filter: Vec<String>` (`#[arg(long =
  "filter")]`), documented identically to `Command::Stop::filter`:
  mutually exclusive with an explicit `ID`/`--name`, `--cidfile`, and
  `--all`.
- `cmd_rm` gains a `filter: &[String]` parameter and the same mutual-
  exclusivity check `cmd_stop` already has; a new branch (checked
  before the existing `match (ids.is_empty(), all)` logic) mirrors
  the existing `(true, true)` (`--all`) loop exactly — attempt every
  match, report the first real failure at the end, keep going past
  it — but narrows to only the containers `matches_label_until_
  filters` actually matches, instead of every container.

No new struct/parser/matcher was needed at all — this increment is
almost entirely CLI wiring plus one new loop reusing 0418/0419's own
already-proven shared code, exactly matching the "each should be a
similarly small, mechanical port" prediction `0419`'s own design note
made.

## Tests

Two new tests in `tests/tests/ociman_ps.rs` (where this project's own
existing `rm` test suite already lives, alongside `ps`):
`rm_filter_label_only_removes_a_matching_container` (two real,
already-exited containers — `rm` without `--force` refuses a non-
`Stopped` container, the same real gate `rm --all` without `--force`
already has, so this needed genuinely stopped containers via `run`,
not bare `create` — one matching `--filter label=env=prod`, one not;
only the matching one is removed) and `rm_filter_combined_with_all_
or_an_explicit_id_is_a_clear_error` (both mutual-exclusivity cases).
All 49 prior tests in `ociman_ps.rs` continue to pass unmodified
(51/51 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures on a clean run — one earlier attempt hit the
known, pre-existing `ociman_run.rs` cgroup-timing host-contention
flake, confirmed environmental via an immediate isolated rerun),
`python3 ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`
(clean, 119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip). Touches only `ociman rm`'s own selection
logic, not any hot path at all — no benchmark re-run needed.

## Deliberately still out of scope

Same three items `0419`'s own design note already listed: combining
`--filter` with explicit names/`--cidfile`/`--all`; the wider
`ps`-grammar keys (needs `cmd_ps`'s own inline matching closure
extracted first); `ociman restart`/`pause`/`unpause --filter` — now
down to three remaining sibling commands in this same family, each
still a natural, similarly small, mechanical port reusing the exact
same shared helpers this increment needed zero new code for.
