# Design note 0332: `ociman inspect --format`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_inspect.rs`,
`README.md`.

## What this closes

`ociman inspect` had no way to extract a single field for scripting —
real `podman inspect --format '{{.State.Pid}}'`/`docker inspect
--format` is an extremely common real-world pattern (extracting a pid,
a status, an IP, ...) with no equivalent here at all: `--json` printed
the *entire* record either way, requiring a separate `jq`/`python -c`
pipeline for anything more targeted. Flagged in a fresh, broader survey
(not limited to `ocibox`, whose small remaining gaps had converged onto
"materially bigger" items) as the single most reusable real-world
feature still missing.

## Scope: deliberately narrow, matching this project's own convention

Real Go `text/template` (what `podman`/`docker --format` actually use)
supports pipelines, conditionals, range loops, and a large builtin
function library. This is a genuinely large surface to match fully —
so, matching this project's own established "narrow, honest first
slice" pattern (e.g. `0327`'s icon handling, `0330`'s `--extra-flags`),
this implements only the single most common real usage: one or more
`{{.path.to.field}}` placeholders, dot-separated JSON field lookups,
substituted into otherwise-literal surrounding text. No pipelines
(`{{.Field | println}}`), no functions, no control flow (`{{range}}`/
`{{if}}`) — a real, honest, narrower slice, not silently pretending to
be a full template engine.

**Field-name casing is this project's own JSON output, not real
podman's Go-struct casing.** Real `podman inspect --format` operates on
podman's own internal Go struct, whose exported field names are
PascalCase Go identifiers (`{{.State.Pid}}`) — not necessarily even the
same names as podman's own JSON output keys. This project's own JSON
schema is already a deliberately narrower, differently-shaped thing
(`ContainerInspectView`'s own plain, lowercase Rust field names via
serde's own default `snake_case`-preserving behavior, e.g. `pid`,
`stop_signal`; the image config's own nested `Config`/`RootFS` keep the
real OCI image-spec's own PascalCase field names, e.g. `Cmd`, `Env`,
since that spec genuinely uses that casing). Promising byte-for-byte
template compatibility with real podman would be actively misleading
given the schema itself already differs — so this project's own
`--format` templates address its own JSON output field names directly,
documented clearly in the flag's own help text.

An unresolvable field path is a real, immediate error, matching real Go
templates' own actual behavior (`executing "tmpl" at <.NoSuchField>:
can't evaluate field NoSuchField in type ...` is a real template
*execution* error in Go, not a silently-empty result) — not a
`--format`-specific design choice, but genuine fidelity to what real
`--format` actually does on a typo.

## Implementation

`Command::Inspect` gained `format: Option<String>` (`--format`/`-f`).
`cmd_inspect`'s own two resolution branches (container, then image)
both now go through a new, shared `print_inspect_result` helper:
`--format`, when given, takes priority over `--json` (matching real
podman's own identical precedence) and renders the template via a new
`render_format_template`; otherwise the existing pretty-JSON-either-way
behavior (a pre-existing quirk — `--json` and no-`--json` already
printed identically before this note, untouched here) is unchanged.

`render_format_template` scans for `{{`/`}}` delimiter pairs, resolves
each placeholder's own dot-path via `resolve_json_path` (a plain,
one-segment-at-a-time `serde_json::Value::get` walk), and renders the
resolved value via `format_json_scalar`: a string prints with no
surrounding quotes, a number/bool prints its own natural
representation, `null` prints empty — matching Go's own default
`fmt.Sprint`-based scalar rendering exactly. An object/array (this
project's own template engine has no object/array-specific formatting
verb at all yet, unlike Go's own `map[k:v]`/`[a b c]` syntax) prints as
its own compact JSON representation instead — a real, deliberate
narrowing, not a silent misrepresentation.

## Verified

`cargo build -p ociman --locked`; manual smoke test with a real,
running container: `ociman inspect <name> --format '{{.pid}}'`,
`--format '{{.status}}'`, a combined multi-placeholder template, and a
typo'd field name (a real, immediate error) all behave as designed.
Also confirmed against a real image: `ociman inspect <ref> --format
'{{.config.Cmd}}'` (a nested field, into the OCI-spec-cased `Config`)
renders the array as compact JSON.

Five new integration tests in `tests/tests/ociman_inspect.rs` (12
total, 7 pre-existing, all pass unchanged): a single scalar string
field renders unquoted; multiple placeholders plus literal text
interpolate correctly, including a numeric field rendering as a plain
number; a nested field on an image config resolves and an array prints
as compact JSON; and an unknown field is a real, immediate error.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test result blocks,
0 failures — two known, previously-documented occasional single-test
flakes in `ocicri_container.rs` hit during this turn's full local
verification, both re-verified passing in isolation and the full
suite/`ci/native-ci.sh` re-run clean afterward), `python3 ci/guards.py`,
`cargo deny check`, `ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg
-i`/`--version`/`dpkg -r` round trip).

Performance: `ociman inspect` is a one-shot, offline, read-only
command, not part of any hot-path benchmark tracked in
`docs/benchmarks.md`; the no-`--format` case is unchanged in shape and
cost (an extra branch check only). No re-benchmark needed.

## Still ahead

`ociman ps --format`/`ociman images --format` are natural, small
follow-on candidates reusing this exact same template engine
(`render_format_template`/`resolve_json_path`/`format_json_scalar`
would need no changes at all, just a second/third call site) — real
users reach for `docker images --format '{{.Repository}}:{{.Tag}}'`
just as often as `inspect --format`. Deliberately not done in this same
note to keep this one narrowly scoped to `inspect` first and verify the
engine's own real behavior end to end before reusing it elsewhere.
`ociman`/`ocirun`'s own other remaining gaps (`--restart` policy,
`ocirun run --no-pivot`/`--console-socket`) and `ocibox`'s own
remaining gaps (`stop`/`upgrade`/`generate-entry`/`assemble`,
`export --sudo`/`--enter-flags`) remain separately-scoped future
candidates.
