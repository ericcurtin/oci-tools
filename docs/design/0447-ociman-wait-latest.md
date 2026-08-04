# Design note 0447: `ociman wait --latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_wait.rs`.

## What this closes

`ociman wait` had no `--latest`/`-l` flag at all — continuing the
same rollout `0434`-`0437`/`0443`-`0446` already established.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/wait.go:69,76`: `validate.
AddLatestFlag` — the exact same flag/validation `ociman rm --latest`
(`0434`) already ports. Its own validation (lines ~92-96) is a real,
simple two-case check, no positional-argument ambiguity at all
(unlike `exec`/`top`, `0443`/`0446`, whose own positionals double as
something else): `wait`'s own positionals are only ever container
references, never a trailing command:

```go
if !waitOptions.Latest && len(args) == 0 {
    return fmt.Errorf("%q requires a name, id, or the \"--latest\" flag", cmd.CommandPath())
}
if waitOptions.Latest && len(args) > 0 {
    return errors.New("--latest and containers cannot be used together")
}
```

`ociman wait` already supported multiple explicit targets (unlike
`ociman logs`, `0445`, which only ever took one) — `--latest` simply
resolves to a single-element list feeding the exact same already-
multi-target-capable path.

## Implementation

- `Command::Wait::ids` drops its previous `#[arg(required = true)]`
  (now genuinely optional at the CLI level, omittable when using
  `--latest`); new `latest: bool` (`#[arg(short = 'l', long)]`).
- The dispatch arm performs the same two-case validation as real
  podman's own `wait.go`, in its exact wording; with `--latest`, `ids`
  becomes a single-element vec from `resolve_latest_container`
  (`0434`); without it, the given `ids` are used unchanged (now
  validated non-empty manually, replacing the flag-level `required`
  clap used to enforce). `cmd_wait`'s own signature and multi-target
  implementation are completely unchanged.

## Tests

Four new tests in `tests/tests/ociman_wait.rs`: `wait_latest_prints_
the_most_recently_created_containers_own_exit_code` (two containers
with a real, distinguishable creation-time gap and genuinely
different exit codes; `wait --latest` prints only the newer one's —
note: `ociman run`'s own process exit code mirrors the container's
real exit code, matching real podman/docker exactly, so this test
checks `.status.code()` rather than `.success()` for the two
deliberately-nonzero-exiting `run` calls, a real, easy-to-miss
correctness detail caught while writing the test, not merely assumed
safe to skip), `wait_latest_and_explicit_id_together_is_a_clear_
error`, `wait_with_no_container_and_no_latest_is_a_clear_error`, and
`wait_latest_on_an_empty_store_is_a_clear_error`. All 8 prior tests
in the file pass unmodified (12/12 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the second full run — the first hit the
pre-existing, previously-documented `ocicri_container.rs` host-
contention flakiness, confirmed unrelated and transient by an
immediate isolated rerun), `python3 ci/guards.py`, `cargo deny check`,
`bash ci/native-ci.sh` (clean, 120/120 — one earlier run hit a
different, previously-unseen instance of the identical class of
host-contention flakiness, this time a real 1-second `created`-
timestamp boundary race in the entirely unrelated, pre-existing
`ociman_build.rs`'s own `build_unsetenv_adds_no_history_entry_of_its_
own` test — confirmed unrelated to this change (that file is
untouched) and transient by several immediate isolated reruns, all
passing, then a clean full rerun), `bash ci/build-deb.sh` (real `dpkg
-i`/`--version`/`dpkg -r` round trip). Touches only `ociman wait`'s
own selection logic, not any hot path at all — no benchmark re-run
needed.

## Deliberately still out of scope

Continuing this same rollout: `attach`, `diff`, `inspect`, `stats`,
`start`, `port`, and `checkpoint`/`restore` (the last two CRIU-based,
a much larger, separately-scoped gap) still don't have `--latest`
here at all — each a natural, separate future increment.
