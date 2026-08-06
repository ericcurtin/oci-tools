# Design note 0504: `ociman container attach` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0503` started: `attach` — the eighteenth member of real
podman's own `podman container <verb>` family closed so far — was
still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/attach.go:16-51`:
  `containerAttachCommand` (`Parent: containerCmd`) and top-level
  `attachCommand` share the exact same `Use`/`Short`/`Long`/`Args`
  (`validate.IDOrLatestArgs`)/`RunE`/`ValidArgsFunction`, and both get
  the identical flag set applied via the one shared `attachFlags
  (cmd)` helper (`--detach-keys`, `--no-stdin`, `--sig-proxy`) plus
  `validate.AddLatestFlag` — a byte-identical alias, the same shape
  `0492` already established for `Self::Kill`.

## Implementation

`ContainerCommand::Attach` is a new variant, field-for-field
identical to the already-existing `Command::Attach` (`id`, `latest`)
— this project's own top-level `attach` has always been deliberately
output-only, with no `--no-stdin`/`--detach-keys`/`--sig-proxy` flags
at all (this project's own current architecture only ever wires up a
container's stdin once, at its original `run`/`create` time — see
`Command::Attach`'s own doc comment), so the alias mirrors that same,
already-existing scope exactly rather than inventing a wider one that
doesn't exist at the top level either — the same pattern `0491`'s
`Start`/`0494`'s `Restart`/`0498`'s `Logs`/`0501`'s `Commit`/`0503`'s
`Stats` variants already established.

Since `Command::Attach`'s own dispatch arm does its explicit-id-wins-
over-latest resolution inline (there's no dedicated `cmd_attach`-
adjacent wrapper that already takes raw, unresolved `id`/`latest`
fields), the new arm replays the identical logic verbatim before
calling the same `cmd_attach(&resolved_id)` — the same "replay the
top-level arm's own inline validation" shape `0488`/`0491`/`0496`/
`0497`/`0498`/`0503` already used.

## Tests

One new integration test added to `tests/tests/ociman_container.rs`:

- `container_attach_is_a_byte_identical_alias_for_top_level_attach`
  — proves the alias actually streams a real, running container's
  own full output (from the start, not just what's written after
  attach began) and propagates its own real exit code, exactly like
  the top-level command.

Full `attach` semantics (attaching to an already-stopped container's
own clear error, `--latest`, explicit-id-wins-over-latest) are
already exhaustively tested against the top-level command in
`ociman_attach.rs` — this note's own test deliberately only proves
the alias itself reaches the identical function with the identical
fields, not re-testing `attach`'s own semantics a second time.

All 30 tests in `tests/tests/ociman_container.rs` pass (29 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0503`; needed one retry — a transient
`ocicri_container.rs` flake in the same already-documented class,
independently confirmed passing in isolation, then clean with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (one transient flake in the
same class on the first attempt, independently confirmed passing in
isolation, then clean on the second attempt with
`RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). No
benchmark re-run needed: `ociman container attach` is not exercised
by `ci/bench.sh`, and this is a pure dispatch-reuse addition touching
no existing function's body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `run`, `create`, `exec`, `port`, `mount`/
  `unmount`, `init`, `runlabel` — each a pure-alias candidate of the
  same shape as this one and `0488`-`0503`, left for future
  increments to keep each one individually small and independently
  verified.
- Real podman's own richer `podman attach`/`podman container attach`
  (`--no-stdin`/`--detach-keys`/`--sig-proxy`, real stdin forwarding
  into an already-running container) — a genuinely separate, still-
  open architectural gap in the *top-level* `ociman attach` itself
  (not something this alias increment introduces or could close on
  its own), left for its own future increment.
</content>
