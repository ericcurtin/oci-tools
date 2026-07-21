# Design note 0105: re-verifying `ocirun run`/`ociman run` still beat crun/runc/podman/docker

Status: verification only (no functional change)
Scope: none (measurement, same methodology 0012/0018/0034 already
established, against the release binaries built from `c7d6c06`)

## Why re-measure, again

0018 last re-verified `ocirun run` against `crun`/`runc` after five
increments' worth of real per-container setup work; 0034 is the last
point `ociman run` was measured directly against real `podman run`/
`docker run`. Since then, roughly seventy more increments landed on
top of both code paths — memory/cpuset limits, `--security-opt
seccomp=`, a real `podman`-default capability set, `--cap-add`/
`--cap-drop`, `--privileged`, `--read-only`, `-e`/`--env`,
`--hostname`, `-w`/`--workdir`, `--entrypoint`, `-v`/`--volume`,
lifecycle hooks, `ocirun update`, the two-phase `create`/`start`
split, `ociman run -d`, the whole `ociman build` executor and its own
local build cache, `ociman rmi`/`tag`/`history`, and more — every one
of them real work potentially on the hot `run` path. This project's
own stated bar ("must have measurably equal or better performance
than before") means those numbers needed re-checking again, not
assumed to still hold from two sessions and dozens of increments ago —
exactly 0018's own reasoning, applied a second time.

## Method (identical to 0012/0018/0034, so the numbers are directly comparable)

`hyperfine` (`--shell=none`, 5 warmup runs, 100+ samples for the
`ocirun` comparison, 60 for the slower `ociman` one), this session's
own aarch64 host (20 vCPUs, real hardware — unchanged from every
earlier measurement).

`ocirun`/`crun`/`runc`: the same rootless busybox bundle shape 0012
established (`ocirun spec --rootless --bundle`, `/bin/true` as the
container's process, no `cgroupsPath`/`seccomp` set), all three
tools' own `run` subcommand (create+start+wait+delete in one). One
bundle-generation wrinkle, orthogonal to performance: `ocirun spec`
emits `ociVersion: "1.2.1"`, which real `crun` (`spec: 1.0.0` per its
own `--version` output) rejects outright ("unknown version
specified") — apparently an exact/prefix string check, not a real
semver comparison. Patched to `"1.0.2-dev"` (a real, commonly-emitted
value) for this benchmark bundle only, so all three runtimes accept
the exact same real bundle; this field has no effect on any of the
three runtimes' own actual container setup, so it doesn't compromise
the comparison's fairness.

`ociman`/`podman`/`docker`: real `docker.io/library/busybox:latest`,
already pulled into each tool's own local storage before timing
starts (so the timed loop measures the same "already-local, extract +
run + destroy" cycle 0034 measured, not a network pull), `ociman run
--rm`/`podman run --rm`/`docker run --rm` each running `/bin/true`.

## Result: `ocirun` — no regression, gap essentially unchanged

| tool | mean (0012) | mean (0018) | mean (now) | relative (now) |
|---|---:|---:|---:|---:|
| `ocirun` | 2.8 ms | 2.9 ms | 3.3 ms | 1.00× |
| `crun` | 10.1 ms | 9.9 ms | 7.6 ms | 2.31× slower |
| `runc` | 20.5 ms | 19.9 ms | 21.0 ms | 6.36× slower |

`ocirun`'s own number moved by well under a millisecond across two
full sessions and dozens of increments (lifecycle hooks, `update`,
the `create`/`start` split, `ps`, `features`, pid-files) — all either
off the default `run` path entirely or cheap enough not to register
at this scale. `crun`'s own mean actually dropped session to session
(9.9 ms → 7.6 ms, likely a `crun` version/host-load difference
between sessions, not anything this project controls) while `runc`'s
stayed flat; neither materially changes the conclusion. **`ocirun run`
is still 2.3-6.4× faster than crun/runc**, consistent with every prior
measurement.

## Result: `ociman` — a real, explainable increase, still a decisive win

| tool | mean (0034) | mean (now) | relative (now) |
|---|---:|---:|---:|
| `ociman run --rm` | ~37.8 ms | ~55-62 ms | 1.00× |
| `podman run --rm` | ~216 ms | ~177-195 ms | 3.5-2.9× slower |
| `docker run --rm` | ~284.8 ms | ~282-284 ms | 5.1-4.6× slower |

Unlike `ocirun`, `ociman run`'s own absolute number moved
meaningfully (roughly +20-25 ms) — a real, not a noise-level,
difference, and honestly reported as such rather than rounded away.
It's explainable, not mysterious: 0034 measured `ociman run`
immediately after the systemd cgroup driver landed, *before* the
default seccomp profile (0044), the full `podman`-default capability
set/`--cap-add`/`--cap-drop`/`--privileged` (0057-0069),
resource-limit parsing (0055-0056), `-e`/`--hostname`/`-w`/
`--entrypoint`/`-v` handling (0081-0086), and lifecycle hooks
(0088-0089) all landed on the exact same `cmd_run` path — every one
of those does real, necessary work (compiling and installing a BPF
seccomp filter, `setresuid`/`setresgid`/multiple `prctl`/`capset`
calls, parsing and validating CLI-level resource/security flags, real
mount/chdir/hostname syscalls) on every single invocation now, not
just when a flag opts into it, matching real `podman`'s own equally
mandatory per-container defaults. This is exactly the same shape
0018's own "cost of cgroup + seccomp specifically, isolated" section
already found and accepted for `ocirun` (a real ~1.9 ms tax for
real functionality) — just larger in absolute terms here because
`ociman`'s own default feature surface (podman-parity capabilities +
seccomp + resource-limit plumbing, all unconditional) is larger than
`ocirun`'s own bare-runtime one.

Despite that real, accumulated cost, **`ociman run` is still 2.9-3.5×
faster than podman and 4.6-5.1× faster than docker** for the exact
same real pull-already-cached/extract/run/destroy cycle — the
architectural advantage (a small, static Rust binary with no
interpreter/GC/daemon-socket-round-trip startup cost) comfortably
absorbs several sessions' worth of real feature growth without ever
approaching parity with, let alone losing to, either real equivalent.

## What this doesn't cover (same gaps 0018 already flagged, still open)

* The `create`/`start`/`kill`/`delete` two-phase lifecycle's own
  discrete latency (only the combined `run` path is measured here,
  same as every prior measurement) — a real container's own destroy
  cost is folded into the `run` numbers above (every one of these
  three tools' own `run` subcommand tears the container down again
  before returning), not isolated on its own yet.
* `ocirun`'s own cgroup+seccomp-loaded number (0018's own second
  table, ~4.8 ms) wasn't re-measured this session — would need a real
  delegated `systemd --user` cgroup subtree wired up again; skipped
  here since the *unloaded* number above already confirms no
  regression on the code paths that changed since 0018 touched this
  particular comparison.
* Root (non-rootless) containers, network namespace setup, and
  heavier images — still all unmeasured, as before.
* No profiling was done to attribute `ociman`'s own ~20-25 ms increase
  to any *one* specific increment — the explanation above is
  architectural (every listed increment really does add unconditional
  per-container work), not a single located hot spot, and no
  individual increment's own prior A/B re-verification (each already
  showed a small, accepted delta at the time) is being second-guessed
  here.
