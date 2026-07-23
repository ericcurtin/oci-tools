# Design note 0210: `ociman build --timestamp`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Build.timestamp`);
`bin/ociman/src/build.rs` (`cmd_build`, `StageContext.forced_mtime`,
`run_instruction`/`copy_instruction`/`add_instruction`, every
`commit_layer`/`squash_layer`/`record_layer`/`record_empty_history`
call site); `crates/oci-dockerfile/src/commit.rs` (`record_layer`/
`record_empty_history` gain `forced_created`, plus the new
`created_timestamp` helper); `tests/tests/ociman_build.rs`.

## Completing 0199's/0209's deferred flag

0199 investigated `--timestamp` and deferred it whole, pending a
shared "forced mtime" primitive in `oci_layer::export`. 0209 built and
thoroughly verified exactly that primitive on its own. This increment
is the remaining half: the actual `--timestamp <SECONDS>` CLI flag,
wired all the way through every place a build creates a new timestamp
— both the "layer" half (0209's own forced mtimes) and the "image
info" half (`created`/every new history entry's own `created`) — the
same two things real `podman build --timestamp`/`buildah build
--timestamp` both cover, confirmed directly against their own source
in 0209.

## How the value reaches every call site

Rather than adding a new parameter to every one of the ~20 places a
build creates a history entry, `--timestamp`'s parsed value
(`Option<i64>`, `None` if not given — matching real buildah's own
`Flag("timestamp").Changed` check, not just "nonzero") is carried on
`StageContext.forced_mtime`, the same struct that already carries
`dockerignore` for exactly this reason (see its own doc comment,
0130-0133): "every function already threading `stage_ctx` through can
reach it without yet another parameter of its own." `apply_instruction`
(which handles every metadata-only instruction: `ENV`/`LABEL`/
`WORKDIR`/`USER`/`ENTRYPOINT`/`CMD`/`EXPOSE`/`VOLUME`/`STOPSIGNAL`/
`MAINTAINER`/`HEALTHCHECK`/`ONBUILD`/`SHELL`) and `copy_instruction`
both already receive the whole `stage_ctx`, so their own
`record_empty_history`/`commit_layer`/`record_layer` calls read
`stage_ctx.forced_mtime` directly. `run_instruction` and
`add_instruction` didn't receive `stage_ctx` before this (only
`copy_instruction` did) and each gained one new `forced_mtime:
Option<i64>` parameter of their own, passed `stage_ctx.forced_mtime`
from their own call sites in `apply_instruction`. The `--label`/
`--unsetenv`/`--unsetlabel` post-processing in `cmd_build` itself
(after every stage's own instructions have already run) reads
`cmd_build`'s own new `timestamp: Option<i64>` parameter directly
(it has no `stage_ctx` of its own to read from — that's stage-scoped,
this runs once for the whole build after stages are already built).

## `oci_dockerfile::record_layer`/`record_empty_history` gain `forced_created`

A new `created_timestamp(forced_created: Option<i64>) -> String`
helper: the real, current wall-clock time formatted as RFC 3339 if
`None`, else `UNIX_EPOCH + Duration::from_secs(seconds)` — the same
`Option<i64>` convention 0209 already established for
`oci_layer::export`'s own `forced_mtime`, kept consistent across both
halves of this one feature. Both functions now take an extra
`forced_created: Option<i64>` parameter, threaded to every call site
in `build.rs`/`main.rs` (the `ociman commit`/`ociman commit --squash`
call sites in `main.rs` pass `None` — real buildah's own `commit` also
supports `--timestamp`, but that's unscoped here, deliberately, to
keep this increment to `ociman build` alone as originally framed).

## A cache hit is untouched either way

A build cache hit (`reuse_cached_layer`) never calls `record_layer`/
`record_empty_history` at all — it pushes the *cached* `HistoryEntry`
straight through, verbatim, exactly as it was originally recorded
(whatever timestamp *that* earlier build used, forced or not).
Verified by hand: building the same Containerfile a second time with
no `--timestamp` at all still shows the `RUN` step's own history entry
from the *first* build (a cache hit, real timestamp from when
`--timestamp 1700000000` was given), while the `ENV`/`LABEL` entries
(never cached — no rootfs diff to match against) correctly get the
real, current time — exactly matching real `podman build`'s identical
"a cache hit changes nothing about an already-committed layer's own
metadata" behavior.

## Verified by hand

* `ociman build --timestamp 1700000000` on a real Containerfile
  (`RUN`, `ENV`, `LABEL`): every new history entry and the image's own
  top-level `created` read back as exactly `2023-11-14T22:13:20Z`; the
  base image's own already-existing history entry (inherited, not
  produced by this build) is completely untouched.
* The same build with no `--timestamp` at all: every new entry gets
  the real, current time (checked to be within seconds of "now").
* `--no-cache` plus `--timestamp` bypasses any stale cache entry, as
  expected.
* Two separate `--no-cache --timestamp 1700000000` builds of
  byte-identical content, with a real 2-second sleep in between,
  produce the exact same image digest — the concrete reproducibility
  guarantee this whole two-part feature (0209 + 0210) exists to
  deliver.
* A container run from a `--timestamp`-built image still works
  completely normally (files/env variables all correct) — the flag
  changes metadata/mtimes only, never real file content.

## Tests

Three new integration tests in `tests/tests/ociman_build.rs`
(`build_timestamp_sets_created_on_every_new_history_entry_and_the_
top_level_field`, `build_without_timestamp_uses_the_real_current_time`,
`build_timestamp_makes_two_differently_timed_builds_produce_the_same_
digest`) plus two new unit tests in `crates/oci-dockerfile/src/
commit.rs` (`record_layer`/`record_empty_history` each honor
`forced_created`). Every one of the 113 pre-existing `ociman_build.rs`
tests and every pre-existing `oci-dockerfile`/`oci-layer` test
continues to pass unmodified.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 88/88 result blocks — `ociman_build.rs` now
116 (was 113), `oci-dockerfile` now 154 (was 152); no new test
binaries)/`cargo fmt --all --check`/`cargo clippy --workspace
--all-targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo
deny check`/`bash ci/native-ci.sh` all clean. One pre-existing, already
-documented, non-actionable `VerityFs` test-fixture stray mount+loop
device found and cleaned up after the first full test run (routine
habit, not a regression — see `crates/oci-erofs/src/verity.rs`'s own
doc comment). No performance regression (`ociman run --rm`, ~74ms,
within this project's own previously-observed 60-75ms noise band —
this change touches no runtime/launch code at all, only build-time
layer-committing paths).

## What this doesn't do yet

`ociman commit --timestamp` (real buildah's own `commit` supports it
too) remains unscoped. Real `podman build`'s separate
`--source-date-epoch`/`--rewrite-timestamp` pair (a different, related
mechanism for reproducible builds without an explicit `--timestamp`)
is not implemented either.
