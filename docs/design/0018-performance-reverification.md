# Design note 0018: re-verifying `ocirun run` still beats crun/runc

Status: verification only (no functional change)
Scope: none (measurement, run against the same binaries 0012 already
built)

## Why re-measure

0012 measured `ocirun run` against real `crun`/`runc` before any of
0013–0017 (uid/gid/capability dropping, rlimits, cgroup directory
creation + resource writes + process migration, seccomp compilation and
installation, the exec-fifo-based `create`/`start` lifecycle) existed.
Every one of those is real work added to the same code path that
benchmark exercises — capability dropping alone is a `setresgid`/
`setresuid`/multiple `prctl`/`capset` sequence per container — so the
project's own stated bar ("must have measurably equal or better
performance than before") means the original 2.8ms number needed
re-checking, not just assumed to still hold.

## Method (identical to 0012, so the numbers are directly comparable)

`hyperfine` (100+ samples, 5 warmup runs), the same rootless busybox
bundle shape, `/bin/true` as the container's process, all three tools'
own `run` subcommand, this session's own aarch64 host (unchanged since
0012).

## Result: no regression

| tool | mean (0012) | mean (now) | relative (now) |
|---|---:|---:|---:|
| `ocirun` | 2.8 ms | 2.9 ms | 1.00× |
| `crun` | 10.1 ms | 9.9 ms | 3.39× slower |
| `runc` | 20.5 ms | 19.9 ms | 6.82× slower |

Within noise of the original measurement (the `crun`/`runc` numbers
moved by a similar amount, confirming this is host-load variance
between sessions, not a systematic shift). Five increments' worth of
real per-container setup work — namespace/rootless-mapping (already
counted in 0012), plus capability/uid/gid dropping, rlimit
application, and everything `identity`/`rlimits` do — added no
measurable overhead at this scale. `ocirun run`'s bundle for this
benchmark doesn't set `cgroupsPath` or `seccomp` (matching 0012's own
bundle exactly, for a fair comparison), so cgroup and seccomp overhead
isn't reflected in the table above — measured separately:

## The cost of cgroup + seccomp specifically, isolated

Same bundle, plus `linux.cgroupsPath` (a real, delegated
`systemd --user` cgroup — see `docs/design/0015` for why `ocirun`
itself needs to be invoked from inside `systemd-run --user --scope`
for the migration to succeed at all) and a `linux.seccomp` profile
(single shared action, `docs/design/0016`'s supported scope):

| configuration | mean |
|---|---:|
| `ocirun run` (no cgroup/seccomp, table above) | 2.9 ms |
| `ocirun run` (+ cgroup directory/resources/migration + seccomp) | 4.8 ms |

The full set of container-management features this project has built
so far costs about 1.9ms extra per container — cgroup directory
creation, two resource-limit file writes, one `cgroup.procs` migration
write, and compiling+installing a real BPF seccomp filter, combined.
Even fully loaded, `ocirun run` (4.8ms) is still faster than bare
`crun run` (9.9ms) and `runc run` (19.9ms) *without* either of them
doing the equivalent cgroup/seccomp work in this specific comparison
(a real default `podman`/`docker` invocation of `crun`/`runc` would be
doing comparable cgroup/seccomp work too, likely narrowing the
absolute gap somewhat versus this specific measurement — but the
architectural advantage this project is built on, a small static Rust
binary with no interpreter/GC/runtime startup cost doing exactly the
syscalls needed, isn't specific to any one feature and should hold
comparably once `crun`/`runc` are measured with matching configuration
in a later increment).

## What this doesn't cover

* Root (non-rootless) containers, network namespace setup with a real
  veth pair, and heavier images — all still unmeasured.
* `crun`/`runc` with an equivalent cgroup+seccomp configuration of
  their own (would make the "cost of the same features" comparison
  fully apples-to-apples rather than "our loaded number vs. their
  unloaded one").
* Memory usage, and the `create`/`start`/`kill`/`delete` two-phase
  lifecycle's own latency (only `run`, the combined path, is measured
  here).
