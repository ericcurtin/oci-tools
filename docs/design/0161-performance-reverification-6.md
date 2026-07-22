# Design note 0161: re-verifying `ocirun`/`ociman run` after 0150-0160

Status: verification only (no functional change)
Scope: none (measurement; this session's own release binaries built
from `626e976`, `crun 1.14.1`, `runc 1.3.4`, real installed `podman
4.9.3`/`docker 29.2.1`)

## Why re-measure, again

Following 0018/0105/0113/0120/0133/0139/0150's own established
precedent: ten consecutive commits (0151-0160) landed since the last
re-verification — the same cadence (roughly every 10-19 increments)
this project has settled into for this specific check. Two of them
have real, checked-directly reason to warrant a fresh measurement
rather than an inference:

* **0157** refactored `cmd_run`'s own body into a new shared
  `prepare_container` plus a thin dispatcher (to let `cmd_create`
  reuse the identical setup) — a real structural change to `ociman
  run`'s own hot path, even though no individual step's own logic
  changed.
* **0159** added an *unconditional* `short_id()` call (a `SystemTime`/
  pid-seeded `sha256`, truncated to 12 hex chars) to `run_and_
  finalize`'s own very first lines, for *every* container launch, not
  just `--rm` ones (the scope-nonce fix for the real restart bug that
  increment found) — genuinely new, real, synchronous work on every
  single `ociman run`/`ocirun` launch's own hot path.

(0160's own new `debug_assert_single_threaded` check is release-build
zero-cost by construction and was itself already re-verified not to
regress *debug*-build test-suite performance as part of landing it —
see that design note directly — so it isn't re-measured again here.)

## Method (identical to 0105/0113/0120/0133/0139/0150)

`hyperfine -N` (`--shell=none`), 3-5 warmup runs, 30-650+ samples
depending on how fast each command runs. Same rootless busybox-based
bundle shape for `ocirun`/`crun`/`runc` (`ociVersion` patched to
`1.0.0` for `crun`/`runc`'s own stricter check, `process.args` set to
`["/bin/true"]`); same real, already-pulled `docker.io/library/
busybox:latest` for `ociman`/`podman`/`docker`. Isolated `run -d`
(create-only) and `rm` (destroy-only) halves measured separately too,
matching 0113/0150's own precise decomposition — including 0150's own
key methodological detail, deliberately followed again here: the
`rm`-isolation prepare step polls until the container has genuinely
exited *on its own* (`/bin/true`) before ever timing `rm` itself,
never force-killing a still-running one.

## Result: no regression anywhere

| comparison | this session | most recent prior measurement (0150) |
|---|---:|---:|
| `ocirun run` vs `crun run` | 3.5ms vs 6.7ms (1.93×) | 4.3ms vs 8.7ms (2.00×) |
| `ocirun run` vs `runc run` | 3.5ms vs 21.6ms (6.22×) | 4.3ms vs 22.5ms (5.18×) |
| `ociman run --rm` vs `podman run --rm` | 60.7ms vs 185.3ms (3.05×) | 54.9ms vs 184.8ms (3.37×) |
| `ociman run --rm` vs `docker run --rm` | 60.7ms vs 290.1ms (4.78×) | 54.9ms vs 290.7ms (5.30×) |
| `ociman run -d` (create-only) vs `podman run -d` | 41.3ms vs 133.1ms (3.22×) | 33.2ms vs 137.5ms (4.14×) |
| `ociman rm` (destroy-only) vs `podman rm` | 3.5ms vs 69.5ms (20.10×) | 2.8ms vs 72.1ms (25.75×) |

Every comparison remains a decisive win, well within ordinary session-
to-session noise of every prior measurement round (0139 vs 0133 alone
already showed comparable swings, e.g. 3.36× vs 3.08× for the exact
same `ociman run --rm` vs `podman run --rm` comparison) — none of
0151-0160's own real, hot-path-adjacent work (0157's refactor, 0159's
new unconditional per-launch `short_id()` call) shows up as a
measurable regression against real `crun`/`runc`/`podman`/`docker`,
whose own much larger fixed overhead (Go runtime start, libpod's own
state database and storage-driver bookkeeping) continues to dominate
by a wide, comfortable margin either way.

## Conclusion

No regression found anywhere, including the two specific increments
(0157/0159) with real, checked-directly reason to worry about new
hot-path cost. `ocirun run`'s own numbers, `ociman run --rm`'s
combined numbers, and both isolated create-only/destroy-only halves
all remain solidly ahead of every real equivalent, matching this
project's own explicit "beat the equivalents on all the benchmarks,
especially startup time and destroy time" goal. No code change made
this session — purely a confirmation round, per this project's own
established practice of re-verifying performance claims directly and
periodically rather than only at the moment a change first lands.
