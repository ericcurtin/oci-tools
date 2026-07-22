# Design note 0157: `ociman create`

Status: implemented
Scope: `bin/ociman/src/main.rs` (new `RunArgs` struct, shared by
`Command::Run` and new `Command::Create` via `#[command(flatten)]`;
new `prepare_container`, factored out of `cmd_run`'s own original body
unchanged; `cmd_run` now a thin wrapper around it; new `cmd_create`;
`cmd_start`'s own precondition relaxed to also accept `Status::Created`;
`cmd_ps`'s own default filter fixed to also hide a `Created` container,
matching real podman's own identical default); `tests/tests/
ociman_create.rs` (4 new integration tests).

## What this does

`ociman create <image> [args...]`: pull (if not already present) and
extract an image's container, exactly like `ociman run` — same
rootfs setup, same synthesized `/etc/hosts`, same persisted base
filesystem snapshot, same `config.json` — but never launch it,
matching real `docker create`/`podman create` exactly. The container
is left in a real `Status::Created` state, ready for a later `ociman
start` to actually run it for the first time.

## `ocirun`'s own separate lifecycle, exposed through `ociman` for the first time

`Status::Created` already existed (`crates/oci-runtime-core/src/
state.rs`) and `ocirun`'s own low-level `create`/`start`/`delete`
lifecycle (milestone 3) already used it — this increment's only new
piece is exposing that same concept through `ociman`'s own,
higher-level `create` subcommand. `cmd_start`'s own precondition
needed exactly one change: accept `Status::Created` alongside the
already-accepted `Status::Stopped` (checked directly against real
podman's own `prepareToStart`, `~/git/podman/libpod/
container_internal.go`, which accepts `Configured`/`Created`/
`Stopped`/`Exited` — this project's simpler two-name split maps onto as
`Created`/`Stopped`). No other change to `cmd_start`'s own body was
needed at all: a `Created` container's own already-on-disk bundle is
just as complete and valid as a `Stopped` one's — `cmd_start` never
cared *why* a container hadn't run yet, only that a valid bundle
already exists right now.

## A real bug found and fixed before it could ever actually manifest

`cmd_ps`'s own default (non-`-a`) filter was `effective_status() !=
Status::Stopped` — which would have shown a merely-`Created`, never-
started container in a *plain* `ociman ps` by default, since `Created`
isn't `Stopped`. Checked directly against a real `podman create` +
plain `podman ps` (shows nothing) vs. `podman ps -a` (shows it,
`Created` status) before writing this fix: the filter now excludes
both `Stopped` *and* `Created`. This bug was latent rather than
previously user-visible: nothing before this increment ever left a
container sitting in `Created` for any observable length of time (a
plain `ociman run` only passes through `Creating` transiently, on its
way to `Running`).

## No duplicated flags: a shared, flattened `RunArgs`

`ociman create` supports every flag `ociman run` does except `--rm`/
`--detach` (real podman's own shared "same options as run" documented
convention) — implemented via a new `RunArgs` struct
(`#[derive(clap::Args)]`), flattened into both `Command::Run` (which
keeps its own `rm`/`detach` fields alongside the flattened `args`) and
the new `Command::Create` (`args` only). `cmd_run`'s own original body
was split into a new `prepare_container(&RunArgs) -> PreparedContainer`
(everything through a validated, on-disk bundle — resolve/pull, rootfs
setup, `/etc/hosts`, base snapshot, spec synthesis, `config.json`) plus
a thin `cmd_run` wrapper that only decides *whether and when* to
actually launch it; `cmd_create` calls the exact same `prepare_
container`, then just sets `Status::Created` and prints the id. Neither
function duplicates the other's own setup logic, and neither can drift
out of sync with the other on which flags are supported — matching this
project's own explicit design pillar ("one implementation per
function") just as much as any shared `crates/` code does, applied here
to CLI argument definitions themselves for the first time.

## What this doesn't do yet

* **`--rm`** on `create` — a real, valid `podman create --rm`
  combination (auto-remove once the container eventually runs, via a
  later separate `start`, and exits). This project's `cmd_start`/
  `cmd_restart` currently always pass a hardcoded `rm: false` to
  `launch_detached_and_confirm`/`run_and_finalize`, with no persisted
  record anywhere of what a container's own original `--rm` even was —
  a real, pre-existing gap (present since 0154, not introduced or
  worsened here) this increment doesn't attempt to fix: correctly
  threading `--rm` through a `create` that might not be started until
  an arbitrarily later, separate `ociman start` invocation needs a new
  persisted annotation `cmd_start`/`cmd_restart` would also both need
  to consult — a bigger, cross-cutting change better scoped as its own
  future increment.
* **`-a`/`--attach`** on a later `start` of a created container — same
  already-documented gap `cmd_start` (0154) has for a `Stopped`
  container; unchanged by this increment.

## Real, automated tests

Four new integration tests in `tests/tests/ociman_create.rs`:
a created container is genuinely hidden from plain `ps` but shown by
`ps -a`, with status `created` and no evidence yet of its own command
ever having run; `start` on a created container runs it for the first
time (verified via a real marker file), and a second `start` re-runs it
again (the same already-established `run`+`start` code path, now also
reached via `create`+`start`); `create --name` is resolvable by name;
`create` of a nonexistent image is a clear error.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, plus the full existing `ociman_run`/
`ociman_start`/`ociman_ps` suites re-run to confirm the `cmd_run`/
`cmd_ps` refactors changed nothing observable)/`cargo fmt --all
--check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check`/`bash
ci/native-ci.sh` all clean.
