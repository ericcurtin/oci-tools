# Design note 0160: a debug-only single-threaded-at-`fork()` safety net

Status: implemented
Scope: `crates/oci-runtime-core/src/process.rs` (new `debug_assert_
single_threaded`, called from `fork` before the real `libc::fork()`;
new `parse_thread_count` helper plus 5 new unit tests).

## Closing 0159's own "what this doesn't do yet"

0159 found and fixed one real, concrete instance of a caller violating
`fork()`'s own documented single-threaded-caller safety contract
(`cmd_restart` calling a function that spawns a background D-Bus
thread, then forking in the same process). Its own "what this doesn't
do yet" flagged the obvious risk directly: the same class of bug could
recur anywhere a future increment adds a new thread-spawning helper
upstream of any `process::fork`/`launch_detached_and_confirm` call
site, with no structural guard against it. This increment adds exactly
that guard, directly in `fork` itself — the one place every such call
already goes through.

## What it does

`fork()` now calls `debug_assert_single_threaded()` immediately before
the real `libc::fork()`. In a debug build (`cfg!(debug_assertions)`,
compiled out entirely — zero cost — in release), it reads the real
kernel-reported thread count from `/proc/self/status`'s own `Threads:`
line and panics with a clear, actionable message (pointing at 0159's
own real, previously-hit instance) if more than one thread is alive.

## A real performance regression found and fixed *while building this same increment*

The first implementation counted `/proc/self/task`'s own directory
*entries* (`std::fs::read_dir(...).count()`) — one real `readdir`
syscall round trip per thread already alive, rather than a single,
fixed-cost read. Under `cargo test --workspace`'s own full concurrent
load (many test binaries launching containers, i.e. calling `fork()`,
at the same time), this measurably perturbed a real, independently
timing-sensitive test (`ociman_logs.rs`'s own `logs_follow_streams_a_
running_containers_output_and_stops_when_it_exits`) into flaking —
reproduced directly, not assumed: `bash ci/native-ci.sh` failed with
this exact test twice in a row with the `read_dir` implementation, and
passed cleanly four times in a row (plus two clean plain `cargo test
--workspace` runs) after switching to a single `/proc/self/status`
read instead. The unmodified baseline (before this increment's own
check existed at all) never reproduced the failure in three separate
runs, confirming the *original* implementation's own added overhead —
not a pre-existing, unrelated flake — was the actual cause.

This is exactly the kind of thing this whole project's own "must have
measurably equal or better performance" requirement exists to catch:
a debug-only safety net is still code that runs on every single
`fork()` call in a debug build, and `oci-tools`' own test suite forks
once (at least) per container launch across hundreds of tests — a
real, cumulative cost under concurrent load, not a rounding error.

## Excluding this crate's own unit tests, correctly

`debug_assert_single_threaded` is also a no-op under `#[cfg(test)]`
(this crate's own test binary specifically, not the real `ociman`/
`ocirun` binaries integration tests exercise as genuinely single-
threaded-at-`fork()`-time separate processes via `Command::new`):
confirmed directly, the crate's own `process::tests::*`/`overlay::
tests::*` unit tests that call `fork`/`fork_and_wait` directly failed
immediately (20+ threads reported — `cargo test`'s own worker pool)
the first time this check was added without the exclusion.

## Real, automated tests

Five new unit tests for the extracted, pure `parse_thread_count`
helper (split out specifically so this parsing logic has direct
coverage, independent of the `cfg!(test)` no-op that would otherwise
make it untestable from within this crate's own test binary): a
realistic multi-line `/proc/[pid]/status`-shaped string, a single-
thread case, a missing `Threads:` line, unparseable content, and a
real, direct read of this test process's own actual `/proc/self/
status` file (confirming the parsing logic handles real, current-
kernel-format input, not just synthetic strings).

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` (4 clean runs,
specifically re-verifying the flake described above no longer
reproduces) all clean.

## What this doesn't do yet

A release build has no equivalent safety net at all (by design — the
whole point is zero cost there). If the same class of bug were ever
introduced in a way that only manifests in `--release` (unlikely,
since the underlying issue is about real OS thread state, not
optimization level, but not structurally impossible), it would need to
be caught by other means (the same kind of direct measurement 0159's
own investigation used, or a future integration test that specifically
exercises a `stop`-then-`start`-in-the-same-process sequence end to
end).
