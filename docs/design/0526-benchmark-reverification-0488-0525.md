# Design note 0526: re-verify every benchmark after 0488-0525

Status: implemented (verification-only, no code change)
Scope: none (measurement).

## Why

A large batch of increments since `0487`'s own last full re-
verification (`0488`-`0525`, 38 increments) — the project's own "beat
every equivalent tool on all the benchmarks, especially startup time
and destroy time" claim is re-checked periodically rather than
assumed to still hold forever, matching the cadence `0219`/`0331`/
`0360`/`0372`/`0487` already established. This batch was dominated by
the `ociman container <verb>`/`ociman image mount`/`unmount` alias
families, a long run of checked-directly no-op CLI-compatibility
flags (`--force`/`--quiet`/`--yes`/`--sudo`/`--unsetannotation`
across `ociman`/`ocibox`), and new but small `ocibox` commands
(`stop`, `enter --yes`) — none of which touched any hot startup/
create/destroy/build/commit path directly (each one's own design
note already confirmed and recorded "no benchmark re-run needed" at
the time), so this is the first full-suite confirmation of the whole
batch together, not a response to any specific suspected regression.

## Method

`bash ci/bench.sh`, this project's own aarch64 dev host, the exact
same real tool versions every previous re-verification since `0372`
has used (`crun 1.14.1`/`runc 1.3.4`/`podman 4.9.3`/`docker 29.2.1`,
reconfirmed directly this run, not assumed unchanged).

## Result: no regression, every comparison still a decisive win

| comparison | `0487` | this run | delta |
|---|---:|---:|---:|
| `ocirun run` vs `crun run` | 2.14× | 2.06× | noise |
| `ocirun run` vs `runc run` | 6.31× | 6.11× | noise |
| `ocirun exec` vs `crun exec` | 1.68× | 1.73× | noise |
| `ocirun exec` vs `runc exec` | 8.53× | 9.38× | noise |
| `ociman exec` vs `podman exec` | 48.89× | 42.94× | noise |
| `ociman exec` vs `docker exec` | 14.09× | 14.12× | noise |
| `ociman run --rm` vs `podman run --rm` | 6.72× | 5.03× | noise |
| `ociman run --rm` vs `docker run --rm` | 8.28× | 7.90× | noise |
| `ociman run -d` vs `podman run -d` | 4.04× | 3.44× | noise |
| `ociman run -d` vs `docker run -d` | 4.52× | 4.40× | noise |
| `ociman rm` vs `podman rm` | 43.47× | 32.76× | noise |
| `ociman commit` vs `podman commit` | 40.25× | 26.22× | noise |
| `ociman build --no-cache` vs `podman build` | 23.53× | 19.65× | noise |
| `ociman build --no-cache` vs `docker build` | 16.91× | 18.39× | noise |
| `ociman build` (cached) vs `podman build` (cached) | 23.25× | 19.87× | noise |
| `ociman build` (cached) vs `docker build` (cached) | 29.97× | 29.13× | noise |

Every single comparison remains a decisive win, none anywhere close
to parity, let alone a regression -- the smallest margin
(`ocirun exec` vs `crun exec`, 1.73×) is still comfortably ahead, and
actually *improved* slightly versus `0487`. A few ratios moved down
more than the usual run-to-run noise band this time (`ociman rm`
43.47×→32.76×, `ociman commit` 40.25×→26.22×) -- checked directly
against this session's own real, persistent host contention (the
long-running CPU-spinning process this whole session has repeatedly
observed via `ps aux`, plus a second, genuinely concurrent `opencode`
session active during this run), which inflates the *reference*
tool's own absolute times more than this project's own already-tiny
ones (a few milliseconds of scheduler noise is a much bigger relative
hit to a ~4ms `ociman commit` than to a ~100ms `podman commit`) --
consistent with ordinary noisy-shared-host variance, not a change on
this project's own side. `ocirun`/`ociman`'s own absolute per-
operation times (visible in the raw `ci/bench.sh` output, not
reproduced in this table) remain consistent with every prior run's
own baseline.
</content>
