# Design note 0021: `ociman ps`/`rm`, persistent-by-default `run`

Status: implemented
Scope: `bin/ociman`'s `ps`/`rm` subcommands, `run --rm`; wires
`oci_runtime_core::StateStore` into `ociman` for the first time.

## The gap this closes

0020's `ociman run` was deliberately ephemeral: every run extracted
into a `tempfile::TempDir`, explicitly cleaned up right before the
process exited, with no record surviving to inspect afterward. That
was an honest, narrow scope for landing the pull-extract-synthesize-
launch pipeline on its own — this increment builds the natural next
piece milestone 3 already names (`ociman run/exec/ps/logs`): container
records that persist by default (matching real `docker run`/`podman
run`, which keep a container around unless `--rm`), `ociman ps` to
list them, and `ociman rm` to clean them up.

## Reusing `oci_runtime_core::state`, not inventing a second schema

`ociman`'s containers are tracked with the exact same `StateStore`/
`PersistedState`/`Status` `ocirun` itself uses — the same "share as
much Rust code as possible" pillar this whole workspace is built
around applies to `ociman`'s own bookkeeping, not just container
execution. `ociman`-specific metadata a runtime-spec state record has
no field for (which image, what command, the eventual exit code) goes
into the *existing* `annotations` map — its own doc comment already
says "arbitrary metadata, opaque to the runtime", which is exactly
this use case, so no schema change to a struct `ocirun`'s own `state`/
`list` output also depends on was needed.

One deliberate difference from `ocirun`'s own convention: `ocirun`
roots its `StateStore` under `oci_cli_common::runtime_root` (`/run`,
tmpfs, matching runc's own default — a low-level runtime invoked by a
supervisor that manages its own state's lifetime). `ociman` roots its
container store under `oci_cli_common::storage::default_root()` (the
same persistent root images already live under) instead: unlike a
low-level runtime, `ociman`'s own containers are meant to be listable/
removable well after the process that created them exits, including
across a reboot — the same reasoning real `podman` stores its
container metadata under `/var/lib/containers` rather than `/run`.

Each container's bundle (`config.json` + `rootfs/`) and its
`state.json` now live in the exact same directory
(`StateStore::container_dir`, already the right shape for a bundle
directory) rather than a separate temp directory — one less moving
part than 0020's design, and it falls out naturally from reusing
`StateStore` as the single source of truth for "where does this
container's stuff live".

## `run --rm`, and the ordering that makes cleanup-on-failure safe

`run` now creates a `state.json` record (status `Creating`) *before*
pulling/extracting/launching, so a crash partway through still leaves
something `ps`/cleanup can reason about rather than nothing at all —
and on any failure, the whole record is removed (matching the
`StateStore::create` cleanup-on-write-failure precedent it already
follows internally). On success, `--rm` removes the record
immediately after; otherwise the record is updated to `Stopped` with
the exit code stashed in `annotations`, exactly the outcome `ps -a`
needs.

## `rm`'s honest limitation: no live pid for a still-running foreground `run`

`ocirun`'s own `create`/`start`/`kill`/`delete` (0017) can `kill` a
real live container because `create` leaves it backgrounded with a
recorded pid. `ociman run` is still fully foreground (0020's own
scope) — `oci_runtime_core::launch::run` blocks until the container
exits and never exposes an intermediate pid to its caller — so a
`state.json` record never has a live `pid` to act on. `rm --force`
against a container a *concurrent* `ociman run` invocation is still
running would therefore find nothing to signal (`state.pid` stays
`None` throughout) and just proceed to remove the record once that
`run` eventually finishes on its own. A real, narrow gap, not
something this increment quietly papers over — resolving it properly
needs `ociman run` to gain the same backgrounded/pid-tracking shape
`ocirun create` already has, which is exactly the kind of follow-up
this design note flags rather than half-implements.

## Verified against a real image, then covered offline

Manually verified end to end against a real `docker.io/library/busybox`
pull (network, deleted after): `run` (persists by default, `ps -a`
shows it stopped with exit code 0), `run --rm` (no record left), exit-
code propagation (a real `exit 7` came back as `ociman run`'s own exit
code), `rm` (removes a stopped container; refuses a non-stopped one
without `--force`), and a real column-alignment bug in `ps`'s own table
formatting (fixed-width columns glued together for a realistically
long image reference — caught by literally reading the printed output,
not by inspecting the format string).

The automated tests (`tests/tests/ociman_ps.rs`, four cases) reproduce
the `ps`/`rm`/`--rm` scenarios above with the same fully offline,
network-free seeded-image approach 0020 established
(`oci_tools_tests::seed_image`, now shared between `ociman_run.rs` and
this file rather than duplicated, since both need the identical
synthetic-but-structurally-real image).

## What's still not here

* `exec` (run an additional process inside an already-running
  container) and `logs` (needs stdout/stderr redirected to a per-
  container log file for a backgrounded run, not the terminal
  inheritance `run` still relies on) — both need the backgrounded/
  detached execution model this increment's own "honest limitation"
  section above describes, not yet built.
* `stop` (a graceful-then-forceful kill, distinct from `rm --force`'s
  immediate `SIGKILL`).
* `--name` (human-chosen container names, `ps`/`rm` only accept the
  generated short ID right now).
