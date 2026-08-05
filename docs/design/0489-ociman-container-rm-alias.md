# Design note 0489: `ociman container rm` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488` started: `rm` — the second-richest member of real podman's own
`podman container <verb>` family (after `inspect`, closed in `0488`)
— was still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/rm.go:39-49`: `containerRmCommand`
  (`Parent: containerCmd`) and top-level `rmCommand` share the exact
  same `Use`/`Short`/`Long`/`RunE`/`Args`/`ValidArgsFunction` verbatim
  — a byte-identical alias, the identical shape `0480` already
  established for `ImageCommand::Rm` on the image side (not the
  "forced type" shape `0488`/`0482`'s own `inspect` variant used —
  `rm` needs no such forcing at all, since a container is the only
  kind of thing it ever resolves).

## Implementation

`ContainerCommand::Rm` is a new variant, field-for-field identical to
the already-existing `Command::Rm` (`ids`, `force`, `all`, `cidfile`,
`ignore`, `time`, `filter`, `latest`), dispatching into the exact same
`cmd_rm` function `ociman rm` itself already calls with the identical
argument order — zero new business logic, zero new primitive, the
same pure-alias shape every other member of the `image`/`container`
families already used.

## Tests

Two new integration tests added to `tests/tests/ociman_container.rs`:

- `container_rm_is_a_byte_identical_alias_for_top_level_rm` — proves
  the alias actually removes a real container and prints its id,
  exactly like the top-level command.
- `container_rm_force_kills_a_still_running_container_first` — proves
  the alias's own flag set works too: refuses a running container
  without `--force`, removes it with `--force` given.

Full `rm` semantics (multi-id resolve-then-act, `--all`, `--cidfile`,
`--ignore`, `--time`, `--filter`, `--latest`) are already exhaustively
tested against the top-level command in `ociman_ps.rs` — this note's
own tests deliberately only prove the alias itself reaches the
identical function with the identical fields, not re-testing `rm`'s
own semantics a second time.

All 12 tests in `tests/tests/ociman_container.rs` pass (10 prior + 2
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0488`), `python3 ci/guards.py` (clean),
`cargo deny check` (clean), `bash ci/native-ci.sh` (one transient
failure on the first attempt — `ocicri_container.rs`'s own
`create_container_capabilities_add_and_drop_change_the_real_process_capability_sets`,
the same already-documented flakiness class, `exit_code: 126`
"process exited before exec"; independently confirmed passing
instantly in isolation, then the full script rerun clean on the
second attempt, both under the same already-documented long-running
CPU-spinning background process this host has had since Jul23), `bash
ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r` round
trip). No benchmark re-run needed: `ociman container rm` is not
exercised by `ci/bench.sh`, and this is a pure dispatch-reuse addition
touching no existing function's body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `stop`, `start`, `top`, `logs`, `cp`, `diff`,
  `commit`, `kill`, `pause`/`unpause`, `rename`, `restart`, `wait`,
  `run`, `create`, `exec`, `attach`, `export`, `port`, `mount`/
  `unmount`, `init`, `stats`, `runlabel` — each a pure-alias candidate
  of the same shape as this one and `0488`, left for future increments
  to keep each one individually small and independently verified.
</content>
