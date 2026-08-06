# Design note 0536: `ocibox rm --verbose`/`-v`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_list_rm.rs`.

## What this closes

Real `distrobox` has a root-level, inherited `--verbose`/`-v` global
flag every subcommand (including `rm`) accepts. `ocibox rm` had no
equivalent at all — a real CLI flag it would reject as unrecognized.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/root.go:76-81`: `--verbose`/`-v` is a
  real, root-level global flag (`cli.BoolFlag{Name: "verbose",
  Aliases: []string{"v"}, ...}`), inherited by every subcommand
  including `rm` — `rm.go` itself declares no local `verbose` flag of
  its own (confirmed via `internal/cli/parse.go:45`'s own comment:
  "Scan with the sub-command's flags plus the inherited globals
  (--verbose)."), yet reads it successfully.
- `~/git/distrobox/internal/cli/rm.go:68`: `Verbose: cmd.Bool
  ("verbose")` on the constructed `RmOptions`.
- Traced its entire real chain of custody directly (not assumed):
  `~/git/distrobox/pkg/commands/rm.go`'s own `removeContainer` never
  passes it into the actual `containerManager.Remove` call at all
  (only `Force`/`RemoveHome`/`ContainerHome`, lines 158-162) — its
  one remaining use is `cleanup`'s own `GenerateEntryOptions.Verbose`
  field (lines 187-193), which `~/git/distrobox/pkg/commands/
  generate_entry.go`'s own `Execute` *declares* (line 35) but
  **never actually reads anywhere in its own body** (confirmed by
  exhaustive grep across that whole file) — genuinely dead, unused
  input in real distrobox itself at this exact commit, not merely a
  flag this project has no equivalent mechanism for.
- The provider-level `verbose` struct field (`~/git/distrobox/pkg/
  containermanager/providers/podman.go:27,41-51`, set from this same
  global flag at container-manager-construction time) is *also*
  confirmed dead — `grep -n "p\.verbose" providers/*.go` finds zero
  matches anywhere; the *only* real, live consumer of any `verbose`
  value anywhere in the whole codebase is `generateEnterCommand`'s
  own separate, explicitly-passed parameter (`podman.go:816-821`,
  appending `--log-level debug` to the generated `podman exec`
  command) — used only by `enter`, never `rm`.

No `distrobox` binary is installed on this host to cross-check live
(only podman/docker/crun/runc are) — verified directly against
source only, the same verification depth `0531`'s own identical
`ocibox enter --no-tty` note already established for this binary.

## Implementation

`bin/ocibox/src/main.rs`: new `Command::Rm::verbose: bool`, `#[arg
(long, short = 'v')]` (no short-flag collision on `Rm` — unlike
`Create`/`Ephemeral`, where `-v` is already this project's own
established convenience alias for `--volume`, confirmed directly
before choosing this flag as the cleaner of two real dead-code
candidates the same source audit turned up). Accepted and
immediately discarded (`verbose: _`) at the one dispatch site,
exactly like `force`/`yes`/`rm_home` already are; `cmd_rm`'s own
function signature is untouched.

## Tests

One new integration test in `tests/tests/ocibox_list_rm.rs`:
`rm_verbose_flag_and_its_short_alias_are_accepted_and_behave_identically`
— proves both `--verbose` and `-v` parse and still genuinely remove a
real box exactly as a plain `rm` would.

Manually exercised end to end beyond the automated test: a real image
built via `ociman build`, two real boxes created, then `ocibox rm
--verbose`/`ocibox rm -v` against each, confirmed identical output
and a fully removed box directory in both cases.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean after one auto-fix), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), the full `ocibox_
list_rm.rs` suite (22/22), a full `cargo test --workspace --locked`
run (this host had several genuinely concurrent `opencode` sessions
active throughout; one run hit a single, isolated `ociman_run.rs`
cgroup-write test failure, `run_cgroup_conf_flag_writes_a_real_
cgroup_v2_file`, confirmed transient by an immediate isolated rerun
— a new specific test in this class versus prior turns, but the same
already-documented concurrent-session-driven cgroup/systemd
contention; a fully clean second run: 126 test-result blocks, 0
failures), `python3 ci/guards.py` (clean), `cargo deny check`
(clean), `bash ci/native-ci.sh` (clean on the first attempt), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). Pure CLI-parsing-and-discard
addition — no hot path touched, no `ci/bench.sh` rerun needed.
