# Design note 0230: `ociman attach`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Attach`, `cmd_attach`).

## A real, previously-acknowledged gap

`cmd_start`'s own doc comment already named this directly: "this
project has no `attach`-to-an-already-running-container command at
all yet, only this function's own `--attach`, which only ever applies
to an already-`Stopped`/`Created` container." This closes that gap —
a real, standalone `ociman attach <container>`, matching real `docker
attach`/`podman attach`'s own observable output behavior.

## Reuses `attach_and_wait_for_exit` verbatim, no new plumbing

`cmd_start --attach`'s own `attach_and_wait_for_exit` helper already
polls the container's *raw* on-disk status (not tied to having just
launched it in the same process invocation) and streams its log file
live until it stops. That means it already works identically whether
the container was started moments ago by this same command or by a
completely separate, earlier invocation — exactly what a standalone
`attach` needs. `cmd_attach` is almost entirely: resolve the id,
check it's genuinely `Running` right now, call the existing helper.

## Deliberately output-only

Real `docker attach`/`podman attach` forward this process's own real
stdin into the container by default (`--no-stdin` to disable). This
project's own current architecture only ever wires up a container's
stdin once, at its original `run`/`create` time (the same `-i`/
`--interactive` decision already established, 0187/0188) — there is no
live channel an already-detached, already-running container's own
stdin could be reattached to later. Rather than half-implement this
(or silently accept a `--no-stdin`/`--detach-keys`/`--sig-proxy` flag
that would do nothing), `ociman attach` offers none of those flags at
all — matching this project's own established "never accept a flag a
command can't actually honor" convention. This is the same category of
honestly-scoped-down gap `cmd_start`'s own doc comment already used for
real terminal/pty allocation.

## Verified

- Manual, hands-on verification first: a real `ociman run -d` followed
  by a real, separate `ociman attach` in a second invocation streamed
  live output correctly and returned the container's own real exit
  code; attaching to an already-stopped container produced a real,
  clear error naming its own current status.
- Three new integration tests in `tests/tests/ociman_attach.rs`: a
  real detached container, attached to from an entirely separate
  `ociman` invocation, streams its own full captured output (from the
  very start, not just whatever's written after attach began
  watching) and propagates the real, nonzero exit code; attaching to
  an already-stopped container is a clear, real error naming its own
  current status; an unknown container id is a clear error too.
- Full workspace: `cargo build`, `cargo test --workspace` (96/96
  result blocks — one new test file, `ociman_attach`, everything else
  unchanged — 0 failures), `cargo fmt --check`, `cargo clippy
  --all-targets -- -D warnings`, `python3 ci/guards.py` (18 capability
  groups, unaffected), `cargo deny check` (only the pre-existing
  benign warning), `bash ci/native-ci.sh`, hyperfine perf sanity on
  `ociman run --rm` (no regression — this change adds a new, separate
  subcommand entirely, touching nothing on `run`'s own hot path).

## What this doesn't do yet

Real stdin forwarding into an already-running container (see above —
this project's own current stdin-wiring architecture would need real
new plumbing, e.g. a persistent, reattachable channel created at
launch time, to support this honestly); real terminal/pty allocation
(`-t`/`--tty`, a separate, already-named, wholly unstarted gap);
`--detach-keys`-style manual detach-without-stopping (this
implementation always blocks until the container itself exits, never
offering a way to detach and leave it running while still attached —
matching `cmd_start --attach`'s own identical, already-established
scope exactly, not a new limitation invented for this command).
