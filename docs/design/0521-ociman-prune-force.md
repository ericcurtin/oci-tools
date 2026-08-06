# Design note 0521: `--force`/`-f` on `prune`, `system prune`, `image prune`, `volume prune`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_prune.rs`,
`tests/tests/ociman_image_prune.rs`, `tests/tests/ociman_volume.rs`.

## What this closes

Four of the five real `prune`-family commands this project already
implements were missing `--force`/`-f` entirely: `Command::Prune`
(flat top-level `ociman prune`), its `SystemCommand::Prune` alias
(`ociman system prune`), `ImageCommand::Prune` (`ociman image
prune`), and `VolumeCommand::Prune` (`ociman volume prune`). Only
`ContainerCommand::Prune` (`0418`) already had it, as an accepted-
and-ignored no-op. Confirmed by grepping every `Prune {` block in
`bin/ociman/src/main.rs` before this note, and confirmed genuinely
unexamined -- no design note (`0111`/`0117`/`0181`/`0192`/`0198`/
`0357`/`0359`/`0433`/`0485`) ever discussed or deferred it.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/system/prune.go:47`: `flags.BoolVarP
  (&force, "force", "f", false, ...)`.
- `~/git/podman/cmd/podman/images/prune.go:47`: `flags.BoolVarP
  (&force, "force", "f", false, "Do not prompt for confirmation")`.
- `~/git/podman/cmd/podman/volumes/prune.go:46`: `flags.BoolP
  ("force", "f", false, "Do not prompt for confirmation")`.

In every case its only real effect is skipping an interactive "Are
you sure?" confirmation prompt (`bufio.NewReader(os.Stdin)` +
`reader.ReadString('\n')`) -- this project has no such prompt
anywhere to skip in the first place, the identical "nothing to skip"
reasoning `ContainerCommand::Prune::force` (`0418`) and `SystemCommand
::Reset::force` (`0198`) already established.

## Implementation

`force: bool` (`#[arg(short, long)]`) added to `Command::Prune`,
`SystemCommand::Prune`, `ImageCommand::Prune`, and `VolumeCommand::
Prune`, each accepted and immediately discarded (`force: _`) at its
own dispatch site. None of `cmd_prune`/`cmd_image_prune`/
`cmd_volume_prune`'s own function signatures changed at all.

## Tests

Three new integration tests: `prune_force_flag_is_accepted_and_
behaves_identically` (`tests/tests/ociman_prune.rs`, exercising both
`prune --force` and its `system prune -f` alias), `image_prune_force_
flag_is_accepted_and_behaves_identically` (`tests/tests/
ociman_image_prune.rs`), and `volume_prune_force_flag_is_accepted_
and_behaves_identically` (`tests/tests/ociman_volume.rs`) -- each
proving a real reclaim still happens exactly as a plain, unforced
prune would.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (123
test-result blocks -- no new test file added, so the block count is
unchanged from `0520`; the documented transient `ocicri_container.rs`
flakiness under this host's own persistent CPU contention (plus a
second, genuinely concurrent process observed this session) showed
up once, confirmed transient by rerunning the specific failing test
in isolation -- passed -- then a clean full-suite rerun), `python3
ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (clean on the first attempt with
`RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). Pure
CLI-parsing addition -- no hot path touched, no `ci/bench.sh` rerun
needed.

## Deliberately still out of scope

Bare-invocation "list every currently-cached image" mode/`--format`
for `ociman image mount`/`unmount` (`0519`/`0520`'s own still-open
gaps), `ocibox upgrade`/`export --app` (genuinely bigger), and
`ocibox enter --yes`/`-y` (a likely faithful no-op, same class as
this note, independently identified but not yet implemented) remain
open candidates for future increments.
</content>
