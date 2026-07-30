# Design note 0333: `ociman ps --format`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`,
`README.md`.

## What this closes

`0332`'s own "Still ahead" flagged `ps --format`/`images --format` as
natural, small follow-ons reusing the exact same template engine — real
users reach for `docker ps --format '{{.Names}}'` just as often as
`inspect --format`. This closes the `ps` half.

## Implementation

`Command::Ps` gained `format: Option<String>` — **no `-f` short
alias** here, unlike `inspect --format` (`0332`): `ps` already has
`-f`/`--filter`, so real `podman ps` itself has no `-f` shorthand for
`--format` either (checked directly, `~/git/podman/cmd/podman/
containers/ps.go`'s own help example only ever shows `--format`
spelled out) — matching real podman's own actual flag surface, not an
oversight.

Reuses `0332`'s own `render_format_template`/`resolve_json_path`/
`format_json_scalar` completely unchanged: `cmd_ps` now checks `format`
first (before the existing `quiet`/`json`/table branches), rendering
the template against each listed `ContainerView`'s own JSON value and
printing one line per container — matching real `podman ps
--format`'s own identical "one line per row" semantics. Field names
are `ContainerView`'s own JSON field names directly (e.g. `{{.name}}`,
not `{{.names}}` — the struct field is singular, `Option<String>`, only
the *display column header* says "NAMES"). `--format` takes priority
over `--quiet`/`--json`/the default table when given, matching real
podman's own identical precedence (checked directly).

`cmd_ps` grew an eighth parameter doing this, tripping clippy's own
`too_many_arguments` — this file already had six other pre-existing
`#[allow(clippy::too_many_arguments)]` uses, so this note follows that
same, already-established local convention rather than introducing a
new bundling struct (`ocibox export`'s own `ExportArgs`, `0329`, was a
one-off for a much larger, more naturally-groupable flag set).

## Verified

`cargo build -p ociman --locked`; manual smoke test with two real
containers: `ociman ps --format '{{.name}}={{.status}}'` prints one
line per container in the expected shape; `ociman ps --help` renders
the new flag correctly, documenting the missing `-f` alias and why.

Two new integration tests in `tests/tests/ociman_ps.rs` (34 total, 32
pre-existing, all pass unchanged): one line per listed container with
the correct field substitution; and `--format` taking priority over
`-q`/`--json`/the default table plus a real, immediate error for an
unresolvable field, mirroring `inspect --format`'s own identical
coverage.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test result blocks,
0 failures — one known, previously-documented occasional single-test
flake in `ocicri_container.rs` hit during this turn's full local
verification, re-verified passing in isolation and the full suite
re-run clean afterward), `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman ps` is a one-shot, offline, read-only command,
not part of any hot-path benchmark tracked in `docs/benchmarks.md`;
the no-`--format` case is unchanged in shape and cost (one extra
branch check). No re-benchmark needed.

## Still ahead

`ociman images --format` remains the last natural consumer of this
same template engine, reusing it unchanged just as `ps --format` did
here — deliberately not done in this same note to keep it narrowly
scoped and verified one command at a time, matching `0332`'s own
stated reasoning. `ociman`/`ocirun`'s own other remaining gaps
(`--restart` policy, `ocirun run --no-pivot`/`--console-socket`) and
`ocibox`'s own remaining gaps (`stop`/`upgrade`/`generate-entry`/
`assemble`, `export --sudo`/`--enter-flags`) remain separately-scoped
future candidates.
