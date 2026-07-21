# Design note 0120: re-verifying `ocirun`/`ociman run`/`ociman build` after 0112-0119, and why the deferred-teardown idea is trickier than it looks

Status: verification only (no functional change)
Scope: none (measurement; this session's own release binaries built
from `1318ae2`, `crun 1.14.1`, `runc 1.3.4`, real installed `podman`/
`docker`)

## Why re-measure, again

0112-0119 (eight consecutive commits) all landed real, non-trivial
changes to `ociman build` and shared code it touches
(`oci_store::ensure_cached`'s own signature, `rootfs_setup.rs`) —
`FROM scratch`, `COPY --from=<external-image>` cache reuse,
`HEALTHCHECK`, `ociman prune --all`, `ONBUILD` (real cross-build
execution), and `RUN` steps seeing `ARG` values. This project's own
repeated standard ("must have measurably equal or better performance
than before") means these needed re-checking against real equivalents
again, not assumed to still hold after eight increments' worth of real
work on shared code paths — the same reasoning 0105/0113 already
established, applied a third time.

## Method (identical to 0105/0113)

`hyperfine --shell=none`, 5+ warmup runs, 30-900+ samples depending on
how fast each command runs. Same rootless busybox-based bundle shape
for `ocirun`/`crun`/`runc` (patched `ociVersion` for `crun`'s own
stricter check); same real, already-pulled `docker.io/library/
busybox:latest`/`docker.io/library/ubuntu:24.04` for
`ociman`/`podman`/`docker`, same `FROM ubuntu:24.04` + `RUN echo hello`
Containerfile for the build comparison (0112's own exact benchmark).

## Result: no regression anywhere, one real fix needed along the way

| comparison | this session | most recent prior measurement |
|---|---:|---:|
| `ocirun run` vs `crun run` | 3.0ms vs 6.7ms (2.22×) | 0105: 3.3ms vs 7.6ms (2.31×) |
| `ocirun run` vs `runc run` | 3.0ms vs 20.7ms (6.83×) | 0105: 3.3ms vs 21.0ms (6.36×) |
| `ociman run --rm` vs `podman run --rm` | 52.3ms vs 179.6ms (3.43×) | 0105: ~55-62ms vs ~177-195ms (2.9-3.5×) |
| `ociman run --rm` vs `docker run --rm` | 52.3ms vs 293.1ms (5.60×) | 0105: ~55-62ms vs ~282-284ms (4.6-5.1×) |
| `ociman build` (warm) vs `podman build` (warm) | 99.7ms vs 85.8ms (1.16× slower) | 0112: 93.6-101.9ms vs 83.6-87.4ms (~1.2× slower) |

Every real-runtime comparison (`ocirun`/`ociman run`) is unchanged
within noise, or very slightly better, confirming zero regression from
0112-0119's own real work on shared code (`ensure_cached`'s signature
refactor, `rootfs_setup.rs`) — none of it touches `ocirun`'s own code
at all, and `ociman run`'s own hot path (overlay-based rootfs setup)
is unaffected by any of the eight `ociman build`-focused commits.

`ociman build`'s own residual ~1.16-1.2× gap against `podman build` is
**exactly the same, already-documented, already-analyzed gap 0112
found and explained** — none of the eight subsequent increments
(`FROM scratch`, cache-reuse for `COPY --from=`, `HEALTHCHECK`,
`ONBUILD`, `ARG`-in-`RUN`-environment, `prune --all`) added any
measurable overhead to this specific Containerfile's own build path
(none of their own new code paths fire for a plain `FROM <image>` +
one `RUN`), and none of them regressed the underlying cache-reuse/
overlay work 0110/0112/0115 already landed.

`hit a real, minor issue along the way (fixed, not a regression)`: real
`runc run` needs `--rootless=true` as a **global** flag (before the
subcommand: `runc --rootless=true run ...`), not a `run`-subcommand
flag the way it was invoked in this session's first attempt — a
`runc` CLI usage detail, not a project bug; corrected before taking
any measurement above.

## A real, non-obvious finding: the "defer build-scratch teardown" idea from 0112 needs more design care than first assumed

0112's own "what this doesn't do yet" section named the final
recursive `remove_dir_all` of a build's own scratch rootfs (paid
synchronously, at the end of every `ociman build`, once the built
image's own manifest is already safely stored) as roughly half of the
residual gap, and suggested "giving `ociman build` its own overlay-
based rootfs lifecycle" as the eventual fix, explicitly deferred as
bigger/riskier. Revisited here to actually scope a *smaller* fix (a
plain rename-instead-of-delete, deferring the real cleanup to a later,
separate `ociman prune` invocation) — and found a genuine, worth-
recording subtlety that make it clear why this isn't as simple as it
first looks:

* **A background thread doesn't safely help here.** Unlike `ocirun`/
  `ociman run`'s own `create_scope` (0033: a background D-Bus thread,
  safe because the *container's own process* stays alive afterward,
  giving the abandoned thread real time to finish or safely time out
  before the whole process eventually exits), `ociman build` is a
  short-lived, one-shot CLI invocation that reports success and exits
  almost immediately after its own last real step. A `remove_dir_all`
  spawned on a background thread and never joined would very likely
  get killed mid-delete when the process exits (all threads in a
  process die together at exit, unlike a genuinely separate forked
  process) — a real risk of a permanently half-deleted, orphaned
  scratch directory, a disk-space correctness regression, not an
  optimization.
* **Renaming instead of deleting doesn't reduce total work across
  *repeated* invocations the way it first appears to — only the
  *first* one.** The instinctive fix (rename the finished scratch
  directory into a "graveyard" holding area — an `O(1)` `rename(2)` —
  and let a later, separate invocation do the real `O(n)` delete)
  looks like a clear win for *this* build's own measured wall-clock
  time. But if that same later invocation is *also* what's being
  timed (exactly the situation a `hyperfine`-style repeated-invocation
  benchmark like the one above creates: build N always ends up
  sweeping build N-1's own leftover directory), the total delete work
  across N builds is unchanged — only *which specific invocation* pays
  for it shifts by one. A real measured improvement requires the
  actual delete to happen somewhere that *isn't* automatically swept by
  the very next `ociman build` invocation — i.e., genuinely deferred to
  an explicit, separately-invoked `ociman prune` pass (a fourth
  reclamation target alongside blobs/rootfs-cache/unused-images,
  matching the exact same already-accepted "explicit reclaim, not
  automatic" trade-off this project already uses for the other three)
  — not something the next ordinary build opportunistically cleans up
  for free.
* **That, in turn, needs a real liveness check to be safe.** Once
  build-scratch directories are only ever reclaimed by an explicit,
  possibly-concurrent `ociman prune`, prune needs a way to tell "this
  scratch directory belongs to a build that's still actually running"
  from "this one is abandoned garbage, safe to remove" — unlike the
  rootfs cache's own already-solved version of this problem (0109's
  own atomic build-then-`rename`-into-place, where an in-progress
  build is simply invisible at its own final path until it's
  completely done), a build's own scratch directory is *read from and
  written to throughout the entire build*, not atomically published at
  the end, so there's no equivalent "not yet visible" trick available.
  A real fix needs either a lock file held for the build's own full
  duration (checked non-blockingly by `prune`) or an mtime-based age
  threshold (simpler, matches common `tmpreaper`/`systemd-tmpfiles`
  practice, at the cost of a low-probability race against a
  same-machine, unusually-long-running build) — genuinely more design
  surface than a "small, safe, reversible" single-turn slot
  comfortably fits, which is why this note stops at analysis rather
  than attempting it this session.

This is exactly the kind of finding worth writing down rather than
either silently shipping a half-safe version or silently dropping the
idea: the *actual* fix is real, and now more precisely scoped
(rename-to-graveyard, reclaimed only by an explicit `ociman prune`
pass, gated by an age threshold or lock file) — a good, concretely-
actionable candidate for its own dedicated future increment, not this
one.

## What this doesn't cover

* Root (non-rootless) containers, network namespace setup, `docker
  build`'s own equivalent comparison, and heavier multi-`RUN` builds:
  still all unmeasured, the same standing gaps 0018/0105/0113 already
  noted.
* No code changed this session — every number here was already this
  good beforehand (or, for `ociman build`, already this
  well-understood); this note is purely closing a re-verification gap
  and recording a real design finding, not fixing a regression (none
  was found) or shipping the deferred-teardown optimization itself.
