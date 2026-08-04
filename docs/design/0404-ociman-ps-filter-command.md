# Design note 0404: `ociman ps --filter command=<substring>`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`, `README.md`.

## What this closes

Real `podman ps --filter command=` — matching against a container's
own first command element (the executable itself) — had no
equivalent in `ociman ps`'s existing `PsFilters`, which already
supported `status=`/`id=`/`name=`/`label=`/`before=`/`since=`/
`ancestor=`/`exited=`/`until=`.

## Real, checked-directly confirmation

- `~/git/podman/pkg/domain/filters/containers.go` (both the
  `libpod.Container` and `types.ListContainer` filter-function
  tables): `case "command": ... util.StringMatchRegexSlice(c.
  Command()[0], filterValues)` — a real regex match, but critically
  only against **`Command()[0]`**, the container's own first command
  element (the executable path/name), never its arguments.
- `~/git/podman/pkg/util/utils.go`'s own `StringMatchRegexSlice`:
  `regexp.MatchString(r, s)` — Go's own unanchored (substring-
  equivalent for plain alphanumeric patterns, no metacharacters)
  regex search, the exact same simplification this project's own
  `name=` filter already documents and applies (avoiding a new,
  direct `regex` dependency this project has nowhere else).

## Implementation

- `PsFilters` gains `command: Vec<String>`; `parse_ps_filters` gains
  a `command=` branch (identical shape to the existing `name=`
  branch — non-empty-value check, OR'd together).
- The filter closure in `cmd_ps` extracts the *first* whitespace-
  separated token from the container's own stored, space-joined
  `ANNOTATION_COMMAND` value (`process.args.join(" ")`, already
  recorded at creation time) — the equivalent of real podman's own
  `Command()[0]` — and substring-matches it against each given value,
  the same "avoid a new regex dependency" simplification `name=`
  already established, applied to the correct (first-element-only)
  scope rather than the whole command line.
- Updated `Command::Ps`'s own `--filter` doc comment and the
  "not yet supported" error message's key list.

## A real, previously-latent naive-implementation trap avoided

A naive port would substring-match the *entire* stored command
string (all arguments included), which would silently diverge from
real podman's own scope: a container running `/bin/sh -c "sleep 300"`
would then incorrectly match `--filter command=sleep` (a real
argument, not the executable), something real podman's own
`Command()[0]`-only scope never does. Caught before shipping by
writing the test below *first* and confirming it would have failed
against that naive approach, not discovered afterward.

## Tests

One new end-to-end integration test in `tests/tests/ociman_ps.rs`,
`ps_filter_command_matches_only_the_first_command_element` — two
containers with genuinely different executables (`true` vs.
`/bin/sh -c "sleep 300"`), proving `command=true`/`command=sh` each
match exactly the right one, while `command=sleep` (an argument of
the second container, never its own first element) matches neither —
the real distinction described above, proven live rather than
assumed. All existing tests continue to pass unmodified (49/49 in
`ociman_ps.rs`).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This touches only `ociman ps`'s own filter-matching path, not
`launch.rs`'s hot path at all — no benchmark re-run needed.

## Deliberately still out of scope

Real podman's own remaining `ps`/image filter keys surveyed alongside
this one (`should-start-on-boot`, `network`, `pod`, `volume`) each
need a real subsystem (boot policy, container networking, pods,
volume-mount tracking per container) this project either has none of
at all or hasn't wired into `ps` specifically — real, separate,
unrelated gaps, matching this project's own already-documented scope
limits.
