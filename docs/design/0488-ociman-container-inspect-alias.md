# Design note 0488: `ociman container inspect` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

The `ociman container` subcommand family (`0357`, `0431`) had `exists`,
`list`/`ls`, `prune`, and `clone` (`0474`), but not `inspect` — the
richest, most-used member of real podman's own `podman container
<verb>` family and a real, currently-missing gap.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/inspect.go:14-25`: `inspectCmd`
  (`Use: "inspect [options] CONTAINER [CONTAINER...]"`, `Parent:
  containerCmd`) registers the exact same flag set as the richer
  top-level `podman inspect`: `--size`/`-s`, `--format`/`-f` (default
  `"json"`), and `validate.AddLatestFlag` (`--latest`/`-l`) — a
  full-featured alias, not a narrower one like `image inspect`'s own
  `format`-only surface (`0482`).
- `~/git/podman/cmd/podman/containers/inspect.go:43-46`:
  `inspectExec` unconditionally sets `inspectOpts.Type =
  common.ContainerType` before calling the exact same shared
  `inspect.Inspect(args, *inspectOpts)` function the top-level
  `podman inspect --type container` also reaches — never falling back
  to an image on a miss, the identical "same flags, forced type" shape
  `0482` already established for `ImageCommand::Inspect`.
- `~/git/podman/cmd/podman/containers/container.go:13-20`:
  `containerCmd` itself (`Use: "container"`, `TraverseChildren:
  true`), confirming the nesting shape this project's own
  `Command::Container`/`ContainerCommand` already mirrors.

## Implementation

`ContainerCommand::Inspect` is a new variant with the same four
fields as the top-level `Command::Inspect` minus `inspect_type` (never
a user-facing choice here — always forced): `reference: Option<String>`,
`latest: bool`, `format: Option<String>`, `size: bool`.

Its dispatch arm replays the exact same two validation checks the
top-level `Command::Inspect` arm already has — `--latest` +
explicit reference together is an error, and no reference without
`--latest` is an error — then resolves the reference (via
`resolve_latest_container` when `--latest`, otherwise the given
reference directly) and calls the *same* `cmd_inspect` with
`InspectType::Container` hardcoded. The top-level arm's third check
(`latest && inspect_type == InspectType::Image`, an error) is omitted
entirely here: it can never fire since this command's own type is
always `Container`.

Zero new business logic, zero new primitive: 100% reuse of the
existing `cmd_inspect` function, `ContainerInspectView`,
`resolve_latest_container`, and `InspectType::Container`'s own
already-established never-falls-back-to-an-image resolution path
(`cmd_inspect`'s own `if inspect_type != InspectType::Image` branch,
`0409`) — the exact same size and shape as `0480`/`0482`'s own
additions to the `image` alias family.

## Tests

Five new integration tests added to `tests/tests/ociman_container.rs`:

- `container_inspect_is_a_byte_identical_alias_for_top_level_inspect_forced_to_container_type`
  — proves byte-identical stdout against `ociman inspect --type
  container`, both bare and with `--format`.
- `container_inspect_never_falls_back_to_an_image_on_a_container_miss`
  — mirrors `ociman_inspect.rs`'s own `inspect_type_container_never_
  resolves_an_image`, proving the alias has the identical no-fallback
  behavior.
- `container_inspect_latest_works` — proves `--latest` resolves the
  most recently created container (using the same 1200ms wall-clock
  gap `ociman_mount.rs`'s own `unmount_latest_targets_the_most_
  recently_created_container` established, needed since this
  project's own `created` timestamp comparison is otherwise
  ambiguous between two containers created back-to-back), and that
  `--latest` plus an explicit reference together is a clear error.
- `container_inspect_size_works` — proves `--size` populates a real
  `ContainerSizeView`.

All 10 tests in `tests/tests/ociman_container.rs` pass (5 prior + 5
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test *file* was added, so the
block count is unchanged from `0487`), `python3 ci/guards.py` (clean),
`cargo deny check` (clean), `bash ci/native-ci.sh` (clean), `bash
ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r` round
trip). No benchmark re-run needed: `ociman container inspect` is not
exercised by `ci/bench.sh`, and this is a pure dispatch-reuse addition
touching no existing function's body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `rm`, `stop`, `start`, `top`, `logs`, `cp`,
  `diff`, `commit`, `kill`, `pause`/`unpause`, `rename`, `restart`,
  `wait`, `run`, `create`, `exec`, `attach`, `export`, `port`, `mount`/
  `unmount`, `init`, `stats`, `runlabel` — each a pure-alias candidate
  of the same shape as this one and `0480`/`0482`, left for future
  increments to keep each one individually small and independently
  verified.
</content>
