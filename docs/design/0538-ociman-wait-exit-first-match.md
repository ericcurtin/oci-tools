# Design note 0538: `ociman wait --exit-first-match`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_wait.rs`,
`tests/tests/ociman_container.rs`.

## What this closes

`ociman wait`'s own gap for `--exit-first-match` was flagged twice
already in this project's own design notes (`0189`, `0496`) and once
more in the `ContainerCommand::Wait` alias's own doc comment, and
simply never picked up since. This closes it.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/wait.go:60-61` (inside the
  shared `waitFlags(cmd)` helper, applied to both the top-level and
  nested `container wait` commands identically):
  ```go
  waitExitFirst := "exit-first-match"
  flags.BoolVar(&waitOptions.ExitFirstMatch, waitExitFirst, false, "Wait for exit of first container which matches conditions, ignore other ones")
  ```
- `~/git/podman/pkg/domain/infra/abi/containers.go:188-192,218-249`
  (`ContainerWait`/`waitExitOnFirst`): every resolved container races
  concurrently (`go waitFunction(...)` per container, each
  independently polling the identical condition set the non-racing
  path uses), and only the *first* response received on a buffered
  channel is returned; the context is cancelled once that happens,
  and a container already resolved to `doesNotExist` (tolerated only
  under `--ignore`) never even enters the race — `for _, c := range
  containers { if c.doesNotExist { continue } ... }`.
- Real docker has no equivalent flag at all (`grep -rn
  "exit-first-match" ~/git/moby` finds nothing) — a podman-only
  surface, the same class as `--latest`, which this command already
  has.
- The installed `podman 4.9.3`'s own `wait --help` doesn't show this
  flag at all; the cloned `~/git/podman` source is a materially newer
  dev version (`v5.4.0-rc1-3480-g39ff0ea7cd`) where it's real. Only
  confirmable from source here, the same situation `0536` (`ocibox`,
  no `distrobox` binary installed) already handled identically.

## Implementation

`bin/ociman/src/main.rs`:
- New `Command::Wait::exit_first_match: bool`, `#[arg(long)]` (no
  short flag, matching upstream), and the identical field on the
  nested `ContainerCommand::Wait` alias.
- `cmd_wait` branches to a new `wait_exit_first_match` once every
  container is resolved exactly as before (fail-fast, matching real
  podman): one real OS thread per *resolved* target (a container
  already resolved to `None` under `--ignore` never gets a thread at
  all, matching real podman's own identical `doesNotExist` skip),
  each running the identical polling loop the non-racing path already
  uses, racing over a `std::sync::mpsc` channel; the main thread does
  one `recv()`, prints exactly that one exit code, and returns
  (abandoning the other, still-polling threads — sound, since process
  exit reaps them, and none of them hold anything needing explicit
  cleanup).
- A real, deliberate divergence: if *every* given target was resolved
  to `None` (all `--ignore`d as nonexistent), real podman's own
  identical case spawns no goroutine at all and then blocks forever
  on an empty channel with no sender — a genuine, checked-directly
  upstream deadlock, confirmed by reading `waitExitOnFirst`'s own
  `for` loop directly. This project deliberately does not reproduce
  that hang, printing the same `-1` the non-racing path already
  prints for a single such container instead.

## A real, pre-existing bug found and fixed along the way

Manually racing two containers with very different completion times
(`sleep 1` vs `sleep 60`, later widened after an initial `sleep 5`
margin still occasionally flipped under load) surfaced a real,
previously-latent bug in `cmd_wait`'s own *existing*, already-shipped
sequential loop, not something new introduced by `--exit-first-match`
itself: `display_status`'s own `effective_status()` can report
`Status::Stopped` purely because a container's recorded pid is no
longer alive, even while the *raw*, on-disk status is still `Running`
and its own [`ANNOTATION_EXIT_CODE`] hasn't been written yet — the
exact same real race `docs/design/0154`'s own `wait_for_keeper_to_
finalize` was built to guard against for `stop_container`, but never
applied at this call site. A poll landing in that narrow window
(normally milliseconds, genuinely widened under heavy host
contention delaying the container's own detached *keeper* process)
would read back a real container's own genuine exit code as a
spurious `-1`.

Fixed with a new shared `wait_for_and_read_exit_code` helper (calls
the existing `wait_for_keeper_to_finalize`, then re-reads the exit
code from the freshly-reloaded state) — applied to *both* the
pre-existing sequential loop and the new racing threads, since both
independently poll the same status/exit-code pair. No new primitive
needed; this is a correct application of an already-established
fix pattern to a call site that had never gotten it.

## Tests

Three new integration tests in `tests/tests/ociman_wait.rs`:
- `wait_exit_first_match_prints_only_the_first_containers_own_exit_code`
  — two genuinely still-running containers with very different
  completion times; only the fast one's own exit code is ever
  printed, well within a generous wall-clock bound.
- `wait_exit_first_match_with_only_ignored_nonexistent_containers_prints_negative_one`
  — the deliberate real-podman-deadlock divergence above, bounded to
  prove it never actually hangs.
- `wait_exit_first_match_ignores_a_nonexistent_container_and_waits_for_the_real_one`
  — a mix of a real target and an `--ignore`d nonexistent one.

Plus one new alias-proof test in `tests/tests/ociman_container.rs`
(`container_wait_exit_first_match_flag_works_through_the_alias`).

The first version of the timing test used a `sleep 5`-vs-`sleep 1`
gap and *intermittently* failed under this host's own heavy
concurrent-`opencode`-session load — not with a timing-bound miss,
but with the *wrong* exit code (or, once, a spurious `-1`) printed,
which is what led directly to discovering the real bug above rather
than just assuming environmental noise. After the fix, reran the
timing test 15 consecutive times with no failures (plus widened the
sleep gap to `1s`/`60s` for extra headroom against unrelated
scheduling jitter, at no cost in the success case since the test
still finishes in ~2s either way).

A second, separate test-design issue turned up right after (also not
a feature bug): the test's own setup asserted the *fast* container
was still `running` at the moment it checked, but under real,
observed host contention its 1s sleep had already completed (and its
own final state fully written) by the time that check ran — a real,
if narrow, false assumption in the test itself. Fixed by dropping
that specific assertion (only the *slow* container's own `running`
status is actually load-bearing for this test's own premise); reran
10 consecutive times afterward, including some genuinely slow
(10-12s) runs under real contention, with no further failures.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), the full `ociman_wait.rs` (15/15) and
`ociman_container.rs` (48/48) suites, a full `cargo test --workspace
--locked` run (126 test-result blocks, 0 failures, fully clean on
the first attempt), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (failed once on its own
internal `cargo test` with two different, already-documented
transient `ocicri_container.rs` failures under this host's own
concurrent-session load, confirmed transient by isolated rerun; a
fully clean rerun with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh`
(clean on the first attempt, real `dpkg -i`/`--version`/`dpkg -r`
round trip). `ociman wait` is not part of any create/start/destroy
hot path and doesn't appear in `ci/bench.sh` — no rerun needed.
