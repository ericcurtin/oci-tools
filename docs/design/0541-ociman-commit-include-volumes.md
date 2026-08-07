# Design note 0541: `ociman commit --include-volumes`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_commit.rs`.

## What this closes

`Command::Commit`'s own doc comment (added in `0537`) explicitly named
`--config`/`--include-volumes` as its last two open gaps. This closes
`--include-volumes`, on both the top-level `ociman commit` and the
`ociman container commit` alias (`0501`) -- `--config` (an arbitrary
override-config JSON blob merge) remains out of scope, being a much
larger surface with no equally small, well-scoped slice.

## Real, checked-directly confirmation

- Flag definition: `~/git/podman/cmd/podman/containers/commit.go:83`
  -- `flags.BoolVar(&commitOptions.IncludeVolumes, "include-volumes",
  false, "Include container volumes as image volumes")`.
- Wired through: `~/git/podman/pkg/domain/infra/abi/containers.go:677`
  (`IncludeVolumes: options.IncludeVolumes,`).
- Live-consumed behavior:
  `~/git/podman/libpod/container_commit.go:137-155` -- with the flag,
  every one of the container's own declared mount destinations
  (`c.config.UserVolumes`) is added to the new image's
  `config.Volumes` map (`importBuilder.AddVolume(v)`); without it,
  only *anonymous* named volumes among those (`vol.Anonymous()`) are
  added instead. Traced the full call chain from
  `cmd/podman/containers/commit.go` through
  `pkg/domain/infra/abi/containers.go:ContainerCommit` into
  `libpod/container_commit.go:Commit` to confirm this is real,
  executed code, not dead/config-gated.

## Why the default (no flag) stays a faithful no-op

This project has already established, twice over and independently of
this increment, that it has no anonymous-vs-named volume distinction
of any kind (`Command::Run::volume`'s and `Command::Create::volume`'s
own doc comments): the docker/podman-style anonymous (container-path-
only, no host source) volume shorthand isn't supported here. Since
real podman's own unflagged default only ever adds *anonymous* named
volumes, and this project can never have any, the honest, faithful
port of that default is exactly what already happened before this
increment (nothing added) -- not a shortcut invented here.

## Implementation

`bin/ociman/src/main.rs`: `include_volumes: bool`
(`#[arg(long = "include-volumes")]`, no short alias, matching real
podman which has none either) added to `Command::Commit` (full doc
comment, citing the above) and its `ContainerCommand::Commit` alias
(cross-referencing it, the same convention every other alias field
already uses). `cmd_commit`/`commit_inner` both grew the new
parameter, threaded through both dispatch sites.

`commit_inner` reuses the exact same "list every mount beyond the
fixed proc/dev/sys/... default set" primitive `ociman inspect`'s own
`mounts` field already established (`extra_mounts`, keyed off
`DEFAULT_MOUNT_DESTINATIONS`) rather than inventing a second one, and
writes each destination into `config.config.volumes` the exact same
way `--change VOLUME` already does (`serde_json::json!({})` per
path). Applied *before* the `--change` loop, so an explicit
`--change VOLUME=...` still wins for any overlapping path -- the same
"explicit `--change`/`--annotation` always wins over inherited data"
precedent `0522` already established for a different flag.

## Tests

`commit_include_volumes_adds_declared_mount_destinations_but_only_when_given`
(`tests/tests/ociman_commit.rs`): seeds a real container with an
explicit `-v <host-dir>:/data` bind mount, commits it twice (once
plain, once with `--include-volumes`), and asserts via `ociman
inspect --json` that the plain commit's `config.Volumes` is absent
entirely while the flagged commit's contains exactly `{"/data": {}}`.

No new alias-specific test: the existing
`container_commit_is_a_byte_identical_alias_for_top_level_commit`
(`tests/tests/ociman_container.rs`, from `0501`) already proves the
alias reaches the identical `cmd_commit` function with the identical
field set, the same precedent `--format`/`0537` already established
(no alias-specific test added there either).

Manually exercised end to end beyond the automated tests: a real
container created with `ociman create -v <host>:/data busybox ...`,
committed once without and once with `--include-volumes` (and once
more through the `ociman container commit` alias), each verified via
`ociman inspect --json`'s own `config.Volumes` field.

## Verification

`cargo build --workspace --locked` (clean), `cargo fmt --all` (clean,
no changes needed after the initial auto-format pass), `cargo clippy
--workspace --all-targets --locked -- -D warnings` (clean), targeted
`ociman_commit.rs`/`ociman_container.rs` runs (22/22, 48/48), a full
`cargo test --workspace --locked` run (126 test-result blocks, 0
failures, clean on the first attempt), `python3 ci/guards.py` (clean),
`cargo deny check` (clean), `bash ci/native-ci.sh` (clean on the first
attempt), `bash ci/build-deb.sh` (clean on the first attempt, real
`dpkg -i`/`--version`/`dpkg -r` round trip). Pure CLI-parsing plus a
small, already-precedented metadata-writing addition (identical shape
to `--change VOLUME`) -- no hot path touched, no `ci/bench.sh` rerun
needed.
