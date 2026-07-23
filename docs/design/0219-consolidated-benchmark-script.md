# Design note 0219: `ci/bench.sh`, a consolidated benchmark script

Status: implemented (tooling only, no functional change)
Scope: `ci/bench.sh`; `docs/benchmarks.md`; `docs/HACKING.md`.

## Why now

This project's own explicit, stated goal names beating every real
equivalent "on all the benchmarks, especially startup time and destroy
time" — and every prior increment that measured this (0012, 0018, 0105,
0113, 0120, 0139, 0150, 0161, 0170, 0176, 0183) did so by hand: typing
the same `ocirun spec --rootless --bundle`/busybox-rootfs setup, the
same `hyperfine` invocation shape, re-derived from memory or a prior
design doc's own prose each time, never as a saved, runnable script.
That's been fine for a one-off re-verification tied to a specific code
change, but it means "run all the benchmarks" has, until now, always
meant "re-read several design docs and manually reconstruct their own
setup" rather than a single command. `ci/bench.sh` closes that gap —
purely a consolidation of existing, already-established methodology,
not a new one.

## What it covers, and why exactly these three

* `ocirun run` vs `crun run`/`runc run` (rootless busybox bundle,
  `/bin/true`) — the runtime layer's own combined startup+destroy
  cycle, this project's own most fundamental benchmark since 0012.
* `ociman run --rm` vs `podman run --rm`/`docker run --rm` (a real
  already-pulled `busybox:latest`) — the engine-level equivalent, the
  actual command a real end user types.
* `ociman rm` vs `podman rm`, isolated (an already-created, already-
  stopped container being removed) — this project's own goal names
  "destroy time" as its own, separate benchmark, not just whatever's
  left folded into a combined `run` total; 0113 first made exactly
  this same isolation argument for `create`/`delete` at the `ocirun`
  layer.

Every one of these three was chosen because it's the smallest, most
direct, most-already-established comparison in this project's own
history — not a new benchmark shape invented for this script.

## A real gotcha this script encodes, not rediscovers

Real `crun` rejects `ocirun spec --rootless`'s own generated
`ociVersion: "1.2.1"` outright ("unknown version specified") — an
exact/prefix check, not real semver comparison. Already documented in
0105 (patched there, by hand, to `"1.0.2-dev"`); `ci/bench.sh` patches
it to `"1.1.0"` instead (tested directly: also accepted by `crun`/
`runc`/`ocirun` alike) as part of the script itself, so this doesn't
need rediscovering — or copy-pasting from a five-year-old design doc's
own prose — every time someone runs a benchmark by hand again.

## Real, direct testing done while building this script

Confirmed directly, not assumed:

* `crun run <id>`/`runc run <id>` both tolerate being invoked
  repeatedly with the exact same container id with no cleanup step
  needed in between — both tools remove their own container's state
  automatically once a foreground `run` finishes (`runc run --help`'s
  own `--keep` flag documents this default: "do not delete the
  container after it exits", i.e. it *does*, by default) — so the
  runtime-layer comparison needs no per-iteration id rotation or
  `--prepare` cleanup at all.
* `ociman rm` refuses a container that's merely `created` (never
  actually started) with "not stopped" unless `--force` is given —
  narrower than real `podman rm`, which accepts a `created` container
  with no such flag needed. Not changed by this increment (out of
  scope for a benchmark script; a real, possibly worth narrowing gap
  for its own future increment) — `ci/bench.sh` simply uses
  `ociman rm --force` uniformly for the destroy-only comparison so it
  measures the same real removal work either way, not this particular
  displayed-state strictness difference.
* Every comparison's own opportunistic skip logic (busybox/crun/runc/
  podman/docker missing, or the benchmark image not already pulled
  into a given tool's own store) was exercised directly by temporarily
  renaming/hiding each prerequisite in turn and confirming the script
  still completes cleanly, printing a clear skip message, rather than
  failing outright.

## Verified: reproduces the established, already-documented results

Two full runs, back to back, on this project's own aarch64 dev host:

| comparison | run 1 | run 2 |
|---|---:|---:|
| `ocirun run` vs `crun run` | 2.33× | (consistent, not separately re-tabulated) |
| `ocirun run` vs `runc run` | 6.48× | 6.39× |
| `ociman run --rm` vs `podman run --rm` | 3.09× | 2.63× |
| `ociman run --rm` vs `docker run --rm` | 4.70× | 4.08× |
| `ociman rm` vs `podman rm` | 69.61× | 87.31× |

All well within this project's own already-established session-to-
session noise band (see 0183's own identical observation) — no
regression, this project's own binaries remain solidly ahead on every
axis the script measures. Confirmed clean afterward both times: no
stray `benchbox` container left in `ociman`/`podman`/`docker`, no
leftover temp directories, disk usage unchanged.

## What's out of scope here

Not wired into `.github/workflows/ci.yml` (a shared, possibly-
contended runner without a guaranteed crun/runc/podman/docker
installation is a poor host for wall-clock-timing-relative-to-other-
real-tools, same reasoning as `ci/build-rpm.sh`/`ci/build-deb.sh`'s own
"local/manual only" scoping) — local, on-demand use only. `ociman
commit` vs `podman commit` and the `create`/`start` half-isolated
figures 0113 established aren't folded into the script yet — real,
separate, still-ahead follow-up work, not attempted here.
