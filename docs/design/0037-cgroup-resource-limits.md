# Design note 0037: cgroup resource limits reach the systemd driver, and `ociman run --memory`

Status: implemented (memory limit end to end; cpu/pids translation
built but not yet exposed via any `ociman` CLI flag — see below)
Scope: `oci_runtime_core::{cgroups, systemd_cgroup, launch}`,
`bin/ociman/src/main.rs`.

0033/0034 built and wired in the systemd cgroup driver but explicitly
flagged, twice, that it dropped `LinuxResources` entirely: a container
started via `ociman run` (which always prefers the systemd driver) got
real cgroup *membership* but zero *limit enforcement* — a memory limit
set in `config.json` simply had no effect. `ociman` itself never even
had a way to ask for one in the first place: `synthesize_spec` never
touched `linux.resources`, and there was no `--memory`/`--cpus`/
`--pids-limit` flag anywhere on `ociman run`.

## Translating `LinuxResources` into systemd unit properties

`oci_runtime_core::systemd_cgroup` gained `resource_properties`,
translating the same `LinuxResources` the cgroupfs driver
(`cgroups::plan_resources`) already turns into raw file writes into
systemd `StartTransientUnit` properties instead — checked directly
against real `crun`'s own translation
(`cgroup-systemd.c`'s `append_resources`), not re-derived from
documentation:

* `MemoryMax`/`MemoryLow` from `memory.limit`/`.reservation`.
* `TasksMax` from `pids.limit`.
* `CPUWeight` from `cpu.shares`, via the exact same quadratic
  curve-fit conversion `cgroups::plan_cpu` already uses for the
  cgroupfs driver (`convert_cpu_shares_to_weight`, made `pub(crate)`
  so both drivers share one implementation rather than two that could
  quietly drift apart).
* `CPUQuotaPerSecUSec`/`CPUQuotaPeriodUSec` from `cpu.quota`/`.period`,
  using the same "only apply once a quota is actually set, defaulting
  an unset period to the kernel's documented 100000µs" rule
  `cgroups::plan_cpu` already applies — so both drivers behave
  identically for the same spec, not just similarly.
* A `-1` value (this ecosystem's own "unlimited" convention) becomes
  `u64::MAX` once cast to the properties' own `u64` type, which is
  also systemd's own "infinity" convention for every one of these
  properties — the same reinterpret-cast coincidence real `crun`'s own
  C code relies on, not something invented here.

`create_scope`/`create_scope_dbus_roundtrip` gained a `resources:
Option<&LinuxResources>` parameter, appending `resource_properties`'
own output to the existing `Delegate`/accounting property list when
present. `launch::CgroupSetup::Systemd` gained a matching `resources:
Option<LinuxResources>` field (owned — `create_scope` already moves
its own arguments into a background thread, per 0034), and `ociman`'s
`cmd_run` now passes `bundle.spec.linux.resources.clone()` through
instead of nothing.

## A real bug caught by *actually enforcing* a limit, not just setting the property

The first working version of `resource_properties` translated
`memory.swap` (a *combined* memory+swap limit — the runtime-spec's own
cgroup-v1-derived convention, already documented and handled by
`cgroups::convert_memory_swap_to_v2` for the cgroupfs driver) straight
through as `MemorySwapMax`, which is *swap-only* — mirroring what a
literal reading of real `crun`'s own `get_memory_swap_max` appears to
do, but inconsistent with this project's *own* already-established,
already-tested cgroupfs-driver convention for the very same field.
Fixed by running `memory.swap` through the exact same
`convert_memory_swap_to_v2` conversion (made `pub(crate)`, shared
between both drivers) before handing it to systemd, so both drivers
now treat identical `LinuxResources` input identically.

This wasn't caught by reasoning about the translation in isolation —
it surfaced from actually trying to trigger a real OOM kill by hand: a
container given `--memory 16m` and a shell command allocating ~300 MB
(`x=$(yes | head -c 300000000)`) **ran to completion successfully**
instead of being killed. `systemctl --user show <scope> -p MemoryMax`
confirmed the property itself was set correctly (`16777216`) — the gap
was that `MemorySwapMax` was never set at all (an unrelated,
`memory.swap`-only omission when no swap limit was requested), so the
container's cgroup had *no* swap limit whatsoever, and the kernel
simply paged the excess out to this host's own real swap space instead
of ever hitting the OOM killer. Real `docker run --memory` has exactly
this same failure mode baked into its own well-known default —
see the next section.

## `ociman run --memory`: real docker's own default, not a stricter invented one

`ociman run` gained a `--memory <SIZE>` flag (`128m`/`1g`/plain bytes;
binary `k`/`m`/`g`/`t` units, checked directly against
`docker/go-units@v0.5.0/size.go`'s `RAMInBytes` — vendored identically
into `moby`/`podman`/`runc`/`cri-o`/`containerd`), populating
`linux.resources.memory.limit` in `synthesize_spec`.

Without a separate `--memory-swap` flag (which doesn't exist yet),
`synthesize_spec` defaults `memory.swap` to **twice** the memory limit
— checked directly against real `moby`'s own
`adaptContainerSettings` (`daemon_unix.go`): *"By default, MemorySwap
is set to twice the size of Memory"*. This is deliberately the same
default real `docker run --memory` (without `--memory-swap`) has always
had — including its own well-known caveat that a *bare* memory limit
alone doesn't actually cap total memory+swap usage at that limit; some
additional swap headroom is still allowed by default. Matching that
default (not inventing a stricter "always disable swap entirely" one)
keeps `ociman run --memory`'s behavior a faithful drop-in match for
real `docker`/`podman`, at the cost of the same "some swap still
allowed" surprise those tools' own users already have to know about.

## Real, automated, end-to-end verification

* `tests/tests/ociman_run.rs` gained
  `run_memory_limit_actually_gets_enforced_by_the_kernels_own_oom_killer`:
  a real container, a real `--memory 16m`, a real ~300 MB allocation
  via `yes | head -c 300000000` (no `/dev/zero` needed — confirmed
  directly that this rootless bundle's own minimal `/dev` doesn't have
  one — `yes`/`head` are ordinary busybox applets instead), asserting
  the real kernel OOM-kills it (`SIGKILL`, exit code 137). This is the
  test that would have caught the swap-conversion bug above; it was
  run against the pre-fix code by hand first and confirmed to fail
  (the container completed normally instead of being killed) before
  the fix, then confirmed to pass after.
* `oci_runtime_core::systemd_cgroup`'s own unit tests:
  `resource_properties_translates_memory_cpu_and_pids` (every
  property name and value for a full `LinuxResources`, including the
  swap conversion), `resource_properties_setting_swap_equal_to_memory_
  disables_swap_entirely` (the specific case the bug above involved),
  `resource_properties_translates_unlimited_as_u64_max`, and
  `resource_properties_skips_cpu_quota_without_a_positive_quota`.
* `create_scope_migrates_a_real_child_pid_and_leaves_the_caller_alone`
  (already existed) gained a real memory limit and a direct
  `systemctl --user show <scope> -p MemoryMax --value` check against
  the *real* running scope, confirming systemd itself reports back the
  exact value given — real verification one level below "the container
  actually got killed", isolating the D-Bus property translation from
  everything else that could otherwise also explain an OOM kill.
* `bin/ociman/src/main.rs` gained direct unit tests for
  `parse_memory_limit` (every real unit suffix, whitespace handling,
  garbage/overflow rejection) — the one piece of this increment with
  no process/namespace involvement, so an ordinary in-process test is
  both possible and the most direct way to check it, unlike the rest
  of this binary which relies entirely on spawning the real built
  binary.
* Manually confirmed (real containers, not just the automated test
  above) that a container with *no* `--memory` and one with a
  generous `--memory 512m` both complete the same ~300 MB allocation
  successfully — proving the limit is neither a no-op nor an
  unconditional kill regardless of value.

## Performance

`resource_properties` only runs when `bundle.spec.linux.resources` is
`Some` — a container with no resource limits configured (every
`ocirun run` invocation, and every `ociman run` invocation without
`--memory`) pays nothing beyond the existing systemd cgroup driver
cost already measured in 0034: `resources` is `None` in that case, so
`resource_properties` is never even called, and the `CgroupSetup`
enum's own new `Option<Box<LinuxResources>>` field costs a single
`None` pointer, not an inline copy of the (boxed specifically to avoid
this) larger struct.

Re-confirmed `ocirun run` unaffected: **2.8ms mean**, unchanged within
noise from the established ~2.6-3.1ms baseline (`CgroupSetup::FromSpec`
never touches any of this increment's own code).

For `ociman run` itself (real busybox pull-already-cached
run-destroy cycle, no `--memory` given so `resources` is `None`
either way): a direct before/after `git stash` A/B comparison on the
same contended host (a concurrent, unrelated session was genuinely
competing for CPU during this measurement, per `ps aux` — noted rather
than hidden) measured 41.9ms ± 11.3ms before this change and 44.7ms ±
9.7ms after, both `hyperfine`, 100 runs, 10 warmups. The ~2.8ms
difference is well within one standard deviation of either
measurement (~10ms) — not a statistically distinguishable regression,
just noise, consistent with the actual code change being a few extra
struct-field/branch checks on an already-existing, already-measured
code path.

## What's still not here

* No `--cpus`/`--pids-limit` (or `--memory-swap`) flags on `ociman
  run` yet — `resource_properties`' own CPU/pids translation is built
  and unit-tested, just not reachable from any CLI surface yet. Adding
  them is now a small, purely additive follow-up (parse the flag,
  populate the corresponding `LinuxResources` field) — the hard part
  (the translation itself, and getting `MemorySwapMax` consistent
  between drivers) is what this increment actually closed.
* `ocirun` itself still never sets `CgroupSetup::Systemd` at all (see
  0034) — this increment's own systemd-side resource translation is
  only ever reachable via `ociman run`.
* Real `crun`'s own `append_resources` also covers `cpuset.cpus`/
  `cpuset.mems` (`AllowedCPUs`/`AllowedMemoryNodes`) and block-IO
  weight — neither `LinuxCpu.cpus`/`.mems` nor block-IO limits are
  translated by either of this project's own two cgroup drivers yet
  (a pre-existing gap, not something this increment touches).
