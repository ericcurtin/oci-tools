# Design note 0540: `ocibox --root`/`-r` across `create`/`list`/`rm`/`stop`/`enter`/`ephemeral`/`generate-entry`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_create.rs`,
`tests/tests/ocibox_list_rm.rs`, `tests/tests/ocibox_enter.rs`,
`tests/tests/ocibox_ephemeral.rs`, `tests/tests/ocibox_generate_entry.rs`.

## What this closes

Real `distrobox`'s `--root`/`-r` is a cross-cutting flag, applied via
one shared `withRoot` composition to `list`/`generate-entry`/`create`/
`enter`/`rm`/`stop`/`ephemeral` — deliberately *not* `export`. This
project's `ocibox` had none of them at all — a real CLI flag every
one of those seven commands would reject as unrecognized.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/root.go:106-169` (`subcommands`):
  `list`/`generateEntry`/`create`/`enter`/`rm`/`stop`/`ephemeral` are
  all composed with `withRoot`; `assemble` and (checked directly by
  omission from every composition) `export` are not.
- `~/git/distrobox/internal/cli/root.go:259-266` (`withRoot`) — this
  is where the flag is actually *registered*, not in any one
  command's own `Flags` list: `cmd.Flags = append(cmd.Flags,
  &cli.BoolFlag{Name: "root", Aliases: []string{"r"}, ...})`. This is
  exactly why a plain read of, say, `stop.go`'s own source alone (its
  own directly-declared flags are only `--all`/`-a` and `--yes`/
  `-Y`) would miss this flag entirely — confirmed by tracing
  `root.go`'s own composition wiring directly rather than assuming
  from any single command's own file.
- `~/git/distrobox/pkg/containermanager/providers/podman.go:41-56,
  387,416,476-478` — confirms `root` is a genuinely live, consumed
  value in real distrobox (not dead code): `newPodman(command, root,
  sudoCommand, ...)`, later `if p.root { ... command = p.sudoCommand
  ... }`, toggling whether every generated command actually runs
  through `sudo`.
- No `distrobox` binary is installed on this host to cross-check live
  (only podman/docker/crun/runc are) — verified directly against
  source only, the same verification depth `0531`/`0536` already
  established for this binary.

## Why this is a real, faithful no-op here

This project's own `ocibox` has no rootful/rootless distinction of
any kind at all: every box is always the real, checked-directly
equivalent of real distrobox's own rootless default, with no
alternate, privilege-elevated code path to switch into in the first
place. Accepting `--root` is therefore a genuine, faithful no-op —
not a half-implemented approximation of real root support — the same
"accept the real flag, honestly error on the unsupported value" class
this project has already established repeatedly for flags whose real
target concept simply doesn't exist here.

## Implementation

`bin/ocibox/src/main.rs`: one canonical, fully-cited doc comment on
`Command::Create::root: bool` (`#[arg(long, short = 'r')]`), with
the identical field on `Command::List`/`Rm`/`Stop`/`Enter`/
`Ephemeral`/`GenerateEntry` each cross-referencing it briefly (the
same "same reasoning X already gives" convention this file already
uses throughout). `-r` had no collision anywhere in the file
(confirmed via a full grep of every existing `short = '...'` before
choosing it, matching real distrobox's own exact alias). Accepted and
immediately discarded (`root: _`) at all seven dispatch sites;
`export` deliberately gets no `--root` field at all, matching real
distrobox's own identical exclusion — not merely an oversight.

## Tests

Seven new integration tests, one per command, each proving `--root`
parses and produces byte-identical (or equivalently successful)
real behavior versus the same invocation without it:
- `create_root_flag_is_accepted_and_behaves_identically`
  (`ocibox_create.rs`)
- `list_root_flag_is_accepted_and_behaves_identically`,
  `rm_root_flag_is_accepted_and_behaves_identically`,
  `stop_root_flag_is_accepted_and_behaves_identically`
  (`ocibox_list_rm.rs`)
- `enter_root_flag_is_accepted_and_behaves_identically`
  (`ocibox_enter.rs`)
- `ephemeral_root_flag_is_accepted_and_behaves_identically`
  (`ocibox_ephemeral.rs`)
- `generate_entry_root_flag_is_accepted_and_behaves_identically`
  (`ocibox_generate_entry.rs`)

Manually exercised end to end beyond the automated tests: a real
image built via `ociman build`, then `ocibox create --root`, `list
--root`, `enter --root` (a real command executed inside, output
verified), `stop --root`, and `rm --root`, each confirmed identical
to the same invocation without the flag; confirmed `ocibox export
--help` genuinely has no `--root`/`-r` at all, matching real
distrobox's own identical exclusion.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean after one auto-fix), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), the full test suites
for every affected file (`ocibox_create.rs` 14/14, `ocibox_list_rm.rs`
25/25, `ocibox_enter.rs` 18/18, `ocibox_ephemeral.rs` 10/10,
`ocibox_generate_entry.rs` 7/7), multiple full `cargo test --workspace
--locked` runs (this host had several genuinely concurrent `opencode`
sessions active throughout; the already-documented transient
`ocicri_container.rs` and `ociman_run.rs` cgroup-write flakiness each
showed up once, both confirmed transient by isolated rerun; a fully
clean run: 126 test-result blocks, 0 failures), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (clean on
the first attempt), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). Pure
CLI-parsing-and-discard addition across seven commands — no hot path
touched, no `ci/bench.sh` rerun needed.
