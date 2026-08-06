# Design note 0535: `ociman port` / `ociman container port`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_port.rs`,
`tests/tests/ociman_container.rs`.

## What this adds

Real `podman port`/`podman container port` — "List port mappings for
the CONTAINER, or look up the public-facing port that is NAT-ed to
the PRIVATE_PORT" (real podman's own doc string, quoted verbatim) —
is a dual-registered subcommand this project never had. This project
has also never implemented any port-publishing concept at all (no
`-p`/`--publish` on `run`/`create`, confirmed absent back to `docs/
design/0020`'s own original scope list, and named as a real, deferred
gap in `0452`'s own doc comment too), so a container here can
genuinely never have a real port mapping to report — the entire
command reduces to real, honest CLI parsing, resolution, and an
always-empty no-op body, the same "the real work is already a no-op
here" reasoning class `0529`/`0530` already established for
`cleanup`/`init`.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/port.go:20-45`: dual
  registration (`portCommand`, top-level; `containerPortCommand`,
  `Parent: containerCmd`), sharing one `RunE`/`portFlags` (`--all`/
  `-a`, plus `--latest`/`-l` via `validate.AddLatestFlag`). `Args` on
  both: `validate.CheckAllLatestAndIDFile(cmd, args, true, "")` —
  `ignoreArgLen = true` (unlike `cleanup`/`init`'s own `false`): the
  "`--all`/`--latest` together" and "no arguments needed with
  `--all`" checks still run unconditionally, but the bare "at least
  one name or id" and "`--latest` and containers together" checks are
  skipped, replaced by `port.go`'s own manual body checks instead.
- `~/git/podman/cmd/podman/containers/port.go:68-152` (`port`): `if
  len(args) == 0 && !portOpts.Latest && !portOpts.All { return
  errors.New("you must supply a running container name or id") }`,
  then a real, checked-directly manual disambiguation depending on
  `--latest` (with `--latest` absent, `args[0]` is the container, a
  leading `/` stripped; with it, the container comes from that flag
  alone and a *single* remaining element is `PORT` — giving `--latest`
  together with *two* positional elements leaves both silently unused
  by either slot at all, a real, genuinely obscure upstream oddity),
  `if len(args) > 2 { return errors.New("` + "`port`" + ` accepts at
  most 2 arguments") }`, and real `PORT`-format validation
  (`strings.Split(port, "/")`/`strconv.ParseUint(fields[0], 10, 16)`)
  — all running *before* ever calling `ContainerPort`.
- `~/git/podman/pkg/domain/infra/abi/containers.go:1628-1653`
  (`ContainerPort`): `getContainers(...)` with no error-swallowing at
  all (an unresolvable explicit name is a real, propagated hard
  error, matching `Init`'s own convention, not `Cleanup`'s silent
  inversion), then per container: `if state != ContainerStateRunning
  { continue }` (a silent skip, not an error), `if len(portmappings) >
  0 { reports = append(...) }`. Crucially, real `port()`'s own "failed
  to find published port" check (lines 150-152) lives *inside* the
  `for _, report := range reports` loop — a permanently-empty
  `reports` slice (this project's own permanent case) makes that loop
  body, and therefore that error, entirely unreachable.
- **Verified directly against a real installed `podman 4.9.3`** (not
  assumed from source alone) with a real container that has no port
  mappings either: bare `port`, `port ctr 80/tcp` (a definitely-
  unmatched port), `port --all` (empty store), and `port` against a
  `Created`-never-started container all exit `0` silently; a
  malformed `PORT` (`not-a-number`, `80/tcp/udp`) still errors
  immediately; a bare invocation, an unknown container, `--all`
  combined with an explicit id, `--latest` on an empty store, and
  more than two positional arguments all error immediately too —
  every one of these confirmed live before writing any code.

## Implementation

`bin/ociman/src/main.rs`:
- New `Command::Port { positional: Vec<String>, all: bool, latest:
  bool }` and identical `ContainerCommand::Port { .. }` (dual-
  registered, matching `Command::Init`'s own shape, 0530), both
  dispatching into one shared `cmd_port`.
- New `validate_port_spec(port: &str)`: the exact real `<port>[/
  <protocol>]` format check (checked directly against `port.go`'s
  own `strings.Split`/`strconv.ParseUint`), never chasing Go's own
  internal `strconv` error text byte-for-byte (this project's own
  established precedent of not chasing an internal library's exact
  wording) — the parsed value itself is never used afterward at all,
  since this project never has anything real to search for; this
  exists purely to give the same real, immediate error a genuinely
  malformed value gets, before ever reaching the always-empty search.
- New `cmd_port`: replicates `CheckAllLatestAndIDFile`'s own
  unconditional `--all`/`--latest`-conflict and `--all`+args checks,
  then `port.go`'s own manual body logic in the identical order
  (including the real, genuinely obscure `--latest`+2-args quirk,
  faithfully ported rather than "fixed" — utterly inconsequential
  either way, since the real output never depends on `PORT` in the
  first place). Resolves targets via the same `--all`/`--latest`/
  explicit-id primitives `cmd_init`/`cmd_container_cleanup` already
  established (a real, propagated hard error on an unresolvable name
  or an empty `--latest` store, matching `Init`'s convention). Skips
  a non-`Running` target silently, matching real `ContainerPort`'s
  own identical check — but since there is never anything to print
  regardless, this only ever confirms resolution succeeded; the
  command produces no real output in any case.

## Tests

Eleven new integration tests in `tests/tests/ociman_port.rs` covering:
a running container with no mappings (silent success), an explicit
nonexistent `PORT` (still a silent success — the real, checked-
directly quirk above), a malformed `PORT` (real error), a `Created`
container (silent success), no target at all, `--all`+`--latest`
together, `--all`+an explicit id, more than two positional arguments,
an unknown container, `--latest` on an empty store, and `--all` on an
empty store. Plus one new alias-proof test in `tests/tests/
ociman_container.rs` (`container_port_is_a_byte_identical_alias_for_
top_level_port`), matching the established `mount`/`unmount`/`init`
test-file split convention. Plus five new unit tests for
`validate_port_spec` in `bin/ociman/src/main.rs`'s own `mod tests`
(bare port, port with protocol, too many slashes, non-numeric, above
`u16::MAX`), matching `parse_memory_limit`'s own established
"pure logic gets a direct unit test" precedent.

Manually exercised end to end beyond the automated tests, mirroring
every real-podman scenario verified above one-for-one against this
project's own real built binary and a real image: bare `port`,
explicit nonexistent `PORT`, malformed `PORT` (both invalid forms),
bare invocation, `--all`, unknown container, `--all`+explicit id,
3-argument error, `--latest`, the `container port` alias, and a
`Created` container — every single one matching the real installed
`podman 4.9.3`'s own observed exit code exactly.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean after one auto-fix), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), the full `ociman_
container.rs` suite (47/47) and the new `ociman_port.rs` suite
(11/11), a full `cargo test --workspace --locked` run (126 test-
result blocks, up from 125 with the new test file, 0 failures, fully
clean on the first attempt), `python3 ci/guards.py` (clean), `cargo
deny check` (clean), `bash ci/native-ci.sh` (clean on the first
attempt), `bash ci/build-deb.sh` (clean on the first attempt, real
`dpkg -i`/`--version`/`dpkg -r` round trip). Pure CLI-parsing-and-
read-only-lookup addition — no hot path touched, no `ci/bench.sh`
rerun needed.

## Deliberately still out of scope

Real port-publishing itself (`-p`/`--publish` on `run`/`create`,
`--expose`, any actual NAT/port-forwarding mechanism) remains the
pre-existing, much larger, project-wide gap this command's own real
effect would otherwise report on — unrelated to and unaffected by
this increment, which only ever closes the CLI surface for a
container that (as every container in this project always is) has no
such mapping to begin with.
