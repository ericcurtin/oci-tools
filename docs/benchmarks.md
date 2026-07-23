# Benchmarks

This project's own explicit goal calls out beating every real
equivalent "on all the benchmarks, especially startup time and destroy
time." Since 0012 (`ocirun run`'s own first increment), every
performance-sensitive change has been measured directly against a real
installed `crun`/`runc`/`podman`/`docker`, by hand, with `hyperfine` —
see the many `docs/design/NNNN-performance-reverification-N.md` notes
(0018, 0105, 0113, 0120, 0139, 0150, 0161, 0170, 0176, 0183, 0221) for the
full, individual, dated results.

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
* **`ociman run --rm` vs `podman run --rm` vs `docker run --rm`** — the
  same shape one level up, a real already-pulled
  `docker.io/library/busybox:latest`, the full engine-level
  startup+destroy cycle a real end user actually types.
* **`ociman rm` vs `podman rm`** — destroy time in isolation (an
  already-created, already-stopped container being removed), since
  this project's own goal names destroy time as its own, separate
  benchmark, not just whatever's left over inside a combined `run`
  figure.

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

From `docs/design/0183` (the most recent full re-verification as of
this writing), this project's own aarch64 dev host, `crun 1.14.1`/
`runc 1.3.4`/`podman 4.9.3`/`docker 29.2.1`:

| comparison | this project | real equivalent | speedup |
|---|---:|---:|---:|
| `ocirun run` vs `crun run` | 3.4ms | 7.5ms | 2.20× |
| `ocirun run` vs `runc run` | 3.4ms | 21.8ms | 6.37× |
| `ociman run --rm` vs `podman run --rm` | 66.8ms | 189.9ms | 2.84× |
| `ociman run --rm` vs `docker run --rm` | 66.8ms | 289.9ms | 4.34× |
| `ociman rm` (destroy-only) vs `podman rm` | 5.2ms | 72.4ms | 13.94× |
| `ociman commit` vs `podman commit` | 2.6ms | 98.7ms | 38.19× |

Absolute numbers vary session to session (host load, exact tool
versions) and will differ on any other host entirely — the relative
gap holding steady release after release, re-verified repeatedly
rather than assumed to still be true forever, is the actual point.
Most recently reconfirmed (`docs/design/0221`, same tool versions, 37
commits later, none touching this path): every figure above still a
decisive win, some sessions faster/slower than others purely from host
load, never from a real regression.
`ociman commit` isn't wired into `ci/bench.sh` yet (see below).

## What this doesn't cover yet

* `ociman commit`, `ociman create -d`/create-only timing, and every
  other individual `docs/design/*-performance-reverification-*` figure
  that isn't one of the three comparisons above — real, still-ahead
  follow-up work to fold into the script rather than leaving them
  hand-run-only.
* Not wired into `.github/workflows/ci.yml`, deliberately: a shared,
  possibly-contended CI runner (and one that may not even have crun/
  runc/podman/docker installed at all) is a poor host for a benchmark
  whose whole point is real wall-clock timing relative to other real
  tools — local/manual use only, like `ci/build-rpm.sh`/
  `ci/build-deb.sh`.
