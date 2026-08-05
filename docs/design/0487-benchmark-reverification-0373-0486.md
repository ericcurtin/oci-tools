# Design note 0487: re-verify every benchmark after 0373-0486

Status: implemented (verification-only, no code change)
Scope: none (measurement).

## Why

A large batch of increments since `0372`'s own last full re-
verification (`0373`-`0486`, well over a hundred) — the project's own
"beat every equivalent tool on all the benchmarks, especially startup
time and destroy time" claim is re-checked periodically rather than
assumed to still hold forever, matching the cadence `0219`/`0288`/
`0314`/`0331`/`0360`/`0372` already established. This batch was
dominated by CLI-surface/alias additions and correctness fixes across
`ociman`/`ocicri`/`ocibox` (the `ociman image`/`system` alias
families, `--latest`/`--all`/multi-id support on several commands,
`container clone`, `ocicri` security-context/stop-signal fields,
`ocibox create`/`ephemeral --clone`, `export --enter-flags`) — none
of which touched any hot startup/create/destroy/build/commit path
directly (each one's own design note already confirmed and recorded
"no benchmark re-run needed" at the time), so this is the first full-
suite confirmation of the whole batch together, not a response to any
specific suspected regression.

## Method

`bash ci/bench.sh`, this project's own aarch64 dev host, same real
tools installed as every previous run (`crun`/`runc`/`podman`/
`docker`, versions unchanged since `0372`).

## Result: no regression, every comparison still a decisive win

| comparison | `0372` | this run | delta |
|---|---:|---:|---:|
| `ocirun run` vs `crun run` | 2.24× | 2.14× | noise |
| `ocirun run` vs `runc run` | 6.67× | 6.31× | noise |
| `ocirun exec` vs `crun exec` | 1.60× | 1.68× | noise |
| `ocirun exec` vs `runc exec` | 9.82× | 8.53× | noise |
| `ociman exec` vs `podman exec` | 50.62× | 48.89× | noise |
| `ociman exec` vs `docker exec` | 15.58× | 14.09× | noise |
| `ociman run --rm` vs `podman run --rm` | 5.78× | 6.72× | up |
| `ociman run --rm` vs `docker run --rm` | 9.00× | 8.28× | noise |
| `ociman run -d` vs `podman run -d` | 4.29× | 4.04× | noise |
| `ociman run -d` vs `docker run -d` | 5.33× | 4.52× | noise |
| `ociman rm` vs `podman rm` | 42.45× | 43.47× | noise |
| `ociman commit` vs `podman commit` | 30.16× | 40.25× | up |
| `ociman build --no-cache` vs `podman build` | 21.52× | 23.53× | noise |
| `ociman build --no-cache` vs `docker build` | 18.41× | 16.91× | noise |
| `ociman build` (cached) vs `podman build` (cached) | 22.23× | 23.25× | noise |
| `ociman build` (cached) vs `docker build` (cached) | 29.77× | 29.97× | noise |

Every single comparison remains a decisive win, none within
measurement noise of parity, let alone a regression. `ociman commit`
nudged up more than usual (30.16×→40.25×, driven by `podman commit`'s
own absolute time increasing from ~118ms to ~155ms this run — this
project's own side, `~3.9ms`, unchanged within noise) — consistent
with ordinary session-to-session real-tool variance this project has
no control over, not a change on this project's own side.
`ocirun`/`ociman`'s own absolute per-operation times are all still
consistent with `0372`'s own baseline.

## Cleanup

Podman's own store had silently accumulated 585 dangling images
across the whole batch since `0372` (13.38MB reclaimable — mostly
tiny, layer-shared test fixtures) — pruned via `podman image prune
-f`. Docker's own store had similarly accumulated 177 dangling images
(926.4MB reclaimed) — pruned via `docker image prune -f`. Neither
real tool's own pre-existing, unrelated images/containers/volumes/
build-cache were touched. Disk space remains healthy throughout
(2.6T free of 3.7T both before and after this cleanup).

## Still ahead

Same as every previous re-verification: no remaining individual
performance gap beyond keeping this cadence going forward. Next full
re-verification due after another ~40-100 increments, or immediately
if any future increment touches a hot startup/destroy/build/commit
path directly.
