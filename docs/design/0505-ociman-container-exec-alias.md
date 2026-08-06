# Design note 0505: `ociman container exec` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0504` started: `exec` — the nineteenth member of real
podman's own `podman container <verb>` family closed so far, and the
richest flag set in the whole family — was still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/exec.go:28-127`:
  `containerExecCommand` (`Parent: containerCmd`) and top-level
  `execCommand` share the exact same `Use`/`Short`/`Long`/`RunE`/
  `ValidArgsFunction`, and both get the identical flag set applied
  via the one shared `execFlags(cmd)` helper — a byte-identical
  alias, the same shape `0492` already established for `Self::Kill`.

## Implementation

`ContainerCommand::Exec` is a new variant, field-for-field identical
to the already-existing `Command::Exec` (`positional`, `latest`,
`cidfile`, `user`, `workdir`, `env`, `env_file`, `interactive`,
`preserve_fds`, `privileged`) — this project's own top-level `exec`
has always been an honestly narrower first slice than real podman's
own richer one (no `--detach`/`--detach-keys`/`--tty`, see
`Command::Exec`'s own doc comment), so the alias mirrors that same,
already-existing scope exactly rather than inventing a wider one that
doesn't exist at the top level either.

Since `Command::Exec`'s own dispatch arm does its manual container-
reference-vs-command disambiguation inline (there's no dedicated
`cmd_exec`-adjacent wrapper that already takes a raw, unresolved
`positional`/`latest`/`cidfile` triple), the new arm replays the
identical logic verbatim — the same `determineTargetCtrAndCmd`-
matching resolution, the same combined `--env-file`-then-`--env`
environment-merge step, before calling the same `cmd_exec(&id,
user.as_deref(), workdir.as_deref(), &combined_env, preserve_fds,
interactive, privileged, &args)` — the same "replay the top-level
arm's own inline validation" shape `0488`/`0491`/`0496`/`0497` (`Top`)
already used, just longer given `exec`'s own richer flag set.

## Tests

One new integration test added to `tests/tests/ociman_container.rs`:

- `container_exec_is_a_byte_identical_alias_for_top_level_exec` —
  proves the alias actually runs a real command inside a real,
  running container and captures its output, exactly like the
  top-level command.

Full `exec` semantics (`--user`, `--workdir`, `--env`/`--env-file`,
`--interactive`, `--preserve-fds`, `--privileged`, `--latest`/
`--cidfile`, docker-compatibility leading-slash stripping) are already
exhaustively tested against the top-level command in
`ociman_exec.rs` — this note's own test deliberately only proves the
alias itself reaches the identical function with the identical
fields, not re-testing `exec`'s own semantics a second time.

All 31 tests in `tests/tests/ociman_container.rs` pass (30 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0504`; needed one retry — a transient
`ociman_exec.rs` flake in the same already-documented class,
independently confirmed passing in isolation, then clean with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (one transient
`ocicri_container.rs` flake in the same already-documented class on
the first attempt, independently confirmed passing in isolation,
then clean on the second attempt with `RUST_TEST_THREADS=2`), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). No benchmark re-run needed:
`ociman container exec` is not exercised by `ci/bench.sh`, and this
is a pure dispatch-reuse addition touching no existing function's
body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `run`, `create`, `port`, `mount`/`unmount`,
  `init`, `runlabel` — each a pure-alias candidate of the same shape
  as this one and `0488`-`0504`, left for future increments to keep
  each one individually small and independently verified.
- Real podman's own richer `podman exec`/`podman container exec`
  (`--detach`/`--detach-keys`/`--tty`) — a genuinely separate,
  still-open gap in the *top-level* `ociman exec` itself (not
  something this alias increment introduces or could close on its
  own), left for its own future increment.
</content>
