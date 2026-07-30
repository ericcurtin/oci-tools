# Design note 0337: `ociman info --format`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_info.rs`,
`README.md`.

## What this closes

A fresh, broader survey (moving past the `inspect`/`ps`/`images`/
`volume ls` quartet, `0332`-`0335`) found `ociman info` was a genuine
5th consumer of the same shared template engine that vein hadn't
reached yet — checked directly, both real `podman info --format`/
`docker info --format` are real, documented, commonly-scripted flags,
and `ociman info` already builds one plain `Serialize`-able
`InfoReport` struct with no engine changes needed to reuse
`render_format_template` against it.

## Implementation

`Command::Info` (previously a bare, field-less variant) gained
`format: Option<String>`. `cmd_info` checks `format` first (before the
existing `json`/plain-text branches), rendering the template against
`InfoReport`'s own JSON value and printing the single resolved line —
matching real `podman info --format`'s own identical "one report, one
formatted line" semantics (as opposed to `ps`/`images`/`volume ls
--format`'s own "one line per row" shape, which doesn't apply here
since `info` only ever describes one thing). Field names are
`InfoReport`'s own nested JSON structure directly: `{{.host.hostname}}`,
`{{.store.images}}`, `{{.version.version}}`, etc. `--format` takes
priority over `--json`/the default plain-text report when given; an
unresolvable field path is a real, immediate error — same precedence
and error behavior the whole family already established.

No new `#[allow(clippy::too_many_arguments)]` needed — `cmd_info` only
had one parameter before this, two now.

## Verified

`cargo build -p ociman --locked`; manual smoke test: `ociman info
--format '{{.host.hostname}}'`, `--format '{{.store.images}}'`,
`--format '{{.version.version}}'`, and a combined multi-placeholder
template all render correctly; `ociman info --help` renders the new
flag correctly.

Three new integration tests in `tests/tests/ociman_info.rs` (6 total, 3
pre-existing, all pass unchanged): a nested field (`version.version`)
renders correctly; multiple placeholders across different top-level
sections (`store.images`/`store.containers`) interpolate correctly
while also confirming `--format` wins over `--json` when both are
given; and an unknown field is a real, immediate error.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman info` is a one-shot, offline, read-only command,
not part of any hot-path benchmark tracked in `docs/benchmarks.md`; the
no-`--format` case is unchanged in shape and cost (one extra branch
check). No re-benchmark needed.

## Still ahead

The broader survey that found this also checked (and ruled out as
genuinely bigger, not just superficially so) `COPY`/`ADD --link`
(would need the build executor's own whole-rootfs-diff model
restructured into an isolated-layer-then-merge one, a real
architectural change, not a parser addition), `ociman search` (needs
an entirely new, non-pull/push registry-catalog client), and `ociman
generate`/`kube` (a large, structurally new Kubernetes-YAML-targeting
feature) — none of these are small candidates despite surface
appearances. `ociman history --format`/`ociman stats --format` remain
real, small, immediate follow-ons reusing this exact same engine
(per-row, like `ps`/`images`/`volume ls`, rather than single-object
like `inspect`/`info`) — deliberately not done in this same note to
verify each command's own real behavior individually first, matching
this whole family's own established pattern. A possible, smaller-than-
`--link` candidate, `COPY --exclude=<pattern>`, reuses this project's
own already-threaded `DockerIgnore` filter machinery but needs its own
dedicated scoping pass before committing to it. `ociman`/`ocirun`'s
other remaining gaps (`--restart` policy, `--console-socket`) and
`ocibox`'s own remaining gaps (`stop`/`upgrade`/`generate-entry`/
`assemble`, `export --sudo`/`--enter-flags`) remain separately-scoped
future candidates.
