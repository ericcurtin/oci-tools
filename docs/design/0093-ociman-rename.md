# Design note 0093: `ociman rename` (milestone 3)

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Rename`, `cmd_rename`),
`tests/tests/ociman_rename.rs`.

`ociman` gained real `docker rename`/`podman rename`: rewrite a
container's own `--name`. Found via the same small-gaps survey that
produced 0090-0092, ranked as the most trivial remaining candidate —
`ANNOTATION_NAME`, `validate_container_name`, and `resolve_container_id`
all already existed (built for `run --name`, 0032), so `rename` needed
no new state or primitives at all, only reusing them from a different
call site.

## Verified against real podman source first, not assumed

Read `~/git/podman/cmd/podman/containers/rename.go` directly:
`podman rename CONTAINER NAME`, two positional arguments, silent on
success (no id/name echoed back — unlike `kill`/`stop`, which print
the id). `cmd_rename` matches this exactly.

## Reused, not reimplemented

`cmd_rename` is almost entirely composed of existing pieces:
`resolve_container_id` (id-or-name lookup, already shared by every
container-targeting subcommand), `validate_container_name` (the same
charset check `run --name` already applies), and a name-collision
check identical in spirit to `run --name`'s own (`if let Ok(existing)
= resolve_container_id(...) { bail! }`) — with one small, deliberate
addition `run --name` never needed: renaming a container to its own
*current* name is a harmless no-op, not a self-collision error (a
container can never already be running under the name it's about to
be created with at `run` time, but `rename` can genuinely be asked to
"rename" something to what it's already called).

## Real, manual verification against a real, freshly-pulled busybox

Built the release binary and exercised every real scenario: renaming
by the container's current `--name` and by its own real id, the
renamed container immediately usable under its new name (`rm`
succeeding against it right away), a no-op self-rename, a rejected
collision with a different container's own name, a rejected invalid
new name (matching `validate_container_name`'s own existing charset
rule), and a clean error for an unknown container.

## Real, automated tests

Six integration tests in `tests/tests/ociman_rename.rs`, mirroring
`ociman_name.rs`'s own established seeded-image helpers: rename by
name (and confirms the new name is immediately usable, plus that
`rename` itself prints nothing on success), rename by id, the
self-rename no-op, the name-collision rejection, the invalid-name
rejection, and the unknown-container error.

## Not a hot-path change — no A/B perf re-verification needed

Purely additive: one new `Command` enum variant, one new, wholly
independent function (`cmd_rename`), one new match arm. Confirmed
directly via `git diff --stat`: `synthesize_spec`/`resolve_seccomp`/
`command_for` and every cgroup driver are completely untouched.

## What's still not here

* `ociman inspect` (for containers, not just images)/`top`, `ociman
  run -d`/`--detach`, `ocirun update`/`pause`/`resume`, automated
  failed-systemd-scope cleanup, the build cache, `ONBUILD`/
  `HEALTHCHECK` — all still exactly as earlier increments left them,
  unrelated to this increment's own scope.
