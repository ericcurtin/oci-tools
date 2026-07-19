# Design note 0030: `ociman stop`

Status: implemented
Scope: `bin/ociman/src/main.rs`'s `cmd_stop`.

## The gap

0021 explicitly flagged this as "still not here": a graceful-then-
forceful stop, distinct from `rm --force`'s immediate `SIGKILL`.
Neither `ocirun` nor `oci_runtime_core` need to change at all for
this — matching the same architecture division this project has
applied repeatedly (named-user resolution, `logs`, lifecycle hooks):
`ocirun kill` (and `oci_runtime_core::process::kill`) is, correctly, a
single raw signal with no wait/escalation policy — real low-level
runtimes (`crun`/`runc`) only ever provide that same minimal primitive
too. The graceful-then-forceful *policy* is squarely a higher-level
engine's job, exactly like real `podman`/`docker`'s own architecture:
their own low-level runtime's `kill` is just as minimal, and `stop` is
implemented in the engine on top of it.

## Policy: matches real `docker stop`/`podman stop`

Send a signal (`TERM` by default, `--signal` overridable, name or
number via the existing `oci_runtime_core::signal::parse`), then poll
`oci_runtime_core::process::alive` for up to `--time` seconds
(default 10, matching both real tools' own default). If the container
is still alive once that window elapses, escalate to an unmaskable
`KILL` and poll briefly for that to take effect too — reusing the
exact same bounded kill-then-poll loop `rm --force` (0021) already
established, just with an initial graceful attempt prepended.

A container that's already stopped is a no-op (still prints the id,
still exits 0), not an error — matching real `docker stop`/`podman
stop`'s own idempotent behavor (a second `stop` on an already-stopped
container isn't a mistake worth erroring over).

Doesn't explicitly rewrite the container's persisted status to
`Stopped`: `PersistedState::effective_status` already recomputes this
dynamically from whether the recorded pid is still alive (see
`oci_runtime_core::state`), so once the process actually exits (from
either the graceful signal or the `KILL` escalation), every other
command (`ps`, `state`, a subsequent `stop`/`rm`) already sees
`stopped` without `stop` needing to write anything itself — the same
reasoning `rm --force` already relies on.

## What's deliberately not here yet

* The image's own `StopSignal` (`ContainerConfig::stop_signal`,
  already a modeled field — see `oci_spec_types::image`) isn't
  consulted as the *default* signal the way real `docker`/`podman` do
  (falling back to it before `TERM` when no `--signal` is given). This
  increment's own scope is the graceful-then-forceful *policy* itself;
  wiring in a per-image default signal is a small, separable follow-up
  that would need a store/image lookup `cmd_stop` doesn't currently do
  (it only has the container's own persisted state, not its image
  reference resolved back to a config) — not implemented here to keep
  this increment's own diff focused.
* `--all` (stop every running container in one invocation).

## Real, automated, end-to-end tests

`tests/tests/ociman_stop.rs` (4 cases, using the same seeded-image +
`spawn()`+detached-stdio+poll approach `ociman_exec.rs`/`ociman_logs.rs`
established for a genuinely concurrent "still running" scenario):

* A container that installs a `TERM` trap and exits gracefully — `stop`
  returns well within its generous grace window (proving the graceful
  signal alone worked, not an escalation), and the container's own
  persisted exit code is the trap's `exit 0`, not a `KILL`-derived one.
* A plain `sleep 30` (a pid-namespace's own init, which — per 0017's
  own real-kernel finding — ignores an unhandled-default-action `TERM`
  outright) with a deliberately short `--time`: `stop` correctly
  escalates to `KILL`, and the container ends up `stopped`.
* `stop` on an already-`stopped` container succeeds as a no-op.
* `stop` on an unknown container id is a clear error.

A real shell-scripting footgun was caught (not by inspection — by the
VM CI matrix's own test run failing) while writing the first case
above: the first version used `trap 'exit 0' TERM; sleep 30` as the
"handles `TERM` gracefully" container command. That trap is installed
correctly, but a shell commonly defers actually *running* a pending
trap until its current foreground child (the `sleep 30` process)
finishes on its own — so `stop`'s `TERM` was received immediately, but
the trap that would act on it didn't run until the full 30 seconds
elapsed, at which point `stop`'s own (much shorter) grace window had
already lapsed and it had escalated to `KILL` well before that — the
test failed by taking the entire window rather than exiting quickly.
Verified directly against a real, `ocirun`-created pid namespace (not
just recalled from general shell-scripting folklore) that replacing the
single long `sleep 30` with a short-sleep loop (`while true; do sleep
0.2; done`) bounds the same deferral to a fraction of a second instead:
the fixed version reacts to `TERM` in ~3ms in a real, manually-verified
`ocirun create`/`start`/`kill` sequence, and consistently passes across
repeated local runs. Not a pid-namespace-specific kernel restriction
(0017's own finding is about *unhandled* signals specifically; this
one had a real handler installed throughout) — a general, well-known
shell behavior this test's own container command had to account for,
same as any real container entrypoint script would.

## Performance

Doesn't touch `oci_runtime_core::launch`/`process`/`exec` at all — pure
CLI-level policy in `ociman`'s own `cmd_stop`, built entirely from
primitives (`signal::parse`, `process::kill`, `process::alive`) that
already existed and are already exercised by `rm --force`/`ocirun kill`.
No re-benchmark needed, consistent with every prior increment that only
touched non-hot-path code.
