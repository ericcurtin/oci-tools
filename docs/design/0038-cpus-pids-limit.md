# Design note 0038: `ociman run --cpus`/`--pids-limit`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`resources_from_cli`, CLI flags).

0037 built and tested the full `LinuxResources` -> systemd unit
property translation (memory, CPU, pids) but only ever exposed the
memory piece via a CLI flag, explicitly flagging `--cpus`/
`--pids-limit` as "a small, purely additive follow-up ... the hard
part ... is what \[0037\] actually closed." This increment is that
follow-up.

## `--cpus`: a rate limit, not a hard cap

`--cpus <N>` (`N` may be fractional, e.g. `1.5`) sets
`linux.resources.cpu.quota`/`.period`, converted the same way real
`moby` converts its own `NanoCPUs` setting (`daemon/daemon_unix.go`,
checked directly): a fixed 100ms (100000µs) period, with `quota = N *
period`. `0.5` CPUs becomes a 50000µs quota over a 100000µs period;
`1.5` becomes 150000µs (spread across however many real cores are
available — a value above `1.0` only means anything on a
multi-core host, exactly like real `docker`/`podman`).

## `--pids-limit`: docker's own "0 or negative means unlimited" convention, not a raw pass-through

`--pids-limit <N>` sets `linux.resources.pids.limit`. Checked directly
against real `moby`'s own `getPidsLimit`
(`daemon/daemon_unix.go`): `N <= 0` is translated to `-1` (this
ecosystem's "unlimited" convention), not passed through as a literal
non-positive `pids.max`/`TasksMax` value (which the kernel would
likely just reject or treat surprisingly) — matching real `docker run
--pids-limit`'s own documented behavior exactly, not merely "whatever
value the user happened to type."

## Real, automated, end-to-end verification

* `run_pids_limit_actually_gets_enforced_by_the_kernels_own_pids_
  controller` / `run_without_pids_limit_can_fork_far_more_than_five_
  processes` (`tests/tests/ociman_run.rs`): a real container under a
  real `--pids-limit 5`, running a shell loop attempting 50 background
  forks, must fail partway through (the kernel's own cgroup v2
  `pids.max` refusing the underlying `clone()` outright — confirmed by
  hand first: busybox `sh` reports `can't fork: Resource temporarily
  unavailable` and the whole script exits non-zero); the identical
  script with no limit at all must complete normally, proving the
  failure above is really the limit's doing, not fork-loop fragility
  in general.
* `run_cpus_flag_sets_the_real_systemd_scopes_own_cpu_quota`: `--cpus`
  is a *rate* limit, not a hard cap that fails an operation outright
  the way the other two do, so there's no clean, fast,
  contention-proof way to prove actual *throttling* happened without a
  flaky, timing-based test. Verifying the real running container's own
  systemd scope's `CPUQuotaPerSecUSec` property instead (via
  `systemctl --user show`, the exact technique 0037's own
  `create_scope_migrates_a_real_child_pid_and_leaves_the_caller_alone`
  test already established for `MemoryMax`) is deterministic while
  still querying a real, live value this test doesn't already know in
  isolation. Confirmed by hand first (a real backgrounded `ociman run
  --cpus 1.5`, `systemctl --user show <scope> -p CPUQuotaPerSecUSec`
  reporting back `1.500000s`) before writing the assertion.
* A real bug caught while writing the `--cpus` test, not in the
  translation itself: the test's first version queried the scope
  immediately after the container's id appeared in `ociman ps -a`,
  which lists a container in its own earlier `creating` state too —
  before the systemd scope (and its resource properties) has
  necessarily been created yet. `record_running` (the point `ps` would
  first report `running`) only fires *after* `create_scope` has
  already returned in `run_reporting_pid`'s own sequence, so waiting
  for status `running` specifically (the same `wait_for_container_
  status` pattern `ociman_stop.rs` already established), not just
  presence in `ps -a`, is what makes the property query race-free.
* `bin/ociman/src/main.rs`'s own unit tests for `resources_from_cli`:
  `None` when nothing was given, the `--cpus` quota/period conversion,
  every `--pids-limit` case (positive, zero, negative), and all three
  flags combined independently.

## Performance

No new cost on any path that doesn't set one of these flags:
`resources_from_cli` returns `None` immediately when all three
parameters are `None` (the case for `ocirun run`, always, and every
existing `ociman run` invocation without any of these flags), leaving
`spec.linux.resources` untouched exactly as before this increment.

## What's still not here

* `--memory-swap` — `--memory`'s own default (twice the memory limit,
  matching real `docker`'s own default, per 0037) is the only way to
  influence swap right now; there's no way to request a different
  ratio or disable swap entirely via the CLI yet.
* `--cpuset-cpus`/`--cpuset-mems` — `LinuxCpu.cpus`/`.mems` still
  aren't translated by either cgroup driver at all (a pre-existing gap
  0037 already flagged, not touched here).
* No CLI flag surfaces `memory.reservation`, `cpu.shares` (relative
  weight, as opposed to `--cpus`' absolute rate limit), or `cpu.burst`
  — `resource_properties` already translates all of them when present
  in a spec, just nothing on `ociman run`'s own CLI populates them yet.
