# Benchmarks

This project's own explicit goal calls out beating every real
equivalent "on all the benchmarks, especially startup time and destroy
time." Since 0012 (`ocirun run`'s own first increment), every
performance-sensitive change has been measured directly against a real
installed `crun`/`runc`/`podman`/`docker`, by hand, with `hyperfine` —
see the many `docs/design/NNNN-performance-reverification-N.md` notes
(0018, 0105, 0113, 0120, 0139, 0150, 0161, 0170, 0176, 0183, 0221, 0245,
0288, 0314, 0331, 0360, 0372, 0487) for the full, individual, dated
results.

`ci/bench.sh` consolidates that same, previously ad hoc (re-typed by
hand each time) methodology into one reusable, runnable script:

```sh
ci/bench.sh
```

## What it measures

* **`ocirun run` vs `crun run` vs `runc run`** — a full
  create+start+wait+destroy cycle of a trivial rootless container from
  an identical OCI bundle (`ocirun spec --rootless --bundle`, a real
  `busybox` rootfs, `/bin/true` as the process). The actual runtime
  layer's own combined startup+destroy cost.
* **`ocirun exec` vs `crun exec` vs `runc exec`**, and **`ociman exec`
  vs `podman exec` vs `docker exec`** — a hot, frequently-probed path
  (kubelet liveness/readiness probes route through this exact
  machinery via `ocicri ExecSync`, `0240`) no other section measures:
  each tool gets its own real, persistent, long-running container
  (`sleep 60`), set up once before timing starts, then repeated
  `exec ... true` calls into it are timed (see `0279` for the wiring).
* **`ociman run --rm` vs `podman run --rm` vs `docker run --rm`** — the
  same shape one level up, a real already-pulled
  `docker.io/library/busybox:latest`, the full engine-level
  startup+destroy cycle a real end user actually types.
* **`ociman rm` vs `podman rm`** — destroy time in isolation (an
  already-created, already-stopped container being removed), since
  this project's own goal names destroy time as its own, separate
  benchmark, not just whatever's left over inside a combined `run`
  figure.
* **`ociman run -d` vs `podman run -d`/`docker run -d`** — the
  isolated create+start half of the startup story (the combined
  `run --rm` cycle includes destroy), the same figure every
  performance-reverification note since 0161 measured by hand; each
  sample starts a real detached container and returns once it's
  running, with the previous sample's container removed in
  `--prepare`, outside the timed region (see `0245` for the wiring).
* **`ociman commit` vs `podman commit`** — the exact methodology
  every performance-reverification note since 0161 used by hand (see
  `docs/design/0176`'s own "Method" section, and `0235` for the
  wiring): one real, already-stopped container per tool (`sh -c "echo
  hi > /f.txt"`, a real, nonempty diff layer), reused every sample,
  each sample re-committing over the same tag — a real, no-error
  operation for both tools, with the content-identical layer
  deduplicating in both stores so repeated runs don't grow them. The
  ociman half runs against a scratch storage root (cleaned up with
  the run) whose rootless-overlay probe marker is pre-seeded `false`,
  seeded offline via `ociman save`/`load` — the plain-`Extract`
  forcing every hand-run measurement implicitly relied on, since
  `ociman commit` rejects an overlay-rootfs container (`0146`), now
  encoded in the script (see `0235` for the full story).
* **`ociman build` vs `podman build` vs `docker build`** — a real,
  small multi-step build (a base image, four `RUN` steps, one `COPY`)
  measured two ways (see `0264` for the wiring): `--no-cache` (every
  layer genuinely re-executes, the "cold CI build" scenario this
  project's own build cache — 0101/0121/0130-0133 — can't
  short-circuit) and fully cached (the common "iterate on something
  else, rebuild the same image" case, `hyperfine`'s own `--warmup`
  runs populating each tool's real cache for real before any timed
  sample starts). `ociman`'s own half runs against a scratch storage
  root, the same technique the `commit` comparison above already
  established, seeded offline via `save`/`load`; `docker build` alone
  needs an explicit `-f Containerfile` (checked directly: unlike
  `ociman`/`podman`, it never looks for a plain `Containerfile` by
  default, only `Dockerfile`).

Every comparison is opportunistic: any one real equivalent (or
`busybox`, or an already-pulled image) that isn't actually installed
on the host running the script is skipped with a clear message, not a
hard failure — this project's own binaries are still benchmarked alone
in that case.

## A real, fair-comparison gotcha this script encodes so it doesn't need re-discovering by hand again

`ocirun spec --rootless` emits `ociVersion: "1.2.1"` (matching real
`runc`'s own reported spec version). Real `crun` rejects that outright
("unknown version specified") — an exact/prefix version check, not a
real semver comparison, first found and documented in `docs/design/
0105`. `ci/bench.sh` patches the generated bundle's `ociVersion` to
`"1.1.0"` (accepted by `crun`/`runc`/`ocirun` alike) before benchmarking
— this field has no effect on any of the three runtimes' own actual
container setup, so it doesn't compromise the comparison's fairness,
it just stops it from failing outright on `crun`.

## Representative historical results

From `docs/design/0487` (the most recent full re-verification as of
this writing), this project's own aarch64 dev host, `crun`/`runc`/
`podman`/`docker` (same versions as `0372`):

| comparison | this project | real equivalent | speedup |
|---|---:|---:|---:|
| `ocirun run` vs `crun run` | 3.3ms | 7.1ms | 2.14× |
| `ocirun run` vs `runc run` | 3.3ms | 20.9ms | 6.31× |
| `ocirun exec` vs `crun exec` (`0279`) | 2.2ms | 3.7ms | 1.68× |
| `ocirun exec` vs `runc exec` (`0279`) | 2.2ms | 18.7ms | 8.53× |
| `ociman exec` vs `podman exec` (`0279`) | 3.3ms | 160.0ms | 48.89× |
| `ociman exec` vs `docker exec` (`0279`) | 3.3ms | 46.1ms | 14.09× |
| `ociman run --rm` vs `podman run --rm` | 35.5ms | 238.5ms | 6.72× |
| `ociman run --rm` vs `docker run --rm` | 35.5ms | 294.0ms | 8.28× |
| `ociman run -d` vs `podman run -d` | 38.1ms | 153.6ms | 4.04× |
| `ociman run -d` vs `docker run -d` | 38.1ms | 172.0ms | 4.52× |
| `ociman rm` (destroy-only) vs `podman rm` | 2.1ms | 89.2ms | 43.47× |
| `ociman commit` vs `podman commit` | 3.9ms | 155.1ms | 40.25× |
| `ociman build --no-cache` vs `podman build --no-cache` | 67.8ms | 1596ms | 23.53× |
| `ociman build --no-cache` vs `docker build --no-cache` | 67.8ms | 1147ms | 16.91× |
| `ociman build` (cached) vs `podman build` (cached) | 8.7ms | 201.9ms | 23.25× |
| `ociman build` (cached) vs `docker build` (cached) | 8.7ms | 260.3ms | 29.97× |

Absolute numbers vary session to session (host load, exact tool
versions) and will differ on any other host entirely — the relative
gap holding steady release after release, re-verified repeatedly
rather than assumed to still be true forever, is the actual point.
Reconfirmed at every single increment since 0219 (each commit message
carries its own `ci/bench.sh` figures), most recently and formally in
`docs/design/0487` (spanning 0373-0486, well over a hundred
increments since `0372`, none of which touched a hot path directly —
each one's own design note already recorded "no benchmark re-run
needed" at the time) — every figure above still a decisive win,
sessions varying with host load, never a real regression.

## What this doesn't cover yet

* Any remaining individual
  `docs/design/*-performance-reverification-*` figure that isn't one
  of the comparisons above — the historically hand-run set is now
  fully folded in (run/run --rm/rm/commit/run -d/build).
* Not wired into `.github/workflows/ci.yml`, deliberately: a shared,
  possibly-contended CI runner (and one that may not even have crun/
  runc/podman/docker installed at all) is a poor host for a benchmark
  whose whole point is real wall-clock timing relative to other real
  tools — local/manual use only, like `ci/build-rpm.sh`/
  `ci/build-deb.sh`.
