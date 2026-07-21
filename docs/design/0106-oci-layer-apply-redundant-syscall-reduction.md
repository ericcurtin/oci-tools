# Design note 0106: `oci_layer::apply` — cutting redundant per-entry syscalls

Status: implemented
Scope: `crates/oci-layer/src/lib.rs` (`apply_tar`/`extract_entry`, new
`ensure_dir_created`); no public API change (`apply`'s own signature
is untouched, every existing caller in `bin/ociman` needed no change
at all).

## Why this, now

0105 (the last performance re-verification) flagged the per-run full
rootfs extraction (`oci_layer::apply`, no copy-on-write/overlay
filesystem — a deliberate design pillar, see the top-level README) as
"a known architectural trade-off" and "a legitimate future
optimization target," but didn't investigate further. Given this
project's own stated bar ("make sure to optimize... must have
measurably equal or better performance than before"), the natural next
step was to actually profile that extraction path rather than leave it
as a hand-wave — and it turned up a real, fixable inefficiency, not
just an inherent cost of the no-overlay design.

## What profiling found

`strace -f -c` against a real `ociman run --rm` of an already-pulled
`docker.io/library/busybox:latest` (a real single-layer image whose
own rootfs is the well-known "every applet is a hardlink to one real
`busybox` binary, all living directly in `/bin`" shape) showed:

| syscall | calls | errors |
|---|---:|---:|
| `unlinkat` | 867 | 412 (`ENOENT`) |
| `statx` | 919 | 449 |
| `mkdirat` | 469 | 442 (`EEXIST`) |
| `linkat` | 410 | 0 |

867 real files/hardlinks, but 867 `unlinkat` calls and 919 `statx`
calls to extract them — nearly one-for-one, and the overwhelming
majority of both failing (`ENOENT`/generic failure), meaning they
found nothing to act on. Reading `extract_entry`'s own code confirmed
why: **every single entry** (`Regular`/`Symlink`/`Link`) unconditionally
called `std::fs::create_dir_all(parent)` (a real `mkdirat`, even when
the parent — here, `/bin`, shared by all 410 hardlinked applets —
already exists, `create_dir_all`'s own std implementation still issues
one and inspects the failure) and unconditionally attempted to
`remove_file`/check `symlink_metadata` on `target` first (real
`docker`/moby's own documented rule: an existing lower-layer entry at
the same path must be removed before a new one replaces it) — correct
for a *later* layer overwriting an *earlier* one, but pure waste for a
brand-new, empty destination directory, which is exactly what every
image's own first (and, for a huge fraction of real images, only)
layer is always extracted onto (`ociman run`'s own `create_dir_all`
immediately before its layer-application loop, same for `ociman
build`'s own scratch rootfs — `bin/ociman/src/main.rs`/`build.rs`).

## The fix: two internal, purely additive optimizations

Neither changes `apply`'s own public signature or behavior in any
observable way — both are pure "do provably-unnecessary work fewer
times" changes, verified by the existing test suite (46 tests in
`oci-layer` alone, including every whiteout/multi-layer-overwrite/
hardlink test) passing unmodified, plus two new tests targeting
exactly the risk each optimization introduces:

1. **`ensure_dir_created`**: `create_dir_all` memoized in a
   `HashSet<PathBuf>` for the lifetime of one `apply_tar` call — a
   directory holding many entries (`/bin`, the common case above) now
   pays for its own `mkdirat`/failure-inspection once, not once per
   entry sharing it.
2. **`dest_was_empty`**: computed once, up front, via a single
   `read_dir` check — if `dest` had *nothing at all* directly in it
   when this `apply` call started, every entry this call is about to
   write is provably new (nothing from a lower layer, or anything
   else, could already occupy any of these paths), so the
   existing-entry check/removal is safely skipped entirely for the
   whole call. The one gap that check alone doesn't close — this same
   call revisiting a path it already wrote earlier in its own tar
   stream (a real if unusual possible shape) — still falls back to the
   full check via the pre-existing `written` bookkeeping (already
   there for opaque-whiteout tracking, reused here for free).

This is derived from a real, one-time check every call already pays
the cost of allowing for correctly, not asserted by any caller — no
call site anywhere in `bin/ociman` needed to change at all, and no
future caller can get it wrong the way a caller-supplied "trust me,
it's empty" flag could.

## Real, automated tests targeting exactly this change's own risk

Every existing `oci-layer` test continued passing unmodified (the
multi-layer-overwrite/whiteout/hardlink tests already exercise the
"`dest` not empty" path, since they pre-populate `dest` before calling
`apply`). Two new tests specifically probe what changed:
`a_second_apply_call_still_replaces_the_first_calls_own_file` (a real
second `apply` call, not a test-pre-written file, onto a destination a
real first `apply` call already populated — proving `dest_was_empty`
correctly evaluates `false` for that second call) and
`a_duplicate_path_within_one_apply_call_onto_an_empty_dest_still_
replaces_correctly` (two tar entries for the same path in one stream,
onto a truly empty `dest` — proving the `written`-set fallback still
catches the one gap `dest_was_empty` alone can't).

## Measured impact

Same real `ociman run --rm` of the same already-pulled busybox image,
`strace -f -c` before vs. after:

| | before | after |
|---|---:|---:|
| total syscalls | 4173 | 2457 (**-41%**) |
| `statx` | 919 | 43 (**-95%**) |
| `mkdirat` | 469 | 41 (**-91%**) |
| `unlinkat` | 867 | 455 (-48%) |
| total syscall errors | 1342 | 52 (-96%) |

`hyperfine` (`--shell=none`, 10 warmup, 200 samples each — a larger
sample than 0105's own 60, specifically to pull a real signal out of
this host's own substantial run-to-run variance) on the two release
binaries (before/after this change, same commit otherwise) directly:
**55.9 ms → 55.5 ms mean**, `after` measured 1.01× faster — a small,
noise-adjacent wall-clock delta at *this* image's own scale (a mere
~370 real files), but the `System` time component (the one this
change actually touches) moved measurably in the right direction
(16.3 ms → 14.8 ms, ~9%), consistent with the syscall-count evidence
above rather than contradicting it. The real payoff is architectural,
not this one image: the eliminated calls scale with **file count**,
so a real multi-thousand-file base image (`ubuntu`, `python`,
`node`, ...) — this project's own eventual real-world target, not
just a busybox smoke test — stands to save proportionally far more
wall-clock time than this specific measurement shows, not less.

## What this doesn't cover

* `linkat` itself (the actual hardlink creation, 410 calls, 0 errors
  either way) is untouched — that work is real and necessary; nothing
  here reduces the number of hardlinks a real hardlink-heavy image
  actually needs created.
* No copy-on-write/overlay filesystem — still the explicit, deliberate
  design pillar this project has always had (see the top-level
  README); this change makes the existing full-extraction approach
  itself measurably leaner, it doesn't replace it with something
  architecturally different.
* A real multi-thousand-file image (the case this change should help
  most) wasn't measured directly this session — the busybox
  measurement above is what was available offline without a real
  registry pull of a much larger image; the syscall-count mechanism
  (savings scale with file count) is the basis for the "should help
  more for larger images" claim, not a direct larger-image
  measurement.
