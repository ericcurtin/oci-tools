# Design note 0091: `ociman kill` (milestone 3)

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Kill`, `cmd_kill`),
`tests/tests/ociman_kill.rs`.

`ociman` gained real `docker kill`/`podman kill`: send a signal to a
running container's own init process, once, with no wait and no
escalation at all — distinct from `ociman stop`'s own graceful-then-
`KILL` policy. Found via the same small-gaps survey that produced
0090's `ocirun ps`, ranked as the cleanest "thin wrapper around
already-existing logic" candidate: `cmd_stop`'s own signal-sending
primitives (`oci_runtime_core::signal::parse`/`process::kill`) already
existed and needed no changes at all.

## Verified against real podman source first, not assumed

Read `~/git/podman/cmd/podman/containers/kill.go` and its own backend
(`pkg/domain/infra/abi/containers.go`'s `ContainerKill`) directly:

* Default signal is **`KILL`**, not `TERM` (`stop`'s own default) —
  `flags.StringVarP(&killOptions.Signal, "signal", "s", "KILL", ...)`.
* One `con.Kill(sig)` call, no wait, no escalation of any kind.
* A container that isn't running is a real, returned error
  (`define.ErrCtrStateInvalid`) — not a silent no-op the way `stop` on
  an already-stopped container already is in this project (see
  `docs/design/0021`/`ociman_stop.rs`'s own `stop_is_a_noop_on_an_
  already_stopped_container` test).

`cmd_kill` matches all three exactly: `--signal`/`-s` default `"KILL"`,
a single `oci_runtime_core::process::kill` call, and an explicit
`Status::Stopped` check that bails out with a real error rather than
returning success.

## Real, manual verification against a real, freshly-pulled busybox

Built the release binary and ran a real `docker.io/library/busybox`
container in the background (the same "background the CLI invocation
itself" technique this project's own test suite already uses, since
`ociman run -d`/`--detach` doesn't exist yet): confirmed the default
`kill` (no `--signal`) stops the container immediately; confirmed
`kill --signal TERM` returns success but the container **stays
running** — the exact same, already-documented (0017) kernel
behavior where an unhandled-default-action signal sent to a
pid-namespace's own init is silently ignored, and (unlike `stop`)
`kill` never escalates past whatever signal was actually asked for;
confirmed a second `kill` (default `KILL`, unmaskable) on the same
still-running container actually stops it; confirmed `kill` on an
already-stopped container is a real, surfaced error, not a no-op.

## Real, automated tests

Four integration tests in `tests/tests/ociman_kill.rs`, mirroring
`ociman_stop.rs`'s own established helpers/patterns exactly (seeded
offline images, `spawn()`+detached-stdio+poll for a container that
needs to stay running while a separate invocation acts on it): the
default-`KILL`-stops-immediately case, the custom-`TERM`-never-
escalates case (asserting the container is *still running* afterward
— the correct, expected outcome for a single-signal primitive, not a
bug), the already-stopped-container real-error case, and the
unknown-container real-error case.

## Not a hot-path change — no A/B perf re-verification needed

Purely additive: one new `Command` enum variant, one new, wholly
independent function (`cmd_kill`), and one new match arm in `main`.
`synthesize_spec`/`resolve_seccomp`/`command_for` (this project's own
named `ociman`-side hot-path functions) and every cgroup driver are
completely untouched — confirmed directly via `git diff --stat`.

## What's still not here

* `ociman wait`/`rename`/`inspect` (for containers)/`top` — the other
  small candidates from the same survey, not attempted here.
* `ociman run -d`/`--detach`, `ocirun update`/`pause`/`resume`,
  automated failed-systemd-scope cleanup, the build cache,
  `ONBUILD`/`HEALTHCHECK` — all still exactly as earlier increments
  left them, unrelated to this increment's own scope.
