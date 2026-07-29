# Design note 0264: `ci/bench.sh` gains a real `build` comparison

Status: implemented
Scope: `ci/bench.sh`, `docs/benchmarks.md`.

## A real gap in an otherwise-comprehensive benchmark script

`ci/bench.sh` already covered `run`/`run --rm`/`rm`/`run -d`/`commit`
— every comparison every hand-run performance-reverification note
since 0161 used — but had no `ociman build` vs `podman build`/`docker
build` comparison at all, despite `ociman build`'s own real build
cache (0101/0121/0130-0133) having had real, measured optimization
work done on it before. This project's own explicit goal is beating
every real equivalent "on all the benchmarks" — build time was a real,
previously-unmeasured gap in that claim, not a deliberately-scoped-out
one.

## What got measured, and the real numbers

A real, small multi-step build (a base image, four `RUN` steps, one
`COPY`), measured two ways — matching how a real developer actually
experiences build time, not just one arbitrary point:

- **`--no-cache`** (every layer genuinely re-executes — the cold CI
  build case): **`ociman build` is 16-22× faster** than
  `docker build`/`podman build` (68.7ms vs. 1102ms/1345ms on this
  project's own dev host).
- **Fully cached** (the common "iterate on something else, rebuild
  the same image" case — `hyperfine`'s own `--warmup` runs populate
  each tool's real cache before any timed sample starts): **`ociman
  build` is 21-27× faster** (8.4ms vs. 178.6ms/226.5ms).

Both numbers were reproduced across multiple full script runs before
being written down here, matching this project's own established "not
just measured once" rigor for performance claims.

## Two real, checked-directly gotchas this script now encodes

- **`docker build` needs an explicit `-f Containerfile`**: unlike
  `ociman`/`podman`, real `docker build` never looks for a plain
  `Containerfile` by default (only `Dockerfile`) — confirmed directly
  (a bare `docker build` against this section's own context failed
  outright, "open Dockerfile: no such file or directory", before this
  flag was added).
- **`ociman`'s own half runs against a scratch storage root** (the
  exact same technique the pre-existing `commit` comparison already
  established), seeded offline via `save`/`load` from the same
  already-pulled default-store image every other section already
  requires — keeping every repeated build off the default store
  entirely, reclaimed with `$workdir` at script exit regardless of how
  many samples ran.

## A real, accepted trade-off, not a new one

Neither section ever passes `-t` to podman/docker (matching real
`docker build`/`podman build` with no `-t` at all), so repeated,
non-byte-identical samples can leave a real dangling image behind in
each of their own stores — the exact same trade-off `ci/bench.sh`'s
own pre-existing `commit` section cleanup comment already accepts
("Deliberately no `podman image prune` here... reclaims them whenever
the host wants") rather than a new one introduced here. Verified by
hand: a full script run left a real but small (single-digit-MB)
residue in both stores, cleaned up manually for this session and
reclaimable at any time by either tool's own real `image prune` — or,
now, by this project's own `ociman system df`/`prune` (0263) for its
own half.

## Verified

Ran the full, updated `ci/bench.sh` end to end twice, confirming: both
new sections integrate cleanly with the existing opportunistic
skip-if-missing pattern, produce consistent, reproducible numbers
across runs, and every other pre-existing section's own numbers are
unchanged (no regression from this addition, which touches no Rust
code at all).

## Still ahead

`docs/benchmarks.md`'s own "What this doesn't cover yet" section
still applies: not wired into `.github/workflows/ci.yml` (deliberately
— a shared CI runner is a poor host for real wall-clock comparisons).
