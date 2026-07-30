# Design note 0353: `ocirun update`'s ad-hoc resource flags

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_update.rs`.

## Closing a real, long-standing, forgotten gap

`docs/design/0099` (milestone 3, `ocirun update`'s own introduction)
explicitly named this exact gap by name: *"Real runc's own individual
ad-hoc flags (`--memory`, `--cpu-shares`, `--pids-limit`, ...) as an
alternative to the JSON-file mode — a deliberate, documented scope
limit, not attempted here."* That note has sat untouched through
roughly 250 subsequent design notes. Closing a first slice of it now
— the same "genuine, long-standing, forgotten gap from this project's
own history" shape `0345`'s `ocirun list OWNER` fix already
established as a real, worthwhile pattern to keep revisiting.

## Real, checked-directly semantics

Read `~/git/runc/update.go` directly: real runc's own `update` has a
much larger ad-hoc flag set than this note implements (`--blkio-
weight`, `--cpu-period`/`--cpu-quota`/`--cpu-burst`/`--cpu-share`/
`--cpu-rt-period`/`--cpu-rt-runtime`/`--cpu-idle`, `--kernel-memory`/
`--kernel-memory-tcp` (both explicitly marked `Hidden`/"obsoleted; do
not use"), plus Intel RDT-only `--l3-cache-schema`/`--mem-bw-schema`).
This note deliberately implements only the subset real `crun update`
also supports (`~/git/crun/src/update.c`): `--memory`, `--memory-
swap`, `--pids-limit`, `--cpuset-cpus`, `--cpuset-mems` — the same
"first narrow slice" framing `0343`/`0351` already used for other
multi-flag increments, and the exact subset both reference runtimes
agree on.

Two real, checked-directly details mattered:

1. **Precedence, when both `--resources` and an ad-hoc flag are given
   together.** Real runc's own doc string is explicit: *"if data is
   to be read from a file or the standard input, all other options
   are ignored."* Not an error — every ad-hoc flag is silently
   ignored the moment `--resources` resolves to anything. Ported
   verbatim (matching this project's own established precedent for
   preserving a real, even-surprising upstream behavior exactly rather
   than "fixing" it into a more intuitive-seeming merge or error, e.g.
   `0330`/`0343`).
2. **`--memory`/`--memory-swap`'s own value syntax genuinely differs
   between the two reference runtimes.** Real runc's own `update.go`
   parses via `units.RAMInBytes` — a plain byte count, or one with a
   `k`/`m`/`g`/`t` unit suffix (the same convention real docker/podman
   use, already implemented in this project as `ociman run --memory`'s
   own `parse_memory_limit`). Real crun's own `update.c` treats every
   numeric field as a bare, unit-suffix-free integer string (fed
   straight into a JSON number with no parsing beyond that). Since a
   plain number with no suffix parses identically either way, real
   runc's own richer parser is a strict superset — nothing crun-
   specific is lost by implementing that one instead of duplicating
   crun's narrower behavior.

`--pids-limit` deliberately does **not** get `ociman run --pids-
limit`'s own friendlier "any non-positive value means unlimited"
convenience rule: real runc's own `update.go` is a bare `int64(cmd.
Int("pids-limit"))` pass-through with no clamping or renormalizing at
all, and the real runtime-spec's own already-documented `-1`-means-
unlimited convention (`oci_spec_types::runtime::LinuxPids::limit`'s
own doc comment) already covers the one value that actually matters
in practice — `ocirun` is the lower-level runtime layer here, matching
runc's own literal behavior rather than `ociman`'s own higher-level
convenience layer built on top of it.

## Implementation

`Command::Update::resources` changed from a required `PathBuf` to an
`Option<PathBuf>` (real runc's own flag is optional too — its absence
is what makes the ad-hoc flags reachable at all). Five new fields
(`memory`, `memory_swap`, `pids_limit`, `cpuset_cpus`, `cpuset_mems`)
sit alongside it.

New `parse_memory_limit`/`parse_memory_swap_limit` in `ocirun`'s own
`main.rs` — a real, if small, deliberate duplication of `ociman`'s own
identically-named, identically-implemented functions (this project has
no shared crate for CLI-argument-parsing-only helpers; the same
reasoning `0351`'s own `verify_preserve_fds` duplication already
gives). New `resources_from_flags`, building a `LinuxResources` from
whichever ad-hoc flags were actually given (every unset field stays
`None`/empty — a real no-op, matching the JSON-file mode's own
existing "only ever change what's actually given" convention exactly)
— only ever called when `--resources` is absent. `plan_resources`/
`apply` (already shared with `ociman update`/`ocicri
UpdateContainerResources`) needed zero changes: every target
`LinuxResources`/`LinuxMemory`/`LinuxCpu`/`LinuxPids` field this note
writes already existed and was already wired end to end.

## Verified

New unit tests in `ocirun`'s own `main.rs` (this binary's first
in-process `#[cfg(test)]` unit tests in `main.rs` itself, though
`features.rs` already had its own — the same "no process/filesystem/
cgroup involvement, so a direct unit test is both possible and most
direct" reasoning `ociman`'s own identical test module already
established for its own copy of `parse_memory_limit`):
`parse_memory_limit_handles_every_real_docker_podman_unit_suffix`,
`parse_memory_limit_rejects_garbage_and_empty`,
`parse_memory_swap_limit_accepts_the_real_unlimited_sentinel`,
`resources_from_flags_with_nothing_given_is_a_real_empty_default`,
`resources_from_flags_builds_memory_and_swap_together`,
`resources_from_flags_builds_pids_limit_alone`,
`resources_from_flags_builds_cpuset_cpus_and_mems_together`,
`resources_from_flags_propagates_a_real_parse_error`.

New integration tests in `ocirun_update.rs`, against a real running
container with a real delegated cgroup subtree (the same setup the
existing `--resources`-JSON tests already use):
`update_ad_hoc_memory_and_pids_limit_flags_write_the_real_cgroup`,
`update_resources_file_takes_priority_and_silently_ignores_every_ad_hoc_flag`
(the precedence rule above, proven end to end: a `--resources` file
setting only `pids` combined with `--pids-limit 999 --memory 1g`
leaves `memory.max` completely untouched and honors the file's own
`pids.max` value, not the ad-hoc flag's).

Deliberately **not** covered by a live integration test:
`--cpuset-cpus`/`--cpuset-mems`. Checked directly on this development
host: `cgroup.subtree_control` for the real, delegated `systemd
--user` subtree these tests already rely on lists only `cpu memory
pids` — `cpuset` is genuinely not delegated (the same real, already-
documented rootless limitation `ociman run --cpuset-cpus`'s own doc
comment already notes for its own, differently-plumbed systemd-unit-
property path). A live write to `cpuset.cpus` here would fail with a
real permission error regardless of this note's own correctness, so
`resources_from_flags_builds_cpuset_cpus_and_mems_together`'s own
unit test (proving the `LinuxCpu` struct itself is built correctly)
is the honest, non-environment-dependent substitute.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`.

## Still ahead

Real runc's own remaining ad-hoc flags (`--blkio-weight`/`--cpu-
period`/`--cpu-quota`/`--cpu-burst`/`--cpu-share`/`--cpu-rt-period`/
`--cpu-rt-runtime`/`--cpu-idle`, plus the two `Hidden`/obsoleted
kernel-memory flags neither reference runtime recommends using) remain
a separate, still-deferred candidate — this project's own shared
`LinuxCpu` struct already has most of the target fields
(`shares`/`quota`/`burst`/`period`/`realtime_runtime`/
`realtime_period`), so a future increment closing more of this same
gap would be similarly small.
