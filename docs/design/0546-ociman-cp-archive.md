# Design note 0546: `ociman cp --archive`/`-a`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_cp.rs`.

## What this closes

Adds `--archive`/`-a` to `ociman cp` and its `ociman container cp`
alias (`0500`) — chowning files copied *into* a container to that
container's own primary uid/gid, matching real `podman cp --archive`'s
own real, default-`true` behavior. This also **corrects a previously-
wrong claim** in `docs/design/0500`, which grouped `--archive` together
with real podman's own genuinely inert, `MarkHidden`'d `--extract`/
`--pause` flags and called all three "deprecated-NOP" — `--archive` is
neither hidden nor a no-op.

## Real, checked-directly confirmation

- Flag definition, plainly visible (unlike `--extract`/`--pause`,
  explicitly `MarkHidden`'d right below it):
  `~/git/podman/cmd/podman/containers/cp.go:60` —
  `flags.BoolVarP(&chown, "archive", "a", true, "Chown copied files to
  the primary uid/gid of the destination container.")`.
- Threaded into both real copy directions that have a destination
  container: `cp.go:183` (`copyContainerToContainer`'s own
  `destContainerCopy`, container-to-container) and `cp.go:449`
  (`copyToContainer`, host-to-container) — both pass `Chown: chown`
  into `entities.CopyOptions`. Container-to-*host*
  (`copyFromContainer`, `cp.go:204-`) never references `chown` at
  all — there's no destination container to chown to.
- Real consumption: `~/git/podman/pkg/domain/infra/abi/archive.go:17`
  → `~/git/podman/libpod/container_api.go:1136` →
  `~/git/podman/libpod/container_copy_common.go:190-198` — when
  `chown` is true, `getContainerUser` resolves the destination
  container's own primary uid/gid, passed as `ChownDirs`/`ChownFiles`
  to the actual copier.
- **Live-verified** against a real installed `podman 4.9.3`: `podman
  cp file.txt cptest:/tmp/file.txt` into a container running
  `--user 2000:2000` produced a file owned `2000:2000`; the identical
  copy with `--archive=false` left it owned `root:root` (the host
  copying process's own uid). `podman cp --help` shows `-a, --archive
  ... (default true)` as a plainly visible, documented flag.

## Real, functional gap — and a real, previously-unnoticed default-behavior divergence

This project's own container users are already fully modeled
(`process.user.uid`/`gid`, `ociman run`/`create --user`), so this is a
real, reachable behavior, not an inapplicable upstream concept — and
the mechanism already existed and was already tested for a sibling
feature: `copy_path_recursive` (shared with `COPY --chown`,
`bin/ociman/src/build.rs:3188`) already takes a `chown: Option<(u32,
u32)>` parameter; `cmd_cp` simply always called it with `None`. Since
real podman's own default for `--archive` is `true`, this project's
previous unconditional `None` was a real, previously-unnoticed
divergence from real podman's own default behavior, not merely a
missing opt-in flag — closed here rather than just adding the flag as
a pure no-op.

## Implementation

`bin/ociman/src/main.rs`: `archive: bool`
(`#[arg(short, long, default_value_t = true, num_args = 0..=1,
default_missing_value = "true", action = clap::ArgAction::Set)]`,
the same default-true-but-overridable-false bool-flag shape already
established elsewhere, e.g. `ociman commit --pause`) added to
`Command::Cp` and `ContainerCommand::Cp` (correcting the latter's own
doc comment's prior mis-scoping, see above).

`cmd_cp` resolves the *destination* container's own primary uid/gid
(a new `container_primary_uid_gid` helper, reading the already-
resolved `process.user.uid`/`gid` straight out of the destination
container's own on-disk `config.json` — no `/etc/passwd` re-resolution
needed, unlike real podman's own `getContainerUser`, since this
project already resolved and stored it there at creation time) and
passes it through to `copy_path_recursive` whenever `archive` is true
**and the destination is a container** — both the host-to-container
and container-to-container branches (matching real podman's own
identical dual application, `cp.go:183,449`); the container-to-host
branch never chowns at all, matching `copyFromContainer`'s own
identical omission.

`set_owner` (the existing, already-tested primitive) already tolerates
`EPERM` gracefully (a warning, not a failure) when the calling process
lacks `CAP_CHOWN` — the same rootless single-uid-mapping limitation
this project's own `--user`/`-v`/`COPY --chown` already have, not a
new one `--archive` introduces. Since this project's own rootless
runtime can only ever create a container running as uid/gid `0`
(`resolve_user`'s own already-documented "only container uid/gid 0 is
mapped" restriction — a real, separate, pre-existing, larger gap, not
addressed here), an unprivileged `ociman cp --archive` into any
container this project can actually create will always attempt a
chown to `0:0` and have it tolerated-not-applied unless the calling
process is itself real root.

## Tests

Four new tests in `tests/tests/ociman_cp.rs`:

- `cp_archive_default_never_fails_the_copy_even_when_the_chown_cannot_
  apply` — the "doesn't break the common, unprivileged case" half:
  copying in with the default `--archive` always succeeds regardless
  of whether the chown itself can actually apply.
- `cp_archive_false_never_chowns_and_keeps_the_source_files_own_
  ownership` — deterministic regardless of privilege (uses the
  calling test process's own real uid/gid, the same "guaranteed
  either way" trick `ociman_build.rs`'s own `copy_chown_is_reflected_
  in_the_committed_layers_own_tar_header` already established):
  `--archive=false` leaves the copied file exactly as its source was.
- `cp_archive_chowns_to_the_destination_containers_own_user_when_
  privileged_enough` — the real, observable-difference half: chowns
  the source file to a different uid first, then confirms `--archive`
  (default) overrides that to the destination container's own
  resolved `0:0`. Skipped when not running as real root, the same
  convention `ociman_build.rs`'s own `chown_to_a_different_uid_is_
  tolerated_not_fatal_when_unprivileged` already established for the
  identical `CAP_CHOWN` constraint — confirmed to actually run and
  pass this way in a real-root re-check, not merely skipped
  everywhere.

Manually exercised beyond the automated tests: a real container
(forced plain-`Extract` rootfs), copying a real host file in with the
default `--archive` (confirmed, via `OCI_TOOLS_LOG=debug`, the
tolerated-`EPERM` chown-to-`0:0` attempt is logged) versus
`--archive=false` (confirmed no chown attempt is logged at all).

## Verification

`cargo build --workspace --locked` (clean), `cargo fmt --all` (clean),
`cargo clippy --workspace --all-targets --locked -- -D warnings`
(clean), targeted `ociman_cp.rs`/`ociman_container.rs`/`ociman_build.rs`
runs (13/13, 48/48, 141/141), a full `cargo test --workspace --locked`
run (clean), `python3 ci/guards.py` (clean), `cargo deny check`
(clean), `bash ci/native-ci.sh` (clean), `bash ci/build-deb.sh` (clean,
real `dpkg -i`/`--version`/`dpkg -r` round trip). Reuses the already-
tested `copy_path_recursive`/`set_owner` primitives directly — no new
hot path, no `ci/bench.sh` rerun needed.
