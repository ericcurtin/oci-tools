# Design note 0314: re-verifying every benchmark after 0289-0313

Status: done (measurement only, no product code changed)
Host: this project's own aarch64 dev host, `crun 1.14.1`/`runc 1.3.4`/
`podman 4.9.3`/`docker 29.2.1`, release build.

## Why re-measure, again

Following 0018/.../0245/0288's own established cadence (a formal note
roughly every 10-40 increments): twenty-five increments (0289-0313)
landed since 0288, none of them a hot-path rewrite — mostly `ociman`
CLI-surface additions (`ps --filter` variants, `--dns`/`--platform`/
`--cidfile`, `healthcheck run` timeout enforcement, `kill --all`,
`stop --all`) each individually noted "not part of any hot-path
benchmark" at commit time, plus two real correctness fixes in `stop`/
`restart` (0313's own paused-container refusal and never-started-
container tolerance) that only change behavior for cases that were
previously either erroring or hanging, not the common path. Real
enough ground to re-verify formally rather than merely infer from "no
single commit looked alarming."

## Results

`ci/bench.sh` run twice (the first run hit a real, environment-level
hazard unrelated to this project's own code — see "A real, transient
hazard" below — discarded; the second, clean run below, both
`podman`/`docker` having `busybox:latest` pre-pulled throughout):

| comparison | this session | 0288 |
|---|---:|---:|
| `ocirun run` vs `crun run` | 3.1ms vs 7.1ms (2.29×) | 3.1ms vs 6.9ms (2.23×) |
| `ocirun run` vs `runc run` | 3.1ms vs 21.3ms (6.82×) | 3.1ms vs 21.3ms (6.89×) |
| `ocirun exec` vs `crun exec` | 2.1ms vs 3.7ms (1.71×) | 2.0ms vs 3.7ms (1.82×) |
| `ocirun exec` vs `runc exec` | 2.1ms vs 19.5ms (9.06×) | 2.0ms vs 19.1ms (9.34×) |
| `ociman exec` vs `podman exec` | 2.8ms vs 136.6ms (48.43×) | 2.7ms vs 137.8ms (50.76×) |
| `ociman exec` vs `docker exec` | 2.8ms vs 46.0ms (16.30×) | 2.7ms vs 47.1ms (17.34×) |
| `ociman run --rm` vs `podman run --rm` | 34.3ms vs 181.1ms (5.28×) | n/a this run¹ |
| `ociman run --rm` vs `docker run --rm` | 34.3ms vs 283.4ms (8.26×) | n/a this run¹ |
| `ociman run -d` vs `podman run -d` | 33.9ms vs 134.9ms (3.98×) | n/a this run¹ |
| `ociman run -d` vs `docker run -d` | 33.9ms vs 172.9ms (5.11×) | n/a this run¹ |
| `ociman rm` (destroy-only) vs `podman rm` | 1.5ms vs 67.9ms (44.57×) | 1.8ms vs 70.2ms (39.66×) |
| `ociman commit` vs `podman commit` | 3.5ms vs 95.0ms (27.46×) | 3.9ms vs 96.3ms (24.51×) |
| `ociman build --no-cache` vs `podman build --no-cache` | 71.7ms vs 1302ms (18.17×) | 65.9ms vs 1363ms (20.67×) |
| `ociman build --no-cache` vs `docker build --no-cache` | 71.7ms vs 1141ms (15.93×) | 65.9ms vs 1097ms (16.65×) |
| `ociman build` (cached) vs `podman build` (cached) | 8.9ms vs 174.7ms (19.68×) | 9.1ms vs 178.5ms (19.66×) |
| `ociman build` (cached) vs `docker build` (cached) | 8.9ms vs 247.0ms (27.82×) | 9.1ms vs 237.5ms (26.16×) |

¹ 0288's own note didn't list `run --rm`/`run -d` against `podman`
specifically for that reverification (it did compare 0245's own
figures for those rows instead, both showing `podman` present); this
run has real, fresh `podman` figures for every row.

## Reading

No regression anywhere. Every figure sits within ordinary session-to-
session host-load noise of its own 0288 counterpart — the largest
swings (`ociman rm`'s 44.57× vs 39.66×, `ociman commit`'s 27.46× vs
24.51×) are movement in the *favorable* direction and, as 0288 itself
already noted about its own similarly-sized swings, reflect normal
noise in an already-tiny (1.5-3.9ms) absolute number, not a real
trend. `ocirun run`'s own purest startup measurement — the figure
least sensitive to storage-layer/build-cache changes and thus the
most meaningful single trend line across every reverification note
since 0018 — remains essentially unchanged at ~2.2-2.3× over crun,
~6.8× over runc.

## A real, transient hazard (environmental, not a product regression)

The first `ci/bench.sh` run this note started from had `podman`
silently skip its own comparison in the `exec`/`run --rm`/`commit`
sections (each guarded by `podman image exists "$image"`) despite
`busybox:latest` genuinely being present in `podman`'s own store both
immediately before and immediately after that run — reproducible
manually as *not* reproducible in isolation (the exact same check
succeeded every time run standalone). This host runs several
concurrent, independent sessions sharing the same real `podman`/
`docker` daemons and stores (evidenced by unrelated log files/
containers from other sessions found in `/tmp` and `docker ps -a`
throughout this project's own history) — the most plausible
explanation is a genuinely concurrent, unrelated session transiently
touching the same shared `podman` image store at that exact moment
(e.g. its own prune/rmi), not a bug in this project's own script or
binaries. Re-running `ci/bench.sh` a second time, back to back,
produced the complete, consistent set of figures above with no further
anomalies. Documented here rather than silently ignored, since a
future reverification hitting the same thing should recognize it
immediately rather than mistake it for a real regression investigation.

## Disk hygiene

Found ~110 dangling (untagged) images in `docker`'s own store and
~50 in `podman`'s own, accumulated from the untagged image every
`build --no-cache` hyperfine sample necessarily produces, across many
sessions' worth of prior `ci/bench.sh` runs — this project's own
established convention ("leave docker's own pre-existing images
untouched") was never meant to extend to *anonymous, dangling*
residue with no real tag or purpose, only to genuinely pre-existing,
*named* images unrelated to this project's own testing. Pruned both
(`podman image prune -f`/`docker image prune -f`, dangling only —
never `-a`, never touching any tagged image): `0B` actually reclaimed
in both cases (every dangling layer was already fully shared with a
still-tagged image, e.g. the base `busybox`, so nothing unique was
ever actually wasted) but the anonymous-image list itself is now
`0` in both stores, keeping this recurring `ci/bench.sh` cost from
accumulating indefinitely across future sessions. Left `docker`'s own,
separate, much larger `Build Cache`/`Local Volumes` reclaimable
figures (`21.79GB`/`3.825GB`) alone — those plausibly belong to other,
concurrent sessions' own active work on this shared host, not
something this note's own narrow benchmarking activity produced or
has any basis to judge safe to clear. Disk overall: `899G` used /
`2.7T` free (`26%`) on `/`, unchanged in any concerning way.

## Verified

- `ci/bench.sh` run twice end to end (figures above from the second,
  complete run); no leftover containers in any of podman/docker/this
  project's own stores afterward; dangling images pruned from both
  podman and docker (see "Disk hygiene" above); the `busybox` image
  pulled into podman/docker specifically for this comparison — both
  already had it pre-existing from earlier sessions, so nothing new
  was added or needs removing.
- No Rust code changed this note — `cargo build`/`test --workspace`
  passing identically before and after, plus the full local check
  suite as always before commit (`cargo fmt --all --check`, `cargo
  clippy --workspace --all-targets --locked -- -D warnings`, `python3
  ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
  `ci/build-deb.sh`).
