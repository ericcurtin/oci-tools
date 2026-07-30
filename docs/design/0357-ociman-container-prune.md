# Design note 0357: `ociman container prune`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`,
`README.md`.

## What this closes

`ContainerCommand` (`ociman container ...`) had exactly one subcommand
since `0287`: `exists`. Real `podman container prune` had no
equivalent here at all — flagged in a fresh scoping pass over
`ociman`'s own subcommand families rather than a flat top-level verb
survey (the kind `0341`-`0356` mostly worked through).

## Real, checked-directly semantics

Read `~/git/podman/cmd/podman/containers/prune.go` and
`~/git/podman/libpod/runtime_ctr.go`'s own `PruneContainers` directly:

- `--force`/`-f`: only skips a real, interactive `y/N` confirmation
  prompt before removing anything. This project has no interactive
  prompt anywhere to skip in the first place (the same "nothing to
  skip" reasoning already applied to `ocibox create --pull`'s own
  `--yes`), so the flag is accepted for CLI compatibility but changes
  nothing.
- `--filter` (e.g. `label=<key>=<value>`) also exists on real `podman
  container prune`. Deferred for this first slice, matching this
  project's own established "narrow first slice, revisit later"
  precedent (e.g. `0349`/`0350`'s own `images --filter`
  `id=`/`digest=` survey deliberately leaving `readonly=`/
  `intermediate=`/`manifest=` for later).
- Eligibility (`PruneContainers`'s own `containerStateFilter`): a
  container qualifies only if its real state is `Stopped`, `Exited`,
  `Created`, or `Configured` (Go's own four names) — `Running` and
  `Paused` are never touched. This project's own simpler two-way
  split, already used identically by `ociman rm --all` (`0266`) and
  `cmd_start`'s own precondition, maps onto exactly `Status::Created`
  OR `Status::Stopped` (`Status::Creating` — a container still mid-
  `create`, never actually reached `Created` — is correctly excluded
  too, matching real podman's own omission of any "still being
  configured" state from this list).
- The real removal call inside `PruneContainers`
  (`RemoveContainer(ctx, c, false, false, time)`) never force-kills a
  live process, since none of the eligible states have one.
- Real output (`utils.PrintContainerPruneResults(responses, false)`,
  checked directly): one line per removed container's own id, no
  heading (`heading: false` is the literal argument `prune()` passes)
  — the exact same shape `ociman volume prune` (`0173`) already
  established for its own `Vec<String>` of removed names.

## Implementation

New `ContainerCommand::Prune { force: bool }`; dispatch in `main.rs`
calls `cmd_container_prune(cli.global.json)` (the `force` field is
read but intentionally discarded — see the doc comment on the variant
itself for why). `cmd_container_prune` lists every container, filters
by `matches!(state.effective_status(), Status::Created |
Status::Stopped)`, and removes each match via the existing
`remove_container` primitive (`ociman rm`'s own primitive, no new
removal logic).

One real wrinkle, found only once tests started exercising a genuine
`Created` (never-started) container rather than just a `Stopped` one:
`remove_container`'s own `force` parameter doubles as "skip the *any*
non-`Stopped` refusal" in this project's model, not just "kill a live
process" the way real podman's own `force` argument does. A merely-
`Created` container (no process has ever execve'd) is not `Stopped`
either, so calling `remove_container(.., force: false, ..)` on it
trips that same refusal `ociman rm` (without `--force`) hits on any
non-stopped container. `cmd_container_prune` therefore always passes
`force: true` — safe here specifically because the eligibility filter
above already excludes every status (`Running`/`Paused`) where `force`
would otherwise change anything real (actually signaling a live
process).

`--json` prints the same removed-id list as a JSON array, matching
`ociman volume prune --json`'s own already-established shape.

## Verified

New `tests/tests/ociman_container.rs` (previously only
`ociman_exists.rs` covered this subcommand family):
`container_prune_removes_created_and_stopped_but_not_running` (a real
`Created`, a real `Stopped`, and a real detached `Running` container
in the same store; asserts exactly the first two are reported and
removed, the third survives running, then a second `prune` with
nothing left reports and removes nothing);
`container_prune_force_is_accepted_and_behaves_identically`;
`container_prune_json_emits_an_array_of_removed_ids`.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures; one
transient, unrelated `ocicri_container.rs`
`exec_sync_runs_commands_in_a_running_container` flake on the first
full-suite run, confirmed transient via an isolated `--test-threads=1`
rerun and a second clean full-suite run), `python3 ci/guards.py`,
`cargo deny check`, `bash ci/native-ci.sh`, `bash ci/build-deb.sh`
(real `dpkg -i`/`--version`/`dpkg -r` round trip).
