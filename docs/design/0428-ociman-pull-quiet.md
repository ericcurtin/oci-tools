# Design note 0428: `ociman pull --quiet`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_tls_verify.rs`,
`README.md`.

## What this closes

`ociman pull` had no `--quiet`/`-q` flag at all — the progress
spinner always showed unconditionally. Real `podman pull --quiet`/
`docker pull --quiet` both suppress it. This is the third real call
site for the `spinner_unless_quiet` primitive `0417` built
specifically for this shape (`ociman save --quiet`/`load --quiet`),
reused here unchanged.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/images/pull.go:112`: `flags.BoolVarP(&
pullOptions.Quiet, "quiet", "q", false, "Suppress output information
when pulling images")`, gating the identical progress-writer pattern
at `pull.go:216`: `if !pullOptions.Quiet { ... }` — the same real
shape `0417`'s own design note already cites verbatim for `Save
Image`/`LoadImage`.

## Implementation

- `Command::Pull` gains `quiet: bool` (`#[arg(short, long)]`).
- `pull_unconditionally` (the thin wrapper adding `ociman`'s own
  spinner around the shared `oci_registry::pull_unconditionally`)
  gains a `quiet: bool` parameter, replacing its unconditional
  `progress::spinner(...)` call with `progress::spinner_unless_
  quiet(quiet, ...)`.
- The *other* caller of `pull_unconditionally` — `resolve_or_pull`'s
  own closure, used by `ociman run --pull`/`create`/`build`'s
  implicit `FROM`/`COPY --from=` pulls — now passes a literal
  `false`, preserving its existing always-visible-spinner behavior
  exactly: none of those commands have a `--quiet` concept of their
  own, so this stays deliberately scoped to `ociman pull` alone, not
  a silent behavior change for three other commands.
- `--json`/the final digest line are completely unaffected either
  way, matching real podman's identical scope (`--quiet` only ever
  silences the progress writer, never the final result).

## Tests

One new test in `tests/tests/ociman_tls_verify.rs` (which already
has the real mock-registry infrastructure this needed),
`pull_quiet_still_pulls_correctly`: the spinner only ever draws to
stderr and is already automatically hidden whenever stderr isn't a
real terminal (true of this whole automated suite), so there's no
separately observable output difference to assert on — what's real
and checkable is that the flag is accepted and the pull it performs
still succeeds and lands in local storage correctly. All 8 prior
tests in `ociman_tls_verify.rs` continue to pass unmodified (9/9
total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the second full run — one earlier
attempt hit an unrelated, known, pre-existing `ocicri_container.rs`
host-contention flake, confirmed environmental via an immediate
isolated rerun), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh` (clean, 119/119), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). Touches only `ociman
pull`'s own progress-spinner path, not any hot path at all — no
benchmark re-run needed.

## Deliberately still out of scope

`ociman push --digestfile`/`ociman container list`/`ociman image
list` (real podman `ls` aliases) — real, confirmed gaps identified
alongside this one, each a natural, separate future increment.
