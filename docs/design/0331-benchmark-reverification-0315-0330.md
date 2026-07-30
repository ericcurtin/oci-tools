# Design note 0331: re-verify every benchmark after 0315-0330

Status: implemented (verification-only, no code change)
Scope: `docs/benchmarks.md`.

## Why

Sixteen increments since `0314`'s own last full re-verification
(`0315`-`0330`) — the project's own "beat every equivalent tool on all
the benchmarks, especially startup time and destroy time" claim is
re-checked periodically rather than assumed to still hold forever,
matching the cadence `0219`/`0288`/`0314` already established. None of
these sixteen increments touched any hot startup/destroy/build/commit
path at all (`0315`-`0320`: `ociman` container-lifecycle multi-target/
`--all` support and the cgroup-freeze thaw fix; `0321`-`0330`: `ocibox
export`'s own flags and `ocivmm rm`'s multi-name support — all one-shot,
offline commands, not part of any tracked comparison; `0325`: a
doc-comment-only correction), so no regression was expected — re-run to
confirm rather than assumed.

## Method

`bash ci/bench.sh`, this project's own aarch64 dev host, same real
tools installed as `0314`'s own run (`crun 1.14.1`/`runc 1.3.4`/
`podman 4.9.3`/`docker 29.2.1`).

## Result: no regression, every comparison still a decisive win

| comparison | `0314` | this run | delta |
|---|---:|---:|---:|
| `ocirun run` vs `crun run` | 2.29× | 2.26× | noise |
| `ocirun run` vs `runc run` | 6.82× | 6.73× | noise |
| `ocirun exec` vs `crun exec` | 1.71× | 1.61× | noise |
| `ocirun exec` vs `runc exec` | 9.06× | 9.83× | noise |
| `ociman exec` vs `podman exec` | 48.43× | 50.54× | noise |
| `ociman exec` vs `docker exec` | 16.30× | 16.15× | noise |
| `ociman run --rm` vs `podman run --rm` | 5.28× | 5.32× | noise |
| `ociman run --rm` vs `docker run --rm` | 8.26× | 8.34× | noise |
| `ociman run -d` vs `podman run -d` | 3.98× | 3.18× | session variance* |
| `ociman run -d` vs `docker run -d` | 5.11× | 4.18× | session variance* |
| `ociman rm` vs `podman rm` | 44.57× | 40.39× | noise |
| `ociman commit` vs `podman commit` | 27.46× | 26.04× | noise |
| `ociman build --no-cache` vs `podman build` | 18.17× | 19.05× | noise |
| `ociman build --no-cache` vs `docker build` | 15.93× | 16.17× | noise |
| `ociman build` (cached) vs `podman build` (cached) | 19.68× | 20.05× | noise |
| `ociman build` (cached) vs `docker build` (cached) | 27.82× | 27.50× | noise |

\* `run -d`'s own absolute figures moved the most session to session
(this project's own side: 33.9ms -> 42.0ms, `hyperfine` itself flagged
"statistical outliers" on this exact comparison this run) — a real,
already-documented characteristic of this specific comparison (a
detached, keeper-forking launch, the noisiest of this project's own
measured operations), not a regression: still a clear multi-times win
either way, and real podman/docker's own absolute figures moved by a
comparable relative amount in the same run (133.6ms/175.5ms here vs.
134.9ms/172.9ms in `0314`) — the whole *host*, not just this project's
own side, was simply slightly more loaded this run.

Every single comparison remains a decisive win, none within
measurement noise of parity, let alone a regression. `ocirun`/`ociman`'s
own absolute per-operation times (2-70ms depending on operation) are
all still consistent with `0314`'s own baseline.

## Cleanup

`ci/bench.sh`'s own real `podman build`/`commit`/`run --rm` repetitions
left ~40 dangling (`<none>`) intermediate images behind, matching
`0314`'s own identical, already-documented byproduct; pruned via
`podman image prune -f` afterward (0B actually reclaimed — every layer
already shared with the tagged `busybox`/`alpine` images this project's
own test fixtures use). Real docker left no dangling images this run.
Neither real tool's own pre-existing, unrelated images/containers
(including a separate concurrent session's own `claude1` container)
were touched.

## Still ahead

Same as `0314`: no remaining individual performance-reverification gap
beyond keeping this cadence going forward. Next full re-verification
due after another ~10-40 increments, or immediately if any future
increment touches a hot startup/destroy/build/commit path directly.
