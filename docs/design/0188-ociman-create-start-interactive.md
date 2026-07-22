# Design note 0188: `ociman create -i`, real stdin across a restart, and
a real detached-launch race fix found along the way

Status: implemented
Scope: `bin/ociman/src/main.rs` (`ANNOTATION_INTERACTIVE`,
`Command::Create`'s new `--interactive` flag, `cmd_run`/`cmd_create`
persisting it, `cmd_start` reading it back, `launch_detached_and_
confirm`'s keeper closure — both its conditional stdin preservation and
its real exit code, `wait_for_detached_container_to_start`'s `NotFound`
handling); `tests/tests/ociman_start.rs`, `tests/tests/ociman_run.rs`.

## Real semantics, checked directly first

0187 gave `ociman run` a working `-i`/`--interactive`, but left `ociman
start`'s own "no `-i` of its own, deferred" gap in place. Before
designing a fix, checked directly what real podman actually does:

```
podman create --name c busybox sh -c '...'          # no -i
podman start -i -a c                                 # never forwards stdin
podman rm -f c
podman create -i --name c busybox sh -c '...'        # with -i
podman start -a c                                    # forwards stdin, no -i given here at all
```

This confirms real docker/podman decide "will this container's stdin
ever be forwarded at all" once, at **creation** time, never re-decided
by a later `start`'s own flags — `start -i` itself has no observable
effect either way in every combination tested. So the correct design
is not "add `-i` to `ociman start`" (there is nothing for it to do),
but "persist the setting from `create`/`run`, and have `start` read it
back" — exactly the same mechanism `--rm`/`ANNOTATION_AUTO_REMOVE`
(0158) already established.

## The fix

* New `ANNOTATION_INTERACTIVE` constant, persisted by `cmd_run`
  whenever `-i` is given (regardless of `--detach`) and by `cmd_create`
  whenever its own new `--interactive` flag is given.
* `cmd_start` reads it back (`state.annotations.contains_key(...)`)
  and passes the result as `run_and_finalize`'s `interactive` parameter
  — no CLI flag of `start`'s own needed at all.

## A real, previously-hit bug found while first verifying this end to
end

The very first manual test (`create -i` then `start --attach`) failed
— real stdin was *not* forwarded, even though the annotation was
correctly persisted (checked the raw `state.json` directly to rule out
a persistence bug). Root cause: `launch_detached_and_confirm`'s own
keeper process *unconditionally* redirected its own stdin to
`/dev/null` (via `setsid` + `dup2_stdin`) immediately upon detaching,
regardless of `interactive` — since the keeper is the *direct* parent
of the container's own eventual process, whatever its own fd 0 already
is at that point is the only thing 0187's own `close_stdin: false`
path could ever inherit, no matter what it's told. Fixed by skipping
just that one `dup2` when `interactive` is set (stdout/stderr are still
always silenced either way, matching real `docker run -d`'s own
unconditional "no live output" convention) — the real, conmon-
analogous mechanism this project's own architecture needs: the keeper
holding real stdin open across the detach, for a `start --attach` on
the very next launch to use. Re-verified against three real scenarios
by hand before writing automated tests (`create -i` + `start
--attach`; `create` with no `-i` + `start --attach`; `run -i`
foreground once, then a later `start --attach` with no `-i` anywhere)
— all three now match real podman exactly, including the "combined
log across restarts" catch-up behavior (confirmed real podman does the
same thing: `start -a` on a restarted container shows the *first*
run's own output again too, not just the new one).

## A second, real, previously-hit bug found while performance-checking
the change (0189)

Before finishing, ran a routine performance sanity check on the
detached-launch hot path this change touches: `ociman run -d --rm
busybox /bin/true`, hammered in a tight, zero-delay shell loop.
Roughly 30-50% of iterations failed with `container ... failed to
start (setup failed before it ever reported a real pid)` — reproduced
identically on the *unmodified* `HEAD` too (confirmed by `git stash`),
so this predates this increment entirely; a real `podman run -d --rm
busybox /bin/true`, hammered the exact same way, never fails at all.

Root cause: `wait_for_detached_container_to_start` treated any
`NotFound` from the state store as an unconditional hard failure. But
a `--rm` container whose own command exits almost instantly can run to
completion and have its *entire* record removed (via `--rm`'s own
auto-removal in `run_and_finalize`) so fast that the *caller's* very
first poll — with no delay at all before it — already finds nothing,
indistinguishable from a genuine setup failure (which also removes the
record, via the same function's `Err` branch). Confirmed this
specifically needs a fast, optimized binary to reproduce reliably (a
`--release` build; a `--dev`/debug build's own extra startup overhead
happens to give the caller enough of a head start that the race
essentially never manifests) — relevant since this project's own
benchmarks, and real production use, always mean a release build.

Fixed by making the keeper's own real exit code the deciding signal,
rather than discarding it: the keeper closure now exits with
`oci_runtime_core::launch::SETUP_FAILURE_EXIT_CODE` (125, the same
existing "the tool itself failed" convention) if `run_and_finalize`
itself returned `Err`, `0` otherwise — previously always hardcoded to
`0` regardless (`let _ = run_and_finalize(...)`). `wait_for_detached_
container_to_start`'s own `NotFound` branch now `waitpid`s the keeper
(safe: nothing else ever reaps this specific child, and the keeper
always eventually exits) and only bails if its real exit code is
nonzero.

## Tests

Three new integration tests in `tests/tests/ociman_start.rs`
(`start_attach_forwards_stdin_for_a_container_created_with_
interactive`, `start_attach_never_forwards_stdin_for_a_container_
created_without_interactive`, `restarting_a_container_originally_run_
interactive_still_forwards_stdin`), all using a real piped stdin via
`Command::spawn` (not `.output()`, which would mask the very thing
being tested). One new integration test in `tests/tests/
ociman_run.rs` (`run_detached_rm_with_an_instantly_exiting_command_
never_races_its_own_startup_check`, 40 repeated iterations) for the
0189 race fix — verified this test both fails reliably against the
pre-fix code and passes reliably against the fix, in each case
specifically using `cargo test --release` (the profile the race
actually needs to manifest in; a plain debug-profile `cargo test`
alone does not reproduce it reliably either way, noted above). Full
`cargo build --workspace --locked`/`cargo test --workspace --locked`
(2 clean runs, 83/83 result blocks)/`cargo fmt --all --check`/`cargo
clippy --workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## Performance re-verification

Re-benchmarked directly after the fix: `ociman run --rm` ~68ms
(previously ~66-69ms, no change); `ociman run -d --rm` ~48.7ms vs a
real `podman run -d --rm`'s own ~170.4ms on the same host/image (~3.5×
faster) — and, unlike before this fix, zero failures across 36
repeated `hyperfine` runs of the exact scenario that used to fail
30-50% of the time.

## What this doesn't do yet

Real terminal/pty allocation (`-t`/`--tty`) remains a wholly separate,
unstarted gap; a `-d -i` container's own real "leave stdin open for a
later `attach`" behavior doesn't apply here either — this project has
no `attach`-to-an-already-running-container command at all yet, only
`start --attach`, which only ever applies to an already-`Stopped`/
`Created` container.
