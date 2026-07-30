# Design note 0316: `ociman restart --cidfile` and multi-target ids

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_start.rs`.

## Closing `0315`'s own "still ahead" — with a correction

`0315` named `ociman restart --cidfile`/`--ignore` as real, separately-
scoped candidates. Reading `~/git/podman/cmd/podman/containers/
restart.go` directly before implementing anything (this project's own
established discipline, not assumed from the earlier note's own
guess) turned up a real correction: real `podman restart` has **no
`--ignore` flag at all** (`restartFlags` registers `--all`/
`--running`/`--cidfile`/`--filter`/`--time`, nothing else) — unlike
`rm`/`stop`, which both genuinely have one. This note closes
`--cidfile` only; there is no `--ignore` left to implement for
`restart` at all.

## A deeper, necessary correction found along the way

Real `podman restart`'s own `Use` string is `restart [options]
CONTAINER [CONTAINER...]` — it has always accepted **multiple**
explicit targets, independently of `--cidfile`/`--all`. This project's
own `ociman restart`, though, was still `id: Option<String>` (a single
target, extended with `--all` in `0315`) — narrower than real podman
even before `--cidfile` enters the picture at all. Since `--cidfile`'s
entire purpose is *merging more ids into a list*, implementing it at
all requires the same underlying `ids: Vec<String>` shape `--cidfile`
itself needs — so this note also closes that real, previously-
unnoticed gap as necessary groundwork, not scope creep: `Command::
Restart`'s `id: Option<String>` became `ids: Vec<String>`.

## Real, checked-directly semantics

Read `~/git/podman/pkg/domain/infra/abi/containers.go`'s own
`getContainers` `default` case directly (the one used when no
`--all`/`--running`/`--filter` is given, i.e. a plain multi-id call):
every given name is resolved via `LookupContainer` first, and *any*
resolution failure aborts the *whole* function immediately (`return
nil, err`), before ever attempting to restart even the ones that did
resolve — the same "resolve everything first, abort the whole call on
the first unresolvable one" convention `ociman rm`'s own multi-target
loop already established, not the tolerant "attempt every one anyway"
policy `--all` uses. Once every given id has resolved, though, the
actual restart loop (`ContainerRestart`) attempts every one of them
regardless of an earlier one's own restart failure, accumulating each
one's own error to report at the end — the exact same two-phase shape
`kill`/`stop --all` already established for a different reason
(0312/0313).

`--cidfile` itself: the file's own first line only (`strings.Cut`),
merged into the same target list an explicit `ID`/`--name` argument
already builds, mutually exclusive with `--all` — identical to `rm`/
`stop --cidfile` (0310), just without the `--ignore` tolerance neither
of those two flags has an equivalent of here.

## Implementation

`Command::Restart`: `id: Option<String>` → `ids: Vec<String>`, plus a
new, repeatable `cidfile: Vec<PathBuf>`. `cmd_restart`'s own new shape:

- Cidfile ids are read and appended to `ids` first (hard error on an
  unreadable file — no `--ignore` to tolerate it with).
- `--all`: unchanged from `0315`, now routed through a new shared
  `restart_many` helper (see below).
- Exactly one target, no `--all`: the original, simplest single-target
  path (`restart_one(id, time_secs, false)`), completely unchanged —
  the overwhelmingly common case pays zero extra cost for any of this.
- Two or more explicit targets (a real, new capability — previously
  impossible at all): every one is resolved via `resolve_container_id`
  first, aborting the whole call immediately if any fails, matching
  real podman's own two-phase behavior above exactly. Only once every
  one has resolved does `restart_many` actually attempt each one.

`restart_many(ids, time_secs)`: extracted from `0315`'s own `--all`
loop verbatim (attempt every one regardless of an earlier failure,
report the first real error at the end) — now shared by both `--all`
and an explicit multi-id/`--cidfile` call, both of which need the
identical deferred-scope-reset handling `0315` already built (more
than one real restart in the same process means more than one
`fork()`, and `restart_one`'s own old-scope cleanup thread must never
still be alive for the next one — unchanged reasoning, just reused by
a second caller now).

## Verified

Manual, end-to-end: `--cidfile` (repeatable) restarts every id it
names; `--cidfile` combined with `--all` is a clear error; an
unreadable cidfile is a hard error (no `--ignore` to fall back on);
multiple explicit ids both restart successfully when all resolve, but
one bad id among several aborts the whole call, leaving every real
container completely untouched (verified via each one's own unchanged
pid) rather than partially restarting the ones that did resolve.

Integration (`tests/tests/ociman_start.rs`, 7 new tests, 19 total, 12
pre-existing): `--cidfile` reads the id and ignores trailing content
(reusing `ociman_ps.rs`'s own established plain-name-as-cidfile-
content technique); `--cidfile` + `--all` is a clear error; an
unreadable cidfile is a hard error; multiple explicit ids resolve all
before restarting any, then both genuinely restart once corrected to
only real ids. One test-authoring correction along the way: this
project's own `restart` (like `kill`/`stop`) always prints the
*resolved* id, never the raw name/cidfile-sourced input given (a real,
pre-existing, unrelated-to-this-note behavior, `restart`'s very first
test written against a name rather than an already-resolved id caught
it) — matches this project's own established convention throughout,
just never previously exercised by a name-based restart test.

Regression: all 19 `ociman_start.rs` tests pass (12 pre-existing + 7
new); `ociman_stop.rs` (13) and `ociman_kill.rs` (7) both still pass
unchanged. Full `cargo test --workspace --locked`: 112 test result
blocks, 0 failures.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman restart` is a one-shot, offline command, not part
of any hot-path benchmark tracked in `docs/benchmarks.md`. The
overwhelmingly common single-target case is provably unchanged in cost
(still the exact original code path, no new branching or allocation
before it). No re-benchmark needed.

## Still ahead

`ociman kill`/`stop` remain single-target-only (plus `--all`) —
neither ever got the same `ids: Vec<String>`/multi-target widening
this note gave `restart`, a real, separately-scoped gap of its own
(both real podman `kill`/`stop` also accept `CONTAINER
[CONTAINER...]`). The paused-container `SIGKILL`-delivery gap `0312`
first found remains its own real, separately-scoped, deliberately
deferred future candidate too.
