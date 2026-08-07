# Design note 0539: `ocirun events --interval`

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_events.rs`.

## What this closes

`ocirun events --stats` (`0261`) never accepted `--interval` at all —
a real CLI flag real `runc events` accepts and validates on *every*
invocation, including the `--stats` one-shot mode this project
already implements.

## Real, checked-directly confirmation

- `~/git/runc/events.go:29-46` (current dev tree):
  ```go
  Flags: []cli.Flag{
      &cli.DurationFlag{Name: "interval", Value: 5 * time.Second, Usage: "set the stats collection interval"},
      &cli.BoolFlag{Name: "stats", Usage: "display the container's stats then exit"},
  },
  Action: func(_ context.Context, cmd *cli.Command) error {
      ...
      container, err := getContainer(cmd)
      if err != nil { return err }
      duration := cmd.Duration("interval")
      if duration <= 0 {
          return errors.New("duration interval must be greater than 0")
      }
      status, err := container.Status()
      ...
      if cmd.Bool("stats") {
          s, err := container.Stats()
          ...
          return nil   // duration/`--interval` is never read again on this path
      }
      // periodic ticking loop uses cmd.Duration("interval") here
  ```
  Confirmed byte-identical (modulo the `cli` package's own API churn
  between major versions) in the exact tag matching this host's
  installed binary: `git -C ~/git/runc show v1.3.4:events.go` has the
  same `duration := context.Duration("interval"); if duration <= 0 {
  return errors.New(...) }` ordering — right after confirming the
  container exists, *before* ever branching on `--stats` — even
  though the parsed value is only actually consumed by the periodic
  (non-`--stats`) mode.
- **Verified live against a real installed `runc 1.3.4`**, with a
  real running container: `runc events --interval 0 --stats <ctr>` →
  `time="…" level=error msg="duration interval must be greater than
  0"` (exit `1`); `runc events --interval bogus --stats <ctr>` → a
  real flag-parse failure (`Incorrect Usage: invalid value "bogus"
  for flag -interval: parse error`); `runc events --interval 3s
  --stats <ctr>`/no flag at all both succeed identically. This
  confirms live (not just from source) that the flag is genuinely
  accepted and genuinely validated on the one implemented mode today.

## Implementation

`bin/ocirun/src/main.rs`:
- New `Command::Events::interval: String`, `#[arg(long, default_value
  = "5s")]` (matching real runc's own default exactly).
- New `parse_go_style_duration`, a small, deliberate per-binary
  duplicate of `ociman`'s own already-established `parse_simple_
  duration` shape (`h`/`m`/`s`, compound units) — this project's own
  convention of a small duplicate over a new shared-crate dependency
  for ~20 lines — plus `ms` support and a real, checked-directly
  special case: Go's own `time.ParseDuration` accepts a bare,
  unit-less `"0"` (confirmed live above: `--interval 0` parses fine,
  then separately fails the `<= 0` check — not a parse error).
- `cmd_events` validates `--interval` right after loading the
  container's own state (matching real runc's exact order: existence
  → duration → running-check → `--stats` branch) — an unparseable or
  non-positive value is a real, immediate error; a valid one is
  accepted but, matching real runc's own identical quirk, never
  actually read again on this project's `--stats`-only path. The
  pre-existing "periodic mode not implemented yet" bail (a project-
  specific scope decision with no real runc equivalent) still runs
  first, unconditionally, before ever touching the container at all —
  unchanged from before this increment.

## Tests

Four new integration tests in `tests/tests/ocirun_events.rs`:
- `events_stats_interval_zero_is_a_clear_error` — real runc's own
  exact error message, verified against a genuinely running
  container.
- `events_stats_unparseable_interval_is_a_clear_error`.
- `events_stats_with_a_valid_interval_behaves_identically_to_the_default`
  — `3s`/`500ms`/`1m` all succeed and report the identical shape a
  bare `--stats` (no `--interval`) already does.
- `events_stats_interval_validation_runs_before_the_running_check` —
  proves the exact real validation order against an already-*stopped*
  container: `--interval 0` reports the interval error, not "is not
  running", matching real runc's own identical precedence.

Plus four new unit tests for `parse_go_style_duration` in `bin/ocirun/
src/main.rs`'s own `mod tests` (bare `"0"`, plain units including
`ms`, a compound value, garbage/empty), matching `parse_memory_
limit`'s own established "pure logic gets a direct unit test"
precedent.

Manually exercised end to end beyond the automated tests, mirroring
every real-runc scenario verified live above one-for-one against this
project's own real built binary and a real bundle: `--interval 0`
(error, exact message), `--interval bogus` (error), valid intervals
(`3s`/`500ms`, plus the default) all reaching the same downstream
point a bare `--stats` already does, and the validation-order check
against a container whose `sleep`d init process had already exited
(confirming the interval error still takes priority).

Full workspace: `cargo build --workspace --locked` (clean, after
fixing three "unnecessary qualification" clippy warnings from writing
`std::time::Duration` fully-qualified in a file that already imports
`Duration` directly), `cargo fmt --all` (clean after two auto-fixes),
`cargo clippy --workspace --all-targets --locked -- -D warnings`
(clean), the full `ocirun_events.rs` suite (7/7) and `ocirun`'s own
unit tests (19/19), a full `cargo test --workspace --locked` run (126
test-result blocks, 0 failures, fully clean on the first attempt),
`python3 ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (clean on the first attempt), `bash ci/build-deb.sh`
(clean on the first attempt, real `dpkg -i`/`--version`/`dpkg -r`
round trip). Pure CLI-parsing-and-validation addition on a
diagnostics command — no hot path touched, not part of `ci/bench.sh`
(the sibling `--stats` increment, `0261`, already established that),
no rerun needed.

## Deliberately still out of scope

The periodic (no `--stats`, every `--interval`) OOM-notify mode
itself remains a clear, honest "not yet" error, unchanged from `0261`
— this increment only closes the CLI surface for the flag on the one
mode this project does implement.
