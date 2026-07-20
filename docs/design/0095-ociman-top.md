# Design note 0095: `ociman top` (milestone 3)

Status: implemented
Scope: `crates/oci-runtime-core/src/cgroups.rs` (new
`cgroup_dir_for_running_pid`, `print_ps_table` — moved here from
`ocirun`'s own private copy), `bin/ocirun/src/main.rs` (`cmd_ps` now
calls the shared `print_ps_table`), `bin/ociman/src/main.rs`
(`Command::Top`, `cmd_top`), `tests/tests/ociman_top.rs`.

`ociman` gained real `docker top`/`podman top`'s `ps(1)`-passthrough
mode: list the real processes running inside a container, filtered
into the real host `ps` binary's own table output — the exact same
filtering behavior 0090's `ocirun ps` already implements, now shared
rather than duplicated (this project's own "one implementation per
function" design pillar).

## Sharing, not duplicating: `print_ps_table` moved into `oci-runtime-core`

`ocirun ps` (0090) originally had its own private `print_ps_table`
function. Rather than copy it verbatim into `ociman` (this codebase's
established, deliberate tolerance for *small* duplicates like
`ociboot`'s own `boot_count_status` doesn't extend to a genuinely
real, non-trivial chunk of logic two *different binaries* both need
identically), it moved into `oci_runtime_core::cgroups` as a public
function, with `ocirun`'s own `cmd_ps` updated to call the shared
version — confirmed unchanged behavior via `ocirun_ps.rs`'s own
pre-existing six tests, all still passing against the refactored call
site.

## A real wrinkle `ocirun ps` didn't have: `ociman`'s cgroup path isn't persisted anywhere

`ocirun ps` re-derives a container's cgroup directory from its own
bundle's `config.json` (`cgroupsPath`, the raw cgroupfs driver's own
spec-derived value). `ociman`'s own containers use the *systemd*
driver instead (`docs/design/0033`/`0034`): `systemd_cgroup::
create_scope` returns the real cgroup path it migrated the container's
pid into, but that path is only ever used once, at container-creation
time, inside `run_reporting_pid` — never persisted anywhere `ociman
top` (a wholly separate, later CLI invocation) could read it back.

Rather than invent new persisted state (a new annotation) for this,
`cgroup_dir_for_running_pid` re-derives the real, *current* cgroup
directly from `/proc/<pid>/cgroup` — the exact same technique
`systemd_cgroup::create_scope` itself already uses internally to
return the real path in the first place (see its own doc comment:
"the actual path can vary depending on the caller's own delegated
hierarchy," so it deliberately reads this back rather than assuming a
slice/scope-name shape). This works correctly regardless of which
cgroup driver actually placed the pid there, needs no new state, and
is real, current information rather than a possibly-stale snapshot
from creation time.

## Real, manual verification against a real, freshly-pulled busybox

Built the release binary and ran a real, backgrounded
`docker.io/library/busybox` container (`ociman run` always attempts
the systemd cgroup driver by default, so no special
`systemd-run --user --scope` carrier wrapper was needed at all, unlike
`ocirun`'s own raw-cgroupfs-driver tests): confirmed `ociman top`
correctly shows both the container's own real command and (the same
real, already-documented architectural property `ocirun ps` observed
in 0090) the `ociman run` invocation's own relay process, both genuine
members of the same real cgroup; confirmed `aux` passthrough reaches
the real host `ps` binary (its own distinct `USER` header); confirmed
`top` on a stopped container and on an unknown container id both fail
cleanly.

## Real, automated tests

Two new unit tests for `cgroup_dir_for_running_pid` in `cgroups.rs`
(matching this test process's own real `/proc/self/cgroup` content
independently parsed, and a dead-pid error case), plus four
integration tests in `tests/tests/ociman_top.rs`: the container's own
command showing up in the table, `aux` passthrough proven via its
distinct header, a stopped container being a real error, and an
unknown container being a real error.

## Performance — `cgroups.rs` is a named cgroup-driver module, A/B re-verified

Every change to `cgroups.rs` is purely additive — `all_pids`'s own
existing body is untouched (confirmed directly via `git diff`,
comparing byte-for-byte against the pre-0095 function), and neither
new function (`cgroup_dir_for_running_pid`/`print_ps_table`) is called
from any path `run`/`create` (the only two benchmarked functions) ever
exercise. Still, `cgroups.rs` is explicitly named ("either cgroup
driver") in this project's own "always re-verify" list, so a `git
stash`/`git stash pop` A/B `hyperfine` comparison was run against
`ocirun run` anyway: the "after" version measured 1.06× *faster*, well
within this project's own established noise band — no plausible
regression mechanism either way.

## What's still not here

* `ociman run -d`/`--detach`, `ocirun update`/`pause`/`resume`,
  automated failed-systemd-scope cleanup, the build cache,
  `ONBUILD`/`HEALTHCHECK` — all still exactly as earlier increments
  left them, unrelated to this increment's own scope.
* Real podman's own custom AIX-style format-descriptor engine for
  `top` (`podman top ctrID pid seccomp args %C`, no real `ps`
  invocation at all) — this increment is deliberately the narrower
  `ps(1)`-passthrough-only first slice, matching this project's own
  established "narrow first increment" pattern.
