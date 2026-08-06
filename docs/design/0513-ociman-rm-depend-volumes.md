# Design note 0513: `ociman rm --depend`/`--volumes`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## What this closes

Two never-before-examined real `podman rm` flags (`grep -iln
"\-\-depend" docs/design/*.md` found zero hits before this note --
genuinely fresh territory, not a re-litigated old call): `--depend`
("Remove container and all containers that depend on the selected
container") and `--volumes`/`-v` ("Remove anonymous volumes
associated with the container"). Both map to concepts this project
genuinely has none of, so both are real, faithful no-ops here --
the same class of finding `0510`'s own `crun create --no-subreaper`
and `0512`'s own `volume reload` already established, just applied
to two brand-new flags rather than a re-examined old deferral.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/rm.go:66-68`: `--depend`
  (`rmOptions.Depend`) and `--volumes`/`-v` (`rmOptions.Volumes`),
  both real, documented flags on both `rm` and `container rm`
  (`rmFlags`, shared by both registrations).
- `~/git/podman/pkg/domain/infra/abi/containers.go:467-469`:
  `if options.All || options.Depend { ... RemoveContainerAndDep
  endencies(...) } else { ... RemoveContainer(...) }` -- `--depend`
  only ever changes anything by widening removal to a container's
  own *dependency graph*.
- `~/git/podman/libpod/runtime_ctr.go:645-653`
  (`RemoveContainerAndDependencies`'s own doc comment): "This may
  include pods (if the container or any of its dependencies is an
  infra or service container...). **Otherwise, it functions
  identically to RemoveContainer.**" This project has neither
  concept anywhere at all -- no inter-container namespace-sharing
  (`--net=container:`/`--ipc=container:`/etc., confirmed absent by
  grep), no pods, no infra/service containers -- so a container's
  own dependency graph here is always just itself alone, the "no
  dependents" branch that behaves identically to plain removal.
- `~/git/podman/libpod/runtime_ctr.go:1035-1041`: `--volumes`'s own
  real removal loop only ever removes an attached volume when
  `volume.Anonymous()` is true, `continue`-ing past every other one.
  This project's own volume schema has no anonymous-vs-named
  distinction anywhere at all (every volume here is always
  explicitly named, `VolumeCommand::Prune`'s own doc comment already
  established this reasoning for `--filter anonymous=`) -- so that
  condition can never be true here, and the loop always continues
  past every attached volume regardless.

## Implementation

`Command::Rm` gains `depend: bool` (`--depend`) and `volumes: bool`
(`--volumes`/`-v`), both accepted-and-discarded (`depend: _,
volumes: _`) in the dispatch arm, matching the established
"accepted for real CLI compatibility but changes nothing" convention
(`ocibox rm --force`'s own doc comment). `cmd_rm`'s own signature is
untouched. The byte-identical `ContainerCommand::Rm` alias (`0489`)
gains the identical two fields too, for the same reason its every
other field already mirrors `Command::Rm`'s own.

## Tests

One new integration test in `tests/tests/ociman_ps.rs` (where every
other `rm` test already lives):
`rm_depend_and_volumes_are_accepted_and_behave_identically` -- both
flags given together on a real, just-run container still remove it
exactly as a plain `rm` would.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures -- no new test file added, so the
block count is unchanged from `0512`; clean on the first attempt
with `RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo
deny check` (clean), `bash ci/native-ci.sh` (clean on the first
attempt with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean
on the first attempt, real `dpkg -i`/`--version`/`dpkg -r` round
trip). Pure CLI-dispatch-layer plumbing, `cmd_rm`'s own body
untouched -- no hot path touched, no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

The two flags' own real underlying subsystems (inter-container
namespace-sharing/pods, anonymous-volume tracking) remain entirely
unimplemented -- this increment's own correctness rests specifically
on this project never reaching a state where either flag's real
target could ever exist, not on faithfully reproducing the removal
logic itself.
</content>
