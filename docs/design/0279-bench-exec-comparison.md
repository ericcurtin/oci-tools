# Design note 0279: `ci/bench.sh` exec comparison

Status: implemented
Scope: `ci/bench.sh`, `docs/benchmarks.md`.

## A hot path this script had never measured

Every other common verb (`run`, `run -d`, `rm`, `commit`, `build`) has
had its own `ci/bench.sh` section since `0264`. `exec` — a genuinely
hot, frequently-probed path (kubelet liveness/readiness probes route
through this exact machinery via `ocicri ExecSync`, `0240`) — never
did, despite being an obvious, real gap once actually noticed (a
research pass while scoping `0276` flagged it directly).

## Method

Both new sections (`ocirun`/`crun`/`runc exec`, and `ociman`/`podman`/
`docker exec`, one level up) follow a genuinely different shape than
every other section in this script: `exec` doesn't mutate its target
container's own running state the way `rm`/`commit` do, so each tool's
own real, persistent, long-running container (`sleep 60`, via
`create`+`start` for the low-level runtimes — left running in the
background exactly like `ocirun_lifecycle.rs`'s own tests already rely
on; `run -d` for the higher-level engines) is set up **once**, before
timing starts, and torn down together **once**, afterward — unlike the
per-sample `--prepare`/recreate pattern the `rm`/`run -d` sections use,
since those genuinely do need a fresh target every sample.

The low-level `ocirun`/`crun`/`runc` section reuses the exact same
rootless bundle setup the existing `run` section already established
(`ocirun spec --rootless --bundle`, a real busybox rootfs, the same
`ociVersion` cross-compat patch `0105` already documented), just with
`sleep 60` instead of `/bin/true` as the long-running process.

## A real bug found and fixed while wiring this up

The new sections were inserted *before* the pre-existing `run --rm`
section's own `image=docker.io/library/busybox:latest` variable
definition, but needed that same variable themselves for the
`ociman`/`podman`/`docker exec` half — caught immediately by running
the script end to end (`image: unbound variable`, `set -u` doing its
job), not by static review alone. Fixed by hoisting the one-line
`image=` assignment to the top of the script, alongside `workdir`/
`commit_tag`, removing the now-redundant second assignment further
down — a real, if small, reminder that even a small, mechanical-
looking script insertion needs a genuine end-to-end run, not just a
read-through, before it can be trusted.

## Verified

Ran `ci/bench.sh` end to end (all sections, not just the two new
ones) against real installed `crun`/`runc`/`podman`/`docker` on this
project's own aarch64 dev host — every pre-existing section's own
figures still measured cleanly (no regression from the insertion
point or the reordered `image=` assignment), and both new sections
produced real, sensible results:

- `ocirun exec`: 2.1ms vs `crun exec` 3.7ms (1.75×) vs `runc exec`
  19.2ms (9.08×).
- `ociman exec`: 2.9ms vs `podman exec` 138.1ms (47.86×) vs `docker
  exec` 47.5ms (16.47×).

`shellcheck ci/bench.sh` clean (fixed two `A && B || C` info-level
notes the first draft introduced, matching this same script's own
existing `if need X; then ... ; fi` convention used everywhere else
in it rather than the more failure-prone short-circuit idiom).

Host left exactly as found afterward: the real `podman`/`docker`
images/containers this manual run created (a real `podman commit`
re-commit series' own already-documented dangling-config byproduct,
`0176`/`0235`; a temporarily `podman pull`ed `busybox:latest`) were
cleaned up by hand once verification was complete, same as this
script's own existing, already-documented convention for that exact
byproduct.

Full workspace (unaffected by a shell-script-only change, run anyway
for completeness): `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.
</content>
</invoke>
