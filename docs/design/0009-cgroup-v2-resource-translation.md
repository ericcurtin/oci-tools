# Design note 0009: cgroup v2 resource translation (milestone 3, part 7)

Status: implemented (seventh increment of milestone 3)
Scope: `oci_spec_types::runtime::{LinuxMemory, LinuxCpu, LinuxPids}`;
`oci_runtime_core::cgroups`. Still no CLI wiring, no fork/clone — this is
the last resource-limit building block before `create` can assemble a
container's full environment.

Continues 0003–0008. `create` will eventually need to write a container's
resource limits into a cgroup v2 directory it creates; the "what do these
runtime-spec fields mean in cgroup v2 terms" half of that is pure numeric
translation with no privilege implications, so — same reasoning as 0007
for mount options — it's built and verified on its own first.

## Runtime-spec types

Added `LinuxMemory`, `LinuxCpu`, `LinuxPids` to `oci_spec_types::runtime`,
filling in `LinuxResources` beyond the `devices` field 0003 shipped.
Field names and JSON shape follow the runtime-spec exactly (checked
against a real `runc spec` bundle with resources added — see below), down
to the spec's slightly awkward inherited-from-cgroup-v1 vocabulary
(`shares`/`quota`/`period` for CPU, a combined memory+swap `swap` field)
that a real cgroup v2 host doesn't use directly. Several fields
(`kernel`, `kernelTCP`, `swappiness`, `disableOOMKiller`, `useHierarchy`,
`checkBeforeUpdate`, `realtimeRuntime`, `realtimePeriod`) are cgroup-v1-only
concepts with no cgroup v2 equivalent; they parse (so a real-world
config.json using them doesn't fail) but nothing acts on them, and every
one says so in its own doc comment.

## `oci_runtime_core::cgroups`

`plan_resources(&LinuxResources) -> Vec<(&'static str, String)>` — pure
translation to cgroup v2 interface file writes, ported from runc/crun's
cgroup v2 driver conventions (`opencontainers/cgroups/fs2`), not
invented:

* `-1` means "unlimited" (the container-ecosystem convention inherited
  from cgroup v1's `*_in_bytes` knobs — not in the formal spec text, but
  every real tool honors it), written as cgroup v2's own `"max"` string;
  `0` means "unset" (no write at all); anything else is the decimal
  value verbatim (`numToStr` in `fs2`).
* `memory.swap.max` takes swap *alone*, but the spec's `swap` field is
  memory+swap combined (cgroup v1 style) — converted by subtracting
  `memory.limit`, matching `ConvertMemorySwapToCgroupV2Value` bit for bit
  (including its edge cases: unlimited memory defaults swap to unlimited
  too if unset, equal memory+swap explicitly disables swap rather than
  leaving it unset, etc.).
* `cpu.shares` (cgroup v1, range ~2-262144, default 1024) converts to
  `cpu.weight` (cgroup v2, range 1-10000, default 100) via the same
  quadratic curve-fit formula as `ConvertCPUSharesToCgroupV2Value` —
  chosen upstream specifically so shares' min/max/default map exactly to
  weight's min/max/default, so re-deriving a different formula would be
  actively wrong, not just different.
* `cpu.max` is `"<quota> <period>"` (or `"max <period>"` when quota is
  unset/non-positive), `period` defaulting to `100000` — the kernel's own
  documented default — when unset.

`apply(cgroup_dir, &writes)` performs the writes. Unlike
`namespaces::unshare` or `oci_mount::syscalls::mount`, this *is* covered
by an ordinary automated test against a real filesystem (a temp
directory standing in for a cgroup directory): writing plain text files
needs no special privilege at all — it's the real cgroupfs mount, not the
write syscall, that enforces cgroup semantics, so a temp-dir test still
exercises the real write logic, just not against a real kernel-enforced
limit.

## Verified against a real cgroup v2 hierarchy, not just runc's source

This session's own login already has a systemd-delegated cgroup v2
subtree (`user@1000.service/app.slice`, with `memory`/`pids` enabled in
`cgroup.subtree_control` by default). Created a disposable child cgroup
there, ran `plan_resources`/`apply` on a resource set matching the exact
values in the new `runc-spec-with-resources.json` test fixture (a real
`runc spec` bundle with `memory`/`cpu`/`pids` added, which real `runc
create` would accept and apply), and confirmed every value the kernel
read back exactly matched what was written:

```
memory.max -> "104857600"     (matches the 100MiB limit given)
memory.low -> "52428800"      (matches the 50MiB reservation given)
cpu.weight -> "59"             (matches convert_cpu_shares_to_weight(512))
cpu.max    -> "50000 100000"   (matches quota=50000, period=100000)
pids.max   -> "64"             (matches the limit given)
```

`cpu.weight`/`cpu.max` needed the `cpu` controller temporarily enabled in
`app.slice`'s `cgroup.subtree_control` (only `memory`/`pids` were enabled
by default); it was restored to its exact original value
(`memory pids`) immediately after, and the disposable test cgroup was
removed. No lasting change to the live desktop session's cgroup
configuration.

## Decisions and risks

* Still no cgroup *directory creation*, no BPF device-cgroup filtering
  (cgroup v2 device control is eBPF-based, not the simple
  `devices.allow`/`devices.deny` files cgroup v1 used — a substantially
  larger undertaking on its own, deliberately deferred), no
  `blkio`/`hugetlb`/`rdma`/`cpuset` translation. `create` will need
  cgroup directory creation and process-migration (`cgroup.procs`) next;
  `plan_resources`/`apply` are the pieces it calls once that scaffolding
  exists.
