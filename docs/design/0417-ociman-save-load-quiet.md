# Design note 0417: `ociman save --quiet` / `ociman load --quiet`

Status: implemented
Scope: `crates/oci-cli-common/src/progress.rs`, `bin/ociman/src/
main.rs`, `tests/tests/ociman_save.rs`, `tests/tests/ociman_load.rs`,
`README.md`.

## What this closes

`ociman save`/`ociman load` have always unconditionally shown a
progress spinner while working. Real `podman save --quiet`/`podman
load --quiet` (and their `docker` equivalents) both let a caller
suppress it — a real, genuine semantic match to a mechanism this
project already has (unlike some other `--quiet`-shaped flags real
tools have that this project has no target for at all, see this
note's own "deliberately still out of scope" section on `ociman
commit --quiet`).

## Real, checked-directly confirmation

`~/git/podman/pkg/domain/infra/abi/images.go`:
- `SaveImage`: `if !options.Quiet { saveOptions.Writer = os.Stderr }`
- `LoadImage`: `if !options.Quiet { loadOptions.Writer = os.Stderr }`

Both real functions gate the *entire* progress writer on `!Quiet` —
exactly the same shape as this project's own single `progress`
variable already being conditionally created. `~/git/podman/cmd/
podman/images/save.go:93`/`load.go:64` both register the flag
identically: `flags.BoolVarP(&opts.Quiet, "quiet", "q", false,
"Suppress the output")`.

## Implementation

- `crates/oci-cli-common/src/progress.rs` gains
  `spinner_unless_quiet(quiet: bool, msg)`, returning a real, always-
  `ProgressBar::hidden()` bar when `quiet` is set, or plain `spinner`
  otherwise — the shared backing both `ociman save --quiet`/`ociman
  load --quiet` reuse. Deliberately not relying on `spinner`'s own
  existing auto-hide-on-non-tty behavior alone: that only ever
  depends on the *stream*, not on anything the caller explicitly
  asked for, so `--quiet` needs a real, separate, unconditionally
  hidden path to have any observable effect on a real terminal at
  all.
- `Command::Save` and `Command::Load` each gain a `#[arg(short,
  long)] quiet: bool` field; `cmd_save`/`cmd_load` each gain a
  `quiet: bool` parameter, replacing their own `progress::spinner`
  call with `progress::spinner_unless_quiet(quiet, ...)`.

## Tests

Two new unit tests in `crates/oci-cli-common/src/progress.rs`:
`spinner_unless_quiet_is_unconditionally_hidden_when_quiet` (a real,
environment-independent property — must hold regardless of whether
the test process itself has a real terminal attached to stderr) and
`spinner_unless_quiet_of_false_behaves_exactly_like_plain_spinner`
(proves `quiet: false` is indistinguishable from calling `spinner`
directly, not its own separately-hidden path).

Two new integration tests, `save_quiet_still_writes_a_correct_
archive` (`tests/tests/ociman_save.rs`) and `load_quiet_still_loads_
correctly` (`tests/tests/ociman_load.rs`): the spinner only ever
draws to stderr and is already automatically hidden whenever stderr
isn't a real terminal (true of this whole automated suite), so
there's no separately observable stdout/stderr difference to assert
on at the CLI level — what's real and checkable is that the flag is
accepted and the archive/image it produces/loads is still correct.
All prior tests in both files continue to pass unmodified (8/8 in
`ociman_save.rs`, 6/6 in `ociman_load.rs`).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures on a clean run — one earlier attempt hit the
known, pre-existing `ocicri_container.rs` host-contention flake,
confirmed environmental via an immediate isolated rerun),
`python3 ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`
(clean, 119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip). Touches only the save/load progress-spinner
path, not any hot path at all — no benchmark re-run needed.

## Deliberately still out of scope

`ociman commit --quiet` was considered alongside this — real
podman's own `commit()` has the identical `if !commitOptions.Quiet {
commitOptions.Writer = os.Stderr }` shape (`~/git/podman/cmd/podman/
containers/commit.go`) — but this project's own `commit_inner` has
**no** progress writer/spinner at all yet to suppress. Implementing
`--quiet` there today would be a true no-op flag accepted only for
CLI-surface compatibility, not a real behavior change — a weaker fit
than `save`/`load`'s own genuine, already-present mechanism, left for
whenever `ociman commit` itself grows a real progress indicator (a
separate, bigger gap: layer diffing/tar-writing large images can
take real, visible time real podman already shows progress for).
