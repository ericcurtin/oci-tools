# Design note 0372: re-verify every benchmark after 0360-0371

Status: implemented (verification-only, no code change)
Scope: none (measurement).

## Why

Eleven increments since `0360`'s own last full re-verification
(`0361`-`0371`) — the project's own "beat every equivalent tool on all
the benchmarks, especially startup time and destroy time" claim is
re-checked periodically rather than assumed to still hold forever,
matching the cadence `0219`/`0288`/`0314`/`0331`/`0360` already
established. Two of these eleven increments (`0363`'s own `ocirun
exec --cap`/`--ignore-paused`, `0370`'s own `ociman inspect
started_at`/`finished_at`) already got their own individual, targeted
`ci/bench.sh` re-runs at the time (documented in each one's own design
note) since each genuinely touched a hot path (`ocirun exec`,
`run_and_finalize`) — this note is the broader, full-suite
confirmation the rest of the batch (`0361`/`0362`/`0364`-`0369`/
`0371`: volume/container `mount`/`unmount`, `ocibox generate-entry`,
`ocicri run_as_user` validation, `ocirun update --blkio-weight`,
`ociman system reset`/`diff --format`/`inspect`'s new `mounts` field/
`search --list-tags`) never touched at all.

## Method

`bash ci/bench.sh`, this project's own aarch64 dev host, same real
tools installed as every previous run (`crun 1.14.1`/`runc 1.3.4`/
`podman 4.9.3`/`docker 29.2.1`).

## Result: no regression, every comparison still a decisive win

| comparison | `0360` | this run | delta |
|---|---:|---:|---:|
| `ocirun run` vs `crun run` | 2.18× | 2.24× | noise |
| `ocirun run` vs `runc run` | 7.22× | 6.67× | noise |
| `ocirun exec` vs `crun exec` | 1.79× | 1.60× | noise |
| `ocirun exec` vs `runc exec` | 9.33× | 9.82× | noise |
| `ociman exec` vs `podman exec` | 49.83× | 50.62× | noise |
| `ociman exec` vs `docker exec` | 16.62× | 15.58× | noise |
| `ociman run --rm` vs `podman run --rm` | 5.83× | 5.78× | noise |
| `ociman run --rm` vs `docker run --rm` | 8.85× | 9.00× | noise |
| `ociman run -d` vs `podman run -d` | 3.57× | 4.29× | noise |
| `ociman run -d` vs `docker run -d` | 4.60× | 5.33× | noise |
| `ociman rm` vs `podman rm` | 39.58× | 42.45× | noise |
| `ociman commit` vs `podman commit` | 28.57× | 30.16× | noise |
| `ociman build --no-cache` vs `podman build` | 19.85× | 21.52× | noise |
| `ociman build --no-cache` vs `docker build` | 16.71× | 18.41× | noise |
| `ociman build` (cached) vs `podman build` (cached) | 20.25× | 22.23× | noise |
| `ociman build` (cached) vs `docker build` (cached) | 28.41× | 29.77× | noise |

Every single comparison remains a decisive win, none within
measurement noise of parity, let alone a regression — most figures
nudged very slightly up this run (a couple, e.g. `run -d`, more than
usual: 3.57×→4.29×/4.60×→5.33× against podman/docker respectively),
still well within this project's own already-documented "`run -d` is
the noisiest of this project's own measured operations" session-to-
session variance (`0331`'s own identical caveat). `ocirun`/`ociman`'s
own absolute per-operation times (1.7-63ms depending on operation) are
all still consistent with `0360`'s own baseline.

## Cleanup

Podman's own store had silently accumulated 264 dangling images across
every session since `0360` (only 5.96MB actually reclaimable, all
layers shared with tagged images this project's own test fixtures
use) — pruned via `podman image prune -f`. Docker's own store had
similarly accumulated 77 dangling images (0B reclaimed, same reason)
— pruned via `docker image prune -f`. Neither real tool's own
pre-existing, unrelated images/containers/volumes/build-cache
(including a separate, already-stopped `claude1` container and
docker's own much larger, unrelated build cache) were touched.

## Still ahead

Same as every previous re-verification: no remaining individual
performance gap beyond keeping this cadence going forward, and
continuing to update `docs/benchmarks.md` itself at the next real
re-verification (per `0360`'s own "also fixed" note about that file
having silently drifted once already). Next full re-verification due
after another ~10-40 increments, or immediately if any future
increment touches a hot startup/destroy/build/commit path directly.
