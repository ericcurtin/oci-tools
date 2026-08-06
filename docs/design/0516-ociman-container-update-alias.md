# Design note 0516: `ociman container update` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

A real, checked-directly `ociman container <verb>` alias not yet
ported: `update`. Independently found this turn (not a re-examined
old deferral) by scanning real podman's own `containers` package for
registrations this project's own `ContainerCommand` enum (21 verbs
before this one, `0488`-`0507`/`0511`) didn't already cover.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/update.go:32-40`:
  `containerUpdateCommand` (`Parent: containerCmd`, implicit via
  `registry.Commands` registration) copies top-level `updateCommand`'s
  own `Args`/`Use`/`Short`/`Long`/`RunE`/`ValidArgsFunction` verbatim
  -- the exact same byte-identical-alias shape every other already-
  ported verb here uses.
- `~/git/podman/cmd/podman/containers/update.go:46-49`
  (`updateFlags`): both registrations get the identical flag set
  applied via the one shared function.

## Implementation

`ContainerCommand` gains `Update { args: Box<ContainerUpdateArgs> }`.
`Command::Update` itself has 21 individually-declared fields (not a
single flattened struct the way `RunArgs` already was for `Run`/
`Create`), so embedding them directly in `ContainerCommand` would
trigger `clippy::large_enum_variant` (confirmed: every other
`ContainerCommand` variant is tiny). Rather than duplicate all 21
`#[arg(...)]` declarations inline and *then* box the whole thing,
a new `ContainerUpdateArgs` struct (`#[derive(Debug, clap::Args)]`)
holds them once, flattened via `#[command(flatten)]` and boxed --
the same "only the smaller enum needs boxing, not the larger one
it's shared with" asymmetry `0506`/`0507` already established for
`RunArgs`, just introducing a new shared struct instead of reusing
an existing one (since `Command::Update`'s own fields were never
flattened from a struct in the first place, and this increment
deliberately leaves that original, larger enum's own declarations
completely untouched). The dispatch arm destructures `*args` and
replays the top-level arm's own inline `--latest`/id-resolution
validation verbatim (dispatch shape (b), the same one `Diff`/`Exec`
already use), then calls the identical `cmd_update` with the
identical field set.

## Tests

One new integration test in `tests/tests/ociman_container.rs`:
`container_update_is_a_byte_identical_alias_for_top_level_update` --
a real running container, updated via the alias with `--memory`,
proving a real, live cgroup `memory.max` effect (not just a
successful exit code) to confirm the boxed 21-field struct really
threads every value through the dispatch arm correctly. A new local
`real_cgroup_dir_for` helper duplicates `ociman_update.rs`'s own
identically-named one (a few lines, not worth a cross-test-file
dependency, the same reasoning this project's own production code
already applies elsewhere for small duplicated helpers).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean -- confirming the `Box` genuinely resolves
`clippy::large_enum_variant`), `cargo test --workspace --locked` (122
test-result blocks, 0 failures -- no new test file added, so the
block count is unchanged from `0515`). Two rounds of transient
flakiness hit during verification, both confirmed unrelated to this
change: `ocicri_container.rs`'s
`create_container_oom_score_adj_sets_a_real_value` failed once under
`RUST_TEST_THREADS=2` (passed immediately in isolation), then a
`RUST_TEST_THREADS=1` full-suite rerun hit
`ociman_logs.rs`'s own documented follow-test flake and, on the very
next attempt, `ocicri_container.rs`'s
`create_container_capabilities_add_and_drop_change_the_real_process_
capability_sets` (passed 3/3 in isolation) -- a third full-suite
attempt with `RUST_TEST_THREADS=1` finally ran clean throughout.
`python3 ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (clean on the third attempt, `RUST_TEST_THREADS=1`),
`bash ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). Pure CLI-dispatch-layer plumbing
onto the already-existing `cmd_update` -- no hot path touched, no
`ci/bench.sh` rerun needed.

## Deliberately still out of scope

`port`/`init`/`runlabel` (`0510`'s own re-confirmed-correct
deferrals) and `checkpoint`/`restore` (CRIU-based, an already-
established project-wide out-of-scope gap) remain. `clone` is
correctly container-only with no top-level twin at all (real
podman's own `clone.go` registers it solely under `containerCmd`),
matching this project's own existing container-only `ContainerCommand
::Clone`. One real, previously-unexamined nested-only command found
while compiling this list, `cleanup` (`~/git/podman/cmd/podman/
containers/cleanup.go` -- "Cleans up mount points and network
stacks on one or more containers from the host... used internally
when running containers"): genuinely un-triaged, not yet confirmed
either way, a candidate for a future increment rather than something
this note can honestly claim is closed.
</content>
