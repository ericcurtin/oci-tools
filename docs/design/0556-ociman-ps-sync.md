# Design note 0556: `ociman ps`/`container list --sync`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## What this closes

`docs/design/0290`'s own "Still ahead" list named `--sync` (alongside
`--namespace`/`--ns`, `--pod`, `--watch`, `--external`, `--format`,
`--latest`, `-s`/`--size`) as an open real podman `ps` flag never
ported here. `--format`/`--latest`/`--size` were since closed by
later increments; `--sync` was not, and is not touched by any
increment between `0290` and `0555`. This adds it as a real, faithful
no-op.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/ps.go:98`: `flags.BoolVar
  (&listOpts.Sync, "sync", false, "Sync container state with OCI
  runtime")` -- the flag's own real registration, no short alias.
- `~/git/podman/pkg/ps/ps.go:189-193`: inside `ListContainerBatch`
  (called once per listed container):
  ```go
  if opts.Sync {
      if err := c.Sync(); err != nil {
          return fmt.Errorf("unable to update container state from OCI runtime: %w", err)
      }
  }
  ```
  the live consumer, proving this is real, not dead/vestigial code.
- `~/git/podman/libpod/container_api.go:918-926`, `Sync`'s own doc
  comment (quoted verbatim): "Most of the time, Podman does not
  explicitly query the OCI runtime for container status, and instead
  relies upon exit files created by conmon. This can cause a
  disconnect between running state and what Podman sees in cases
  where Conmon was killed unexpectedly, or runc was upgraded. Running
  a manual Sync() ensures that container state will be correct in
  such situations."

## Why this is a real, faithful no-op here

This project's own status model has no equivalent "trust a cached,
conmon-exit-file-derived value" path for `--sync` to force past in
the first place: `oci_runtime_core::state::PersistedState::
effective_status` (`crates/oci-runtime-core/src/state.rs:171-177`)
unconditionally re-derives `Stopped` from a real, live `/proc/<pid>`
liveness check on every single read, every time -- its own doc
comment already gives the exact reasoning ("matches runc/crun
re-deriving status from the live process rather than trusting a
cached field"). `cmd_ps` (via its own per-container visibility
filter) already calls `effective_status()` for every listed
container regardless of any flag. `ociman ps` is
therefore already always doing, unconditionally, what `--sync` forces
podman to do only on demand -- there is nothing left for this flag to
actually change, the same reasoning class already used for `ociman
container prune --force`/`ocibox create --absolutely-disable-root-
password-i-am-really-positively-sure` (0549).

## Why this is narrow

Entirely contained to two already-existing sibling structs sharing
one implementation: `Command::Ps` (`ociman ps`) and `ContainerCommand::
List` (`ociman container list`/`ls`, which already mirrors `Ps`
field-for-field per its own doc comment, dispatching into the same
`cmd_ps`). No new parameter threaded into `cmd_ps` itself at all --
both dispatch sites simply accept-and-discard (`sync: _`), the same
pattern `ContainerCommand::Prune { force: _, filter }` already
established. No persisted state, no lifecycle reload sites, no other
command needs to know about it.

## Implementation

- `Command::Ps` gains a `sync: bool` field (`#[arg(long)]`, no short
  alias, matching real podman's own identical registration).
- `ContainerCommand::List` gains the identical field (`Same as
  [Command::Ps::sync]`, matching every other flag's own doc-comment
  convention on this alias).
- Both dispatch sites in `main()` destructure `sync: _` and pass
  nothing new into `cmd_ps` -- the function's own signature is
  unchanged.

## Tests

Two new integration tests in `tests/tests/ociman_ps.rs`:
`ps_sync_flag_is_accepted_and_behaves_identically` (a real running
container plus a real stopped one, asserting `ociman ps -a` produces
byte-identical output with and without `--sync`) and
`container_list_sync_flag_is_accepted` (the alias's own identical
parity check).

Manually verified end to end: built a real `scratch`-based image via
`ociman build`, ran a real container to completion, and confirmed
`ociman ps -a` and `ociman ps -a --sync` produce byte-identical
output via a real `diff`.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (128
test-result blocks, all passing -- no new test file added, so the
block count is unchanged from `0555`), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (clean on
the first attempt), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). A pure
CLI-parsing no-op, not a hot path -- no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

Real podman's own remaining `ps` flags -- `--namespace`/`--ns`,
`--pod`, `--watch`, `--external` -- each need genuinely new
subsystems this project doesn't have (namespaces reporting, pods,
polling loops, non-managed/foreign-storage containers) and remain
separately-scoped candidates, unchanged from `0290`'s own list.
