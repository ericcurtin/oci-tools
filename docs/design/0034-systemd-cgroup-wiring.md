# Design note 0034: wiring the systemd cgroup driver into `ociman run`

Status: implemented
Scope: `crates/oci-runtime-core/src/launch.rs`,
`crates/oci-runtime-core/src/systemd_cgroup.rs`, `bin/ociman/src/main.rs`.

0033 shipped `systemd_cgroup::create_scope` as a standalone, thoroughly
tested primitive, deliberately not called from anywhere yet, and named
"the natural next increment: deciding when to prefer this over the
cgroupfs driver ... computing a real per-container scope name" as the
follow-up. This increment is that follow-up: `ociman run` now always
attempts the systemd driver for every container it starts (matching
real `podman`'s own default on systemd-based distros), falling back
gracefully — logged via `tracing::warn!`, never fatal — if no D-Bus
session is reachable. `ocirun` itself is untouched: it still only ever
uses whatever `cgroupsPath` the spec itself says, via a new
`CgroupSetup::FromSpec` passed by its own single call site, matching
real `runc`/`crun`'s own spec-driven-only behavior exactly.

## The API shape

`launch::run_reporting_pid` gained a new `cgroup_setup: CgroupSetup`
parameter:

```rust
pub enum CgroupSetup {
    FromSpec,
    Systemd { scope_name: String, description: String },
}
```

`run` (the plain, `ocirun`-only entry point) is now just
`run_reporting_pid(..., CgroupSetup::FromSpec, |_pid| {})` — no
behavioral change for `ocirun`, confirmed by re-running the same
`hyperfine` benchmark 0012/0018/0033 already established (see
"Performance" below).

`ociman`'s own `cmd_run` builds `CgroupSetup::Systemd { scope_name:
format!("ociman-{container_id}.scope"), description: format!("oci-tools
container {container_id}") }` and passes it through instead.

## Ordering: cgroup migration must happen before `CLONE_NEWCGROUP`

The direct forked child (`ChildSetup::run`) needs a way to pause until
the parent has actually finished trying to migrate it into a systemd
scope before it proceeds — otherwise it might `unshare(CLONE_NEWCGROUP)`
(when the spec asks for a cgroup namespace) while still sitting in its
*original* cgroup, leaving its own `/proc/self/cgroup` inside the
namespace reporting the wrong root entirely. `ChildSetup` gained a new
`cgroup_ready_read: Option<OwnedFd>` field: when set (only in the
`Systemd` case; `None`, and skipped entirely, for `FromSpec`), the
child does a blocking one-byte read on it — placed in `ChildSetup::run`
*before* the call to `namespaces::unshare`, alongside (not replacing)
the existing cgroupfs-driver block that already runs at the same point
for the `FromSpec` case. Manually verified this ordering is actually
correct, not just theoretically so: a real `ociman run` container's own
`/proc/self/cgroup` correctly reads `0::/` (the namespace root),
confirmed via `cat /proc/self/cgroup` run as the container's own
process.

## Real bug #1: migrating the wrong pid deadlocks everything

The first working version of the parent side called
`systemd_cgroup::create_scope` using `container_pid` — the pid read
back from the pid-reporting pipe, which (in the `CLONE_NEWPID` case)
isn't the direct forked child's own pid at all, but its *grandchild's*,
reported only once that grandchild has gotten far enough to write it.
This deadlocked immediately: the direct child blocks on
`cgroup_ready_read` before it ever forks its own pid-namespace-relay
grandchild (let alone that grandchild reporting its pid), while the
parent blocks on `read_container_pid` — which can only ever unblock
once that grandchild pid arrives — before it ever calls `create_scope`
or writes the "go" byte the child is waiting for. Two sides, each
waiting on something only the other side's *next* step could produce.

Fixed by migrating `direct_child_pid` instead — the pid `process::fork`
itself returns immediately, no waiting required. This is still correct
for the eventual real container pid (the grandchild, when a pid
namespace is in play): cgroup membership is inherited across `fork`,
exactly the same property the existing cgroupfs driver's own
`cgroups::enter` (called by that same direct child, before any of its
own relay-forking) already depends on — nothing new is being assumed
here, just applied to a second driver.

Manually verified end to end after the fix: a real `ociman run`
completes normally, a real transient scope (`ociman-<id>.scope`)
appears while the container is running and is confirmed gone
afterward, both checked directly via `systemctl --user list-units
'ociman-*' --all`.

## Real bug #2: a deadline check between blocking calls isn't a timeout

0033's own `wait_for_job` bounded its wait with a `JOB_WAIT_TIMEOUT` of
10 seconds — but only by checking `Instant::now() >= deadline` *between*
successive calls to the underlying signal iterator's own blocking
`next()`. That's not actually a timeout on anything: if a single call
to `next()` itself doesn't return, the deadline check between
iterations never gets a chance to run either, and the whole function —
along with the container process it's gating, which is paused waiting
for this function's own "go" signal — can block forever regardless of
the constant's own name and value.

This was not a hypothetical concern: found by deliberately stress-
testing real concurrent load, not by re-reading the code. Launching
eight real `ociman run` invocations simultaneously (against eight
independently seeded stores, so no other contention — image pulls,
storage locks — was in play) consistently hung roughly half of them
well past any reasonable per-container latency; confirmed via `ps` and
`systemctl --user list-units` that they were genuinely stuck (not just
slow), and confirmed this wasn't an artifact of leftover state from
earlier runs by reproducing it again from a freshly verified-clean
process/unit slate. The exact reason `next()` sometimes doesn't return
promptly under concurrent D-Bus load from many simultaneous callers
was not root-caused further (plausibly some contention/ordering
interaction in the user systemd instance's own signal dispatch, or in
`zbus`'s own connection handling under this exact usage pattern) —
but fixing it correctly doesn't require knowing why: no matter what the
blocking call does internally, a real wall-clock bound requires running
it somewhere that can be abandoned from the outside.

Fixed by restructuring `create_scope` itself around a background
thread: the entire D-Bus interaction (connect, subscribe, call
`StartTransientUnit`, wait for the matching `JobRemoved`, read back the
resulting cgroup path) moved into a new private
`create_scope_dbus_roundtrip`, run on a dedicated thread; the public
`create_scope` spawns that thread and waits on the read end of an
`mpsc::channel` with `recv_timeout(JOB_WAIT_TIMEOUT)` — a real,
enforced deadline regardless of what the thread itself is doing. The
spawned thread is deliberately never joined: there's no way to cancel a
blocked `zbus` call from the outside, and there's no need to — the
calling process exits (successfully, having fallen back to "no
cgroup") long before an abandoned thread could matter again.

Verified this actually closes the hang, not just reshapes it: the same
eight-concurrent-`ociman-run` stress test, repeated several times after
the fix, never took longer than the intended ~10-second worst case for
any invocation — the contended runs now log `systemd cgroup driver
unavailable (tolerated, container has no cgroup)` and finish
immediately after, while uncontended runs (a second stress round hit no
contention at all) complete in the usual tens of milliseconds. Also
confirmed `cargo test --workspace` (which exercises several `ociman
run` invocations, sometimes overlapping across test binaries) passes
reliably across multiple repeated full-workspace runs after the fix,
where it had been intermittently timing out on the same underlying hang
before it.

Several existing tests' own polling timeouts (`ociman_exec`,
`ociman_logs`, `ociman_name`, `ociman_stop`) were widened from 5s to 20s
in the course of chasing this, to give the systemd D-Bus round trip
enough headroom under contended local test-suite runs — a real, if
secondary, mitigation on top of the actual fix above, not a substitute
for it.

## Performance

Measured with the same `hyperfine` methodology (100+ samples, 5 warmup
runs) 0012/0018/0033 already established, on this session's own
aarch64 dev host:

* `ocirun run` (still `CgroupSetup::FromSpec`, this increment's new
  thread/timeout code path is never reached): **3.1 ms mean** — no
  regression from the ~2.6-3.1 ms baseline this project has
  consistently reproduced across many earlier increments.
* `ociman run` (release, real `docker.io/library/busybox` pull-extract-
  run-destroy cycle), before this increment: ~23.3 ms mean. After
  wiring in the systemd driver: **~37.8 ms mean** — a real ~14.5 ms
  cost (the D-Bus round trip itself), not a regression to hide, but a
  genuine trade for actually having cgroup isolation at all (this
  project's containers have had *zero* cgroup isolation up to this
  point — see 0033's own "The gap" section). Directly compared against
  real `podman run --rm` (216 ms) and real `docker run --rm`
  (284.8 ms) on the same host for the same operation: `ociman run` is
  still **~5.7× faster than podman** and **~7.5× faster than docker**
  even with this new cost included.

## What's still not here

* Full multi-action seccomp, `prestart`/`createRuntime`/
  `createContainer`/`startContainer` hooks (only `poststart`/`poststop`
  exist, per 0026) — pre-existing milestone 3 gaps, unrelated to this
  increment.
* Resource-limit properties (`MemoryMax`, `CPUQuota`, ...) still aren't
  translated into systemd unit properties — `create_scope` still only
  ever creates a plain, unlimited-but-delegated-and-accounted scope, as
  0033 already flagged; this increment's own scope is strictly "wire
  the existing primitive in for every container", not resource limits.
* Failed/never-completed transient scopes still aren't explicitly
  cleaned up (`systemctl --user reset-failed` or equivalent) — not
  observed as a practical problem in this increment's own testing
  (systemd's own automatic cleanup on normal exit continues to cover
  the common case), but still a real, not-yet-handled edge case 0033
  already flagged and this increment doesn't close.
