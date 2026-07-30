# Design note 0360: re-verify every benchmark after 0332-0359

Status: implemented (verification-only, no code change)
Scope: `docs/benchmarks.md`.

## Why

Twenty-eight increments since `0331`'s own last full re-verification
(`0332`-`0359`) — the project's own "beat every equivalent tool on all
the benchmarks, especially startup time and destroy time" claim is
re-checked periodically rather than assumed to still hold forever,
matching the cadence `0219`/`0288`/`0314`/`0331` already established.
None of these twenty-eight increments touched any hot startup/destroy/
build/commit path `ci/bench.sh` actually measures: `0332`-`0339` were
`--format` support across `inspect`/`ps`/`images`/`volume ls`/`info`
(a separate, unmeasured output path); `0340`-`0350`/`0352` were
one-shot, offline CLI surface gaps (`build --exclude`, `env-file`,
`env-host`, `ps -s`, `volume rename`/`ls -q`, `commit --iidfile`,
`images --filter id=`/`digest=`, `inspect -s`); `0351` added a cheap,
`n`-defaulting-to-`0` early-return check
(`verify_preserve_fds`) to `cmd_run`/`cmd_exec`/`cmd_create`'s own
start, a no-op loop when the (opt-in, off by default) flag isn't
used; `0353`/`0356` extended `ocirun update` (not on any measured
path); `0354`/`0355` extended `kill`/`rm`'s own less-common flag
surface (`rm`'s own fast, unchanged default path got a dedicated
regression-guard test at the time, `0355`); `0357`-`0359` added
`container`/`image prune` subcommands and extended `ociman prune`
(a separate, unmeasured maintenance command, never `rm`/`run`/`exec`
itself). No regression was expected — re-run to confirm rather than
assumed.

## Method

`bash ci/bench.sh`, this project's own aarch64 dev host, same real
tools installed as `0331`'s own run (`crun 1.14.1`/`runc 1.3.4`/
`podman 4.9.3`/`docker 29.2.1`). `docker.io/library/busybox:latest`
was re-pulled into podman's own store first (evicted since `0331`'s
own run, on this shared host, by something unrelated to this
project) so every podman comparison ran for real rather than being
silently skipped.

## Result: no regression, every comparison still a decisive win

| comparison | `0331` | this run | delta |
|---|---:|---:|---:|
| `ocirun run` vs `crun run` | 2.26× | 2.18× | noise |
| `ocirun run` vs `runc run` | 6.73× | 7.22× | noise |
| `ocirun exec` vs `crun exec` | 1.61× | 1.79× | noise |
| `ocirun exec` vs `runc exec` | 9.83× | 9.33× | noise |
| `ociman exec` vs `podman exec` | 50.54× | 49.83× | noise |
| `ociman exec` vs `docker exec` | 16.15× | 16.62× | noise |
| `ociman run --rm` vs `podman run --rm` | 5.32× | 5.83× | noise |
| `ociman run --rm` vs `docker run --rm` | 8.34× | 8.85× | noise |
| `ociman run -d` vs `podman run -d` | 3.18× | 3.57× | noise |
| `ociman run -d` vs `docker run -d` | 4.18× | 4.60× | noise |
| `ociman rm` vs `podman rm` | 40.39× | 39.58× | noise |
| `ociman commit` vs `podman commit` | 26.04× | 28.57× | noise |
| `ociman build --no-cache` vs `podman build` | 19.05× | 19.85× | noise |
| `ociman build --no-cache` vs `docker build` | 16.17× | 16.71× | noise |
| `ociman build` (cached) vs `podman build` (cached) | 20.05× | 20.25× | noise |
| `ociman build` (cached) vs `docker build` (cached) | 27.50× | 28.41× | noise |

Every single comparison remains a decisive win, none within
measurement noise of parity, let alone a regression — most figures
nudged very slightly *up* this run, well within normal session-to-
session variance, not a real trend. `ocirun`/`ociman`'s own absolute
per-operation times (1.7-68ms depending on operation) are all still
consistent with `0331`'s own baseline.

## Cleanup

`ci/bench.sh`'s own real `podman build`/`commit`/`run --rm`
repetitions, plus whatever accumulated silently across every session
since `0331` (podman's own store had grown to 146 dangling `<none>`
images, only 0B beyond the shared `busybox` layers actually
reclaimable), pruned via `podman image prune -f` afterward. Real
docker had accumulated none at all this time (its own image list is
all named, pre-existing images unrelated to this project). Neither
real tool's own pre-existing, unrelated images/containers (including
a separate, already-stopped `claude1` container) were touched.

## Also noticed, fixed in this same increment

`docs/benchmarks.md`'s own "Representative historical results" table
and reverification-note list had silently drifted since `0331`: that
note's own stated `Scope: docs/benchmarks.md` was never actually
acted on (only the new design-note file itself was added, confirmed
directly via `git show --stat` on `0331`'s own commit) — the doc's
table and note list still cited `0314` as "the most recent". Updated
both to this run's own figures and note number as part of this
increment, so the drift doesn't compound further.

## Still ahead

Same as `0331`: no remaining individual performance-reverification
gap beyond keeping this cadence going forward, and actually updating
`docs/benchmarks.md` each time from now on rather than only the
dedicated design note. Next full re-verification due after another
~10-40 increments, or immediately if any future increment touches a
hot startup/destroy/build/commit path directly.
