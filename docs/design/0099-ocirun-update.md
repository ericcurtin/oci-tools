# Design note 0099: `ocirun update` (milestone 3)

Status: implemented
Scope: `bin/ocirun/src/main.rs` (`Command::Update`, `cmd_update`),
`tests/tests/ocirun_update.rs`.

`ocirun` gained real `runc update`: change a running container's real
cgroup resource limits in place. Chosen for this genuinely useful,
well-scoped "medium" candidate from the ongoing survey — reuses
essentially all of its own machinery from existing code
(`cgroups::plan_resources`/`apply`, already built for container
creation; `Bundle::load`/`cgroups::directory_for`, already established
by `ocirun ps`, 0090, for exactly this "re-derive a running
container's own cgroup path" need).

## Scoped to real runc's own JSON-file mode only, by design

Real `runc update` accepts either a `--resources`/`-r <file>` (a full
`LinuxResources` JSON blob, same shape as `config.json`'s own
`linux.resources`) or a long list of individual ad-hoc flags
(`--memory`, `--cpu-shares`, `--pids-limit`, ...) that get assembled
into the same structure internally. This increment supports only the
JSON-file mode — a deliberately narrower first slice, matching this
project's own established "narrow first increment" pattern for every
other multi-option flag. `ocirun`'s own architecture (spec-driven only,
no policy of its own — see the top-level README's design pillars)
also makes the JSON-file mode the more natural fit: it's the same
"real spec-shaped data in, no CLI-level interpretation" contract
`ocirun spec` itself already has.

## No merge logic needed: `LinuxResources`'s own `Option` fields already are the diff

Real runc's own `update` merges the given resources into its
in-memory copy of the container's *persisted* config, field by field,
so an update that only mentions `pids-limit` doesn't reset memory/cpu
limits back to nothing. This project needed no equivalent merge logic
at all: `oci_spec_types::runtime::LinuxResources` is already an
`Option`-per-field structure (matching the runtime-spec's own shape),
and `cgroups::plan_resources` (already built, unit-tested, and used
for container creation) already only emits a cgroup write for a field
that's actually `Some` — a JSON blob that only sets `pids.limit`
naturally only touches `pids.max`, leaving `memory.max` (or anything
else) completely alone, with zero new code required for that
behavior. Verified directly, not assumed: a real test applies two
sequential updates (one setting both memory and pids, a second
setting only pids) and confirms `memory.max` is provably unchanged by
the second.

## Real, manual verification against a real, running container

Built the release binary and exercised the full real flow (a real
delegated `systemd --user` cgroup subtree, matching `ocirun ps`'s own
established test setup): confirmed `memory.max`/`pids.max` both start
at the kernel's own real default (`max`, unlimited); confirmed a real
`update --resources <file>` writes the exact requested values to the
real, live cgroup interface files; confirmed a container with no
`cgroupsPath` at all, an unknown container id, and invalid JSON all
fail with clear, real errors.

## Real, automated tests

Five integration tests in `tests/tests/ocirun_update.rs`, mirroring
`ocirun_ps.rs`'s own established real-cgroup test setup: real
memory/pids limits written to the real running container's own
cgroup; a second, partial update provably leaving an earlier-set field
untouched; reading resources from stdin (`--resources -`); the
no-cgroup error case; and the unknown-container error case.

## Not a hot-path change — no A/B perf re-verification needed

Purely additive: one new `Command` enum variant, one new, wholly
independent function (`cmd_update`). Confirmed directly via `git diff
--stat`: `cmd_run`/`cmd_create` and every hot-path function are
completely untouched.

## What's still not here

* Real runc's own individual ad-hoc flags (`--memory`, `--cpu-shares`,
  `--pids-limit`, ...) as an alternative to the JSON-file mode — a
  deliberate, documented scope limit, not attempted here.
* Persisting the update back into the container's own `config.json` —
  a later `ocirun state`/`inspect` still shows the limits the
  container was *created* with, not the updated ones. A real,
  documented gap (matching this project's own "narrow first
  increment, document what's not covered" pattern), not attempted
  here since it wasn't judged essential to the core "actually change
  the running limits" value this increment delivers.
* `ocirun pause`/`resume`, the build cache, `ONBUILD`/`HEALTHCHECK`, a
  symbolic `--chmod` mode — all still exactly as earlier increments
  left them, unrelated to this increment's own scope.
