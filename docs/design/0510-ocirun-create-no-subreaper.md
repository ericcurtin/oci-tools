# Design note 0510: `ocirun create --no-subreaper`

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_lifecycle.rs`.

## What this closes

`ocirun run --no-subreaper` has existed since early on (backed by a
real `rustix::process::set_child_subreaper` call), but its own doc
comment carried an unverified claim that `ocirun create` shouldn't
offer the same flag at all, reasoning purely from real runc's own
checked-directly absence of `--no-subreaper` on `create`. That
claim never independently checked crun's own `create` — which does
register the identical flag, just as a real, checked-directly
no-op. Re-examined directly this time (the same "re-examine an old
deferral against exact upstream source" technique `0499` and `0509`
both already used successfully): `ocirun create --no-subreaper`
currently fails hard at the clap level
(`error: unexpected argument '--no-subreaper' found`), which is a
real CLI-compatibility gap against crun specifically, even though
runc genuinely has no equivalent.

## Real, checked-directly confirmation

- `~/git/crun/src/create.c:47`: `create` registers its own
  `--no-subreaper` flag, right alongside `run`'s copy, with the
  literal usage string `"do not create a subreaper process
  (ignored)"` — crun's own help text already documents the no-op.
- `~/git/crun/src/create.c:80-81`: the handler,
  `case OPTION_NO_SUBREAPER: break;` — a bare, literal no-op. crun
  itself never actually sets or clears the subreaper attribute
  during `create`; only `run`/`exec` (the commands that actually
  block waiting on the container's own exit) ever touch it.
- `~/git/runc/create.go`: confirmed absent (also confirmed live via
  `runc create --help` on this host) — a real, checked-directly
  *divergence* between the two reference runtimes. runc simply
  doesn't offer the flag there at all; crun offers it but does
  nothing with it.

## Implementation

`Command::Create` gains `no_subreaper: bool` (`--no-subreaper`),
purely for crun-CLI compatibility. The dispatch arm destructures and
discards it (`no_subreaper: _`) with a one-line comment pointing at
the field's own doc comment — it is never threaded into
`cmd_create`'s own function signature at all, since it does nothing,
matching crun's own identical behavior exactly. `Command::Run::
no_subreaper`'s own doc comment is corrected to stop claiming `create`
shouldn't offer the flag, pointing at `Command::Create::no_subreaper`
instead.

## Tests

One new integration test in `tests/tests/ocirun_lifecycle.rs`:
- `create_no_subreaper_flag_is_accepted_and_behaves_identically` —
  mirrors the existing `create_no_pivot_reaches_running_after_start`
  shape: `create --no-subreaper` succeeds (previously a hard clap
  error), reaches `created`, then `start` reaches `running` exactly
  as a plain `create` would, proving the flag changes nothing at all
  beyond being accepted.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the
block count is unchanged from `0509`; the documented transient
`ociman_logs.rs` follow-test flakiness under this host's own
persistent CPU contention showed up once on the first full-suite
attempt, confirmed transient by rerunning that single test in
isolation under load — failed once more, then passed cleanly with
`RUST_TEST_THREADS=2` — followed by a clean full-suite rerun with
`RUST_TEST_THREADS=2` throughout), `python3 ci/guards.py` (clean),
`cargo deny check` (clean), `bash ci/native-ci.sh` (clean on the
first attempt with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh`
(clean on the first attempt, real `dpkg -i`/`--version`/`dpkg -r`
round trip). This does not touch `cmd_create`'s own real logic at
all (the new field is accepted-and-discarded only, never passed into
`cmd_create`'s own body) — no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

`port` (no networking subsystem), `mount`/`unmount` (cross-concept
aliasing, unverified), `init` (architecture mismatch), and
`runlabel` (low priority, unexamined) remain the last unexamined
`ociman container <verb>`-family candidates from `0488`-`0507`'s own
"Remaining, explicitly NOT well-scoped" list; none of those are
touched by this increment.
</content>
