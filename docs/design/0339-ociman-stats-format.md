# Design note 0339: `ociman stats --format`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_stats.rs`,
`README.md`.

## What this closes

`0338`'s own "Still ahead" flagged `stats --format` as the one
remaining real, small follow-on in the `inspect`/`ps`/`images`/`volume
ls`/`info`/`history --format` family (`0332`-`0338`) — completing it.

## A genuinely different shape than every prior consumer

Every earlier `--format` command in this family is a one-shot,
run-once-and-exit command. `ociman stats` is not: its own default mode
is a **continuous stream**, re-sampling and redrawing every
`--interval` seconds until the target container stops or the process
is interrupted (`0284`). Checked directly against real `podman
stats --format`: it also re-renders the same template on *every*
sample in streaming mode, not just once — so this project's own
implementation needed to apply the template inside the streaming loop
too, not just the `--no-stream` one-shot path.

## Implementation

`Command::Stats` gained `format: Option<String>`. `cmd_stats`'s own
two call sites (the `--no-stream` early-return branch, and the
streaming loop's own per-iteration print) previously each had their
own identical `if json {...} else { print_stats_table(...) }` — both
consolidated into one new, shared `print_stats_sample(view, json,
format)` helper (a small, pure refactor with no behavior change to the
existing two branches beyond the new `format` check itself), which
checks `format` first (before `json`/the plain table), rendering the
template against `ContainerStatsView`'s own JSON value — matching the
whole family's own established "format wins" precedence. Field names
are the struct's own JSON fields directly: `{{.id}}`, `{{.name}}`,
`{{.cpu_percent}}`, `{{.mem_usage}}`, `{{.mem_limit}}`,
`{{.mem_percent}}`, `{{.pids}}`.

No new `#[allow(clippy::too_many_arguments)]` needed — `cmd_stats` had
four parameters before this, five now (`print_stats_sample` itself,
the new shared helper, only takes three).

## Verified

`cargo build -p ociman --locked`; manual smoke test with a real,
running, cgroup-backed container: `ociman stats <id> --no-stream
--format '{{.pids}}'` and a combined multi-placeholder template both
render correctly; `ociman stats --help` renders the new flag
correctly, documenting that it applies to every sample in streaming
mode too.

Two new integration tests in `tests/tests/ociman_stats.rs` (8 total, 6
pre-existing, all pass unchanged), both gated on a reachable `systemd
--user` session the same way the file's own existing real-cgroup tests
already are: a real running container's `--no-stream` sample renders
`{{.id}}` correctly; and `--format` taking priority over `--json`/the
default table plus a real, immediate error for an unresolvable field,
mirroring the whole `--format` family's own identical coverage.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman stats` reads real cgroup accounting files at each
sample regardless of `--format`; the new branch check adds no
measurable cost relative to that existing I/O. Not part of any
tracked benchmark in `docs/benchmarks.md`. No re-benchmark needed.

## Still ahead

The `inspect`/`ps`/`images`/`volume ls`/`info`/`history`/`stats
--format` family (`0332`-`0339`) is now complete — every real,
common-use `podman`/`docker --format` consumer this project's own
narrow template engine can serve without new architecture has one.
`COPY --exclude=<pattern>` (reusing this project's own already-
threaded `DockerIgnore` filter machinery, flagged in `0337`'s own
survey) still needs its own dedicated scoping pass before committing
to it. `ociman`/`ocirun`'s other remaining gaps (`--restart` policy,
`--console-socket`) and `ocibox`'s own remaining gaps (`stop`/
`upgrade`/`generate-entry`/`assemble`, `export --sudo`/`--enter-flags`)
remain separately-scoped future candidates.
