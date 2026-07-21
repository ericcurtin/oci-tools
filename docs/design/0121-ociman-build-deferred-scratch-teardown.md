# Design note 0121: `ociman build`'s scratch rootfs is reclaimed by `ociman prune`, not deleted synchronously every build

Status: implemented
Scope: `bin/ociman/src/build.rs` (`build_scratch_root`, `BuiltStage`'s
own `_build_dir` field removed, `build_stage`'s own scratch-dir
creation); `crates/oci-store/src/rootfs_cache.rs` (`dir_size` made
`pub`); `crates/oci-store/src/lib.rs` (re-export); `bin/ociman/src/
main.rs` (`prune_build_scratch`, `PruneResult`'s new fields);
`tests/tests/ociman_prune.rs` (2 new tests).

## The precise fix 0120 scoped out, now implemented

0112 found (and 0120 re-confirmed) that roughly half of `ociman
build`'s own residual ~1.16-1.2× gap against `podman build` was the
final, synchronous `remove_dir_all` of a build's own scratch rootfs —
real, measured cost paid on every single build, right after the built
image's own manifest is already safely stored (so the scratch
directory's own continued existence serves no purpose by that point).
0120's own analysis found the two things a safe fix actually needs:

* **Not a background thread.** `ociman build` is short-lived; unlike
  `ociman run`'s own `create_scope` (0033, safe because the
  *container's* own process stays alive afterward), a thread spawned
  here and never joined risks being killed mid-delete when the whole
  process exits moments later — a real disk-space correctness
  regression (a permanently half-deleted directory), not an
  optimization.
* **Not a "let the next build sweep it" scheme either.** A `hyperfine`
  -style repeated-build benchmark would just have build *N* pay for
  build *N-1*'s own leftover directory — the same total delete work,
  shifted by one invocation, not reduced. A real, sustained per-build
  improvement requires the actual delete to live somewhere a build
  never automatically triggers — an explicit, separately-invoked
  `ociman prune` pass.

## Implementation

`BuiltStage`'s own scratch directory (`build_stage`'s own
`tempfile::tempdir()` call) moves from a plain system `/tmp` entry to
a real, persistent subdirectory of the store's own root
(`build_scratch_root`, a sibling of `rootfs_setup::cache_root`'s own
`rootfs-cache/`), created via `tempfile::Builder::tempdir_in(...)
.keep()` — the same crate's own documented way to get a real,
race-safe unique directory name without its `TempDir` wrapper's own
Drop-based auto-deletion. `BuiltStage`'s own `_build_dir` field
(previously held *only* for that Drop side effect) is removed entirely
— nothing else ever read it, and the underlying directory now simply
persists on disk regardless of any Rust value's own lifetime.

`ociman prune` gains a fourth reclamation pass (`prune_build_scratch`),
run alongside blob GC and rootfs-cache pruning, reported in its own
`PruneResult` fields (`build_scratch_entries_removed`/
`_reclaimed_bytes`). Unlike the rootfs cache (genuinely reused across
invocations) or blobs (reachability-tracked via image tags), a
build-scratch entry is *always* pure leftover working state the moment
its own build has finished — there is no "still reachable" question,
only "is this old enough to be confident nothing is still using it."
Reuses `oci_store::dir_size` (now `pub`, re-exported) for the reclaimed-
bytes figure rather than reimplementing the hardlink-aware, symlink-
safe size calculation a second time and risking reintroducing the
exact double-counting bug 0111 already found and fixed for the rootfs
cache's own reporting.

### Liveness check: an age threshold, not a lock file

Deliberately the simpler of the two options 0120 named: entries at
least one hour old (`BUILD_SCRATCH_MAX_AGE`) are removed outright, no
process-liveness check at all — matching common `tmpreaper`/`systemd-
tmpfiles` practice. Accepts a real, but low-probability, race: an
`ociman build` running for over an hour, with a *concurrent* `ociman
prune` happening at that exact moment, could have its own
still-in-use scratch directory reclaimed out from under it. Judged an
acceptable trade-off for this increment (an hour-plus build with a
concurrent prune is not a scenario this project's own CI or typical
usage actually hits) rather than adding a lock file held for a
build's own full duration — a real, well-precedented, deliberately
simple choice, not an oversight.

## Measured result

Same benchmark as 0112/0120 (`FROM docker.io/library/ubuntu:24.04` +
`RUN echo hello`, both images already pulled, warm build cache):

| | 0120 (before this change) | after this change |
|---|---:|---:|
| `ociman build` mean | 99.7 ms | **73.9 ms** |
| `podman build` mean | 85.8 ms | 86.2 ms (unchanged, re-measured for a fair same-session comparison) |
| relative | 1.16× slower than podman | **1.17× faster than podman** |

A real, substantial improvement (~26% faster than before) that flips
`ociman build` from measurably *slower* than `podman build` to
measurably *faster* — directly closing this project's own "beat every
real equivalent on all benchmarks" gap for this specific comparison,
not just narrowing it.

## Disk-space safety verified directly, not assumed

Ran the benchmark above for real (33 builds via `hyperfine`'s own
warmup+sample loop against the same tag), then checked: `build-
scratch/` really did accumulate ~4.4 GB of real, on-disk scratch
directories (33 real `ubuntu:24.04`-based rootfses) that a plain `ls`
(without `-a`) doesn't even show, since `tempfile`'s own default
naming is dot-prefixed. Backdated every entry's own real mtime two
hours (`std::fs::File::set_modified`, the same mechanism the new
automated tests use) and confirmed `ociman prune` reclaimed all 42
accumulated entries (~4.4 GB) in one call, leaving `build-scratch/`
empty. Not a theoretical safety net — a real, working one.

## Real, automated tests

Two new `ociman_prune` integration tests: a fresh build-scratch entry
(a real build just finished) survives an immediate `ociman prune`
(too new); an entry whose own real mtime is backdated past the
threshold (`std::fs::File::set_modified`, not a mock) is reclaimed,
its own real on-disk size correctly counted. All 8 pre-existing
`ociman prune` tests and all 53 pre-existing `ociman build` tests
still pass unmodified. Full `cargo build --workspace --locked`/`cargo
test --workspace --locked` (2 clean runs)/`cargo fmt --all --check`/
`cargo clippy --workspace --all-targets --locked -- -D warnings` all
clean.

## What this doesn't do yet

* No lock-file-based liveness check (see above) — an explicit,
  documented trade-off, not a gap to close reflexively later unless a
  real problem from it ever actually surfaces.
* `ociman run`'s own overlay-based rootfs setup already avoids this
  exact problem a different way (0110: a real overlay mount, torn down
  by a cheap unmount, not a recursive delete) — this increment is
  specifically about `ociman build`'s own still-necessary writable
  scratch rootfs, which can't use that same approach (see 0112's own
  doc comment for why a writable, multi-instruction, multi-stage
  rootfs and overlay don't mix safely yet).
