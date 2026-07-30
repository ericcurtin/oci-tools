# Design note 0318: `ociman stop --cidfile`/`--ignore` and multi-target ids

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_stop.rs`,
`tests/tests/ociman_ps.rs`.

## Closing `ociman stop`'s own remaining real gaps

`0317` named `ociman stop` as still single-target-only (plus `--all`),
with no real `--cidfile`/`--ignore` at all — real podman's `stop` has
both, plus `CONTAINER [CONTAINER...]`. This note closes all three
together for `stop`, mirroring the exact combination `ociman rm`
already established (0310/0311) and the multi-target shape `restart`/
`kill` already got (0316/0317).

## Real, checked-directly semantics

Read `~/git/podman/cmd/podman/containers/stop.go` directly:
`--all`/`-a` (0313, unchanged), `--ignore`/`-i` ("Ignore errors when a
specified container is missing"), `--cidfile` (repeatable, first line
only, merged into the same target list an explicit id already
builds). The multi-target two-phase behavior (resolve everything
first, abort the whole call on an unresolvable one; once resolved,
attempt every target regardless of an earlier failure) is the same
`getContainers` `default`-case shape `0316`/`0317` already
established for `restart`/`kill`.

## A real correction to `0311`'s own original claim

Implementing `stop --ignore --cidfile` required reading real podman's
own `stop.go` cidfile-reading loop closely: `if stopOptions.Ignore &&
errors.Is(err, os.ErrNotExist) { continue }` — **`--ignore` does
tolerate a missing cidfile itself**, not just an unresolvable name.
Checking `rm.go`'s own identical loop confirmed the exact same
tolerance exists there too. This directly contradicts `0311`'s own
claim ("`--ignore` does not tolerate `--cidfile`'s own separate 'the
file itself can't be read' case... kept even now that `--ignore`
exists") — a real, previously-unnoticed, incorrect assumption in this
project's own `ociman rm --ignore`, confirmed live: `ociman rm
--ignore --cidfile <missing>` hard-errored before this note, when real
podman succeeds silently in that exact case. Fixed alongside `stop`'s
own new implementation, since it's the same bug in the same kind of
loop, directly informing what `stop` needed to get right the first
time rather than repeat the same mistake.

A second, related subtlety, checked directly against real podman's
own CLI-level `validate.CheckAllLatestAndIDFile`: the "you must
provide at least one name or id" validation is judged by whether
`--cidfile` was ever *given* as a flag at all, never by whether it
later actually resolves to anything. So `stop --ignore --cidfile
<missing>` with nothing else given is a **silent, successful no-op**
in real podman (the cidfile read fails, `--ignore` tolerates it, the
target list ends up empty, and an empty target list with no `--all`
is not itself an error) — not the "no container ID/name given" error
a naive "is the final merged list empty" check would produce. Both
`stop` and `rm` now capture whether *anything* (an id or a `--cidfile`
flag) was given *before* the cidfile-merge step specifically to get
this right.

## Implementation

`Command::Stop`: `id: Option<String>` → `ids: Vec<String>`, plus new
`cidfile: Vec<PathBuf>` and `ignore: bool` fields, matching `Command::
Rm`'s own established shape. `cmd_stop`'s new structure: cidfile ids
are read and merged first (tolerating `NotFound` under `--ignore`);
the "nothing given at all" check happens against the pre-merge id/
cidfile counts, not the post-merge list; `--all` is unchanged from
`0313`; otherwise every given id is resolved first (dropped under
`--ignore` if unresolvable, aborting the whole call otherwise), and —
only once resolution is fully done — every resolved target is
genuinely attempted regardless of an earlier one's own stop failure.
`cmd_rm` got the identical `NotFound`-under-`--ignore` cidfile
tolerance and the identical pre-merge "was anything given" capture.

## Verified

Manual, end-to-end: `stop id1 id2` stops both; `stop id1 nonexistent`
aborts the whole call (id1 completely untouched); `stop --ignore id1
nonexistent` tolerates the bad one and still stops `id1`; `stop
--cidfile` reads and stops the named container; `stop --all --cidfile`
is a clear error; `stop --cidfile <missing>` is a hard error without
`--ignore`, a silent success with it (verified: exit 0, empty stdout);
`stop` / `stop --ignore` with truly nothing given at all are both
still clear errors. `rm --ignore --cidfile <missing>` (previously a
hard error) now also succeeds silently, matching the identical fix.

Integration: `tests/tests/ociman_stop.rs` gained 6 new tests (19
total, 13 pre-existing) covering multi-target stop, abort-vs-`--ignore`
for an unresolvable id among several, `--cidfile` reading, `--all`+
`--cidfile` conflict, cidfile-missing hard-error-vs-`--ignore`-no-op,
and "nothing given" always erroring. `tests/tests/ociman_ps.rs`: one
existing test (`rm_ignore_still_errors_on_an_unreadable_cidfile`,
encoding the incorrect old claim) renamed to `rm_ignore_tolerates_an_
unreadable_cidfile` and rewritten to assert the corrected, real-
podman-matching behavior; its neighboring test's own doc comment
corrected to stop describing the old, wrong claim.

Regression: all 19 `ociman_stop.rs` tests and all 32 `ociman_ps.rs`
tests pass. Full `cargo test --workspace --locked`: 112 test result
blocks, 0 failures.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman stop`/`rm` are one-shot, offline commands, not
part of any hot-path benchmark tracked in `docs/benchmarks.md`. No
re-benchmark needed.

## Still ahead

With this note, `ociman rm`/`stop` both have the full real podman
`--all`/`--cidfile`/`--ignore`/multi-target combination; `kill`/
`restart` have multi-target (0316/0317) but no `--ignore` at all,
matching real podman's own identical absence for both. The paused-
container `SIGKILL`-delivery gap `0312` first found remains its own
real, separately-scoped, deliberately deferred future candidate.
