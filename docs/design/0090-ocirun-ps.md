# Design note 0090: `ocirun ps` (milestone 3)

Status: implemented
Scope: `crates/oci-runtime-core/src/cgroups.rs` (new `all_pids`,
`read_procs_file`), `bin/ocirun/src/main.rs` (new `Command::Ps`,
`cmd_ps`, `print_ps_table`), `tests/tests/ocirun_ps.rs`.

`ocirun` gained real runc's own `ps` subcommand: list the real
processes running inside a container. Found via a fresh small-gaps
survey (`explore` subagent) as the single cleanest candidate of
everything surveyed — no schema changes, no shared-crate churn beyond
one new, self-contained function, and real runc's own implementation
(`~/git/runc/ps.go`) is itself genuinely small.

## Verified against real runc source first, not assumed

Read `~/git/runc/ps.go` and `~/git/runc/libcontainer/container_linux.go`'s
`Processes()` directly:

* `Container.Processes()` returns `cgroupManager.GetAllPids()` — every
  pid in the container's own cgroup, and recursively every pid in any
  nested sub-cgroup underneath it (real cgroup-v2 `GetAllPids`,
  vendored from `opencontainers/cgroups/getallpids.go`: a plain
  recursive directory walk, reading `cgroup.procs` from every
  directory found).
* `ignoreCgroupError` tolerates the cgroup simply not existing
  (`os.ErrNotExist`) as "the container has already stopped" — not a
  real failure.
* The CLI itself (`ps.go`): `--format json` prints the bare pid array;
  the default `table` format runs the real host `ps` binary (`ps_args`
  if given, else `-ef`), finds the `PID` column in its own header, and
  prints only the header plus lines whose `PID` field is one of the
  container's own pids. A line whose `PID` field fails to parse is a
  **hard error**, not silently skipped — real `ps` output is
  well-formed by construction, so a parse failure would mean the
  column index itself is wrong, worth surfacing loudly.

`oci-tools`'s own implementation mirrors all of this exactly, function
for function.

## `all_pids`: one new, self-contained function in the cgroupfs driver's own module

`crates/oci-runtime-core/src/cgroups.rs` gained `all_pids(cgroup_dir:
&Path) -> io::Result<Vec<i32>>` — a plain recursive directory walk
reading `cgroup.procs` from every directory found, tolerating a
missing top-level directory as "no processes" (matching real runc's
own `ignoreCgroupError`). No existing function in this module
(`apply`/`enter`/`remove`/`directory_for`/`plan_resources`) was
touched at all — this is purely additive, and `all_pids` itself is
only ever called from the brand-new `cmd_ps`, so no existing call path
(including `run`/`create`, the only two functions this project
benchmarks) changed behavior in any way.

## `ocirun`'s own new subcommand

`Command::Ps { id, format, ps_args }` — `id` positional, `--format`/
`-f` (default `"table"`), and `ps_args` a `trailing_var_arg` (same
`allow_hyphen_values` pattern `ocirun exec`'s own trailing command
args already established, confirmed to correctly handle bare
hyphenated tokens like `-ef`/`aux` without needing an explicit `--`
separator). `cmd_ps` re-loads the bundle from `state.bundle` (same
established pattern `remove_cgroup_directory_if_any` already uses),
resolves the cgroup directory via the existing `cgroups::directory_for`,
and calls `all_pids` — a bundle with no `cgroupsPath` at all (this
project's own bundles routinely have none: cgroup management is
opt-in) simply reports zero processes, not an error.

## A real, observed architectural property, not a bug

Manually verified against a real, delegated `systemd --user` cgroup
subtree first: the container's own `cgroup.procs` genuinely contains
**two** real pids while running — the container's actual command
(e.g. `/bin/sh -c "sleep 30"`) *and* this project's own PID-namespace
relay process (`ChildSetup::run`'s own documented relay-fork
mechanism: cgroup membership happens before the relay forks its
grandchild, and is inherited across `fork`, so the relay process
itself remains a real, live member of the same cgroup for as long as
it's waiting on the grandchild). `ocirun ps` reports both, correctly —
matching real runc's own `GetAllPids`, which is equally agnostic about
*what* a pid in the cgroup represents, not something to filter out.

## Real, manual verification against a real kernel first

Before writing any automated test: a real running container with a
real delegated cgroup subtree, confirming `--format json` reports the
real container pid, `--format table` (default `-ef`) shows the
container's own command line, and passing `aux` through actually
reaches the real host `ps` binary (its own `USER` column vs. `-ef`'s
`UID` column, a real, observable difference proving the passthrough
worked). Also verified a container with no `cgroupsPath` at all
reports zero processes (header-only table, `[]` json) rather than an
error, and an unknown container id fails cleanly.

## Real, automated tests

Six integration tests in `tests/tests/ocirun_ps.rs`, run against the
actual built `ocirun` binary (five needing a real, reachable
`systemd --user` session for a genuine delegated cgroup subtree — same
skip-cleanly-where-unavailable pattern `ocirun_lifecycle.rs`'s own
cgroup-cleanup test already established): JSON format reporting the
real pid, table format showing the container's own command, `aux`
passthrough proven via its distinct header, the no-cgroup case
reporting zero processes, an unknown container id failing, and an
invalid `--format` value failing with the right message.

## Performance — cgroupfs-driver module touched, A/B re-verified out of caution

`crates/oci-runtime-core/src/cgroups.rs` is explicitly named in this
project's own "always re-verify" list ("either cgroup driver"), even
though the new `all_pids` function is only ever called by the
brand-new `cmd_ps` (no existing call path touched at all — `run`/
`create` never call it). A `git stash`/`git stash pop` A/B `hyperfine`
comparison against `ocirun run` (a hookless, cgroup-less bundle) was
still run out of caution: 1.05× "faster" before, well within this
project's own established noise band. No plausible regression
mechanism: zero new code runs on the `run`/`create` path at all.

## What's still not here

* `ocirun events` (stats streaming), `ocirun update` (live resource
  limit changes), `ocirun pause`/`resume` (needs a new `Status`
  variant threaded through every state-matching call site) — all
  larger, separate candidates from the same survey, not attempted
  here.
* Automated failed-systemd-scope cleanup, the build cache,
  `ONBUILD`/`HEALTHCHECK`, `ociboot`'s other subcommands — all still
  exactly as earlier increments left them, unrelated to this
  increment's own scope.
