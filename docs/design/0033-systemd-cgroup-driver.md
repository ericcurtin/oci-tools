# Design note 0033: the systemd cgroup driver (foundational primitive)

Status: implemented (the primitive itself; **not yet wired into
`ociman run`/`ocirun`** — see "What's still not here")
Scope: `crates/oci-runtime-core/src/systemd_cgroup.rs`.

## The gap

0015 flagged the systemd cgroup driver as a remaining gap when the
cgroupfs-only driver first shipped, and it's been re-flagged in every
cgroup-adjacent increment since (0018, 0022, 0027). It matters more
than a checkbox: `ociman run`'s own `synthesize_spec` has never once
set `cgroupsPath` on any container it creates, so **every container
`ociman run` has ever started has had zero cgroup isolation at all** —
no resource limits are even possible yet, since nothing computes a
cgroup path for them to apply to.

Naively fixing this by having `ociman` compute a cgroupfs-driver path
itself (the way this project's own cgroup *tests* already do, wrapped
in `systemd-run --user --scope`) wouldn't actually work for the
overwhelming common case: `cgroups::enter`'s own raw `cgroup.procs`
write only succeeds when the *calling* process already has write access
to the common ancestor of source and destination cgroups (0015's own
finding) — and an ordinary interactive shell or SSH session's own
cgroup never has that. Real `podman`/`crun` don't hit this because they
default to the *systemd* cgroup driver on systemd-based distros (this
project's own first-class targets): they ask systemd itself to create
the cgroup and migrate the pid, using systemd's *own* authority over
the subtree it manages, which doesn't depend on the caller's own
cgroup at all.

## Verified against the real thing before writing any application code

A scratch program (deleted after, per this project's own established
discipline for exactly this kind of foundational-primitive
verification) confirmed two things empirically, not by assumption:

1. **No `systemd-run` wrapper needed.** Forking a real child process
   from an ordinary SSH-session shell (in `user-1000.slice/
   session-N.scope`, not any special delegated wrapper) and asking
   systemd to create a transient scope for *only the child's pid*
   correctly left the parent shell in its own original cgroup while the
   child ended up under `.../user@<uid>.service/app.slice/<scope>.scope`
   — exactly the shape this project's own `systemd-run --user --scope`-
   wrapped tests already use, but without ever needing that wrapper.
2. **The method call reply is not synchronous.** Checking
   `/proc/<pid>/cgroup` immediately after `StartTransientUnit`'s own
   D-Bus reply, with no wait at all, consistently showed the *old*
   cgroup across five repeated runs — the actual migration happens
   asynchronously as systemd processes the unit's own "start" job.
   Subscribing to the `JobRemoved` signal *before* issuing the call (to
   avoid racing an early signal) and waiting for the one matching the
   returned job's own object path — the same ordering real `crun`'s own
   `cgroup-systemd.c` uses, read directly from
   `~/git/crun/src/libcrun/cgroup-systemd.c` rather than re-derived
   from documentation prose alone — reliably works instead (verified
   across five repeated runs after adding the wait).

A related, separate finding: a scope that successfully finishes (its
only member process exits normally) is automatically stopped and
removed by systemd on its own, no explicit cleanup call needed at all
— unlike the cgroupfs driver's own explicit `cgroups::remove` (0027).
But a scope whose migration never actually completed (e.g. the caller
crashed between a successful `StartTransientUnit` call and the target
pid existing) can be left behind in a `failed` state rather than
cleaned up automatically — observed directly while iterating on this
module's own scratch verification, and noted as a real, not-yet-handled
edge case below.

## `zbus`: a pure-Rust D-Bus client, matching this project's own precedent

New capability group in `ci/guards.py` ("D-Bus client":
`zbus`/`dbus`/`dbus-tokio`) — `zbus` chosen the same way `flate2`'s Rust
backend and `ruzstd` already were: it implements the D-Bus wire
protocol itself, with no `libdbus` C dependency at all, keeping this
project's all-Rust design intact. `zbus::blocking::Connection` uses its
own internal `async-io`-based executor rather than requiring this
project to adopt a full async runtime (like `tokio`) anywhere in its
own, otherwise entirely synchronous, binaries.

`zbus`'s own dependency tree forced one real, ordinary version
duplication: `async-trait`/`serde_repr` (both `zbus` dependencies) pull
in `syn` 3, while the rest of the workspace's proc-macro crates
(`clap_derive`, `thiserror-impl`, ...) are still on `syn` 2 — the same
"ecosystem forces a transient duplicate" case `deny.toml` already
documents for `ring`'s own `getrandom`/`windows-sys` versions, handled
the same way (a `skip` entry with a clear reason, not a broader
allowance).

## `create_scope`: the one primitive this increment ships

`oci_runtime_core::systemd_cgroup::create_scope(pid, scope_name,
description) -> io::Result<PathBuf>`: connects to the calling user's
own D-Bus **session** bus (matching `systemd --user`, the only mode
this rootless-only project runs containers in so far — no system-bus/
root support yet), creates a transient scope with `pid` as its sole
member (`Delegate` plus every accounting knob enabled, matching real
`crun`'s own property set exactly), waits for the real, verified-
necessary `JobRemoved` confirmation (bounded by a 10-second timeout — a
generous margin over the well-under-one-second completion time every
real, repeated verification run actually took), and returns the real
cgroup path read back from `/proc/<pid>/cgroup` (not reconstructed from
an assumed slice-naming convention, since the actual path can
legitimately vary depending on the caller's own delegated hierarchy).

## Real, automated tests

Two pure unit tests (name-suffix validation, `/proc/pid/cgroup`
parsing including a hybrid-cgroup-v1-shaped negative case) plus one
genuinely real, end-to-end test: forks an actual child process (not the
test's own pid — proving a *different* pid gets migrated while the
caller's own cgroup stays untouched, the real use case), creates a real
transient scope for it against this session's own real `systemd --user`
instance, confirms the child's own `/proc/<pid>/cgroup` reflects the
new scope while the parent's is provably unchanged, then always kills
and reaps the child regardless of the outcome. Gated on a real,
self-cleaning `systemctl --user is-system-running` probe (the same
"check real availability, not just that a binary/path exists" pattern
0015's own cgroup tests established for `systemd-run --user --scope`),
so an environment with no reachable user session skips it, printing
why, rather than failing. Verified consistently clean across 5 repeated
runs before considering this increment done.

## Performance

Not called from `launch.rs`, `ociman`, or `ocirun` anywhere yet (see
below), so there is zero runtime impact on any hot path by
construction. Confirmed the *build-time* impact is negligible too: the
release binaries grew by well under 2KB each (`ociman`: +1.7KB;
`ocirun`: +944 bytes) — Rust's own dead-code elimination strips the
entire unused module (and its `zbus` dependency's own compiled code)
from binaries that never call it. Re-benchmarked `ocirun run` with the
same `hyperfine` methodology already established regardless: 2.6ms
mean, no regression from the ~2.9-3.1ms established baseline (within
the same noise band this project's benchmark has shown across
sessions).

## What's still not here

* **Not wired into `ociman run`/`ocirun` at all yet.** This increment
  ships and thoroughly tests the primitive on its own, matching how
  this project's own foundational pieces (`namespaces.rs`, `rootfs.rs`,
  `process.rs`) were each built and unit-tested independently across
  several earlier increments *before* `launch.rs` ever assembled them
  into a real `ocirun run` — the natural next increment is deciding
  when to prefer this over the cgroupfs driver (falling back
  gracefully when no D-Bus session is reachable, matching this
  project's existing "tolerate known rootless limitations" pattern),
  computing a real per-container scope name, and translating
  `LinuxResources` into systemd unit properties (real `crun`'s own
  `append_resources`) rather than raw cgroupfs file writes.
* Failed/never-completed transient scopes aren't explicitly cleaned up
  (`systemctl --user reset-failed` or equivalent) — observed directly
  as a real, not hypothetical, leftover-state possibility during this
  module's own development, deferred to the wiring increment above,
  which will have an actual container lifecycle to hook cleanup into.
* System-bus (root) support — irrelevant to this rootless-only project
  so far, but the same `create_scope` shape would need a
  `Connection::system()` variant if that ever changes.
* Resource-limit properties (`MemoryMax`, `CPUQuota`, ...) aren't
  translated to systemd unit properties at all yet — `create_scope`
  only ever creates a plain, unlimited-but-delegated-and-accounted
  scope; that's the wiring increment's job too, alongside deciding
  whether raw cgroupfs writes into the delegated (but now
  systemd-owned) cgroup remain a valid fallback for properties systemd
  itself doesn't expose a unit property for.
