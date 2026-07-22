# Design note 0158: `--rm` for `ociman create`, and a real `restart` bug found and fixed along the way

Status: implemented
Scope: `bin/ociman/src/main.rs` (new `ANNOTATION_AUTO_REMOVE`;
`cmd_run`/`cmd_create` persist it; `cmd_start` reads it instead of a
hardcoded `false`; `cmd_restart` clears/restores it around its own
internal `stop_container` call; `run_and_finalize`'s own auto-remove
branch re-checks persisted state fresh rather than trusting a value
captured once at launch time); `tests/tests/ociman_create.rs` (1 new
test), `tests/tests/ociman_start.rs` (1 new test).

## Closing 0157's own deferred gap

0157's own "what this doesn't do yet" named this directly: `ociman
create --rm` (a real, valid `podman create --rm` combination — auto-
remove once the container eventually runs, via a later `ociman start`,
and exits) had no persisted record anywhere of what a container's own
original `--rm` even was, since `cmd_start`/`cmd_restart` always passed
a hardcoded `rm: false` to `launch_detached_and_confirm`/`run_and_
finalize`.

Fixed with a new annotation, [`ANNOTATION_AUTO_REMOVE`]: `cmd_run`
persists it whenever `--rm` is given (alongside its own existing,
immediate use of the `rm: bool` it was already given directly);
`cmd_create` persists it the same way, with no immediate use at all
(there is nothing to remove yet — the container hasn't run); `cmd_
start` now reads it back (`state.annotations.contains_key(...)`)
instead of hardcoding `false`, correctly reviving `--rm` for a
container being started for the first time via `create`, or re-started
via a later `ociman start`/`ociman restart`.

## A real, deeper, pre-existing bug found in the process — not introduced by this change

While testing the interaction between `--rm` and `ociman restart`
(0154), a genuinely reproducible bug turned up, confirmed to already
exist on the unmodified codebase before any of this increment's own
changes (verified directly: `git stash`, rebuilt, reproduced the exact
same failure, `git stash pop` to restore): `ociman run -d --rm image
sleep 30`, followed by `ociman restart <id>` while it's still running,
made the whole container vanish — `restart`'s own internal `stop`
(`stop_container`, shared with `cmd_stop`) made the process exit, and
the *original* keeper process (still the one from the very first
launch, since `stop_container` never itself launches anything new)
noticed and auto-removed the entire container before `cmd_start`'s own
half of `restart` ever got a chance to relaunch it — the exact same
architecture that made this project's own newly-added `--rm`-for-
`create` persistence necessary in the first place also, unfixed, would
have broken `restart` immediately the moment anyone combined the two.

Checked directly against real podman for the correct behavior first,
not assumed: `podman run -d --rm ...` followed by `podman restart`
leaves the container running again (survives), while a real, standalone
`podman stop` on the same container does still remove it. Real podman
achieves this because its own `restartWithTimeout`
(`~/git/podman/libpod/container_internal.go`) calls a low-level `c.stop`
that never goes through its own auto-removal path at all — a
distinction this project's own single, shared `stop_container` doesn't
have (`cmd_stop` needs exactly the opposite behavior for the same call).

### The fix: a transient, self-restoring suppression, not a new stop code path

Rather than fork `stop_container` into "real" and "restart-internal"
variants, `run_and_finalize`'s own auto-remove decision (`if rm { ...
}`) now re-checks [`ANNOTATION_AUTO_REMOVE`] from a **fresh** read of
persisted state at the exact moment the container's own process has
just exited, rather than trusting the `rm: bool` parameter it was
called with once, back at launch time. `cmd_restart` uses this: it
clears the annotation (persisting the removal immediately) *before*
its own internal `stop_container` call, then restores it again
(persisting that too) immediately *after* `stop_container` returns but
*before* calling `cmd_start` — so the brand new run, however fast it
exits, always sees the annotation back in place, ready for a real,
future stop to correctly auto-remove again. The freshly-reloaded state
(not the stale, launch-time-captured `state` variable, whose own
in-memory `annotations` snapshot would still include a since-cleared
marker if blindly re-persisted) is what actually gets written for the
`Stopped` case, so `cmd_restart`'s own suppression can never be
silently undone by the tail logic's own write. The extra disk read
only happens inside the already-existing `if rm` branch, so a
container that was never launched with `--rm` at all pays nothing
extra.

Verified with a real reproduction test (`restart_does_not_auto_remove_
a_rm_container_but_a_later_real_stop_still_does`, `tests/tests/
ociman_start.rs`): restart on a running `--rm` container leaves it
running afterward (not removed, not stuck erroring), and a subsequent
real, standalone stop on the same container still removes it correctly.

## A second, separate real finding: systemd's own transient-scope-name reuse costs several real seconds — not fixed here

While root-causing the above, a second, independent, real, measured
issue turned up (not a hang, an actual delay): reusing the exact same
systemd scope name (`ociman-<id>.scope`, derived from the container's
own id, unchanged across every launch of the same container) for a
restarted container's *second* launch makes that second launch's own
keeper take several real seconds (~2-3s observed, reproducibly) before
its own final `Stopped`/removal write actually lands — confirmed
directly: `systemctl --user status` on the scope shows it already fully
unloaded immediately after the restart's own internal stop, yet the
new keeper's own tail-finalize write is measurably delayed regardless,
consistent with systemd's own internal job-queue/garbage-collection
timing needing genuine, non-instant real time to fully settle before a
transient unit of the identical name can be recreated and its own new
job's completion observed.

This is **not fixed in this increment** — it's a real, separate,
deeper issue (scope-name reuse across restarts of the same container),
distinct from the `--rm`/`restart` interaction bug above, and
deliberately out of scope here (this project's own established
"narrow first increment" convention): the likely correct fix is making
each *launch* (not just each container) get a unique scope name (e.g.
appending a monotonic per-launch counter), sidestepping systemd's own
reuse-timing behavior entirely by construction, rather than trying to
wait it out — a bigger, cross-cutting change (every place that derives
a scope name from a container id alone) better scoped as its own
future increment. The new `restart_does_not_auto_remove_a_rm_
container_but_a_later_real_stop_still_does` test accounts for this
real delay with a generous (20s) polling window rather than asserting
instantaneously, so it isn't itself flaky because of it — but `ociman
restart`'s own real wall-clock latency for a container's *next* stop
after being restarted is a real, currently-unaddressed performance gap
worth its own dedicated future increment.

## Real, automated tests

`create_rm_auto_removes_the_container_once_it_finally_exits` (`tests/
tests/ociman_create.rs`): a container created with `--rm`, started for
the first time, and confirmed genuinely removed (`ociman ps -a`) once
its own command exits. `restart_does_not_auto_remove_a_rm_container_
but_a_later_real_stop_still_does` (`tests/tests/ociman_start.rs`): the
real regression test for the bug found above, both halves (survives a
restart, still removed by a later real stop).

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, plus repeated standalone runs of the affected
test files)/`cargo fmt --all --check`/`cargo clippy --workspace
--all-targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo
deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* **Unique-per-launch systemd scope names** — see above; a real,
  separate, deeper performance issue, deliberately deferred to its own
  future increment.
* Everything 0157 already named beyond `--rm` itself remains
  unchanged: `-a`/`--attach` on a later `start`, still not implemented.
