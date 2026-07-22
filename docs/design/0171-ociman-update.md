# Design note 0171: `ociman update`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Update`, `cmd_update`,
new shared `parse_and_validate_memory_and_cpus`; `prepare_container`
refactored to call it instead of its own inline copy of the same
validation); `tests/tests/ociman_update.rs`.

## Exposing `ocirun update`'s own logic through `ociman`, for the
first time

`ocirun update` (0099) already applies real cgroup resource changes to
a running container via `oci_runtime_core::cgroups::plan_resources`/
`apply` — but only via a JSON-file `--resources` blob, matching real
`runc update -r` exactly. `ociman run` separately already has real
`--memory`/`--memory-swap`/`--cpus`/`--pids-limit`/`--cpuset-cpus`/
`--cpuset-mems` flags with their own real validation and translation
(`resources_from_cli`). `ociman update` is the first command to give
those same, already-working flags a *second* real use: not just at
launch time, but to change an already-running container's own limits
in place — matching real `podman update` for exactly that subset (see
below for what's deliberately not supported).

This is the same shape 0157 (`ociman create`) already established:
exposing an already-existing, already-tested `crates/`/`ocirun`-level
capability through `ociman` for the first time, reusing it directly as
a library call (never exec'ing `ocirun` — this project's own explicit
design pillar).

## A real, small refactor to share validation, not just translation

`resources_from_cli` (the `--memory`/`--memory-swap`/`--cpus`/
`--pids-limit`/`--cpuset-cpus`/`--cpuset-mems` -> `LinuxResources`
translation) was already shared. The *validation* around it (memory-
swap requires memory; memory-swap must be at least memory; cpus must
be positive and finite) was not — it lived inline in `prepare_
container`, `ociman run`'s own single caller. Extracted into a new
`parse_and_validate_memory_and_cpus`, called by both `prepare_
container` (unchanged behavior, same three `ensure!`s, same error
text) and the new `cmd_update` — so there is exactly one
implementation of "does this combination of flags even make sense",
not two copies that could silently drift apart the next time either
one changes.

## Deliberately narrower than real `podman update`

Real `podman update` also supports `--cpu-shares`/`--cpu-period`/
`--cpu-quota`/`--cpu-rt-period`/`--cpu-rt-runtime`/`--memory-
reservation`/`--memory-swappiness`/`--blkio-weight[-device]`/
`--device-{read,write}-{bps,iops}` — none of which `ociman run` itself
supports either, so none of them are added here (the same "narrower
flag set than real podman/docker, but a real, working subset, not a
half-implemented larger one" pattern already established throughout
this project). `ociman update` only ever accepts the exact six flags
`ociman run` already has.

Also unlike real `podman update` (which can update an already-*stopped*
container's own persisted spec, taking effect on its *next* start):
this requires the container to actually be **running** right now —
this project's own cgroup only exists while its systemd scope is alive
at all, and (matching `ocirun update`'s own already-documented
limitation, 0099) the container's own persisted state is never
rewritten to reflect the change, so there is nothing meaningful to
"apply later" to a stopped container the way real podman's own model
allows. A stopped container is a clear, named error rather than a
silent no-op or a misleading success.

Giving no resource flag at all is also a clear, named error (matching
real `podman update`'s own requirement that at least one limit
actually be given) rather than a silent, do-nothing success.

## Verified against real `podman update`

`podman update --memory 64m <container>` (a real, running container)
was confirmed to succeed with the identical flag/argument shape this
implementation uses. The real, kernel-level effect on this project's
own side was independently confirmed by reading the actual cgroup v2
accounting files back directly after `ociman update --memory 64m
--cpus 0.5 --pids-limit 42`: `memory.max` reads exactly `67108864`
(64MiB), `cpu.max` reads exactly `50000 100000` (matching
`resources_from_cli`'s own quota-over-100ms-period conversion for
`0.5` CPUs), and `pids.max` reads exactly `42`.

## Tests

Six new unit tests for `parse_and_validate_memory_and_cpus` (no flags
at all; memory+swap combined correctly; swap without memory rejected;
swap smaller than memory rejected; unlimited swap `-1` accepted; zero/
negative/NaN cpus rejected) — the exact validation `prepare_container`
already relied on, now covered directly rather than only indirectly
through `ociman run`'s own integration tests. `tests/tests/
ociman_update.rs` adds four integration tests: an unknown container is
a clear error; giving no resource flags at all is a clear error; an
already-stopped container is a clear error; and the real, convincing
end-to-end check — updating a genuinely running container's `--memory`/
`--cpus`/`--pids-limit` together and reading the real cgroup v2 files
back directly to confirm the kernel itself now enforces the new
limits, not just that the command exited `0`. Full `cargo build
--workspace --locked`/`cargo test --workspace --locked` (2 clean
runs)/`cargo fmt --all --check`/`cargo clippy --workspace --all-
targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo deny
check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* Real `podman update`'s own larger flag set (`--cpu-shares`, etc. —
  see above) — unchanged from `ociman run`'s own identical gap.
* Updating an already-stopped container's own persisted limits for
  its next start — this project's own state model has nowhere to
  store that yet (see above).
