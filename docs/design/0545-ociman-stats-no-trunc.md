# Design note 0545: `ociman stats --no-trunc`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_stats.rs`.

## What this closes

Adds `--no-trunc` to `ociman stats` and its `ociman container stats`
alias (`0503`) — a flag `statFlags` applies to both real commands
upstream that this project's own `Command::Stats`/`ContainerCommand::
Stats` never had at all.

## Real, checked-directly confirmation

- Flag definition, shared by both real commands:
  `~/git/podman/cmd/podman/containers/stats.go:61,73` —
  `notrunc bool`; `flags.BoolVar(&notrunc, "no-trunc", false, "Do not
  truncate output")`, registered via the one shared `statFlags(cmd)`
  helper (`stats.go:63-79`) applied to both `statsCommand` and
  `containerStatsCommand` (`stats.go:88,94`) identically.
- The *only* real consumer: `~/git/podman/cmd/podman/containers/
  stats.go:190-195` —
  ```go
  func (s *containerStats) ID() string {
      if notrunc {
          return s.ContainerID
      }
      return s.ContainerID[0:12]
  }
  ```
  No other column formatter method in that file ever reads `notrunc`
  at all — checked directly by reading every `func (s
  *containerStats) ...() string` in the file (`CPUPerc`/`AVGCPU`/
  `Up`/`MemPerc`/`NetIO`/`BlockIO`/`PIDS`/`MemUsage`/
  `MemUsageBytes`), none of which reference it.

## Real, total no-op (checked directly, not assumed)

This project's own container ids are always the short, 12-hex-
character form (`short_id()`, hashing then truncating to 12 hex
chars) — there is no separate, longer form ever recorded anywhere to
reveal. `print_stats_table` already prints the id unconditionally,
with no truncation logic of its own at all today. Accepting
`--no-trunc` and leaving output unchanged is therefore a real, total
no-op — the same "already always short, nothing to un-truncate"
reasoning this project already established for the *ID column
specifically* in `ociman ps --no-trunc` (`0270`-ish era) and `ociman
history --no-trunc`. The one difference from `ps`'s own `--no-trunc`:
`ps`'s own flag is only a *partial* no-op there (it also un-truncates
a separate `command` column this project's `ps` table does truncate
by default) — `stats`'s own table has no second truncated column at
all, so this is a total no-op here, not a partial one.

## Checked docs/design/ and README.md first

- `grep -rn "no.trunc" docs/design/*.md` shows only `ps`/`history`/
  `volume ls`/`mount` precedents — no design note ever mentions
  `stats --no-trunc`.
- `docs/design/0339-ociman-stats-format.md` (`--format`) and
  `docs/design/0450-ociman-stats-latest.md`/`0503-ociman-container-
  stats-alias.md` (the two most recent `stats`-touching notes) each
  explicitly flag `--all` as this command's own known, deliberately
  narrower-first-slice gap, but never mention `--no-trunc` at all —
  a genuinely fresh gap, not a rediscovery of something already
  deferred.

## Implementation

`bin/ociman/src/main.rs`: `no_trunc: bool`
(`#[arg(long = "no-trunc")]`) added to `Command::Stats` (full doc
comment citing the above) and `ContainerCommand::Stats`
(cross-referencing it, updating that alias's own doc comment to list
`--no-trunc` among the shared `statFlags`-applied set). Accepted and
immediately discarded (`no_trunc: _`) at both dispatch sites —
`cmd_stats`'s own signature is untouched, the same "nothing to skip"
convention `ociman commit --quiet` (`0523`) already established for a
total no-op.

## Tests

`stats_no_trunc_flag_is_accepted_and_behaves_identically`
(`tests/tests/ociman_stats.rs`): runs a real container, calls `stats
--no-stream` and `stats --no-stream --no-trunc`, asserts both succeed
and both show the exact same 12-hex-character id (proving no
additional truncation was applied either way, since there was never
anything more to reveal).

Manually exercised beyond the automated tests: a real running
container's `ociman stats --no-stream`/`--no-stream --no-trunc`/
`ociman container stats --no-stream --no-trunc`, all producing the
identical table shape and the same 12-hex-character id.

## Verification

`cargo build --workspace --locked` (clean), `cargo fmt --all` (clean,
no changes needed for the new test), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), targeted
`ociman_stats.rs`/`ociman_container.rs` runs (48/48, 13/13), a full
`cargo test --workspace --locked` run (clean), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (clean),
`bash ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r`
round trip). Pure CLI-parsing-and-discard addition — no hot path
touched, no `ci/bench.sh` rerun needed.
