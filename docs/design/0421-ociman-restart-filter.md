# Design note 0421: `ociman restart --filter`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_start.rs`,
`README.md`.

## What this closes

`ociman restart` had no `--filter` support at all. This closes the
`label=`/`label!=`/`until=` slice — the fourth port in the same
`--filter` family `ociman container prune`/`ociman stop`/`ociman rm`
(0418-0420) already established, reusing the exact same shared
`LabelUntilFilters`/`parse_label_until_filters`/`matches_label_
until_filters` those increments already put in place. Like `0420`,
no new shared code was needed at all.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/restart.go:66-68,114-119`: real
`podman restart --filter`, wired through the identical
`getContainers` (`~/git/podman/pkg/domain/infra/abi/containers.go`)
real `podman stop`/`rm --filter` already go through — the same
"filter replaces the base selection, explicit names would further
narrow it" semantics `0419`'s own design note already confirmed.
Deliberately not attempting that further-narrowing interaction here
either, for the same reason `0419`/`0420` both gave.

## Implementation

- `Command::Restart` gains `filter: Vec<String>` (`#[arg(long =
  "filter")]`), documented identically to `Command::Stop::filter`/
  `Command::Rm::filter`: mutually exclusive with an explicit `ID`/
  `--name`, `--cidfile`, and `--all`.
- `cmd_restart` gains a `filter: &[String]` parameter and the same
  mutual-exclusivity check `cmd_stop`/`cmd_rm` already have; when
  non-empty, the matched ids are collected and handed to the
  already-existing `restart_many` exactly the same way `--all`'s own
  full container list already is — so `--filter` automatically
  shares `restart_many`'s own deferred-scope-reset handling (the
  real, previously-hit `fork()`-safety fix `0315`/`0316` already put
  in place for any multi-target restart) with no filter-specific
  change to that logic needed at all.

## Tests

Two new tests in `tests/tests/ociman_start.rs` (where this project's
own existing `restart` test suite already lives):
`restart_filter_label_only_restarts_a_matching_container` (two
never-started, `Created` containers, one matching `--filter
label=env=prod`, one not; only the matching one is started for the
first time — its own `true` command runs and exits, landing on
`Stopped`, exactly matching `restart --all`'s own already-established
treatment of a never-started container — while the non-matching one
stays untouched, still `Created`) and `restart_filter_combined_with_
all_or_an_explicit_id_is_a_clear_error` (both mutual-exclusivity
cases). All 19 prior tests in `ociman_start.rs` continue to pass
unmodified (21/21 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg
-r` round trip). Touches only `ociman restart`'s own selection
logic, not any hot path at all — no benchmark re-run needed.

## Deliberately still out of scope

Same items `0419`/`0420`'s own design notes already listed:
combining `--filter` with explicit names/`--cidfile`/`--all`; the
wider `ps`-grammar keys (needs `cmd_ps`'s own inline matching closure
extracted first); `ociman pause`/`unpause --filter` — now down to two
remaining sibling commands in this same family, each still a
natural, similarly small, mechanical port reusing the exact same
shared helpers this increment needed zero new code for.
