# Design note 0094: `ociman inspect` for containers (milestone 3)

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Inspect`'s doc comment,
`cmd_inspect`, new `ContainerInspectView`), `tests/tests/
ociman_inspect.rs`.

`ociman inspect` previously only ever resolved against the image
store — a container id/name given to it fell straight through to
`Reference::parse` and produced a confusing "no such image" error.
Found via the ongoing small-gaps survey (0090-0093), ranked small: no
new state, no schema change, only a new view struct built entirely
from fields `PersistedState` already tracks.

## Verified against real podman source first, not assumed

Read `~/git/podman/cmd/podman/inspect/inspect.go` directly: the
default (no `--type` given) resolution order tries a container first,
falling back to an image, then volume, then network
(`inspectAll`). `oci-tools` only has the first two so far —
`cmd_inspect` now tries the container store first, falling back to the
image store (the pre-existing, unchanged behavior) exactly matching
that order.

## A deliberately narrower view than real podman's own inspect

Real `podman inspect` on a container returns a large, real
`Config`/`HostConfig`/`NetworkSettings`/`Mounts`/`State` structure.
`ContainerInspectView` here is deliberately smaller: the same fields
`ContainerView` ("`ps`") already exposes (id/name/image/command/
status/created/exit_code), plus the lower-level `pid`/`bundle`/
`rootfs` `PersistedState` already tracks (the same three fields real
`runc state` reports) — a genuine, real improvement over "image only"
today, not a claim of parity with real podman's own much richer
output.

## The resolution order, matched exactly with a clear fallback error

`cmd_inspect` tries `open_container_store()` +
`resolve_container_id()` (the same id-or-`--name` lookup every other
container-targeting subcommand already shares) first; only if that
fails does it fall through to the existing, unchanged image-store
logic. A reference matching neither still produces the same clear,
image-store-flavored error message this function has always given for
an unknown image — not a confusing "neither a container nor an image"
compound message, matching this project's own established preference
for the clearer of two plausible errors over a technically more
complete one.

## Real, manual verification against a real, freshly-pulled busybox

Built the release binary and exercised every real scenario: inspect
by a container's own `--name`, inspect by its real id (identical
data), inspect an image reference (completely unchanged output,
confirming the fallback path still works exactly as before), and a
clear error for a reference matching neither.

## Real, automated tests

Four integration tests in `tests/tests/ociman_inspect.rs`: inspect by
name (checking every `ContainerInspectView` field, including the
image reference's own real normalized form —
`docker.io/<repo>:<tag>`, a real detail caught while writing the test:
an early draft asserted the unnormalized reference string and failed),
inspect by id (confirming `name` is omitted entirely when no `--name`
was given, matching `ContainerView`'s own established
`skip_serializing_if`), the image fallback (confirming the returned
JSON is a real `ImageConfig` — has `architecture`, not `status`/
`pid`), and the unknown-reference error case.

## Not a hot-path change — no A/B perf re-verification needed

Confirmed directly via `git diff --stat`/`--stat` hunks: only
`Command::Inspect`'s own doc comment, `cmd_inspect`, and the new
`ContainerInspectView` struct/impl changed. `synthesize_spec`/
`resolve_seccomp`/`command_for` and every cgroup driver are completely
untouched.

## What's still not here

* `ociman top`, `ociman run -d`/`--detach`, `ocirun update`/`pause`/
  `resume`, automated failed-systemd-scope cleanup, the build cache,
  `ONBUILD`/`HEALTHCHECK` — all still exactly as earlier increments
  left them, unrelated to this increment's own scope.
* Real podman's own much richer container-inspect fields (`Config`/
  `HostConfig`/`NetworkSettings`/`Mounts`/`State`) — this increment is
  deliberately the narrow first slice, matching this project's own
  established "narrow first increment" pattern.
