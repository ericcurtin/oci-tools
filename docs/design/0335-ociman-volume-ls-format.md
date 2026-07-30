# Design note 0335: `ociman volume ls --format`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_volume.rs`,
`README.md`.

## What this closes

`0332`-`0334` implemented the `inspect`/`ps`/`images --format` trio
real users reach for most, but a fresh survey of that trio's own
framing found it had missed a genuine 4th consumer: `volume ls` had no
`--format` at all (only `--json` and a plain table) — checked directly,
both real `podman volume ls --help`/`docker volume ls --help` document
`--format` with real Go-template examples (podman's own default output
is even implemented as `--format '{{range .}}{{.Driver}}\t{{.Name}}
\n{{end -}}'` under the hood).

## Why this wasn't found by the earlier survey

The `0332`-`0334` sequence was framed narrowly around "the `inspect`/
`ps`/`images` trio" from the start — `volume ls` simply fell outside
that self-imposed scope, not because it was considered and rejected.
A broader re-survey (specifically re-checking `ociman volume ls`
against real `podman`/`docker volume ls --help`) caught it.

## Implementation

`VolumeCommand::Ls` (previously a bare, field-less variant) gained
`format: Option<String>`. `cmd_volume_ls` reuses `render_format_template`
(from `0332`) completely unchanged, checking `format` first (before the
existing `json`/table branches) and rendering the template against each
listed volume's own `VolumeView` JSON value — one line per volume,
matching real `podman volume ls --format`'s own identical "one line per
row" semantics. Field names are `VolumeView`'s own four JSON fields
directly: `{{.name}}`, `{{.driver}}`, `{{.mountpoint}}`,
`{{.created_at}}`. `--format` takes priority over `--json`/the default
table when given; an unresolvable field path is a real, immediate
error — same precedence and error behavior the trio already
established.

No new `#[allow(clippy::too_many_arguments)]` needed — `cmd_volume_ls`
only had one parameter before this, two now.

## Verified

`cargo build -p ociman --locked`; manual smoke test with two real
volumes: `ociman volume ls --format '{{.name}} {{.driver}}'` and
`--format '{{.mountpoint}}'` both render correctly; `ociman volume ls
--help` renders the new flag correctly.

Two new integration tests in `tests/tests/ociman_volume.rs` (19 total,
17 pre-existing, all pass unchanged): one line per listed volume with
the correct field substitution; and `--format` taking priority over
`--json`/the default table plus a real, immediate error for an
unresolvable field, mirroring `inspect`/`ps`/`images --format`'s own
identical coverage.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman volume ls` is a one-shot, offline, read-only
command, not part of any hot-path benchmark tracked in
`docs/benchmarks.md`; the no-`--format` case is unchanged in shape and
cost (one extra branch check). No re-benchmark needed.

## Still ahead

A fresh, broader survey (beyond the `inspect`/`ps`/`images` trio's own
self-imposed framing) confirmed no other similarly-small `--format`
consumers remain: `ocirun list`/`ocirun ps` already correctly match
real `runc list`/`ps`'s own `table`/`json`-only behavior (no
Go-template support in the real tool either, checked directly against
`~/git/runc/list.go`) — not a gap. `ociman cp`/`diff`/`rename` are
already complete. `ocirun run/create --no-pivot` is a real, genuinely
smaller-than-previously-assumed candidate (the core mechanism is
already-available syscalls, deliberately narrower than real runc's own
mountinfo-scanning hardening, matching real crun's simpler approach
instead) but spans 7-8 files across every binary that calls `launch.rs`
— a real, if modest, multi-file change, not a single-file one. `--restart`
policy (confirmed zero supervisor/watcher infrastructure anywhere) and
`--console-socket` (confirmed blocked on this project's own
already-documented "no PTY allocation" gap) remain correctly deferred,
bigger candidates. `ocibox`'s own remaining gaps (`stop`/`upgrade`/
`generate-entry`/`assemble`, `export --sudo`/`--enter-flags`) are
unaffected by this note.
