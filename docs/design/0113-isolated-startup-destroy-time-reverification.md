# Design note 0113: isolating `create`/`delete` (startup/destroy) time, in their own right

Status: verification only (no functional change)
Scope: none (measurement; this session's own release binaries built
from `5dae225`, `crun 1.14.1`, `runc 1.3.4`, real installed `podman`)

## Why this, now

0105 (last full re-verification) explicitly flagged one gap every prior
measurement (0012/0018/0034/0105 itself) shared: every number was for
the *combined* `run` subcommand (create+start+wait+delete in one), with
a real container's own discrete destroy cost folded into that total,
never isolated on its own. This project's own stated goal calls out
"startup time and destroy time" as the two benchmarks that matter most
— `run`'s own combined total was never enough to actually confirm
*both* halves independently, only their sum. This increment closes that
specific, previously-named gap directly.

## Method

`hyperfine --shell=none` (`-N`), 3 warmup runs, 40-100+ samples per
command depending on how fast it runs (more samples for sub-5ms
commands, matching hyperfine's own guidance for short-running
commands). Each comparison's own untimed setup (creating/starting a
container, polling its `state`/`ps` output until it reaches the state
the timed command needs) runs entirely inside hyperfine's own
`--prepare` hook — a separate real script, invoked directly (no shell
wrapping needed since `--shell=none` executes it like any other
argv[0]) — so only the one real syscall-heavy operation itself is ever
inside the timed window. Same rootless busybox-based bundle shape
0012/0105 established (patched `ociVersion` for `crun`'s own stricter
version check, as before).

### `create` (the actual "startup"/namespace-and-mount-setup half, isolated from `start`)

Prepare: force-delete any leftover container under a fixed id from a
previous run (idempotent, ignored if nothing's there). Timed command:
`create` alone (leaves the container's own process blocked, waiting for
`start` — never actually run).

| tool | mean | relative |
|---|---:|---:|
| `ocirun create` | 2.0 ms | 1.00× |
| `crun create` | 6.6 ms | 3.37× slower |
| `runc create` | 19.6 ms | 9.97× slower |

### `delete` (the actual "destroy" half, isolated from everything before it)

Prepare: `create` + `start` (runs `/bin/true`, exits immediately) +
poll `state` until it reports `"stopped"`. Timed command: `delete`
alone.

| tool | mean | relative |
|---|---:|---:|
| `ocirun delete` | 1.0 ms | 1.00× |
| `crun delete` | 2.2 ms | 2.13× slower |
| `runc delete` | 3.3 ms | 3.15× slower |

### `ociman rm` vs `podman rm` (the same "destroy" isolation one level up, real pulled `busybox:latest`)

Prepare: `run -d` (detached, keeps its own container record after
exiting, matching real `docker run -d`/`podman run -d`) + poll
`ps -a`'s own JSON until the container reports stopped/exited. Timed
command: `rm` alone.

| tool | mean | relative |
|---|---:|---:|
| `ociman rm` | 3.0-3.2 ms | 1.00× |
| `podman rm` | 67.5-68.5 ms | 21.5-22.3× slower |

Re-run twice to confirm this wasn't a one-off — both runs agreed within
noise (22.34× and 21.51×). The gap here is far larger than the raw
runtime-level `delete` numbers above: real `podman rm` pays real Go
runtime startup, its own libpod state-database transaction, and
(unlike the bare-runtime `ocirun`/`crun`/`runc` comparison, which never
touches a network-registry-backed store at all) storage-driver
bookkeeping on top of the same conceptual "remove this container's
on-disk state" operation `ocirun delete` above already showed a much
smaller, but still real, gap for.

## Conclusion: both named benchmarks — startup and destroy — hold up in isolation, not just combined

The combined `run` numbers 0012-0105 already reported were never
hiding a case where one half quietly lost while the other overcompensated:
`ocirun create` alone is 3.4-10.0× faster than `crun`/`runc`'s own
equivalent, `ocirun delete` alone is 2.1-3.2× faster, and `ociman rm`
alone is over 20× faster than real `podman rm`. Every prior combined-`run`
numbers' own reasoning (a small, static Rust binary with no
interpreter/GC/daemon-round-trip cost) applies to each discrete
lifecycle phase independently, not just their sum.

## What this still doesn't cover

* `docker rm`/`docker create` isolated the same way — skipped this
  session (would need a running `dockerd` with its own isolated
  `--data-root`, a heavier harness setup than reusing already-installed
  rootless `podman`/`crun`/`runc` directly); the combined `docker run
  --rm` numbers in 0105 already show a decisive (4.6-5.1×) win and
  nothing in this session's own `ocirun`/`ociman`-side code changed
  since then to cast doubt on it holding up the same way in isolation.
* Root (non-rootless) containers and heavier images: still unmeasured,
  the same standing gap 0018/0105 already noted.
* No code changed this session — every number here was already this
  good beforehand; this increment is purely closing a measurement gap,
  not fixing a regression (none was found).
